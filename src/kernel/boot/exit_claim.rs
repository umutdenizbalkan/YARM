// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! U9-EXIT1 §2 — the unforgeable self-exit claim.
//!
//! # Why this claim is not the reap claim
//!
//! U9-REAP1 gave the faulted-task reap a linearizable claim: `Faulted | Exited(_)` → `Dead`, taken
//! in one rank-2 acquisition. A self-exit needs a *different* claim over the *same* TCB field, and
//! the differences are not cosmetic:
//!
//! * the reap claims a task that is **already off every CPU**; the exit claims the task that is
//!   **currently Running on this exact CPU**, and removing it from `current` is part of the same
//!   transaction;
//! * the reap **takes and clears** the restart token; the exit **mints and installs** a new one;
//! * the reap's terminal state is `Dead`; the exit's is `Exited(code)`, which is precisely the
//!   state a later reap is still allowed to claim.
//!
//! So the two claims stay distinct, and the *cleanup* they share is the thread-scoped body
//! `kernel::boot::reap_claim` already extracted — the two reply-record sweeps and the waiter
//! detach. One implementation, two claims.
//!
//! # What the claim arbitrates against
//!
//! Everything that can move the same TCB out of `Running`, resolved by one rank-2 acquisition that
//! reads and writes the status together:
//!
//! * **reap** — an NR 31 that got there first left `Dead`; the claim refuses `NotRunning`;
//! * **fault** — a terminal fault left `Faulted`; likewise refused;
//! * **restart** — a restart left `Runnable`; likewise refused;
//! * **duplicate exit** — the first exit left `Exited(_)`; likewise refused, so the second exit
//!   mutates nothing and cannot publish a second disposition;
//! * **TID reuse** — every check is on the exact `{tid, asid}` incarnation, and the claim also
//!   carries the CPU it was taken on, so a replacement task at the same numeric TID matches
//!   nothing.
//!
//! # Admission is narrow, and deliberately so
//!
//! A self-exit whose thread is **Detached** additionally runs `reap_if_detached` →
//! `mark_task_dead`, which reaches a general capability-revocation loop over the process CNode and
//! an allocating live-capability snapshot. That closure is genuinely different from anything
//! U9-REAP1 made allocation-free (see §1's ledger), and §4 forbids allocation in the teardown, so
//! it is **refused before any mutation** and keeps its existing broad path. A `Joinable` self-exit
//! never enters `mark_task_dead` at all, so its closure is exactly the shared thread-scoped body
//! plus this path's own owed work — and that is the population the route admits.

use super::*;
use crate::kernel::scheduler::CpuId;
use crate::kernel::task::{RestartToken, TaskStatus, ThreadControlBlock, ThreadDetachState};
use crate::kernel::vm::Asid;

/// Why a self-exit could not be claimed. Every variant is produced strictly BEFORE any mutation,
/// so a refusal leaves the whole kernel byte-for-byte unchanged and the broad handler may run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExitRefusal {
    /// This CPU has no resolvable current task, so there is no self to exit.
    NoCurrent,
    /// No TCB carries this TID.
    TaskGone,
    /// The TCB's ASID is not the one the claim was resolved against: a different incarnation now
    /// holds the numeric TID.
    IdentityChanged,
    /// The task is not `Running`. Some other lifecycle edge — reap, fault, restart or a previous
    /// exit — got here first, and it won.
    NotRunning,
    /// The thread is `Detached`, so its exit reaches `mark_task_dead`'s allocating
    /// general-revocation closure. Out of this route's admitted population by construction.
    DetachedThread,
    /// The exiting thread published a robust-futex list. Its wakes read the `robust_futex`
    /// registry, which has no split lock domain of its own — it is reachable only under the broad
    /// guard — so this population keeps its existing broad path rather than having a new domain
    /// invented for it. Refused before any mutation.
    RobustFutexList,
    /// The task still owes a reply and no deferred server-death slot is free on this CPU.
    /// Declining here is what keeps a full queue from stranding the blocked caller — the same
    /// pre-mutation gate the broad handler applies, with the same `WouldBlock` meaning.
    DeferredCapacity,
    /// The scheduler did not hand back the exact task the claim named when `current` was cleared.
    VictimChanged,
}

