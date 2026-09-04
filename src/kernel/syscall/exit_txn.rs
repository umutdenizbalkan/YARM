// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! U9-EXIT1 §3 — the self-exit as one transaction over owners, ending in a queue advance.
//!
//! # Why this is not the reap transaction
//!
//! U9-REAP1 §1's ledger is the reason. A reap and an exit share their *thread-scoped* cleanup —
//! the two reply-record sweeps and the waiter detach, which live once in `kernel::boot::reap_claim`
//! and are driven here through the same `*_locked` bodies. Everything else differs, and the
//! differences are semantic rather than incidental: the exit writes `Exited(code)` where the reap
//! writes `Dead`, MINTS a restart token where the reap CLEARS one, owes a server-death deferral the
//! reap has no analogue for, owes supervisor/PM/joiner/futex work the reap never does, and — unlike
//! the reap, whose target is already off every CPU — must remove *itself* from `current` and hand
//! the CPU to somebody else.
//!
//! Reusing `run_reap_transaction` here would have written the wrong terminal status, skipped the
//! deferral and the owed wakes, and substituted the reap's process teardown for the exit's, which
//! is materially narrower (it reaches no general capability revoke). So the claim and the owed work
//! are this module's; the shared cleanup is REAP1's; and a guard pins that neither drifts into the
//! other.
//!
//! # Order, and why it is this order
//!
//! ```text
//!   pre-lock  admit: this CPU has a drainer and is the authoritative dispatcher
//!   rank 2    preflight: identity, Joinable, Running                       (refusal: free)
//!   rank 2    does this thread still owe a reply?  -> capacity probe       (refusal: free)
//!   ——        RESERVE the queue-advance deferral                          (refusal: free)
//!   ——        RESERVE the server-death slot, if a reply is owed            (refusal: free)
//!   rank 1    clear `current` — the task is now in no run queue and no CPU's current
//!   rank 2    CLAIM: Running -> Exited(code), install token                (LINEARIZATION POINT)
//!   rank 2    detach the exact reverse link -> publish the deferred completion
//!   rank 3    sweep caller-side reply records     -> release -> detach each link once
//!   rank 3    sweep replier-side records EXCEPT the detached one -> release -> detach once
//!   rank 3    detach every waiter this identity owns -> release -> settle each orphan once
//!   rank 3/1  report the exit to the supervisor and to PM (send, then wake)
//!   rank 1/2  robust-futex wakes, then joiner wakes
//!   ——        return QueueAdvanceCommitted; the EXISTING drain selects and applies the next task
//! ```
//!
//! **Both reservations precede the first irreversible step**, which is what makes every refusal
//! above them free and every step below them owed rather than optional. `QueueAdvanceCommitted` is
//! returned only with a live queue-advance reservation, so the drain is guaranteed to run.
//!
//! Past the claim there is no broad fallback and no return to the exiting PC: re-entering
//! `handle_exit_current_task` would mint a second token, publish a second disposition and sweep
//! records this transaction already retired. A cleanup step that fails therefore fails **closed** —
//! the task stays terminal and off the CPU, and the advance still happens.

use crate::kernel::boot::exit_claim::{
    ExitClaim, ExitPreflight, ExitRefusal, exit_preflight_locked,
};
use crate::kernel::boot::reap_claim::ClosingReplyLink;
use crate::kernel::boot::{
    MAX_ENDPOINT_SENDER_WAITERS, MAX_REPLY_CAPS, MAX_TASKS, SenderWaiter,
    ServerDeathWorkReservation,
};
use crate::kernel::scheduler::CpuId;
use crate::kernel::task::{RestartToken, ServerReplyLink};
use crate::kernel::vm::Asid;

