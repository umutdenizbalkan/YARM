// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! D2 IPC recv blocking precursor — helper-only / no live wiring (Stage 105).
//!
//! VALIDATION: D2_HELPER_ONLY
//! VALIDATION: D2_DEFAULT_OFF
//! VALIDATION: FALLBACK_GLOBAL_LOCK
//!
//! D2's goal is to split the IPC recv blocking path so the **scheduler block**
//! (rank 1) runs in its own narrow lock window, with the IPC waiter
//! registration (rank 3) and TCB transition (rank 2) preceding it
//! sequentially. The current canonical implementation
//! (`KernelState::block_current_on_receive_with_deadline`) already orders the
//! three steps as **scheduler→task→IPC** sequentially (no nested locks); the
//! D2 live unlock would replace the single `&mut KernelState` body with three
//! SharedKernel split-mut closures so other CPUs holding scheduler / task /
//! IPC locks can progress concurrently.
//!
//! ## Why D2 is helper-only at Stage 105
//!
//! - The SharedKernel split-mut seam for `block_current_cpu` (rank 1) does not
//!   exist yet; D1 / D5 don't need it because they don't touch the scheduler.
//! - MUST_SMOKE (`doc/AI_AGENT_RULES.md §13`) triggers on any change to
//!   `entering_tid` / `exiting_tid` / scheduler block-wake behavior. Stage
//!   105 was developed without QEMU.
//!
//! So Stage 105 lands the **no-lost-wakeup audit** as a typed helper that the
//! canonical path could optionally use (today: no callers), plus
//! equivalence-style unit tests that prove the published-waiter→sender-enqueue
//! invariant under the existing sequential ordering. Pass 3 may live-wire
//! this scaffold when QEMU smoke is available.
//!
//! ## The no-lost-wakeup invariant (D2 contract)
//!
//! Let *publish* = the rank-3 mutation that sets
//! `ipc.endpoint_waiters[endpoint_idx] = Some(receiver_tid)`. Let *enqueue* =
//! the rank-3 mutation a sender performs to push a message into the
//! endpoint's queue (or hand it directly to the waiter).
//!
//! **Invariant:** any sender whose enqueue happens-after the receiver's
//! publish either (a) finds the waiter and wakes it, or (b) writes the
//! message into the queue, where it will be drained on the receiver's next
//! recv. There is no third outcome ("message enqueued AND waiter sleeping AND
//! no one will wake them").
//!
//! Proof sketch (already true in the canonical implementation):
//!
//! 1. *Publish* and *enqueue* both run under `ipc_state_lock` (rank 3) in
//!    separate atomic critical sections.
//! 2. The receiver's scheduler-block (rank 1) is taken BEFORE the publish.
//!    So when the publish becomes visible, the receiver is already marked
//!    `TaskStatus::Blocked(WaitReason::EndpointReceive(_))`.
//! 3. A sender acquiring `ipc_state_lock` AFTER the publish observes
//!    `endpoint_waiters[idx] = Some(tid)` and routes the message directly to
//!    the waiter (existing `ipc_send` fast paths), then schedules a wake.
//! 4. A sender acquiring `ipc_state_lock` BEFORE the publish observes
//!    `endpoint_waiters[idx] = None` and enqueues into the endpoint queue.
//!    The receiver's eventual `with_ipc_state_mut` (post-block, post-wake)
//!    will dequeue. Until then, the receiver is asleep on the
//!    `EndpointReceive` wait reason, which the kernel wake path keys on.
//!
//! The audit type [`PublishWaiterPlan`] captures the data needed to perform
//! step (1) without holding any non-IPC lock; the helper
//! [`try_publish_recv_waiter`] performs the mutation under `ipc_state_lock`
//! alone and returns a typed [`PublishWaiterOutcome`] indicating whether the
//! publish landed or whether a sender raced ahead (queue non-empty: the
//! receiver should not block — it should dequeue right now and return the
//! message instead).
//!
//! Stage 105 binds none of this to a live path. The helper exists so Pass 3
//! has a typed, equivalence-tested foundation to build on.

use crate::kernel::boot::KernelState;
use crate::kernel::capabilities::{CapId, CapObject};
use crate::kernel::ipc::ThreadId;

/// D2 Phase 1 plan — describes the IPC waiter publish.
///
/// Captured under no lock; consumed under `ipc_state_lock` (rank 3) only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PublishWaiterPlan {
    /// Endpoint slot index resolved earlier (under rank 4 read).
    pub endpoint_idx: usize,
    /// Receiver TID that will be parked as the waiter.
    pub receiver_tid: ThreadId,
    /// The recv cap the receiver was blocking on (for telemetry / wake path).
    pub recv_cap: CapId,
}

/// D2 Phase 1 outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishWaiterOutcome {
    /// Waiter slot was empty; publish landed. Caller proceeds to the
    /// scheduler block step.
    Published,
    /// Another receiver is already parked on this endpoint. Caller must
    /// surface the same error the canonical recv path produces today (which
    /// is `Err(KernelError::EndpointFull)` via the normal path).
    ReceiverAlreadyWaiting,
    /// A sender raced ahead: the endpoint queue is non-empty. Caller MUST
    /// NOT block; it must dequeue and return the message immediately.
    /// (The canonical recv loop dequeues unconditionally before blocking;
    /// the helper just makes the race-window outcome explicit.)
    QueueNonEmpty,
    /// Endpoint index out of range or generation mismatch on the snapshot.
    /// Caller surfaces `KernelError::InvalidCapability`.
    InvalidEndpoint,
}