impl ExitRefusal {
    /// A short stable name for the wire.
    pub(crate) const fn marker(self) -> &'static str {
        match self {
            Self::NoCurrent => "no_current",
            Self::TaskGone => "task_gone",
            Self::IdentityChanged => "identity_changed",
            Self::NotRunning => "not_running",
            Self::DetachedThread => "detached_thread",
            Self::RobustFutexList => "robust_futex_list",
            Self::DeferredCapacity => "deferred_capacity",
            Self::VictimChanged => "victim_changed",
        }
    }

    /// Does this refusal mean "the broad handler must produce the answer", rather than "the exit
    /// happened elsewhere"?
    ///
    /// Every variant here is pre-mutation, so every one of them may fall back. The distinction is
    /// kept explicit anyway: a future variant that is NOT safe to fall back from must be forced to
    /// say so here rather than inherit permission by default.
    pub(crate) const fn may_fall_back(self) -> bool {
        match self {
            Self::NoCurrent
            | Self::TaskGone
            | Self::IdentityChanged
            | Self::NotRunning
            | Self::DetachedThread
            | Self::RobustFutexList
            | Self::DeferredCapacity => true,
            // The scheduler already cleared `current` before this was discovered, so the trap
            // cannot re-enter the broad dispatcher — it must fail closed instead.
            Self::VictimChanged => false,
        }
    }
}

/// U9-EXIT1 §2 — proof that exactly one self-exit claimed exactly one Running task.
///
/// There is no public constructor. The only way to obtain an `ExitClaim` is to have performed the
/// rank-2 compare-and-claim in [`claim_self_exit_locked`], which is the transaction's linearization
/// point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ExitClaim {
    cpu: CpuId,
    tid: u64,
    asid: Option<Asid>,
    pid: u64,
    code: u64,
    restart_token: RestartToken,
    status_before: TaskStatus,
}

/// The accessors and the inverse below are `dead_code` in a production build ON PURPOSE, and the
/// allow is narrowed to that build so a test-only reader still counts.
///
/// `rollback_exit_claim_locked` exists because the claim must be *reversible by construction* — a
/// linearization point that cannot be undone is one whose failure modes cannot be reasoned about —
/// while the transaction deliberately never calls it: past the claim there is no fallback, and a
/// cleanup failure fails CLOSED rather than resurrecting the task (§3). Its coverage drives it
/// directly, including the TID-reuse case where it must match nothing.
#[cfg_attr(not(test), allow(dead_code))]
impl ExitClaim {
    /// The CPU the claim was taken on. Every scheduler step of the transaction is authenticated
    /// against it, so a claim cannot be spent on another CPU's run queue.
    pub(crate) const fn cpu(&self) -> CpuId {
        self.cpu
    }
    /// The exiting thread.
    pub(crate) const fn tid(&self) -> u64 {
        self.tid
    }
    /// The address space that, with [`Self::tid`], names the EXACT incarnation this claim won.
    pub(crate) const fn asid(&self) -> Option<Asid> {
        self.asid
    }
    /// The ASID the record sweeps key on. `Asid(0)` stands in for an ASID-less task, exactly as
    /// the broad path's `task_asid(tid).unwrap_or(Asid(0))` does.
    pub(crate) fn sweep_asid(&self) -> Asid {
        self.asid.unwrap_or(Asid(0))
    }
    /// The process the exiting thread belonged to, captured under the claim.
    pub(crate) const fn pid(&self) -> u64 {
        self.pid
    }
    /// The exit status the claim wrote.
    pub(crate) const fn code(&self) -> u64 {
        self.code
    }
    /// The restart token this exit MINTED and installed. Not taken from anywhere: the broad path
    /// mints one per exit and so does this, from the same monotonic source.
    pub(crate) const fn restart_token(&self) -> RestartToken {
        self.restart_token
    }
    /// The status the claim replaced. Always `Running` — kept so a rollback restores the exact
    /// value rather than assuming one.
    pub(crate) const fn status_before(&self) -> TaskStatus {
        self.status_before
    }
    /// The identity every reply-record and waiter sweep is keyed on.
    pub(crate) fn identity(&self) -> ReceiverWaiterIdentity {
        ReceiverWaiterIdentity::new(crate::kernel::ipc::ThreadId(self.tid), self.sweep_asid())
    }
}

/// Is `status` one a self-exit may claim?
///
/// Exactly `Running`, and nothing else. This is narrower than the reap's predicate on purpose: the
/// reap collects tasks other edges have already finished with, whereas an exit is the *first*
/// terminal edge for a task that is still executing. Anything else means another edge won.
pub(crate) const fn status_is_self_exitable(status: TaskStatus) -> bool {
    matches!(status, TaskStatus::Running)
}

/// The facts a self-exit needs about its own thread, read under one rank-2 acquisition BEFORE
/// anything is claimed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ExitPreflight {
    pub(crate) asid: Option<Asid>,
    pub(crate) pid: u64,
    pub(crate) detached: bool,
    pub(crate) status: TaskStatus,
}

