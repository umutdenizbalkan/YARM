// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! U9-REAP1 §3 — THE reap of a faulted task, as one no-allocation transaction over owners.
//!
//! # One policy, two owners
//!
//! [`run_reap_transaction`] is the only place that knows what reaping a faulted task means: which
//! steps run, in which order, under which rank, and under which condition. The broad NR31 handler
//! and the split NR31 route both reach it — `reap_faulted_task_noalloc_cleanup` is a thin caller
//! of this function through `BroadReapOwners`, and the split dispatcher reaches the same function
//! through `SharedReapOwners`. There is deliberately no second task-death or revocation
//! implementation: the owners supply acquisitions, never policy.
//!
//! # The discipline this transaction keeps
//!
//! * **No allocation.** Every snapshot is a fixed-capacity array sized by an existing `MAX_*`
//!   bound. Nothing grows, nothing boxes, nothing can fail for want of memory.
//! * **One rank at a time.** Every owner method takes and releases exactly one acquisition (or one
//!   legal vm(5)→memory(6) pair). No unrelated locks are ever held together and no step descends
//!   in rank while holding a higher one.
//! * **Snapshot under the claim, settle after it.** Reverse-link detachment, orphaned-sender
//!   settlement, TLB completion and frame reclaim all happen with no incompatible lock held —
//!   the discipline the base code already kept, preserved rather than invented.
//! * **Exact incarnation, never a numeric TID.** Every step is keyed on the `{tid, asid}` the
//!   claim won, so a TID recycled mid-transaction cannot be mistaken for the target.
//!
//! # Order, and why it is this order
//!
//! ```text
//!   rank 1   prove the target is on no runqueue and is no CPU's current   (refusal: pre-mutation)
//!   rank 2   CLAIM: Faulted|Exited -> Dead, take the restart token        (LINEARIZATION POINT)
//!   rank 3   sweep caller-side reply records          -> release -> detach each reverse link once
//!   rank 3   sweep replier-side reply records         -> release -> detach each reverse link once
//!   rank 3   detach every waiter this identity owns   -> release -> settle each orphan once
//!   rank 2   release the kernel context and stack
//!   rank 2   last-thread rule: any sibling not Dead? -> the process teardown below is SKIPPED
//!     rank 2   collect the process's DISTINCT address spaces
//!     vm5/6    per address space: drain -> release -> queue TLB work -> reclaim
//!     rank 3   purge the process's transfer envelopes, releasing one pin per shared region
//!     rank 3   snapshot active transfer mappings -> unmap (vm5/6) -> clear slot -> account
//!     rank 4   snapshot delegation links -> rank 2 resolve -> rank 4 clear the matching ones
//!     rank 4   reap the process cspace and its process record
//!   retire the reap claim                                                             (LAST)
//! ```
//!
//! The process-scoped half is gated on the EXISTING last-thread rule, so a shared CNode and a
//! shared address space survive for as long as any sibling does — the rule is read here, not
//! re-decided. Every cross-rank ordering above lives in this function and nowhere else, which is
//! what makes a divergence between the broad owner and the split owner inexpressible.

use crate::kernel::boot::KernelError;
use crate::kernel::boot::reap_claim::{ClosingReplyLink, ReapClaim, ReapRefusal};
use crate::kernel::boot::{
    ActiveTransferMapping, MAX_DELEGATED_CAPABILITY_LINKS, MAX_ENDPOINT_SENDER_WAITERS,
    MAX_REPLY_CAPS, MAX_TASKS, MAX_TRANSFER_ENVELOPES, SenderWaiter, TransferEnvelope,
};
use crate::kernel::capabilities::{CNodeId, CapObject};

use crate::kernel::scheduler::CpuId;
use crate::kernel::vm::{Asid, DrainedMapping, MAX_MAPPINGS};

/// The drained-mapping batch one address-space destroy hands from its drain phase to its reclaim
/// phase. Named here so the two owners cannot disagree about its shape.
pub(crate) type DrainedBatch = [Option<DrainedMapping>; MAX_MAPPINGS];

/// One delegation link as the rank-4 snapshot sees it: `(slot index, source_tid, dest_tid)`.
pub(crate) type DelegationEndpoints = (usize, u64, u64);

/// What one successful reap did. Every field is a count the transaction observed, never a target
/// it was told to hit — the §5 proofs compare these against full pre/post state by identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReapOutcome {
    pub(crate) claim: ReapClaim,
    pub(crate) caller_reply_records_revoked: usize,
    pub(crate) replier_reply_records_revoked: usize,
    pub(crate) reverse_links_detached: usize,
    pub(crate) orphaned_senders_settled: usize,
    /// `false` when a sibling of the process is still alive, so the whole process-scoped half was
    /// correctly skipped.
    pub(crate) process_reaped: bool,
    pub(crate) address_spaces_destroyed: usize,
    pub(crate) transfer_envelopes_purged: usize,
    pub(crate) transfer_mappings_unmapped: usize,
    pub(crate) delegation_links_removed: usize,
    pub(crate) cnode_space_removed: bool,
    pub(crate) process_record_removed: bool,
}