/// What one committed self-exit did. Every field is a count the transaction observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ExitOutcome {
    pub(crate) claim: ExitClaim,
    pub(crate) caller_reply_records_revoked: usize,
    pub(crate) replier_reply_records_revoked: usize,
    pub(crate) reverse_links_detached: usize,
    pub(crate) orphaned_senders_settled: usize,
    /// `true` when a reverse link was detached and its deferred completion published — the blocked
    /// caller will be settled with `ServerGone` by the existing drain, exactly once.
    pub(crate) server_death_published: bool,
    pub(crate) joiners_woken: usize,
    pub(crate) supervisor_reported: bool,
    pub(crate) pm_reported: bool,
}

/// The acquisitions [`run_exit_transaction`] needs. Each method is ONE owner-local acquisition of
/// ONE rank around a body shared with the broad path.
///
/// No method here decides anything: conditions, ordering and counting live in the transaction.
pub(crate) trait ExitOwners {
    // ── admission ───────────────────────────────────────────────────────────────────────────
    /// Is this CPU running a post-lock drainer AND the authoritative dispatcher? Pre-mutation.
    fn exit_route_admitted(&self, cpu: CpuId) -> bool;

    // ── rank 1 — scheduler ──────────────────────────────────────────────────────────────────
    /// This CPU's `current`, read authoritatively from scheduler state — never from userspace.
    fn current_tid_on_cpu(&self, cpu: CpuId) -> Option<u64>;
    /// Clear `current` on this CPU, returning what was removed. The task is NOT re-enqueued, so
    /// between this and the claim it is in no run queue and is no CPU's current.
    fn clear_current(&mut self, cpu: CpuId) -> Option<u64>;
    /// Enqueue a woken task. Used only for joiners and robust-futex waiters — never for the
    /// exiting task, which this transaction must never make runnable again.
    fn enqueue_woken(&mut self, tid: u64);

    // ── rank 2 — tasks ──────────────────────────────────────────────────────────────────────
    /// The exiting thread's own facts, read-only.
    fn exit_preflight(&self, tid: u64) -> Result<ExitPreflight, ExitRefusal>;
    /// Did this thread publish a robust-futex list? Its wakes have no split lock domain, so a
    /// thread that has one is refused before any mutation rather than served here.
    fn has_robust_futex_list(&self, tid: u64) -> bool;
    /// Mint the next restart token. The exit installs a fresh one; it takes none.
    fn mint_restart_token(&mut self) -> RestartToken;
    /// THE compare-and-claim. See `claim_self_exit_locked`.
    fn claim_self_exit(
        &mut self,
        cpu: CpuId,
        tid: u64,
        asid: Option<Asid>,
        code: u64,
        token: RestartToken,
    ) -> Result<ExitClaim, ExitRefusal>;
    /// Restore the exact incarnation a claim won, byte for byte.
    fn rollback_claim(&mut self, claim: &ExitClaim) -> bool;
    /// Does this exact incarnation still hold a reverse link? Pre-mutation probe.
    fn server_reply_link_present(&self, tid: u64, asid: Asid) -> bool;
    /// Detach this incarnation's reverse link, by exact identity.
    fn take_server_reply_link(&mut self, tid: u64, asid: Asid) -> Option<ServerReplyLink>;
    /// Wake every joiner blocked on this thread; returns how many TIDs were written to `out`.
    fn wake_joiners(&mut self, target_tid: u64, out: &mut [Option<u64>]) -> usize;

    // ── the server-death deferral ───────────────────────────────────────────────────────────
    /// Is a deferred slot free on this CPU? Non-mutating.
    fn server_death_capacity_available(&self, cpu: CpuId) -> bool;
    /// Reserve one slot. `None` means the queue is full and NOTHING may be detached.
    fn reserve_server_death(&mut self, cpu: CpuId) -> Option<ServerDeathWorkReservation>;
    /// Publish the completion into the held reservation. `false` collapses a duplicate.
    fn publish_server_death(
        &mut self,
        reservation: ServerDeathWorkReservation,
        tid: u64,
        asid: Asid,
        link: ServerReplyLink,
    ) -> bool;
    /// Release a held reservation that turned out to owe nothing.
    fn release_server_death(&mut self, reservation: ServerDeathWorkReservation);

