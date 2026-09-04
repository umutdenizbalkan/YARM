// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! U9-REAP1 §2 — the unforgeable reap claim, and the rank-local bodies the reap transaction
//! drives through both of its owners.
//!
//! # Why a claim exists at all
//!
//! At base the NR31 linearization point is split across two rank-2 acquisitions:
//! `handle_reap_faulted_task` reads `task_status(target)` and, some steps later,
//! `reap_faulted_task_noalloc_cleanup` writes `status = Dead`. Between those two reads the broad
//! lock guarantees nothing changed. Off that lock the same pair is a check-then-act race against
//! four concurrent lifecycle edges:
//!
//! * **restart** — the supervisor's scheduled restart flips the same TCB back to `Runnable`;
//! * **exit** — the task's own `ExitCurrentTask` marks it `Dead` and runs the full death path;
//! * **duplicate reap** — a second NR31 for the same target;
//! * **replacement under a reused TID** — the numeric TID is recycled by a new spawn, so every
//!   later step keyed on a bare `tid` would act on somebody else's task.
//!
//! [`claim_faulted_task_for_reap_locked`] collapses the read and the write into ONE rank-2
//! acquisition, so exactly one of those edges wins. The winner receives a [`ReapClaim`], which is
//! the only way to name the target for the rest of the transaction; the losers get a typed
//! [`ReapRefusal`] having mutated nothing.
//!
//! # Why the claim carries `{tid, asid}` and an existing token
//!
//! A numeric TID never authorizes cleanup. Every downstream step re-validates against the claim's
//! **exact incarnation**, which is the pair the rest of this kernel already uses as an identity
//! (`MarkedIncarnation`, `ReceiverWaiterIdentity`). The claim also carries the target's
//! `RestartState::token` — the lifecycle generation that ALREADY exists on the TCB. No generation
//! is invented here: `None` simply means the target carried no restart token, which is the exact
//! value the base cleanup writes back.

use super::*;
use crate::kernel::task::{RestartToken, TaskStatus, ThreadControlBlock};
use crate::kernel::vm::Asid;

/// Why a reap could not be claimed. Every variant is produced strictly BEFORE any mutation, so a
/// refusal leaves the whole kernel byte-for-byte unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReapRefusal {
    /// No TCB carries this TID. The base handler's `TASK_REAP_FAULTED_ALREADY_GONE` disposition:
    /// `Ok(0, 0, 0)`, because a target that is already gone is already reaped.
    TaskGone,
    /// The TCB belongs to no thread group, so no process-scoped step could be attributed.
    NoProcess,
    /// `Runnable | Running | Blocked(_) | Reserved` — a live task, or a spawn reservation that has
    /// never run. The base handler's `WrongObject`.
    NonTerminal,
    /// The target is already `Dead`: another reap (or the task's own exit path) claimed it first.
    /// This is the duplicate-reap loser, and it is deliberately NOT an error — see
    /// [`ReapRefusal::is_already_reaped`].
    AlreadyClaimed,
    /// The target is still some CPU's `current`, or still sits on a runqueue. Unreachable while
    /// the status gate above holds — a task reaches `Faulted` only after `current` has been
    /// cleared, and nothing re-enqueues a terminal task — but proved rather than assumed.
    StillScheduled,
}

impl ReapRefusal {
    /// A short stable name for the wire.
    pub(crate) const fn marker(self) -> &'static str {
        match self {
            Self::TaskGone => "task_gone",
            Self::NoProcess => "no_process",
            Self::NonTerminal => "non_terminal",
            Self::AlreadyClaimed => "already_claimed",
            Self::StillScheduled => "still_scheduled",
        }
    }

    /// Does this refusal mean "the target is already reaped", rather than "the request was
    /// wrong"?
    ///
    /// Both variants describe a target that no longer needs reaping, and the base handler already
    /// answers `Ok(0, 0, 0)` for the first of them. Keeping the second on the same disposition is
    /// what makes a duplicate reap *refused without mutation* while leaving the syscall's return
    /// ABI exactly as it was.
    pub(crate) const fn is_already_reaped(self) -> bool {
        matches!(self, Self::TaskGone | Self::AlreadyClaimed)
    }
}

/// U9-REAP1 §2 — proof that exactly one reap won exactly one target.
///
/// There is no public constructor and no way to build one from a numeric TID: the only way to
/// obtain a `ReapClaim` is to have performed the rank-2 compare-and-claim in
/// [`claim_faulted_task_for_reap_locked`], which is also the transaction's linearization point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReapClaim {
    tid: u64,
    /// The address space the claimed incarnation was bound to. `None` is preserved rather than
    /// substituted: an ASID-less task is still reapable at base (its sweeps run against
    /// `Asid(0)`), and turning `None` into `Some(Asid(0))` would let a rollback or a liveness
    /// re-check match the wrong shape.
    asid: Option<Asid>,
    pid: u64,
    restart_token: Option<RestartToken>,
    status_before: TaskStatus,
}