/// The acquisitions [`run_reap_transaction`] needs. Each method is ONE owner-local acquisition of
/// ONE rank (or one legal vm→memory pair) around a body shared with the other owner.
///
/// No method here decides anything: conditions, ordering and counting all live in the transaction.
pub(crate) trait ReapOwners {
    // ── rank 1 — scheduler ──────────────────────────────────────────────────────────────────
    /// Is `tid` some CPU's `current`, or queued on any runqueue? Read-only.
    fn target_is_scheduled(&self, tid: u64) -> bool;
    /// The CPUs that must acknowledge a shootdown for a destroyed address space: online, minus
    /// wake-only APs, which hold no user translations and would otherwise leak the retired-ASID
    /// slot forever.
    fn shootdown_cpu_bitmap(&self) -> u64;
    /// Queue one fire-and-forget TLB shootdown. A full queue is silenced, exactly as at base: the
    /// ASID is already retired and cannot be reused until every CPU acknowledges it.
    fn submit_tlb_shootdown(&mut self, cpu: CpuId, asid: Asid);

    // ── rank 2 — tasks ──────────────────────────────────────────────────────────────────────
    /// THE compare-and-claim. See `claim_faulted_task_for_reap_locked`.
    fn claim_faulted_task(&mut self, tid: u64) -> Result<ReapClaim, ReapRefusal>;
    /// Restore the exact incarnation a claim won, byte for byte.
    fn rollback_claim(&mut self, claim: &ReapClaim) -> bool;
    /// Does the exact `{tid, asid}` incarnation the claim names still exist?
    fn claim_is_live(&self, claim: &ReapClaim) -> bool;
    /// The last-thread rule: is any TCB of this process in a state other than `Dead`?
    fn process_has_live_threads(&self, pid: u64) -> bool;
    /// Fill `out` with the process's DISTINCT address spaces; returns how many.
    fn collect_process_asids(&self, pid: u64, out: &mut [Option<Asid>]) -> usize;
    /// The process a thread belongs to, or its numeric TID when it belongs to none.
    fn owner_pid_of(&self, tid: u64) -> u64;
    /// The address space a thread is bound to.
    fn task_asid(&self, tid: u64) -> Option<Asid>;
    /// Drop the kernel context, stack binding and per-thread metadata.
    fn release_kernel_context(&mut self, claim: &ReapClaim);

    // ── rank 3 — IPC ────────────────────────────────────────────────────────────────────────
    /// Sweep reply records whose CALLER side is this identity; `closing` collects reverse links.
    fn revoke_reply_caps_for_caller(
        &mut self,
        claim: &ReapClaim,
        closing: &mut [Option<ClosingReplyLink>],
    ) -> usize;
    /// Sweep reply records whose REPLIER side is this identity; `closing` collects reverse links.
    fn revoke_reply_caps_for_replier(
        &mut self,
        claim: &ReapClaim,
        closing: &mut [Option<ClosingReplyLink>],
    ) -> usize;
    /// Close one reverse link, with NO rank-3 claim held. Settles the waiting caller through the
    /// existing terminal owner, which wakes it exactly once.
    fn detach_reverse_link(&mut self, link: ClosingReplyLink) -> bool;
    /// Detach every waiter this identity owns; `orphaned` collects senders that still hold a
    /// transferred capability.
    fn clear_ipc_waiters(
        &mut self,
        claim: &ReapClaim,
        orphaned: &mut [Option<(SenderWaiter, usize)>],
    ) -> usize;
    /// Settle one orphaned blocking sender, with NO rank-3 claim held.
    fn settle_orphaned_sender(&mut self, waiter: &SenderWaiter, endpoint_idx: usize);
    /// Snapshot every live transfer envelope as `(slot index, envelope)`; returns how many.
    fn snapshot_transfer_envelopes(&self, out: &mut [Option<(usize, TransferEnvelope)>]) -> usize;
    /// Clear one transfer-envelope slot and account it, releasing a shared-region pin if asked.
    fn purge_transfer_envelope(&mut self, idx: usize, release_pin: Option<CapObject>);
    /// Snapshot every live active transfer mapping as `(slot index, mapping)`; returns how many.
    fn snapshot_active_transfer_mappings(
        &self,
        out: &mut [Option<(usize, ActiveTransferMapping)>],
    ) -> usize;
    /// Clear one active-transfer-mapping slot and account its released bytes.
    fn clear_active_transfer_mapping(&mut self, idx: usize, len: usize);

    // ── vm (5) → memory (6) ─────────────────────────────────────────────────────────────────
    /// Phase one of an address-space destroy: collect its mappings and retire the ASID.
    fn drain_address_space(
        &mut self,
        asid: Asid,
        pending_cpu_bitmap: u64,
    ) -> Result<DrainedBatch, KernelError>;
    /// Phase two: return the drained frames.
    fn reclaim_drained(&mut self, drained: DrainedBatch);
    /// Two-phase unmap of one active transfer mapping's range.
    fn unmap_transfer_range(&mut self, asid: Asid, base: usize, len: usize);