    // ── the queue-advance deferral ──────────────────────────────────────────────────────────
    /// Reserve the ONE queue-advance deferral for this CPU. `false` means another route holds it.
    fn reserve_queue_advance(&mut self, cpu: CpuId, outgoing: u64) -> bool;
    /// Release it. Only ever called on a pre-mutation refusal path.
    fn release_queue_advance(&mut self, cpu: CpuId);

    // ── rank 3 — IPC (SHARED with U9-REAP1) ─────────────────────────────────────────────────
    fn revoke_reply_caps_for_caller(
        &mut self,
        claim: &ExitClaim,
        closing: &mut [Option<ClosingReplyLink>],
    ) -> usize;
    fn revoke_reply_caps_for_replier_except(
        &mut self,
        claim: &ExitClaim,
        except: Option<ServerReplyLink>,
        closing: &mut [Option<ClosingReplyLink>],
    ) -> usize;
    fn detach_reverse_link(&mut self, link: ClosingReplyLink) -> bool;
    fn clear_ipc_waiters(
        &mut self,
        claim: &ExitClaim,
        orphaned: &mut [Option<(SenderWaiter, usize)>],
    ) -> usize;
    fn settle_orphaned_sender(&mut self, waiter: &SenderWaiter, endpoint_idx: usize);

    // ── owed sends and wakes ────────────────────────────────────────────────────────────────
    /// Report the exit to the supervisor: endpoint send, then wake. `false` if no endpoint is
    /// bound, which is not an error — the broad path returns `Ok` for that too.
    fn report_exit_to_supervisor(
        &mut self,
        cpu: CpuId,
        tid: u64,
        code: u64,
        token: RestartToken,
    ) -> bool;
    /// Report the exit to PM: endpoint send, then wake.
    fn report_exit_to_pm(&mut self, cpu: CpuId, tid: u64, code: u64) -> bool;

    // ── the claim's own retirement ──────────────────────────────────────────────────────────
    /// Retire the exit claim. Called LAST, after every owned step has settled.
    fn retire_exit_claim(&mut self, claim: &ExitOutcome);
}

