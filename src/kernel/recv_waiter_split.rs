// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! D2 IPC recv blocking split — typed waiter-publish engine (Stage 105
//! scaffold; Stage 106 / Pass 3 live-wired into the canonical blocking-recv
//! path).
//!
//! VALIDATION: D2_LIVE_SPLIT — `KernelState::publish_recv_waiter_live` is
//! called from `block_current_on_receive_with_deadline` (the canonical
//! endpoint blocking-recv path) since Stage 106. The live primitive performs
//! the atomic queue-recheck + waiter publish in one rank-3 critical section
//! and steers the no-lost-wakeup unwind via
//! [`PublishWaiterOutcome::QueueNonEmpty`].
//! VALIDATION: FALLBACK_GLOBAL_LOCK — the notification-recv blocking path,
//! sender-waiter handling, mapped/shared receive, and every non-endpoint
//! blocking case keep their existing canonical code, unchanged.
//!
//! NOT SMOKE-ACCEPTED: the Stage 106 live wiring was developed without QEMU.
//! Per MUST_SMOKE (`doc/AI_AGENT_RULES.md §13`) the branch requires x86_64
//! `-smp 1` core smoke and optional-FS strict smoke before merge acceptance.
//!
//! ## Live vs audit primitives
//!
//! - [`KernelState::publish_recv_waiter_live`] (Stage 106, **live**):
//!   overwrite semantics for a pre-existing waiter — byte-identical to the
//!   pre-Stage-106 unconditional `endpoint_waiters[idx] = Some(tid)` write —
//!   plus the `QueueNonEmpty` race steer. Never returns
//!   `ReceiverAlreadyWaiting`.
//! - [`KernelState::try_publish_recv_waiter_audit_only`] (Stage 105,
//!   helper-only): refuses on a pre-existing waiter. Retained for the future
//!   strict-single-waiter design study; not on any live path.
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
//! The plan type [`PublishWaiterPlan`] captures the data needed to perform
//! step (1) without holding any non-IPC lock; the helper
//! [`try_publish_recv_waiter`] performs the mutation under `ipc_state_lock`
//! alone and returns a typed [`PublishWaiterOutcome`] indicating whether the
//! publish landed or whether a sender raced ahead (queue non-empty: the
//! receiver should not block — it should dequeue right now and return the
//! message instead).
//!
//! ## The Stage 106 live unwind (QueueNonEmpty after scheduler block)
//!
//! In the live path, the publish runs AFTER `block_current_cpu` (rank 1) and
//! the TCB transition (rank 2). When the live primitive reports
//! `QueueNonEmpty` (sender enqueued between the caller's Phase-1 empty
//! dequeue and this publish — unreachable while the global lock spans both,
//! mandatory once the SharedKernel seam splits the borrow), the caller must
//! NOT remain blocked. The unwind in
//! `block_current_on_receive_with_deadline` restores the task via
//! `wake_tid_to_runnable` (which also clears the staged deadline) and
//! returns, so the caller's Phase-2 dequeue drains the raced message.
//! Telemetry: `d2_publish_race_unwinds` (always 0 pre-seam-split).

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

    // ── Stage 106 / Pass 3: D2 live-wire tests ────────────────────────────────

    #[test]
    fn stage106_d2_live_wire_call_site_present() {
        // Replaces the Stage 105 helper-only assertion: the live primitive
        // must be called from the canonical blocking-recv path, and the
        // canonical publish must now route through it.
        let ipc_src = include_str!("boot/ipc_state.rs");
        assert!(
            ipc_src.contains("fn publish_recv_waiter_live"),
            "live primitive must exist in ipc_state.rs"
        );
        assert!(
            ipc_src.contains(
                "self.publish_recv_waiter_live(endpoint_idx, ThreadId(blocked_tid), recv_cap)"
            ),
            "block_current_on_receive_with_deadline must route the publish through the live primitive"
        );
        assert!(
            ipc_src.contains("D2_PUBLISH_RACE_UNWIND"),
            "the no-lost-wakeup unwind branch must exist"
        );
        // The audit-only primitive stays helper-only (no syscall/runtime use).
        let syscall_src = include_str!("syscall.rs");
        let runtime_src = include_str!("../runtime.rs");
        for name in [
            "try_publish_recv_waiter",
            "try_publish_recv_waiter_audit_only",
        ] {
            assert!(
                !syscall_src.contains(name),
                "{name} must not appear in syscall.rs"
            );
            assert!(
                !runtime_src.contains(name),
                "{name} must not appear in runtime.rs"
            );
        }
    }

    #[test]
    fn stage106_d2_validation_labels_present() {
        let src = include_str!("recv_waiter_split.rs");
        assert!(src.contains("VALIDATION: D2_LIVE_SPLIT"));
        assert!(src.contains("VALIDATION: FALLBACK_GLOBAL_LOCK"));
        assert!(
            src.contains("NOT SMOKE-ACCEPTED"),
            "module must carry the not-smoke-accepted disclosure until smoke runs"
        );
        let ipc_src = include_str!("boot/ipc_state.rs");
        assert!(
            ipc_src.contains("VALIDATION: D2_LIVE_SPLIT"),
            "live call site must carry the D2_LIVE_SPLIT label"
        );
    }

    #[test]
    fn stage106_d2_blocked_recv_publishes_waiter_and_counts_telemetry() {
        // Blocked endpoint recv must publish the waiter through the live
        // primitive (telemetry proves routing) and stage the deadline.
        let mut state = Bootstrap::init().expect("init");
        state.register_task(42).expect("task");
        state.enqueue_current_cpu(42).expect("enqueue");
        state.dispatch_next_task().expect("dispatch");
        state.idle_re_enqueue_for_test().expect("idle");
        let (_eid, _send_cap, recv_cap) = state.create_endpoint(4).expect("endpoint");

        assert_eq!(state.ipc_path_telemetry().d2_recv_waiter_publishes, 0);
        let result = state.ipc_recv(recv_cap).expect("blocking recv");
        assert!(
            result.is_none(),
            "empty endpoint recv blocks (returns None)"
        );
        let telem = state.ipc_path_telemetry();
        assert_eq!(
            telem.d2_recv_waiter_publishes, 1,
            "blocked recv must publish through the live primitive"
        );
        assert_eq!(
            telem.d2_publish_race_unwinds, 0,
            "no race unwind under the serialized global lock"
        );
    }

    #[test]
    fn stage106_d2_sender_after_waiter_delivers_and_wakes() {
        // sender-after-waiter: receiver (tid 0) blocks — waiter published via
        // the live primitive — then a sender (tid 1) delivers; the waiter is
        // consumed and the receiver becomes Runnable. Mirrors the canonical
        // recv_on_empty_endpoint_blocks_then_send_wakes pattern with the new
        // D2 telemetry assertions.
        use crate::kernel::task::{TaskStatus, WaitReason};
        let mut state = Bootstrap::init().expect("init");
        state.register_task(1).expect("register task 1");
        state.enqueue_current_cpu(1).expect("queue task 1");
        let (_eid, send_cap, recv_cap) = state.create_endpoint(2).expect("endpoint");
        let endpoint_obj = state
            .current_task_capability(recv_cap)
            .expect("recv cap")
            .object;
        let send_cap_task1 = state
            .grant_capability_task_to_task(0, send_cap, 1)
            .expect("dup send cap to task1");

        assert_eq!(state.ipc_path_telemetry().d2_recv_waiter_publishes, 0);
        let blocked = state.ipc_recv(recv_cap).expect("blocking recv");
        assert!(blocked.is_none());
        assert_eq!(
            state.task_status(0),
            Some(TaskStatus::Blocked(WaitReason::EndpointReceive(recv_cap)))
        );
        assert_eq!(
            state.ipc_path_telemetry().d2_recv_waiter_publishes,
            1,
            "blocked recv must publish through the live primitive"
        );
        assert_eq!(state.endpoint_waiter_tid(endpoint_obj), Some(ThreadId(0)));

        // Sender arrives after the publish (current task is now tid 1).
        let msg = crate::kernel::ipc::Message::new(1, b"after").expect("msg");
        state
            .ipc_send(send_cap_task1, msg)
            .expect("send wakes waiter");
        assert_eq!(
            state.task_status(0),
            Some(TaskStatus::Runnable),
            "sender-after-waiter must wake the published waiter"
        );
        assert_eq!(
            state.endpoint_waiter_tid(endpoint_obj),
            None,
            "the published waiter must be consumed by the wake path"
        );
    }

    #[test]
    fn stage106_d2_sender_before_waiter_dequeues_without_publish() {
        // sender-before-waiter: message already queued; recv dequeues
        // immediately; the publish never happens (telemetry stays 0).
        let mut state = Bootstrap::init().expect("init");
        let (_eid, send_cap, recv_cap) = state.create_endpoint(4).expect("endpoint");
        let msg = crate::kernel::ipc::Message::new(0, b"before").expect("msg");
        state.ipc_send(send_cap, msg).expect("send");

        let got = state.ipc_recv(recv_cap).expect("recv");
        assert!(got.is_some(), "queued message must be dequeued immediately");
        assert_eq!(got.unwrap().as_slice(), b"before");
        assert_eq!(
            state.ipc_path_telemetry().d2_recv_waiter_publishes,
            0,
            "immediate dequeue must not publish a waiter"
        );
    }

    #[test]
    fn stage106_d2_no_lost_wakeup_unwind_sequence_drains_message() {
        // Executable no-lost-wakeup proof for the future-split race branch:
        // manually replicate the exact unwind sequence the live path performs
        // when publish_recv_waiter_live returns QueueNonEmpty after the
        // scheduler block. The message must be drained, the task must be
        // runnable, and no waiter may remain published.
        use crate::kernel::task::TaskStatus;
        let mut state = Bootstrap::init().expect("init");
        state.register_task(1).expect("register sender task");
        state.enqueue_current_cpu(1).expect("queue sender");
        let (_eid, send_cap, recv_cap) = fresh_endpoint(&mut state);
        let idx = endpoint_index(&state, recv_cap);
        let endpoint_obj = state
            .current_task_capability(recv_cap)
            .expect("recv cap")
            .object;
        let send_cap_task1 = state
            .grant_capability_task_to_task(0, send_cap, 1)
            .expect("dup send cap to task1");

        // Step 1 (receiver tid 0, rank 1): scheduler block + dispatch so the
        // sender task becomes current (mirrors the live block→dispatch).
        let blocked_tid = state.block_current_cpu().expect("block");
        assert_eq!(blocked_tid, 0);
        state.dispatch_next_task().expect("dispatch to sender");
        assert_eq!(state.current_tid(), Some(1));

        // Step 2 (sender tid 1): racing send lands AFTER the block but
        // BEFORE the publish — exactly the window the unwind handles. No
        // waiter is published yet, so the message goes to the queue.
        let msg = crate::kernel::ipc::Message::new(1, b"raced").expect("msg");
        state.ipc_send(send_cap_task1, msg).expect("racing send");
        assert_eq!(state.endpoint_waiter_tid(endpoint_obj), None);

        // Step 3 (receiver, rank 3): publish observes the non-empty queue.
        let outcome = state.publish_recv_waiter_live(idx, ThreadId(0), recv_cap);
        assert_eq!(
            outcome,
            PublishWaiterOutcome::QueueNonEmpty,
            "publish must detect the raced enqueue"
        );
        assert_eq!(
            state.endpoint_waiter_tid(endpoint_obj),
            None,
            "no waiter may be published on the race branch"
        );

        // Step 4 (receiver): unwind — wake back to runnable, exactly what the
        // live path's QueueNonEmpty branch does via wake_tid_to_runnable.
        state
            .wake_tid_to_runnable_for_test(ThreadId(0))
            .expect("unwind wake");
        assert_eq!(
            state.task_status(0),
            Some(TaskStatus::Runnable),
            "unwound receiver must be runnable (it will re-run Phase-2 dequeue)"
        );

        // Step 5: the raced message is in the queue, the receiver is
        // runnable: drained on its next dequeue. Prove the drain by granting
        // the recv cap to the current task and dequeuing — message NOT lost.
        let recv_cap_task1 = state
            .grant_capability_task_to_task(0, recv_cap, 1)
            .expect("dup recv cap");
        let got = state.ipc_recv(recv_cap_task1).expect("drain");
        assert!(got.is_some(), "raced message must be drained, not lost");
        assert_eq!(got.unwrap().as_slice(), b"raced");
        // Telemetry contract: this manual replication did not go through the
        // live unwind branch, so the counter stays 0 here; the live branch is
        // covered by the source-scan + the counter's existence.
        assert_eq!(state.ipc_path_telemetry().d2_recv_waiter_publishes, 0);
    }

    #[test]
    fn stage106_d2_timeout_deadline_staged_through_live_publish_fires() {
        // Deadline staging must survive the new publish path: a recv with a
        // deadline blocks (publish via the live primitive), and the canonical
        // deadline processor fires the staged deadline at the deadline tick,
        // waking the task with ipc_timeout_fired set and the waiter cleared.
        // (The canonical expiry comparison fires on tick == deadline; the
        // pre-deadline behavior is the canonical wrapping comparison and is
        // intentionally untouched by D2.)
        use crate::kernel::task::TaskStatus;
        let mut state = Bootstrap::init().expect("init");
        state.register_task(1).expect("register task 1");
        state.enqueue_current_cpu(1).expect("queue task 1");
        let (_eid, _send_cap, recv_cap) = state.create_endpoint(4).expect("endpoint");
        let endpoint_obj = state
            .current_task_capability(recv_cap)
            .expect("recv cap")
            .object;

        let deadline_tick = 10u64;
        let blocked = state
            .ipc_recv_until_deadline(recv_cap, deadline_tick)
            .expect("recv with deadline");
        assert!(blocked.is_none());
        assert_eq!(
            state.ipc_path_telemetry().d2_recv_waiter_publishes,
            1,
            "deadline recv must publish through the live primitive"
        );
        assert_eq!(state.endpoint_waiter_tid(endpoint_obj), Some(ThreadId(0)));

        // Deadline tick: the blocked receiver fires; waiter slot cleared;
        // task runnable with ipc_timeout_fired staged for the recv return.
        let fired = state
            .process_ipc_timeout_deadlines(deadline_tick)
            .expect("deadline tick");
        assert_eq!(fired, 1, "deadline must fire at the staged tick");
        assert_eq!(
            state.endpoint_waiter_tid(endpoint_obj),
            None,
            "expired waiter must be cleared from the endpoint slot"
        );
        assert_eq!(state.task_status(0), Some(TaskStatus::Runnable));
    }

    fn fresh_endpoint(state: &mut KernelState) -> (usize, CapId, CapId) {
        let (eid, send_cap, recv_cap) = state.create_endpoint(4).expect("endpoint");
        (eid, send_cap, recv_cap)
    }

    fn endpoint_index(state: &KernelState, recv_cap: CapId) -> usize {
        match state
            .current_task_capability(recv_cap)
            .expect("recv cap")
            .object
        {
            CapObject::Endpoint { index, .. } => index,
            other => panic!("expected Endpoint, got {other:?}"),
        }
    }
}