    // ── rank 4 — capabilities ───────────────────────────────────────────────────────────────
    /// Snapshot every live delegation link's endpoints; returns how many.
    fn snapshot_delegation_links(&self, out: &mut [Option<DelegationEndpoints>]) -> usize;
    /// Clear one delegation-link slot. Returns whether a live link was there to clear.
    fn clear_delegation_link(&mut self, idx: usize) -> bool;
    /// The CNode this process is bound to.
    fn process_cnode(&self, pid: u64) -> Option<CNodeId>;
    /// How many slots that CNode carries; reported, never acted on.
    fn cnode_slot_capacity(&self, cnode: CNodeId) -> usize;
    /// Reap the process cspace and its process record. Returns `(cspace removed, record removed)`.
    fn reap_process_cspace(&mut self, pid: u64, cnode: CNodeId) -> (bool, bool);

    // ── the claim's own retirement ──────────────────────────────────────────────────────────
    /// Retire the reap claim. Called LAST, after every owned step has settled, and only then is
    /// the target's identity free for reuse by a replacement.
    fn retire_reap_claim(&mut self, claim: ReapClaim);
}

/// U9-REAP1 §3 — reap the faulted task at `tid`, or refuse having mutated nothing.
///
/// The claim (rank 2) is the only reversible mutation, and the residency re-proof immediately
/// after it is the only step that reverses it. Past the first reply-record sweep there is no
/// rollback and no broad fallback, because the base path has none either and re-entering it would
/// double-sweep every record this one already retired.
pub(crate) fn run_reap_transaction<O: ReapOwners>(
    owners: &mut O,
    tid: u64,
) -> Result<ReapOutcome, ReapRefusal> {
    // (1) rank 1 — residency proof, BEFORE the claim so its refusal costs nothing.
    //
    // A task reaches `Faulted` only through `TaskTransition::FaultRunningCurrent`, which both the
    // broad path and `commit_terminal_fault_transition_shared` apply only AFTER clearing
    // `current`; nothing re-enqueues a terminal task, and a cross-CPU wake refuses one outright
    // (`CrossCpuWakeApplyResult::SkippedFaulted`). So this is a proof of absence, and finding
    // residency here means an invariant broke — refuse rather than tear down a running task.
    if owners.target_is_scheduled(tid) {
        return Err(ReapRefusal::StillScheduled);
    }

    // (2) rank 2 — THE LINEARIZATION POINT. Exactly one of reap / restart / exit / duplicate reap
    // wins here; every loser returns a typed refusal with zero mutation.
    let claim = owners.claim_faulted_task(tid)?;

    // (2b) The proof in (1) was taken before the claim existed, so re-take it against the claim's
    // exact incarnation. This is the LAST point at which the claim can be cleanly undone: nothing
    // below has written anything yet.
    if owners.target_is_scheduled(claim.tid()) {
        owners.rollback_claim(&claim);
        return Err(ReapRefusal::StillScheduled);
    }

    let mut outcome = ReapOutcome {
        claim,
        caller_reply_records_revoked: 0,
        replier_reply_records_revoked: 0,
        reverse_links_detached: 0,
        orphaned_senders_settled: 0,
        process_reaped: false,
        address_spaces_destroyed: 0,
        transfer_envelopes_purged: 0,
        transfer_mappings_unmapped: 0,
        delegation_links_removed: 0,
        cnode_space_removed: false,
        process_record_removed: false,
    };

    // (3) rank 3 — reply records, caller side then replier side. Each sweep snapshots the reverse
    // links it retires UNDER the claim and detaches them AFTER releasing it, so a caller awaiting
    // this target is settled with the canonical server-death result through the existing terminal
    // owner, and is woken exactly once.
    let mut caller_closing: [Option<ClosingReplyLink>; MAX_REPLY_CAPS] = [None; MAX_REPLY_CAPS];
    outcome.caller_reply_records_revoked =
        owners.revoke_reply_caps_for_caller(&claim, &mut caller_closing);
    for link in caller_closing.into_iter().flatten() {
        if owners.detach_reverse_link(link) {
            outcome.reverse_links_detached += 1;
        }
    }

    let mut replier_closing: [Option<ClosingReplyLink>; MAX_REPLY_CAPS] = [None; MAX_REPLY_CAPS];
    outcome.replier_reply_records_revoked =
        owners.revoke_reply_caps_for_replier(&claim, &mut replier_closing);
    for link in replier_closing.into_iter().flatten() {
        if owners.detach_reverse_link(link) {
            outcome.reverse_links_detached += 1;
        }
    }

    // (4) rank 3 — waiters. Endpoint receive waiter by exact identity, endpoint send waiters and
    // notification waiters by TID, each detached exactly once. An orphaned sender still owns a
    // transferred capability (and, for a shared-region transfer, one transient pin), so it is
    // settled sequentially after the claim releases — never under it.
    let mut orphaned: [Option<(SenderWaiter, usize)>; MAX_ENDPOINT_SENDER_WAITERS] =
        [const { None }; MAX_ENDPOINT_SENDER_WAITERS];
    let orphaned_n = owners.clear_ipc_waiters(&claim, &mut orphaned);
    for idx in 0..orphaned_n {
        let Some((waiter, endpoint_idx)) = orphaned[idx] else {
            continue;
        };
        owners.settle_orphaned_sender(&waiter, endpoint_idx);
        outcome.orphaned_senders_settled += 1;
    }

    // (5) rank 2 — the kernel context, stack binding and per-thread metadata, exactly once.
    owners.release_kernel_context(&claim);

    // (6) The process-scoped half, under the EXISTING last-thread rule. A sibling that is not
    // `Dead` keeps the shared CNode and the shared address space, and this whole block is skipped.
    let pid = claim.pid();
    if !owners.process_has_live_threads(pid) {
        outcome.process_reaped = true;
        reap_process_resources(owners, pid, &mut outcome);
    }

    // (7) LAST — retire the claim. Only now is the target's identity free.
    owners.retire_reap_claim(claim);
    Ok(outcome)
}

