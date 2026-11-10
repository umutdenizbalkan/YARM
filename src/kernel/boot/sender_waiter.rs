// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! IPC-SENDER-ID1 — the endpoint sender waiter: who may end a blocking-send cycle, and who may
//! remove its waiter.
//!
//! # Two scopes, two predicates
//!
//! * **One blocking-send cycle** is `{tid, asid, send_generation}`
//!   ([`SenderWaiter::wake_target`]). Ending it — a receiver consumed the message, or its
//!   deadline expired — is ONE rank-2 claim, [`claim_blocked_send_cycle_locked`]. The claim
//!   succeeds only while that exact incarnation is `Blocked(EndpointSend)` with that generation,
//!   and in the same acquisition parks the cycle's completion and makes the sender `Runnable`.
//!   Whoever claims first owns the cycle's ending; every later claimant is refused with no
//!   mutation, so it publishes nothing and wakes nothing. Only a successful claim may be followed
//!   by the wake (the rank-1 enqueue). A stale target — a cycle that a timeout already ended, or
//!   that the sender has since replaced with a newer blocking send — can therefore never wake,
//!   complete or stall another cycle.
//!
//!   The cycle's waiter is removed by [`remove_sender_waiter_for_cycle_locked`], which matches
//!   the whole triple.
//!
//! * **A task** is removed by [`remove_sender_waiters_for_task_locked`] (exit, death, reap). It
//!   matches the numeric TID, deliberately. A TID names one TCB for the life of the system:
//!   registration of an existing TID is a no-op, the dynamic allocator never hands out a TID a TCB
//!   still carries, and no production path removes a live or dead TCB from the table (the one
//!   slot-clearing owner, `spawn_reservation::clear_reservation_slot`, only clears a reservation
//!   or an undone thread spawn that never ran). Every waiter carrying the TID therefore belongs
//!   to the TCB being torn down, whatever ASID or generation it recorded, and removing all of
//!   them is exactly the lifecycle contract. Matching a stored ASID instead would only leave such
//!   a waiter behind with nobody left to remove it.
//!
//! Both removals keep the queue dense and in arrival order, as a receiver's take does, so a
//! removal never lets a later sender overtake an earlier one.
//!
//! An ASID-less sender is a cycle with `asid: None`. It is claimed only by a TCB whose ASID is
//! also `None`, and it is woken without a completion (there is no incarnation to bind one to).

use super::{IpcSubsystem, KernelState, MAX_ENDPOINT_SENDER_WAITERS, SenderWaiter};

use crate::kernel::ipc::{SenderWakeTarget, ThreadId};
use crate::kernel::task::{
    BlockedSyscallClass, BlockedSyscallCompletion, TaskStatus, ThreadControlBlock, WaitReason,
};

/// How a blocking-send cycle ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SendCycleEnding {
    /// A receiver consumed the message; `result` is the completion it publishes
    /// (`KernelState::SEND_COMPLETION_OK` on every production site).
    Consumed { result: u64 },
    /// The armed deadline `deadline` expired; `now` is the clock it was compared against. The
    /// claim also requires the deadline to still be the armed one and still due.
    TimedOut { deadline: u64, now: u64 },
}

/// THE rank-2 claim of one blocking-send cycle. Returns `true` iff this caller ended the cycle.
///
/// On success, in this one acquisition: the cycle's completion is parked (when the sender has an
/// ASID), the sender becomes `Runnable`, and its deadline is disarmed. On refusal nothing is
/// written.
pub(crate) fn claim_blocked_send_cycle_locked(
    tcbs: &mut [Option<ThreadControlBlock>],
    target: SenderWakeTarget,
    ending: SendCycleEnding,
) -> bool {
    let Some(tcb) = tcbs
        .iter_mut()
        .flatten()
        .find(|t| t.tid == target.tid && t.asid == target.asid)
    else {
        return false;
    };
    if tcb.blocked_send_generation != target.send_generation {
        return false;
    }
    if !matches!(tcb.status, TaskStatus::Blocked(WaitReason::EndpointSend(_))) {
        return false;
    }
    let (result, timed_out) = match ending {
        SendCycleEnding::Consumed { result } => (result, false),
        SendCycleEnding::TimedOut { deadline, now } => {
            if tcb.ipc_timeout_deadline != Some(deadline) || now < deadline {
                return false;
            }
            (KernelState::SEND_COMPLETION_TIMED_OUT, true)
        }
    };
    if let Some(asid) = target.asid {
        tcb.pending_syscall_completion = Some(BlockedSyscallCompletion {
            syscall_class: BlockedSyscallClass::IpcSend,
            result,
            tid: target.tid.0,
            asid,
            blocked_generation: target.send_generation,
        });
    }
    tcb.status = TaskStatus::Runnable;
    tcb.ipc_timeout_deadline = None;
    tcb.ipc_timeout_fired = timed_out;
    true
}

/// Remove the waiter of exactly one blocking-send cycle, compacting its queue. Rank 3.
///
/// Returns the removed waiter and its endpoint index, or `None` when no queue holds that exact
/// `{tid, asid, send_generation}` (a receiver took it first).
pub(crate) fn remove_sender_waiter_for_cycle_locked(
    ipc: &mut IpcSubsystem,
    target: SenderWakeTarget,
) -> Option<(SenderWaiter, usize)> {
    for (endpoint_idx, queue) in ipc.endpoint_sender_waiters.iter_mut().enumerate() {
        let queue = &mut queue[..MAX_ENDPOINT_SENDER_WAITERS];
        let Some(slot) = queue
            .iter()
            .position(|s| s.is_some_and(|w| w.wake_target() == target))
        else {
            continue;
        };
        let removed = queue[slot].take().expect("position guarantees Some");
        compact_sender_queue(queue);
        return Some((removed, endpoint_idx));
    }
    None
}

/// Remove every waiter of the task `tid` from every queue, compacting each. Rank 3.
///
/// Each removed waiter that still owns a transferred capability is reported in `orphaned` (with
/// its endpoint index) so the caller can settle its envelope once, after rank 3 is released.
/// Returns how many were reported.
pub(crate) fn remove_sender_waiters_for_task_locked(
    ipc: &mut IpcSubsystem,
    tid: ThreadId,
    orphaned: &mut [Option<(SenderWaiter, usize)>],
) -> usize {
    let mut orphaned_n = 0usize;
    for (endpoint_idx, queue) in ipc.endpoint_sender_waiters.iter_mut().enumerate() {
        let queue = &mut queue[..MAX_ENDPOINT_SENDER_WAITERS];
        let mut removed_any = false;
        for slot in queue.iter_mut() {
            if !slot.is_some_and(|w| w.tid == tid) {
                continue;
            }
            removed_any = true;
            if let Some(removed) = slot.take()
                && removed.msg.transferred_cap().is_some()
                && orphaned_n < orphaned.len()
            {
                orphaned[orphaned_n] = Some((removed, endpoint_idx));
                orphaned_n += 1;
            }
        }
        if removed_any {
            compact_sender_queue(queue);
        }
    }
    orphaned_n
}

/// Shift every present waiter left, keeping arrival order — the same full compaction a
/// receiver's take performs.
fn compact_sender_queue(queue: &mut [Option<SenderWaiter>]) {
    let mut write = 0;
    for read in 0..queue.len() {
        if queue[read].is_some() {
            queue[write] = queue[read].take();
            write += 1;
        }
    }
}