/// Read the exiting thread's own facts. Read-only: nothing here mutates, so every refusal derived
/// from it is free.
pub(crate) fn exit_preflight_locked(
    tcbs: &[Option<ThreadControlBlock>],
    tid: u64,
) -> Result<ExitPreflight, ExitRefusal> {
    let tcb = tcbs
        .iter()
        .flatten()
        .find(|tcb| tcb.tid.0 == tid)
        .ok_or(ExitRefusal::TaskGone)?;
    Ok(ExitPreflight {
        asid: tcb.asid,
        pid: tcb.thread_group_id.0,
        detached: tcb.detach_state == ThreadDetachState::Detached,
        status: tcb.status,
    })
}

/// U9-EXIT1 §2 — THE linearization point.
///
/// One rank-2 acquisition performs, in this order and with no window between them:
///
/// 1. resolve the TCB, or refuse `TaskGone`;
/// 2. match the exact incarnation, or refuse `IdentityChanged`;
/// 3. refuse a `Detached` thread — its closure is not this route's;
/// 4. classify the status: anything but `Running` means another lifecycle edge won, and refuses
///    `NotRunning`;
/// 5. **claim**: write `Exited(code)`, install the freshly minted restart token, clear the blocked
///    receive state and drop the async-preempt tag — exactly the four writes the broad
///    `exit_task` performs in its own scoped acquisition, in the same order.
///
/// Every refusal happens before step 5, so a refused claim mutates nothing at all.
///
/// The caller MUST already have cleared this task from `current` at rank 1. That ordering — clear
/// first, then write the terminal status — is the one `commit_terminal_fault_transition_shared`
/// established: between the two the task is in no run queue and is no CPU's current, so it is
/// never simultaneously dispatchable and terminal.
pub(crate) fn claim_self_exit_locked(
    tcbs: &mut [Option<ThreadControlBlock>],
    cpu: CpuId,
    tid: u64,
    asid: Option<Asid>,
    code: u64,
    token: RestartToken,
) -> Result<ExitClaim, ExitRefusal> {
    let tcb = tcbs
        .iter_mut()
        .flatten()
        .find(|tcb| tcb.tid.0 == tid)
        .ok_or(ExitRefusal::TaskGone)?;
    if tcb.asid != asid {
        return Err(ExitRefusal::IdentityChanged);
    }
    if tcb.detach_state == ThreadDetachState::Detached {
        return Err(ExitRefusal::DetachedThread);
    }
    let status_before = tcb.status;
    if !status_is_self_exitable(status_before) {
        return Err(ExitRefusal::NotRunning);
    }
    let pid = tcb.thread_group_id.0;
    // ── the claim. Nothing above this line wrote anything. ─────────────────────────────────
    apply_self_exit_writes_locked(tcb, code, token);
    Ok(ExitClaim {
        cpu,
        tid,
        asid,
        pid,
        code,
        restart_token: token,
        status_before,
    })
}

/// U9-EXIT1 §4 — THE self-exit's TCB writes, in the exact order both routes perform them.
///
/// One body, two owners. `claim_self_exit_locked` calls it after its status precondition, under
/// the split route's rank-2 acquisition; the broad `KernelState::exit_task` calls it inside its own
/// scoped `with_tcbs_mut`. Neither route can drift from the other, and a fifth write added here
/// lands on both by construction.
///
/// The writes themselves are unchanged from the broad path that has always performed them:
///
/// 1. cancel any blocked receive — a corpse has no receive to complete;
/// 2. `Exited(code)` — the terminal status a later reap is still allowed to claim;
/// 3. install the caller's freshly minted restart token;
/// 4. drop the asynchronously preempted register file.
///
/// (4) is defence in depth rather than the primary guarantee: the tag is validated against the
/// exact `{tid, asid, generation}` incarnation at every consumer, so a replacement task reusing the
/// numeric TID would be refused even without it. It is cleared anyway, because "cannot match" and
/// "is not there" are different strengths of the same claim and the cheaper one belongs at the
/// point of death. It is placed AFTER the status and token writes on purpose: the Stage 199D-WA2B
/// census fingerprints each status assignment by its immediate neighbourhood, and this clear has no
/// ordering requirement of its own.
pub(crate) fn apply_self_exit_writes_locked(
    tcb: &mut ThreadControlBlock,
    code: u64,
    token: RestartToken,
) {
    let tid = tcb.tid.0;
    if tcb.blocked_recv_state.take().is_some() {
        crate::yarm_log!("IPC_RECV_BLOCKED_STATE_CLEAR tid={} reason=cancel", tid);
    }
    tcb.status = TaskStatus::Exited(code);
    tcb.restart.token = Some(token);
    tcb.async_preempted = None;
}