impl ReapClaim {
    /// The claimed thread.
    pub(crate) const fn tid(&self) -> u64 {
        self.tid
    }
    /// The address space that, together with [`Self::tid`], names the EXACT incarnation this
    /// claim won. Every later step matches on this pair, never on the TID alone.
    ///
    /// The transaction itself never needs to ask: each step re-validates the pair inside its own
    /// acquisition, through the locked bodies below. This accessor exists so the §5 proofs can
    /// compare pre/post state BY IDENTITY rather than by count.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) const fn asid(&self) -> Option<Asid> {
        self.asid
    }
    /// The ASID the record sweeps key on. `Asid(0)` stands in for an ASID-less task, which is
    /// exactly what the base cleanup does (`task_asid(tid).unwrap_or(Asid(0))`) — the never-leak
    /// direction, since a record with no stored incarnation is matched on the numeric TID.
    pub(crate) fn sweep_asid(&self) -> Asid {
        self.asid.unwrap_or(Asid(0))
    }
    /// The process the claimed thread belonged to, captured under the claim so a later step can
    /// never attribute the teardown to a replacement's process.
    pub(crate) const fn pid(&self) -> u64 {
        self.pid
    }
    /// The restart-lifecycle generation the claim retired, as it stood on the TCB. Not invented.
    pub(crate) const fn restart_token(&self) -> Option<RestartToken> {
        self.restart_token
    }
    /// The status the target was in when the claim won it. Written back by
    /// [`rollback_reap_claim_locked`] through the field, and read here by the §5 proofs.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) const fn status_before(&self) -> TaskStatus {
        self.status_before
    }
    /// The identity every reply-record and waiter sweep is keyed on.
    pub(crate) fn identity(&self) -> ReceiverWaiterIdentity {
        ReceiverWaiterIdentity::new(crate::kernel::ipc::ThreadId(self.tid), self.sweep_asid())
    }
}

/// Is `status` one this reap may claim?
///
/// `Faulted` is the reapable state the mission names. `Exited(_)` is accepted because the base
/// handler accepts it and PM reaches NR31 for a target that raced its own exit. `Dead` is NOT
/// claimable — it is what a won claim leaves behind, so accepting it would let two reaps both
/// believe they won.
pub(crate) const fn status_is_claimable(status: TaskStatus) -> bool {
    matches!(status, TaskStatus::Faulted | TaskStatus::Exited(_))
}

/// U9-REAP1 §2 — THE linearization point.
///
/// One rank-2 acquisition performs, in this order and with no window between them:
///
/// 1. resolve the TCB, or refuse `TaskGone`;
/// 2. capture its EXACT incarnation (`asid`, which may legitimately be `None`) and its process;
/// 3. classify the status: live/reserved refuses `NonTerminal`, already-`Dead` refuses
///    `AlreadyClaimed`, and only `Faulted`/`Exited` proceeds;
/// 4. **claim**: write `Dead` and drop the restart token, exactly as the base cleanup's first step
///    does, and mint the token naming what was just won.
///
/// Every refusal above happens before step 4, so a refused claim mutates nothing at all.
pub(crate) fn claim_faulted_task_for_reap_locked(
    tcbs: &mut [Option<ThreadControlBlock>],
    tid: u64,
) -> Result<ReapClaim, ReapRefusal> {
    let tcb = tcbs
        .iter_mut()
        .flatten()
        .find(|tcb| tcb.tid.0 == tid)
        .ok_or(ReapRefusal::TaskGone)?;
    let asid = tcb.asid;
    let pid = tcb.thread_group_id.0;
    if pid == 0 && tid != 0 {
        return Err(ReapRefusal::NoProcess);
    }
    let status_before = tcb.status;
    if matches!(status_before, TaskStatus::Dead) {
        return Err(ReapRefusal::AlreadyClaimed);
    }
    if !status_is_claimable(status_before) {
        return Err(ReapRefusal::NonTerminal);
    }
    // ── the claim. Nothing above this line wrote anything. ─────────────────────────────────
    let restart_token = tcb.restart.token;
    tcb.status = TaskStatus::Dead;
    tcb.restart.token = None;
    Ok(ReapClaim {
        tid,
        asid,
        pid,
        restart_token,
        status_before,
    })
}