/// The process-scoped half of the reap. Split out for readability only: it is one straight-line
/// continuation of [`run_reap_transaction`] and is reached from nowhere else.
fn reap_process_resources<O: ReapOwners>(owners: &mut O, pid: u64, outcome: &mut ReapOutcome) {
    // (6a) rank 2 — the process's distinct address spaces, into a fixed buffer.
    let mut asids: [Option<Asid>; MAX_TASKS] = [None; MAX_TASKS];
    let asid_n = owners.collect_process_asids(pid, &mut asids);

    // (6b) Per address space, the two-phase-unmap contract in its required order: drain (which
    // removes the PTEs and retires the ASID) under vm(5)→memory(6), RELEASE, queue the TLB
    // shootdown work at rank 3, and only then reclaim the frames under vm(5)→memory(6) again.
    // Reclaiming before the shootdown is queued would be the one ordering that could hand a frame
    // back while a remote CPU still holds a translation for it.
    let bitmap = owners.shootdown_cpu_bitmap();
    for asid in asids.into_iter().take(asid_n).flatten() {
        let Ok(drained) = owners.drain_address_space(asid, bitmap) else {
            continue;
        };
        for cpu in 0..u64::BITS as usize {
            if (bitmap & (1u64 << cpu)) == 0 {
                continue;
            }
            owners.submit_tlb_shootdown(CpuId(cpu as u8), asid);
        }
        owners.reclaim_drained(drained);
        outcome.address_spaces_destroyed += 1;
    }

    // (6c) rank 3 — transfer envelopes this process is either end of. A shared-region envelope
    // holds one pin, released with it.
    let mut envelopes: [Option<(usize, TransferEnvelope)>; MAX_TRANSFER_ENVELOPES] =
        [None; MAX_TRANSFER_ENVELOPES];
    let envelope_n = owners.snapshot_transfer_envelopes(&mut envelopes);
    for idx in 0..envelope_n {
        let Some((slot, envelope)) = envelopes[idx] else {
            continue;
        };
        let source_pid = owners.owner_pid_of(envelope.source_tid.0);
        let receiver_pid = envelope.receiver_tid.map(|tid| owners.owner_pid_of(tid.0));
        let source_matches = source_pid == pid || envelope.source_tid.0 == pid;
        let receiver_matches = receiver_pid == Some(pid)
            || envelope.receiver_tid == Some(crate::kernel::ipc::ThreadId(pid));
        if !source_matches && !receiver_matches {
            continue;
        }
        let release_pin = envelope
            .shared_region
            .is_some()
            .then_some(envelope.source_object);
        owners.purge_transfer_envelope(slot, release_pin);
        outcome.transfer_envelopes_purged += 1;
    }

    // (6d) rank 3 snapshot -> vm5/6 unmap -> rank 3 clear. Deliberately NOT a capability revoke:
    // the base reap does not revoke the transfer capability here (its sibling
    // `purge_active_transfer_mappings_for_pid` does, and is not on this path), so neither does
    // this. That is the whole reason §1's recomputation found no general-revocation branch
    // reachable from a faulted-task reap.
    let mut mappings: [Option<(usize, ActiveTransferMapping)>; MAX_TRANSFER_ENVELOPES] =
        [None; MAX_TRANSFER_ENVELOPES];
    let mapping_n = owners.snapshot_active_transfer_mappings(&mut mappings);
    for idx in 0..mapping_n {
        let Some((slot, mapping)) = mappings[idx] else {
            continue;
        };
        let owner_pid = owners.owner_pid_of(mapping.owner_tid.0);
        if owner_pid != pid && mapping.owner_tid.0 != pid {
            continue;
        }
        if let Some(asid) = owners.task_asid(mapping.owner_tid.0) {
            owners.unmap_transfer_range(asid, mapping.base.0 as usize, mapping.len);
        }
        owners.clear_active_transfer_mapping(slot, mapping.len);
        outcome.transfer_mappings_unmapped += 1;
    }

    // (6e/6f) rank 4 — delegation links, then the cspace itself. A process with no CNode has
    // nothing left to reap, and that is a success, not a failure.
    let Some(cnode) = owners.process_cnode(pid) else {
        return;
    };
    let _capacity = owners.cnode_slot_capacity(cnode);

    // Snapshot at rank 4, RELEASE, resolve each endpoint's process at rank 2, then clear the
    // matching slots at rank 4. The resolution never happens inside the rank-4 claim, so this
    // sweep cannot descend from capability(4) to task(2) while holding a lock.
    let mut links: [Option<DelegationEndpoints>; MAX_DELEGATED_CAPABILITY_LINKS] =
        [None; MAX_DELEGATED_CAPABILITY_LINKS];
    let link_n = owners.snapshot_delegation_links(&mut links);
    for idx in 0..link_n {
        let Some((slot, source_tid, dest_tid)) = links[idx] else {
            continue;
        };
        if owners.owner_pid_of(source_tid) != pid && owners.owner_pid_of(dest_tid) != pid {
            continue;
        }
        if owners.clear_delegation_link(slot) {
            outcome.delegation_links_removed = outcome.delegation_links_removed.saturating_add(1);
        }
    }

    let (cspace, record) = owners.reap_process_cspace(pid, cnode);
    outcome.cnode_space_removed = cspace;
    outcome.process_record_removed = record;
}