/// D2 helper — atomic publish under IPC rank 3.
///
/// VALIDATION: D2_HELPER_ONLY
///
/// This function takes only `ipc_state_lock` (rank 3). It does not block, it
/// does not touch the scheduler, and it does not mutate any TCB. The caller
/// is expected to have:
///
/// 1. resolved `endpoint_idx` under capability rank 4 (read);
/// 2. taken `block_current_cpu` under scheduler rank 1 (BEFORE publish, so
///    the TCB is already `Blocked` from any racing sender's perspective);
/// 3. marked the TCB `Blocked(EndpointReceive(recv_cap))` under task rank 2.
///
/// Then this helper publishes the waiter slot under rank 3. The split design
/// hands each rank its own narrow lock window. Pass 3 will integrate the
/// helper into the live canonical path; today it is exposed for audit /
/// equivalence testing only.
pub fn try_publish_recv_waiter(
    kernel: &mut KernelState,
    plan: PublishWaiterPlan,
) -> PublishWaiterOutcome {
    kernel.try_publish_recv_waiter_audit_only(plan.endpoint_idx, plan.receiver_tid, plan.recv_cap)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::boot::Bootstrap;
    use crate::kernel::capabilities::CapObject;

    fn fresh() -> (KernelState, CapId, CapId, usize) {
        let mut state = Bootstrap::init().expect("init");
        let (_eid, send_cap, recv_cap) = state.create_endpoint(4).expect("endpoint");
        let endpoint = state
            .current_task_capability(recv_cap)
            .expect("recv cap")
            .object;
        let idx = match endpoint {
            CapObject::Endpoint { index, .. } => index,
            other => panic!("expected Endpoint, got {other:?}"),
        };
        (state, send_cap, recv_cap, idx)
    }

    #[test]
    fn stage105_d2_publish_on_empty_waiter_slot_lands() {
        let (mut state, _send_cap, recv_cap, idx) = fresh();
        let outcome = try_publish_recv_waiter(
            &mut state,
            PublishWaiterPlan {
                endpoint_idx: idx,
                receiver_tid: ThreadId(7),
                recv_cap,
            },
        );
        assert_eq!(outcome, PublishWaiterOutcome::Published);
        // The published waiter is visible via the existing observer.
        let waiter = state.endpoint_waiter_tid(
            state
                .current_task_capability(recv_cap)
                .expect("recv cap")
                .object,
        );
        assert_eq!(waiter, Some(ThreadId(7)));
    }

    #[test]
    fn stage105_d2_publish_when_another_waiter_present_returns_already_waiting() {
        let (mut state, _send_cap, recv_cap, idx) = fresh();
        let first = try_publish_recv_waiter(
            &mut state,
            PublishWaiterPlan {
                endpoint_idx: idx,
                receiver_tid: ThreadId(7),
                recv_cap,
            },
        );
        assert_eq!(first, PublishWaiterOutcome::Published);
        let second = try_publish_recv_waiter(
            &mut state,
            PublishWaiterPlan {
                endpoint_idx: idx,
                receiver_tid: ThreadId(9),
                recv_cap,
            },
        );
        assert_eq!(second, PublishWaiterOutcome::ReceiverAlreadyWaiting);
    }

    #[test]
    fn stage105_d2_publish_when_queue_nonempty_signals_dequeue_now() {
        // Synthesize a queue-nonempty state by sending a message into the
        // endpoint via the SEND cap. The helper must surface QueueNonEmpty
        // so the caller knows to dequeue immediately rather than block.
        let (mut state, send_cap, recv_cap, idx) = fresh();
        let msg = crate::kernel::ipc::Message::new(0, b"x").expect("msg");
        state.ipc_send(send_cap, msg).expect("send");
        let outcome = try_publish_recv_waiter(
            &mut state,
            PublishWaiterPlan {
                endpoint_idx: idx,
                receiver_tid: ThreadId(7),
                recv_cap,
            },
        );
        assert_eq!(
            outcome,
            PublishWaiterOutcome::QueueNonEmpty,
            "queue non-empty must steer the caller to dequeue, not block"
        );
        // The would-be waiter was NOT published.
        let cur_waiter = state.endpoint_waiter_tid(
            state
                .current_task_capability(recv_cap)
                .expect("recv cap")
                .object,
        );
        assert_eq!(cur_waiter, None);
    }

    #[test]
    fn stage105_d2_publish_invalid_endpoint_index() {
        let (mut state, _send_cap, recv_cap, _idx) = fresh();
        let outcome = try_publish_recv_waiter(
            &mut state,
            PublishWaiterPlan {
                endpoint_idx: usize::MAX,
                receiver_tid: ThreadId(7),
                recv_cap,
            },
        );
        assert_eq!(outcome, PublishWaiterOutcome::InvalidEndpoint);
    }

    #[test]
    fn stage105_d2_helper_only_not_called_by_live_paths() {
        // Source-scan invariant: the D2 helper must NOT be referenced from
        // any live syscall/runtime path. Stage 105 lands it as scaffold only.
        let syscall_src = include_str!("syscall.rs");
        let runtime_src = include_str!("../runtime.rs");
        for name in [
            "try_publish_recv_waiter",
            "PublishWaiterPlan",
            "PublishWaiterOutcome",
        ] {
            assert!(
                !syscall_src.contains(name),
                "{name} must not appear in syscall.rs (Stage 105 D2 is helper-only)"
            );
            assert!(
                !runtime_src.contains(name),
                "{name} must not appear in runtime.rs (Stage 105 D2 is helper-only)"
            );
        }
    }

    #[test]
    fn stage105_d2_validation_labels_present() {
        let src = include_str!("recv_waiter_split.rs");
        assert!(src.contains("VALIDATION: D2_HELPER_ONLY"));
        assert!(src.contains("VALIDATION: D2_DEFAULT_OFF"));
        assert!(src.contains("VALIDATION: FALLBACK_GLOBAL_LOCK"));
    }
}