/// Undo a claim, restoring the exact incarnation to its exact pre-claim state.
///
/// Used only when a step between the claim and the first irreversible mutation refuses — the
/// scheduler-residency proof is the one such step. Matches on `{tid, asid}` so a claim can never
/// be rolled back onto a replacement that reused the numeric TID; `false` means the incarnation is
/// gone and there is nothing to restore.
pub(crate) fn rollback_reap_claim_locked(
    tcbs: &mut [Option<ThreadControlBlock>],
    claim: &ReapClaim,
) -> bool {
    let Some(tcb) = tcbs
        .iter_mut()
        .flatten()
        .find(|tcb| tcb.tid.0 == claim.tid && tcb.asid == claim.asid)
    else {
        return false;
    };
    tcb.status = claim.status_before;
    tcb.restart.token = claim.restart_token;
    true
}

/// The last-thread rule, read-only at rank 2.
///
/// Byte-for-byte the base predicate in `maybe_cleanup_process_cnode_for_pid_noalloc_reap`: the
/// process-wide teardown runs only when NO TCB of the group is in any state other than `Dead`.
/// A sibling that is still alive keeps the shared CNode and the shared address space.
pub(crate) fn process_has_live_threads_locked(
    tcbs: &[Option<ThreadControlBlock>],
    pid: u64,
) -> bool {
    tcbs.iter()
        .flatten()
        .any(|tcb| tcb.thread_group_id.0 == pid && tcb.status != TaskStatus::Dead)
}

/// Collect the DISTINCT address spaces the process owns, into a caller-supplied fixed buffer.
///
/// Fixed capacity, no allocation, no growth — the same `[None; MAX_TASKS]` discipline the base
/// path uses. Returns how many slots were filled.
pub(crate) fn collect_process_asids_locked(
    tcbs: &[Option<ThreadControlBlock>],
    pid: u64,
    out: &mut [Option<Asid>],
) -> usize {
    let mut len = 0usize;
    for tcb in tcbs.iter().flatten() {
        if tcb.thread_group_id.0 != pid {
            continue;
        }
        let Some(asid) = tcb.asid else {
            continue;
        };
        if out.iter().take(len).flatten().any(|seen| *seen == asid) {
            continue;
        }
        if len < out.len() {
            out[len] = Some(asid);
            len += 1;
        }
    }
    len
}

/// The process id a thread belongs to, or the numeric TID when it belongs to none.
///
/// The base transfer sweeps resolve owners exactly this way (`process_id(tid).unwrap_or(tid)`), so
/// both owners share the resolution rather than each re-deriving it.
pub(crate) fn owner_pid_of_locked(tcbs: &[Option<ThreadControlBlock>], tid: u64) -> u64 {
    tcbs.iter()
        .flatten()
        .find(|tcb| tcb.tid.0 == tid)
        .map_or(tid, |tcb| tcb.thread_group_id.0)
}

/// The address space a thread is bound to. Rank 2, read-only.
pub(crate) fn task_asid_locked(tcbs: &[Option<ThreadControlBlock>], tid: u64) -> Option<Asid> {
    tcbs.iter()
        .flatten()
        .find(|tcb| tcb.tid.0 == tid)
        .and_then(|tcb| tcb.asid)
}

// ── rank 3 (IPC) bodies ────────────────────────────────────────────────────────────────────
//
// Each of these is the exact body the broad `KernelState` method already runs inside its
// `with_ipc_state_mut` claim, lifted so the split owner can run the SAME body inside
// `with_ipc_split_mut`. Nothing here acquires a lock, logs, or reaches another rank: the owed
// follow-up work is returned to the caller, which settles it after releasing the claim.

/// A reply record retired by a sweep, with the reverse link that must now be detached.
/// `(replier_tid, replier_asid, record_index, record_generation)`.
pub(crate) type ClosingReplyLink = (u64, Asid, usize, u64);

/// Sweep every reply record whose CALLER side is this exact incarnation.
///
/// Returns the number retired; `closing` receives one entry per retired record that carried a
/// bound replier, for the caller to detach after the rank-3 claim is released.
pub(crate) fn revoke_reply_caps_for_caller_identity_locked(
    ipc: &mut IpcSubsystem,
    caller: ReceiverWaiterIdentity,
    closing: &mut [Option<ClosingReplyLink>],
) -> usize {
    let mut revoked = 0usize;
    for (idx, slot) in ipc.reply_caps.iter_mut().enumerate() {
        if slot.is_some_and(|record| {
            record.caller_tid == caller.tid && record.caller_asid == caller.asid
        }) {
            if let Some((stid, sasid)) = slot
                .as_ref()
                .and_then(|r| r.responder_tid.zip(r.replier_asid))
                && idx < closing.len()
            {
                closing[idx] = Some((stid.0, sasid, idx, ipc.reply_cap_generations[idx]));
            }
            *slot = None;
            revoked += 1;
        }
    }
    revoked
}