// ═══════════════════════════════════════════════════════════════════════════════════════════
// The two owners. Neither contains policy: each is a set of acquisitions around the shared
// `*_locked` bodies in `kernel::boot::reap_claim`, so the broad NR31 and the split NR31 run the
// SAME transaction over the SAME code and cannot drift.
// ═══════════════════════════════════════════════════════════════════════════════════════════

use crate::kernel::boot::reap_claim as body;

/// The broad-lock owner. Every method here already runs under the global `SpinLock<KernelState>`,
/// so its "acquisitions" are the existing `with_*` accessors — unchanged in cost and in order.
pub(crate) struct BroadReapOwners<'a> {
    pub(crate) kernel: &'a mut crate::kernel::boot::KernelState,
}

impl ReapOwners for BroadReapOwners<'_> {
    fn target_is_scheduled(&self, tid: u64) -> bool {
        self.kernel.task_present_in_any_runqueue(tid)
    }
    fn shootdown_cpu_bitmap(&self) -> u64 {
        // Stage 183.5's exclusion, preserved exactly: a wake-only online AP holds no user
        // translation for this ASID, and including it would leave the retired-ASID slot pending
        // forever.
        self.kernel.online_cpu_bitmap() & !self.kernel.wake_only_cpu_bitmap()
    }
    fn submit_tlb_shootdown(&mut self, cpu: CpuId, asid: Asid) {
        let _ = self.kernel.submit_cross_cpu_work(
            cpu,
            crate::kernel::smp::WorkItem::TlbShootdown {
                asid,
                va_range: None,
                requester: None,
                sequence: 0,
            },
        );
    }

    fn claim_faulted_task(&mut self, tid: u64) -> Result<ReapClaim, ReapRefusal> {
        self.kernel
            .with_tcbs_mut(|tcbs| body::claim_faulted_task_for_reap_locked(tcbs, tid))
    }
    fn rollback_claim(&mut self, claim: &ReapClaim) -> bool {
        self.kernel
            .with_tcbs_mut(|tcbs| body::rollback_reap_claim_locked(tcbs, claim))
    }
    fn claim_is_live(&self, claim: &ReapClaim) -> bool {
        self.kernel
            .with_tcbs(|tcbs| body::claim_incarnation_is_live_locked(tcbs, claim))
    }
    fn process_has_live_threads(&self, pid: u64) -> bool {
        self.kernel
            .with_tcbs(|tcbs| body::process_has_live_threads_locked(tcbs, pid))
    }
    fn collect_process_asids(&self, pid: u64, out: &mut [Option<Asid>]) -> usize {
        self.kernel
            .with_tcbs(|tcbs| body::collect_process_asids_locked(tcbs, pid, out))
    }
    fn owner_pid_of(&self, tid: u64) -> u64 {
        self.kernel
            .with_tcbs(|tcbs| body::owner_pid_of_locked(tcbs, tid))
    }
    fn task_asid(&self, tid: u64) -> Option<Asid> {
        self.kernel
            .with_tcbs(|tcbs| body::task_asid_locked(tcbs, tid))
    }
    fn release_kernel_context(&mut self, claim: &ReapClaim) {
        let _ = self.kernel.release_kernel_context(claim.tid());
    }

    fn revoke_reply_caps_for_caller(
        &mut self,
        claim: &ReapClaim,
        closing: &mut [Option<ClosingReplyLink>],
    ) -> usize {
        let identity = claim.identity();
        self.kernel.with_ipc_state_mut(|ipc| {
            body::revoke_reply_caps_for_caller_identity_locked(ipc, identity, closing)
        })
    }
    fn revoke_reply_caps_for_replier(
        &mut self,
        claim: &ReapClaim,
        closing: &mut [Option<ClosingReplyLink>],
    ) -> usize {
        let identity = claim.identity();
        self.kernel.with_ipc_state_mut(|ipc| {
            body::revoke_reply_caps_for_replier_identity_locked(ipc, identity, closing)
        })
    }
    fn detach_reverse_link(&mut self, link: ClosingReplyLink) -> bool {
        let (stid, sasid, idx, generation) = link;
        self.kernel
            .detach_server_reply_link_exact(stid, sasid, idx, generation)
            .detached()
    }
    fn clear_ipc_waiters(
        &mut self,
        claim: &ReapClaim,
        orphaned: &mut [Option<(SenderWaiter, usize)>],
    ) -> usize {
        let identity = claim.identity();
        self.kernel.with_ipc_state_mut(|ipc| {
            body::clear_ipc_waiters_for_identity_locked(ipc, identity, orphaned)
        })
    }
    fn settle_orphaned_sender(&mut self, waiter: &SenderWaiter, endpoint_idx: usize) {
        let _ = self
            .kernel
            .settle_blocked_sender_envelope(waiter, endpoint_idx);
    }
    fn snapshot_transfer_envelopes(&self, out: &mut [Option<(usize, TransferEnvelope)>]) -> usize {
        self.kernel
            .with_ipc_state(|ipc| snapshot_envelopes_locked(ipc, out))
    }
    fn purge_transfer_envelope(&mut self, idx: usize, release_pin: Option<CapObject>) {
        if let Some(object) = release_pin {
            self.kernel.adjust_memory_object_pin_refcount(object, -1);
        }
        self.kernel.with_ipc_state_mut(|ipc| {
            ipc.transfer_envelopes[idx] = None;
        });
        self.kernel.note_transfer_record_revoked();
    }
    fn snapshot_active_transfer_mappings(
        &self,
        out: &mut [Option<(usize, ActiveTransferMapping)>],
    ) -> usize {
        self.kernel
            .with_ipc_state(|ipc| snapshot_mappings_locked(ipc, out))
    }
    fn clear_active_transfer_mapping(&mut self, idx: usize, len: usize) {
        self.kernel.with_ipc_state_mut(|ipc| {
            ipc.active_transfer_mappings[idx] = None;
        });
        self.kernel.note_shared_mem_released(len);
        self.kernel.note_transfer_record_revoked();
    }

    fn drain_address_space(
        &mut self,
        asid: Asid,
        pending_cpu_bitmap: u64,
    ) -> Result<DrainedBatch, KernelError> {
        self.kernel.with_vm_then_memory_mut(|vm, memory| {
            crate::kernel::boot::vm_image_locked::drain_address_space_locked(
                vm,
                memory,
                asid,
                pending_cpu_bitmap,
            )
        })
    }
    fn reclaim_drained(&mut self, drained: DrainedBatch) {
        self.kernel.with_vm_then_memory_mut(|vm, memory| {
            crate::kernel::boot::vm_image_locked::reclaim_drained_mappings_locked(
                vm, memory, drained,
            );
        });
    }
    fn unmap_transfer_range(&mut self, asid: Asid, base: usize, len: usize) {
        self.kernel.unmap_range_two_phase(asid, base, len);
    }

    fn snapshot_delegation_links(&self, out: &mut [Option<DelegationEndpoints>]) -> usize {
        self.kernel
            .with_capability_state(|capability| snapshot_delegation_links_locked(capability, out))
    }
    fn clear_delegation_link(&mut self, idx: usize) -> bool {
        self.kernel.with_capability_state_mut(|capability| {
            let present = capability.delegated_capability_links[idx].is_some();
            capability.delegated_capability_links[idx] = None;
            present
        })
    }
    fn process_cnode(&self, pid: u64) -> Option<CNodeId> {
        self.kernel
            .with_capability_state(|capability| body::process_cnode_for_pid_locked(capability, pid))
    }
    fn cnode_slot_capacity(&self, cnode: CNodeId) -> usize {
        self.kernel
            .with_capability_state(|capability| body::cnode_slot_capacity_locked(capability, cnode))
    }
    fn reap_process_cspace(&mut self, pid: u64, cnode: CNodeId) -> (bool, bool) {
        self.kernel.with_capability_state_mut(|capability| {
            crate::kernel::boot::cnode_state::reap_process_cspace_locked(capability, pid, cnode)
        })
    }

    fn retire_reap_claim(&mut self, claim: ReapClaim) {
        emit_reap_claim_retired(&claim);
    }
}