/// U9-EXIT1 §3 — exit the task currently Running on `cpu`, or refuse having mutated nothing.
///
/// A refusal whose [`ExitRefusal::may_fall_back`] is true is pre-mutation and the broad handler may
/// run. `VictimChanged` is the one that is not, and it is reached only after `current` has already
/// been cleared — the caller must fail closed rather than re-enter the broad dispatcher.
pub(crate) fn run_exit_transaction<O: ExitOwners>(
    owners: &mut O,
    cpu: CpuId,
    code: u64,
) -> Result<ExitOutcome, ExitRefusal> {
    // (1) Admission. No drainer, or not the authoritative dispatcher, means the deferral this
    // route depends on would never be consumed — refuse before touching anything.
    if !owners.exit_route_admitted(cpu) {
        return Err(ExitRefusal::NoCurrent);
    }

    // (2) rank 1 — the self is defined by the scheduler, never by an argument.
    let tid = owners.current_tid_on_cpu(cpu).ok_or(ExitRefusal::NoCurrent)?;

    // (3) rank 2 — the exiting thread's own facts. Every refusal below is free.
    let preflight = owners.exit_preflight(tid)?;
    if preflight.detached {
        return Err(ExitRefusal::DetachedThread);
    }
    if !crate::kernel::boot::exit_claim::status_is_self_exitable(preflight.status) {
        return Err(ExitRefusal::NotRunning);
    }
    if owners.has_robust_futex_list(tid) {
        return Err(ExitRefusal::RobustFutexList);
    }
    let sweep_asid = preflight.asid.unwrap_or(Asid(0));

    // (4) The deferred-capacity gate, exactly as the broad handler applies it: a task that still
    // owes a reply must not reach the point of no return unless its completion can be handed off,
    // or the blocked caller is stranded.
    let owes_reply = owners.server_reply_link_present(tid, sweep_asid);
    if owes_reply && !owners.server_death_capacity_available(cpu) {
        return Err(ExitRefusal::DeferredCapacity);
    }

    // (5) RESERVE THE QUEUE ADVANCE BEFORE ANY MUTATION. Holding it is what guarantees the drain
    // will run; returning `QueueAdvanceCommitted` without one is what U9-FT3 got wrong.
    if !owners.reserve_queue_advance(cpu, tid) {
        return Err(ExitRefusal::NoCurrent);
    }

    // (6) RESERVE THE SERVER-DEATH SLOT, still before any mutation. Reserve precedes detach so a
    // full queue leaves the link attached rather than stranding its caller.
    let mut death_reservation: Option<ServerDeathWorkReservation> = None;
    if owes_reply {
        match owners.reserve_server_death(cpu) {
            Some(reservation) => death_reservation = Some(reservation),
            None => {
                owners.release_queue_advance(cpu);
                return Err(ExitRefusal::DeferredCapacity);
            }
        }
    }

    // (7) rank 1 — clear `current`. The task is now in no run queue and is no CPU's current, so it
    // is not dispatchable; it is also not yet terminal. Nothing can observe it as both.
    let removed = owners.clear_current(cpu);
    if removed != Some(tid) {
        // The scheduler handed back somebody else. Undo both reservations and refuse; `current`
        // was not ours to clear, so nothing of this task changed.
        if let Some(reservation) = death_reservation {
            owners.release_server_death(reservation);
        }
        owners.release_queue_advance(cpu);
        return Err(ExitRefusal::VictimChanged);
    }

    // (8) rank 2 — THE LINEARIZATION POINT. Exactly one of exit / reap / fault / restart wins.
    let token = owners.mint_restart_token();
    let claim = match owners.claim_self_exit(cpu, tid, preflight.asid, code, token) {
        Ok(claim) => claim,
        Err(refusal) => {
            // Another edge won between (3) and here. `current` is already cleared, so this cannot
            // fall back — but nothing of THIS transaction was written either, and whichever edge
            // won has left the task terminal and off the CPU. Release the reservations and report
            // the refusal; the caller fails closed.
            if let Some(reservation) = death_reservation {
                owners.release_server_death(reservation);
            }
            owners.release_queue_advance(cpu);
            return Err(refusal);
        }
    };

    let mut outcome = ExitOutcome {
        claim,
        caller_reply_records_revoked: 0,
        replier_reply_records_revoked: 0,
        reverse_links_detached: 0,
        orphaned_senders_settled: 0,
        server_death_published: false,
        joiners_woken: 0,
        supervisor_reported: false,
        pm_reported: false,
    };

    // (9) The server-death handoff. The reservation is already held, so the detach — the
    // irreversible step — can never leave a record without an owner.
    let mut death_link: Option<ServerReplyLink> = None;
    if let Some(reservation) = death_reservation {
        match owners.take_server_reply_link(tid, claim.sweep_asid()) {
            Some(link) => {
                // Detached exactly once, by full incarnation. Whether or not the publish wins, the
                // link is EXCLUDED from the replier sweep below: a duplicate that collapsed still
                // means some owner holds this record's terminal, and clearing the slot here would
                // destroy it.
                death_link = Some(link);
                outcome.server_death_published =
                    owners.publish_server_death(reservation, tid, claim.sweep_asid(), link);
            }
            None => {
                // Nothing owed after all — the link went away between the probe and here. Release
                // the slot rather than holding it.
                owners.release_server_death(reservation);
            }
        }
    }

    // (10) rank 3 — reply records, caller side then replier side, through the SHARED U9-REAP1
    // bodies. Each sweep snapshots the reverse links it retires under the claim and detaches them
    // after releasing it, so a caller awaiting this task is settled exactly once.
    let mut caller_closing: [Option<ClosingReplyLink>; MAX_REPLY_CAPS] = [None; MAX_REPLY_CAPS];
    outcome.caller_reply_records_revoked =
        owners.revoke_reply_caps_for_caller(&claim, &mut caller_closing);
    for link in caller_closing.into_iter().flatten() {
        if owners.detach_reverse_link(link) {
            outcome.reverse_links_detached += 1;
        }
    }

    // The replier sweep EXCLUDES the record whose terminal the deferred completion now owns.
    // Clearing that slot here would destroy the authority the drain is about to settle through.
    let mut replier_closing: [Option<ClosingReplyLink>; MAX_REPLY_CAPS] = [None; MAX_REPLY_CAPS];
    outcome.replier_reply_records_revoked =
        owners.revoke_reply_caps_for_replier_except(&claim, death_link, &mut replier_closing);
    for link in replier_closing.into_iter().flatten() {
        if owners.detach_reverse_link(link) {
            outcome.reverse_links_detached += 1;
        }
    }

    // (11) rank 3 — waiters. Orphaned senders are settled after the claim releases, never under it.
    let mut orphaned: [Option<(SenderWaiter, usize)>; MAX_ENDPOINT_SENDER_WAITERS] =
        [const { None }; MAX_ENDPOINT_SENDER_WAITERS];
    let orphaned_n = owners.clear_ipc_waiters(&claim, &mut orphaned);
    for (waiter, endpoint_idx) in orphaned.into_iter().take(orphaned_n).flatten() {
        owners.settle_orphaned_sender(&waiter, endpoint_idx);
        outcome.orphaned_senders_settled += 1;
    }

    // (12) The owed reports. Each is an endpoint send followed by a wake, and each runs with no
    // unrelated lock held — the transaction holds none between owner calls by construction.
    outcome.supervisor_reported = owners.report_exit_to_supervisor(cpu, tid, code, token);
    outcome.pm_reported = owners.report_exit_to_pm(cpu, tid, code);

    // (13) Robust-futex wakes are NOT owed here: a thread that published a robust list is refused
    // at (3), before anything is reserved or claimed, so this route never has one to walk.

    // (14) Joiner wakes. The rank-2 status writes happen under one acquisition; the rank-1
    // enqueues happen after it releases. The exiting task is never among them: it is not blocked
    // on a join of itself, and this transaction never enqueues the claimed TID.
    let mut joiners: [Option<u64>; MAX_TASKS] = [None; MAX_TASKS];
    let joiner_n = owners.wake_joiners(tid, &mut joiners);
    for woken in joiners.into_iter().take(joiner_n).flatten() {
        debug_assert_ne!(woken, tid, "the exiting task must never be enqueued");
        if woken != tid {
            owners.enqueue_woken(woken);
            outcome.joiners_woken += 1;
        }
    }

    // (15) LAST — retire the claim. The queue-advance reservation stays held: the caller returns
    // `QueueAdvanceCommitted` and the EXISTING post-lock drain consumes it.
    owners.retire_exit_claim(&outcome);
    Ok(outcome)
}