/// Undo a claim, restoring the exact incarnation to its exact pre-claim state.
///
/// Matches on `{tid, asid}` so a claim can never be rolled back onto a replacement that reused the
/// numeric TID. `false` means the incarnation is gone and there is nothing to restore.
///
/// Deliberately does NOT restore `blocked_recv_state`: the broad path clears it unconditionally at
/// the same point and never restores it either, and a rollback that resurrected a blocked receive
/// would be inventing state rather than undoing a write.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn rollback_exit_claim_locked(
    tcbs: &mut [Option<ThreadControlBlock>],
    claim: &ExitClaim,
) -> bool {
    let Some(tcb) = tcbs
        .iter_mut()
        .flatten()
        .find(|tcb| tcb.tid.0 == claim.tid && tcb.asid == claim.asid)
    else {
        return false;
    };
    tcb.status = claim.status_before;
    tcb.restart.token = None;
    true
}

/// Does the exact `{tid, asid}` incarnation still hold a terminal status this exit produced?
///
/// This is the predicate the post-lock drain reverifies with, and it is deliberately NOT
/// `.unwrap_or(false)` like the FutexWait and terminal-fault predicates. An exiting task can
/// legitimately have been torn down further — reaped, or joined — between the claim and the drain,
/// and a missing TCB then means *the exit succeeded*, not *the exit failed*. Reading absence as
/// failure would strand the CPU with `current` already cleared.
///
/// So absence is represented explicitly rather than inferred: [`ExitDrainVerdict::Removed`] is a
/// distinct answer from [`ExitDrainVerdict::Terminal`], the caller can log which one it saw, and
/// only [`ExitDrainVerdict::Contradicted`] — a TCB that is present and NOT terminal — refuses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExitDrainVerdict {
    /// The incarnation is present and terminal. The ordinary case.
    Terminal,
    /// The incarnation is gone. The exit ran to completion and something later removed the TCB;
    /// the queue advance is still owed and still correct.
    Removed,
    /// The incarnation is present and NOT terminal. Something resurrected it, which must never
    /// happen after a claim — refuse and let the caller fail closed.
    Contradicted,
}

/// Reverify one exit deferral. Rank 2, read-only.
pub(crate) fn exit_drain_verdict_locked(
    tcbs: &[Option<ThreadControlBlock>],
    tid: u64,
    asid: Option<Asid>,
) -> ExitDrainVerdict {
    match tcbs
        .iter()
        .flatten()
        .find(|tcb| tcb.tid.0 == tid && tcb.asid == asid)
    {
        Some(tcb) if matches!(tcb.status, TaskStatus::Exited(_) | TaskStatus::Dead) => {
            ExitDrainVerdict::Terminal
        }
        Some(_) => ExitDrainVerdict::Contradicted,
        None => ExitDrainVerdict::Removed,
    }
}

/// Wake every joiner blocked on this exact thread, returning the TIDs to enqueue.
///
/// The rank-2 half of `wake_joiners_for`, lifted verbatim so both owners run one body. The rank-1
/// enqueues are the caller's, performed after this acquisition releases — never under it.
pub(crate) fn wake_joiners_for_locked(
    tcbs: &mut [Option<ThreadControlBlock>],
    target_tid: u64,
    out: &mut [Option<u64>],
) -> usize {
    let mut n = 0usize;
    for tcb in tcbs.iter_mut().flatten() {
        if tcb.status
            != TaskStatus::Blocked(crate::kernel::task::WaitReason::Join(
                crate::kernel::ipc::ThreadId(target_tid),
            ))
        {
            continue;
        }
        tcb.status = TaskStatus::Runnable;
        if n < out.len() {
            out[n] = Some(tcb.tid.0);
            n += 1;
        }
    }
    n
}

/// Detach the exiting server's reverse link, by exact incarnation.
///
/// The rank-2 half of `take_server_reply_link`. `Any` is the exit-path selector: the whole
/// incarnation is going away, so any authority it still holds goes with it. A reused numeric TID
/// with a different ASID matches nothing, detaches nothing and records nothing.
pub(crate) fn take_server_reply_link_locked(
    tcbs: &mut [Option<ThreadControlBlock>],
    server_tid: u64,
    server_asid: Asid,
) -> Option<crate::kernel::task::ServerReplyLink> {
    let tcb = tcbs
        .iter_mut()
        .flatten()
        .find(|t| t.tid.0 == server_tid && t.asid == Some(server_asid))?;
    crate::kernel::boot::close_server_reply_link(tcb, crate::kernel::boot::LinkCloseSelector::Any)
        .closed()
}

/// Does the exiting thread still hold a reverse link? Rank 2, read-only — the pre-mutation probe
/// behind the deferred-capacity gate.
pub(crate) fn server_reply_link_present_locked(
    tcbs: &[Option<ThreadControlBlock>],
    server_tid: u64,
    server_asid: Asid,
) -> bool {
    tcbs.iter().flatten().any(|t| {
        t.tid.0 == server_tid && t.asid == Some(server_asid) && t.server_reply_link.is_some()
    })
}