/// The split owner. Each method takes exactly the one domain lock its body needs, through the
/// existing `with_*_split_mut` seams, with the global `SpinLock<KernelState>` already released.
pub(crate) struct SharedReapOwners<'a> {
    pub(crate) shared: &'a crate::runtime::SharedKernel,
}

impl ReapOwners for SharedReapOwners<'_> {
    fn target_is_scheduled(&self, tid: u64) -> bool {
        self.shared
            .receiver_has_scheduler_membership_split_read(tid)
    }
    fn shootdown_cpu_bitmap(&self) -> u64 {
        self.shared.reap_shootdown_cpu_bitmap_split()
    }
    fn submit_tlb_shootdown(&mut self, cpu: CpuId, asid: Asid) {
        self.shared.reap_submit_tlb_shootdown_split(cpu, asid);
    }

    fn claim_faulted_task(&mut self, tid: u64) -> Result<ReapClaim, ReapRefusal> {
        self.shared
            .with_task_tcbs_split_mut(|tcbs| body::claim_faulted_task_for_reap_locked(tcbs, tid))
    }
    fn rollback_claim(&mut self, claim: &ReapClaim) -> bool {
        self.shared
            .with_task_tcbs_split_mut(|tcbs| body::rollback_reap_claim_locked(tcbs, claim))
    }
    fn claim_is_live(&self, claim: &ReapClaim) -> bool {
        self.shared
            .with_task_tcbs_split_mut(|tcbs| body::claim_incarnation_is_live_locked(tcbs, claim))
    }
    fn process_has_live_threads(&self, pid: u64) -> bool {
        self.shared
            .with_task_tcbs_split_mut(|tcbs| body::process_has_live_threads_locked(tcbs, pid))
    }
    fn collect_process_asids(&self, pid: u64, out: &mut [Option<Asid>]) -> usize {
        self.shared
            .with_task_tcbs_split_mut(|tcbs| body::collect_process_asids_locked(tcbs, pid, out))
    }
    fn owner_pid_of(&self, tid: u64) -> u64 {
        self.shared
            .with_task_tcbs_split_mut(|tcbs| body::owner_pid_of_locked(tcbs, tid))
    }
    fn task_asid(&self, tid: u64) -> Option<Asid> {
        self.shared
            .with_task_tcbs_split_mut(|tcbs| body::task_asid_locked(tcbs, tid))
    }
    fn release_kernel_context(&mut self, claim: &ReapClaim) {
        let _ = self.shared.with_task_tcbs_split_mut(|tcbs| {
            crate::kernel::boot::thread_state::release_kernel_context_locked(tcbs, claim.tid())
        });
    }

    fn revoke_reply_caps_for_caller(
        &mut self,
        claim: &ReapClaim,
        closing: &mut [Option<ClosingReplyLink>],
    ) -> usize {
        let identity = claim.identity();
        self.shared.with_ipc_split_mut(|ipc| {
            body::revoke_reply_caps_for_caller_identity_locked(ipc, identity, closing)
        })
    }
    fn revoke_reply_caps_for_replier(
        &mut self,
        claim: &ReapClaim,
        closing: &mut [Option<ClosingReplyLink>],
    ) -> usize {
        let identity = claim.identity();
        self.shared.with_ipc_split_mut(|ipc| {
            body::revoke_reply_caps_for_replier_identity_locked(ipc, identity, closing)
        })
    }
    fn detach_reverse_link(&mut self, link: ClosingReplyLink) -> bool {
        let (stid, sasid, idx, generation) = link;
        self.shared
            .unregister_server_reply_link_split(stid, sasid, idx, generation)
    }
    fn clear_ipc_waiters(
        &mut self,
        claim: &ReapClaim,
        orphaned: &mut [Option<(SenderWaiter, usize)>],
    ) -> usize {
        let identity = claim.identity();
        self.shared.with_ipc_split_mut(|ipc| {
            body::clear_ipc_waiters_for_identity_locked(ipc, identity, orphaned)
        })
    }
    fn settle_orphaned_sender(&mut self, waiter: &SenderWaiter, endpoint_idx: usize) {
        // The resolution — which envelope this waiter's handle names, that it belongs to THIS
        // endpoint, and which task must clean it up — is the shared body both owners run. Only
        // the consume differs, and each side uses its own EXISTING production settle owner
        // (U6 built the split one for exactly this job), so no third settle path is introduced.
        let resolved = self.shared.with_ipc_split_mut(|ipc| {
            body::resolve_orphaned_sender_envelope_locked(ipc, waiter, endpoint_idx)
        });
        let Some((handle, cleanup_tid)) = resolved else {
            return;
        };
        self.shared
            .settle_blocked_send_envelope_split(handle, endpoint_idx, cleanup_tid);
    }
    fn snapshot_transfer_envelopes(&self, out: &mut [Option<(usize, TransferEnvelope)>]) -> usize {
        self.shared
            .with_ipc_split_mut(|ipc| snapshot_envelopes_locked(ipc, out))
    }
    fn purge_transfer_envelope(&mut self, idx: usize, release_pin: Option<CapObject>) {
        if let Some(object) = release_pin {
            self.shared.with_memory_split_mut(|memory| {
                crate::kernel::boot::KernelState::adjust_memory_object_pin_refcount_locked(
                    memory, object, -1,
                );
            });
        }
        self.shared.with_ipc_split_mut(|ipc| {
            ipc.transfer_envelopes[idx] = None;
            ipc.telemetry.transfer_records_revoked =
                ipc.telemetry.transfer_records_revoked.saturating_add(1);
        });
    }
    fn snapshot_active_transfer_mappings(
        &self,
        out: &mut [Option<(usize, ActiveTransferMapping)>],
    ) -> usize {
        self.shared
            .with_ipc_split_mut(|ipc| snapshot_mappings_locked(ipc, out))
    }
    fn clear_active_transfer_mapping(&mut self, idx: usize, len: usize) {
        self.shared.with_ipc_split_mut(|ipc| {
            ipc.active_transfer_mappings[idx] = None;
            ipc.telemetry.shared_mem_bytes_released = ipc
                .telemetry
                .shared_mem_bytes_released
                .saturating_add(len as u64);
            ipc.telemetry.transfer_records_revoked =
                ipc.telemetry.transfer_records_revoked.saturating_add(1);
        });
    }

    fn drain_address_space(
        &mut self,
        asid: Asid,
        pending_cpu_bitmap: u64,
    ) -> Result<DrainedBatch, KernelError> {
        self.shared.with_vm_then_memory_split_mut(|vm, memory| {
            crate::kernel::boot::vm_image_locked::drain_address_space_locked(
                vm,
                memory,
                asid,
                pending_cpu_bitmap,
            )
        })
    }
    fn reclaim_drained(&mut self, drained: DrainedBatch) {
        self.shared.with_vm_then_memory_split_mut(|vm, memory| {
            crate::kernel::boot::vm_image_locked::reclaim_drained_mappings_locked(
                vm, memory, drained,
            );
        });
    }
    fn unmap_transfer_range(&mut self, asid: Asid, base: usize, len: usize) {
        self.shared.unmap_range_two_phase_split(asid, base, len);
    }

    fn snapshot_delegation_links(&self, out: &mut [Option<DelegationEndpoints>]) -> usize {
        self.shared.with_capability_state_split_mut(|capability| {
            snapshot_delegation_links_locked(capability, out)
        })
    }
    fn clear_delegation_link(&mut self, idx: usize) -> bool {
        self.shared.with_capability_state_split_mut(|capability| {
            let present = capability.delegated_capability_links[idx].is_some();
            capability.delegated_capability_links[idx] = None;
            present
        })
    }
    fn process_cnode(&self, pid: u64) -> Option<CNodeId> {
        self.shared.with_capability_state_split_mut(|capability| {
            body::process_cnode_for_pid_locked(capability, pid)
        })
    }
    fn cnode_slot_capacity(&self, cnode: CNodeId) -> usize {
        self.shared.with_capability_state_split_mut(|capability| {
            body::cnode_slot_capacity_locked(capability, cnode)
        })
    }
    fn reap_process_cspace(&mut self, pid: u64, cnode: CNodeId) -> (bool, bool) {
        self.shared.with_capability_state_split_mut(|capability| {
            crate::kernel::boot::cnode_state::reap_process_cspace_locked(capability, pid, cnode)
        })
    }

    fn retire_reap_claim(&mut self, claim: ReapClaim) {
        emit_reap_claim_retired(&claim);
    }
}