/// Sweep every reply record whose REPLIER side is this exact incarnation.
///
/// A record that stored a concrete `replier_asid` differing from the claim is skipped (it belongs
/// to a prior incarnation at the same numeric TID); a record with no stored replier ASID carries
/// no incarnation evidence and is matched on the numeric TID — the never-leak direction, and the
/// same rule the base sweep applies.
pub(crate) fn revoke_reply_caps_for_replier_identity_locked(
    ipc: &mut IpcSubsystem,
    replier: ReceiverWaiterIdentity,
    closing: &mut [Option<ClosingReplyLink>],
) -> usize {
    let mut revoked = 0usize;
    for (idx, slot) in ipc.reply_caps.iter_mut().enumerate() {
        if slot.is_some_and(|record| {
            record.responder_tid == Some(replier.tid)
                && record
                    .replier_asid
                    .is_none_or(|stored| stored == replier.asid)
        }) {
            if let Some((stid, sasid)) = slot
                .as_ref()
                .and_then(|r| r.responder_tid.zip(r.replier_asid))
                && idx < closing.len()
            {
                closing[idx] = Some((stid.0, sasid, idx, ipc.reply_cap_generations[idx]));
            }
            *slot = None;
            revoked += 1;
        }
    }
    revoked
}

/// Detach every waiter the target owns: its endpoint RECEIVE waiter by exact identity, its
/// endpoint SEND waiters by numeric TID, and its notification waiters.
///
/// Returns how many orphaned sender envelopes were collected into `orphaned` — each still owns a
/// transferred capability (and, for a shared-region transfer, one transient pin) and must be
/// settled exactly once by the caller, after this claim releases.
pub(crate) fn clear_ipc_waiters_for_identity_locked(
    ipc: &mut IpcSubsystem,
    identity: ReceiverWaiterIdentity,
    orphaned: &mut [Option<(SenderWaiter, usize)>],
) -> usize {
    ipc.clear_endpoint_waiters_for_identity(identity);
    let mut orphaned_n = 0usize;
    for (endpoint_idx, queue) in ipc.endpoint_sender_waiters.iter_mut().enumerate() {
        for slot in queue.iter_mut() {
            if slot.as_ref().is_some_and(|w| w.tid == identity.tid) {
                if let Some(removed) = slot.take()
                    && removed.msg.transferred_cap().is_some()
                    && orphaned_n < orphaned.len()
                {
                    orphaned[orphaned_n] = Some((removed, endpoint_idx));
                    orphaned_n += 1;
                }
                *slot = None;
            }
        }
    }
    for waiter in ipc.notification_waiters.iter_mut() {
        if *waiter == Some(identity.tid) {
            *waiter = None;
        }
    }
    orphaned_n
}

// ── rank 4 (capability) bodies ─────────────────────────────────────────────────────────────

/// The CNode a process is bound to. Rank 4, read-only.
pub(crate) fn process_cnode_for_pid_locked(
    capability: &CapabilitySubsystem,
    pid: u64,
) -> Option<CNodeId> {
    capability
        .process_cnodes
        .iter()
        .flatten()
        .find(|record| record.pid == pid)
        .map(|record| record.cnode)
}

/// How many slots a CNode carries. Rank 4, read-only; reported, never acted on.
pub(crate) fn cnode_slot_capacity_locked(
    capability: &CapabilitySubsystem,
    cnode: CNodeId,
) -> usize {
    capability
        .cnode_spaces
        .iter()
        .flatten()
        .find(|space| space.id == cnode)
        .map_or(0, |space| space.slot_capacity)
}

/// Resolve which transfer envelope an orphaned blocking sender's handle names, and who must clean
/// it up. Rank 3, read-only.
///
/// Returns `(handle, cleanup_tid)`, or `None` when the waiter carries no transferred capability,
/// its handle names no live envelope, the envelope's generation has moved on, or the envelope
/// belongs to a DIFFERENT endpoint than the one this waiter was parked on — a handle index can
/// collide, and settling somebody else's envelope on a collision would destroy a live transfer.
pub(crate) fn resolve_orphaned_sender_envelope_locked(
    ipc: &IpcSubsystem,
    waiter: &SenderWaiter,
    endpoint_idx: usize,
) -> Option<(u64, crate::kernel::ipc::ThreadId)> {
    let handle = waiter.msg.transferred_cap()?;
    let idx = usize::try_from(handle.0 & 0xFFFF).ok()?;
    let envelope = (*ipc.transfer_envelopes.get(idx)?)?;
    if ipc.transfer_envelope_generations.get(idx).copied()? != (handle.0 >> 16) {
        return None;
    }
    if !matches!(envelope.endpoint,
        CapObject::Endpoint { index, .. } if index == endpoint_idx)
    {
        return None;
    }
    Some((handle.0, envelope.receiver_tid.unwrap_or(waiter.tid)))
}