// ═══════════════════════════════════════════════════════════════════════════════════════════
// The two owners. Neither contains policy: each is a set of acquisitions around bodies that are
// shared with the other — the thread-scoped cleanup with U9-REAP1, the exit-specific reads and
// writes with `kernel::boot::exit_claim` — so the broad NR 16 and the split NR 16 run the SAME
// transaction over the SAME code and cannot drift.
// ═══════════════════════════════════════════════════════════════════════════════════════════

use crate::kernel::boot::exit_claim as body;
use crate::kernel::boot::reap_claim as shared;

/// The split owner. Each method takes exactly the one domain lock its body needs, with the global
/// `SpinLock<KernelState>` already released.
pub(crate) struct SharedExitOwners<'a> {
    pub(crate) shared: &'a crate::runtime::SharedKernel,
}

impl ExitOwners for SharedExitOwners<'_> {
    fn exit_route_admitted(&self, cpu: CpuId) -> bool {
        self.shared.exit_route_admitted_split(cpu)
    }

    fn current_tid_on_cpu(&self, cpu: CpuId) -> Option<u64> {
        self.shared.current_tid_authoritative(cpu)
    }
    fn clear_current(&mut self, cpu: CpuId) -> Option<u64> {
        self.shared.block_current_on_cpu_split(cpu).ok().flatten()
    }
    fn enqueue_woken(&mut self, tid: u64) {
        let cpu = self.shared.authoritative_dispatch_cpu_split();
        let _ = self.shared.enqueue_task_split(cpu, tid);
    }

    fn exit_preflight(&self, tid: u64) -> Result<ExitPreflight, ExitRefusal> {
        self.shared
            .with_task_tcbs_split_mut(|tcbs| exit_preflight_locked(tcbs, tid))
    }
    fn has_robust_futex_list(&self, tid: u64) -> bool {
        self.shared.has_robust_futex_list_split_read(tid)
    }
    fn mint_restart_token(&mut self) -> RestartToken {
        RestartToken(self.shared.with_restart_split_mut(|restart| {
            let token = restart.next_restart_token;
            restart.next_restart_token = restart.next_restart_token.checked_add(1).unwrap_or(1);
            token
        }))
    }
    fn claim_self_exit(
        &mut self,
        cpu: CpuId,
        tid: u64,
        asid: Option<Asid>,
        code: u64,
        token: RestartToken,
    ) -> Result<ExitClaim, ExitRefusal> {
        self.shared.with_task_tcbs_split_mut(|tcbs| {
            body::claim_self_exit_locked(tcbs, cpu, tid, asid, code, token)
        })
    }
    fn rollback_claim(&mut self, claim: &ExitClaim) -> bool {
        self.shared
            .with_task_tcbs_split_mut(|tcbs| body::rollback_exit_claim_locked(tcbs, claim))
    }
    fn server_reply_link_present(&self, tid: u64, asid: Asid) -> bool {
        self.shared
            .with_task_tcbs_split_mut(|tcbs| body::server_reply_link_present_locked(tcbs, tid, asid))
    }
    fn take_server_reply_link(&mut self, tid: u64, asid: Asid) -> Option<ServerReplyLink> {
        self.shared
            .with_task_tcbs_split_mut(|tcbs| body::take_server_reply_link_locked(tcbs, tid, asid))
    }
    fn wake_joiners(&mut self, target_tid: u64, out: &mut [Option<u64>]) -> usize {
        self.shared
            .with_task_tcbs_split_mut(|tcbs| body::wake_joiners_for_locked(tcbs, target_tid, out))
    }

    fn server_death_capacity_available(&self, cpu: CpuId) -> bool {
        crate::kernel::boot::server_death_work_capacity_available(cpu.0 as usize)
    }
    fn reserve_server_death(&mut self, cpu: CpuId) -> Option<ServerDeathWorkReservation> {
        crate::kernel::boot::server_death_work_reserve(cpu.0 as usize)
    }
    fn publish_server_death(
        &mut self,
        reservation: ServerDeathWorkReservation,
        tid: u64,
        asid: Asid,
        link: ServerReplyLink,
    ) -> bool {
        crate::kernel::boot::server_death_work_publish(
            reservation,
            crate::kernel::boot::DeferredServerDeathCompletion {
                exiting_server: crate::kernel::boot::ReceiverWaiterIdentity::new(
                    crate::kernel::ipc::ThreadId(tid),
                    asid,
                ),
                reply_record_index: link.reply_record_index,
                reply_record_generation: link.reply_record_generation,
            },
        )
    }
    fn release_server_death(&mut self, reservation: ServerDeathWorkReservation) {
        crate::kernel::boot::server_death_work_release(reservation);
    }

    fn reserve_queue_advance(&mut self, cpu: CpuId, outgoing: u64) -> bool {
        crate::kernel::boot::futex_wait_dispatch_try_defer(cpu.0 as usize, outgoing)
    }
    fn release_queue_advance(&mut self, cpu: CpuId) {
        crate::kernel::boot::futex_wait_dispatch_clear(cpu.0 as usize);
    }

    fn revoke_reply_caps_for_caller(
        &mut self,
        claim: &ExitClaim,
        closing: &mut [Option<ClosingReplyLink>],
    ) -> usize {
        let identity = claim.identity();
        self.shared.with_ipc_split_mut(|ipc| {
            shared::revoke_reply_caps_for_caller_identity_locked(ipc, identity, closing)
        })
    }
    fn revoke_reply_caps_for_replier_except(
        &mut self,
        claim: &ExitClaim,
        except: Option<ServerReplyLink>,
        closing: &mut [Option<ClosingReplyLink>],
    ) -> usize {
        let identity = claim.identity();
        self.shared.with_ipc_split_mut(|ipc| {
            shared::revoke_reply_caps_for_replier_identity_locked(ipc, identity, except, closing)
        })
    }
    fn detach_reverse_link(&mut self, link: ClosingReplyLink) -> bool {
        let (stid, sasid, idx, generation) = link;
        self.shared
            .unregister_server_reply_link_split(stid, sasid, idx, generation)
    }
    fn clear_ipc_waiters(
        &mut self,
        claim: &ExitClaim,
        orphaned: &mut [Option<(SenderWaiter, usize)>],
    ) -> usize {
        let identity = claim.identity();
        self.shared.with_ipc_split_mut(|ipc| {
            shared::clear_ipc_waiters_for_identity_locked(ipc, identity, orphaned)
        })
    }
    fn settle_orphaned_sender(&mut self, waiter: &SenderWaiter, endpoint_idx: usize) {
        // The resolution is the shared body both owners run; only the consume differs, and each
        // side uses its own EXISTING production settle owner.
        let resolved = self.shared.with_ipc_split_mut(|ipc| {
            shared::resolve_orphaned_sender_envelope_locked(ipc, waiter, endpoint_idx)
        });
        let Some((handle, cleanup_tid)) = resolved else {
            return;
        };
        self.shared
            .settle_blocked_send_envelope_split(handle, endpoint_idx, cleanup_tid);
    }

    fn report_exit_to_supervisor(
        &mut self,
        cpu: CpuId,
        tid: u64,
        code: u64,
        token: RestartToken,
    ) -> bool {
        self.shared
            .report_task_exit_to_supervisor_split(cpu, tid, code, token.0)
    }
    fn report_exit_to_pm(&mut self, cpu: CpuId, tid: u64, code: u64) -> bool {
        self.shared.report_task_exit_to_pm_split(cpu, tid, code)
    }
    fn retire_exit_claim(&mut self, outcome: &ExitOutcome) {
        emit_exit_claim_retired(outcome);
    }
}

/// The claim's retirement marker. Emitted from BOTH owners so a log can never tell the split route
/// apart from the broad one by its absence — the oracle-blindness trap earlier increments each hit
/// once already.
fn emit_exit_claim_retired(outcome: &ExitOutcome) {
    let claim = &outcome.claim;
    crate::yarm_log!(
        "EXIT_TASK_CLAIM_RETIRED tid={} asid={} pid={} code={} token={} caller_records={} replier_records={} links={} joiners={} server_death={}",
        claim.tid(),
        claim.sweep_asid().0,
        claim.pid(),
        claim.code(),
        claim.restart_token().0,
        outcome.caller_reply_records_revoked,
        outcome.replier_reply_records_revoked,
        outcome.reverse_links_detached,
        outcome.joiners_woken,
        u8::from(outcome.server_death_published)
    );
}