// ── shared rank-local snapshot bodies ──────────────────────────────────────────────────────

fn snapshot_envelopes_locked(
    ipc: &crate::kernel::boot::IpcSubsystem,
    out: &mut [Option<(usize, TransferEnvelope)>],
) -> usize {
    let mut n = 0usize;
    for idx in 0..MAX_TRANSFER_ENVELOPES {
        let Some(envelope) = ipc.transfer_envelopes[idx] else {
            continue;
        };
        if n < out.len() {
            out[n] = Some((idx, envelope));
            n += 1;
        }
    }
    n
}

fn snapshot_mappings_locked(
    ipc: &crate::kernel::boot::IpcSubsystem,
    out: &mut [Option<(usize, ActiveTransferMapping)>],
) -> usize {
    let mut n = 0usize;
    for idx in 0..MAX_TRANSFER_ENVELOPES {
        let Some(mapping) = ipc.active_transfer_mappings[idx] else {
            continue;
        };
        if n < out.len() {
            out[n] = Some((idx, mapping));
            n += 1;
        }
    }
    n
}

fn snapshot_delegation_links_locked(
    capability: &crate::kernel::boot::CapabilitySubsystem,
    out: &mut [Option<DelegationEndpoints>],
) -> usize {
    let mut n = 0usize;
    for idx in 0..MAX_DELEGATED_CAPABILITY_LINKS {
        let Some(record) = capability.delegated_capability_links[idx] else {
            continue;
        };
        if n < out.len() {
            out[n] = Some((idx, record.source_tid, record.dest_tid));
            n += 1;
        }
    }
    n
}

/// The claim's retirement marker. Emitted from BOTH owners so a log can never tell the split route
/// apart from the broad one by its absence — the oracle-blindness trap U9-SPAWN-TXN3 and U9-FORK1
/// each had to fix once already.
fn emit_reap_claim_retired(claim: &ReapClaim) {
    crate::yarm_log!(
        "TASK_REAP_CLAIM_RETIRED tid={} asid={} pid={} restart_token={}",
        claim.tid(),
        claim.sweep_asid().0,
        claim.pid(),
        claim.restart_token().map_or(0, |token| token.0)
    );
}
