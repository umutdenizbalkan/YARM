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
//!
//! # SEND-TIMEOUT-ARB1 — the waiter decides
//!
//! A cycle's message travels with its waiter, so the waiter is the cycle's arbitration token. The
//! one irrevocable step is the rank-3 removal of the exact waiter, and whoever performs it owns the
//! cycle — its ending AND its message's obligations:
//!
//! * a receiver's take (consume or refill) owns delivery, then claims `Consumed`;
//! * the timeout withdraws the waiter first ([`remove_sender_waiter_for_cycle_locked`]), then
//!   claims `TimedOut` and settles the transfer the message still carried;
//! * an endpoint's destruction removes its waiters, so it ends each of their cycles;
//! * a blocker whose waiter could not be published owns its cycle outright (no waiter exists).
//!
//! The rank-2 claim that follows a removal therefore has no competitor: no other actor holds that
//! cycle's waiter, and nothing ends a cycle without one. A timeout that finds no waiter — not yet
//! published, already taken by a receiver, or the cycle already over — changes nothing and leaves
//! the deadline armed, so it can neither report a delivered message as timed out nor end a cycle
//! whose waiter is still to appear. A task's death ends its cycle without a claim.

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
    /// The timeout withdrew the cycle's waiter (SEND-TIMEOUT-ARB1): ownership comes from that
    /// removal, and the drain checked the deadline was due before making it.
    TimedOut,
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
        SendCycleEnding::TimedOut => (KernelState::SEND_COMPLETION_TIMED_OUT, true),
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

/// SEND-TIMEOUT-ARB1: test-only phase handshakes at the two windows a concurrent CPU can land in —
/// between a broad block's task-block (Phase B) and its waiter publication (Phase C), and between
/// the send-timeout drain's two domain claims.
#[cfg(test)]
pub(crate) mod send_arb_phase_hook {
    extern crate std;
    use core::cell::RefCell;
    type BlockHook = alloc::boxed::Box<dyn FnMut(&mut crate::kernel::boot::KernelState)>;
    type DrainHook = alloc::boxed::Box<dyn FnMut()>;
    std::thread_local! {
        static BETWEEN_BLOCK_AND_PUBLISH: RefCell<Option<BlockHook>> = const { RefCell::new(None) };
        static BETWEEN_DRAIN_PHASES: RefCell<Option<DrainHook>> = const { RefCell::new(None) };
    }
    pub(crate) fn arm_between_block_and_publish(hook: BlockHook) {
        BETWEEN_BLOCK_AND_PUBLISH.with(|h| *h.borrow_mut() = Some(hook));
    }
    pub(crate) fn run_between_block_and_publish(state: &mut crate::kernel::boot::KernelState) {
        let hook = BETWEEN_BLOCK_AND_PUBLISH.with(|h| h.borrow_mut().take());
        if let Some(mut hook) = hook {
            hook(state);
        }
    }
    pub(crate) fn arm_between_drain_phases(hook: DrainHook) {
        BETWEEN_DRAIN_PHASES.with(|h| *h.borrow_mut() = Some(hook));
    }
    pub(crate) fn run_between_drain_phases() {
        let hook = BETWEEN_DRAIN_PHASES.with(|h| h.borrow_mut().take());
        if let Some(mut hook) = hook {
            hook();
        }
    }
}
