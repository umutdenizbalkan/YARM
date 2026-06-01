// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::{
    IpcEndpointRecvResult, IpcEndpointSendResult, IpcEndpointSplitRejectReason,
    IpcFastpathResult, KernelError, KernelState, MAX_ENDPOINT_SENDER_WAITERS, MAX_IRQ_LINES,
    NotificationObject,
    ReplyCapRecord, SenderWaiter, kernel_mut, kernel_ref, map_ipc_error,
};
use crate::kernel::capabilities::{CapId, CapObject, CapRights, Capability};
use crate::kernel::ipc::{Endpoint, EndpointMode, Message, ThreadId};
use crate::kernel::syscall::complete_blocked_recv_for_waiter;
use crate::kernel::task::{RecvAbiVariant, TaskStatus, WaitReason};
use yarm_ipc_abi::process_abi::{
    ExecuteRestartReply, ExecuteRestartRequest, PROC_OP_EXECUTE_RESTART,
};

impl KernelState {
    fn wake_tid_to_runnable(&mut self, tid: ThreadId) -> Result<(), KernelError> {
        let old_status = self.task_status(tid.0).ok_or(KernelError::TaskMissing)?;
        crate::yarm_log!("SCHED_WAKE_BEGIN tid={} old_status={:?}", tid.0, old_status);
        if !matches!(
            old_status,
            TaskStatus::Blocked(_) | TaskStatus::Runnable | TaskStatus::Running
        ) {
            crate::yarm_log!(
                "SCHED_WAKE_FAIL tid={} reason=unexpected_status:{:?}",
                tid.0,
                old_status
            );
            return Err(KernelError::WouldBlock);
        }
        if !matches!(old_status, TaskStatus::Runnable) {
            self.with_tcbs_mut(|tcbs| {
                let tcb = tcbs
                    .iter_mut()
                    .flatten()
                    .find(|tcb| tcb.tid.0 == tid.0)
                    .ok_or(KernelError::TaskMissing)?;
                tcb.status = TaskStatus::Runnable;
                Ok::<_, KernelError>(())
            })?;
        }
        crate::yarm_log!("SCHED_WAKE_SET_RUNNABLE tid={} new_status=Runnable", tid.0);
        self.clear_ipc_timeout_for_tid(tid.0)?;
        if self.current_tid() == Some(tid.0) && matches!(old_status, TaskStatus::Running) {
            crate::yarm_log!("SCHED_WAKE_ALREADY_RUNNABLE tid={}", tid.0);
        } else {
            let (cpu, reason) = self.enqueue_woken_task(tid.0)?;
            let queue_len = self
                .with_scheduler_state(|sched| kernel_ref(&sched.scheduler).runnable_count_on(cpu));
            crate::yarm_log!(
                "SCHED_WAKE_ENQUEUE tid={} cpu={} queue_len={} reason={}",
                tid.0,
                cpu.0,
                queue_len,
                reason
            );
        }
        Ok(())
    }

    /// Store the waiter-local Reply cap CapId in the matching global ReplyCapRecord.
    ///
    /// Called by the materialization path after it mints a Reply cap directly into
    /// the waiter's cnode (without delegation tracking).  `ipc_reply` later reads
    /// this value to fast-revoke the exact cnode slot using a kernel-controlled
    /// CapId rather than the user-supplied argument.
    pub(crate) fn set_reply_cap_waiter_cap(&mut self, reply_index: usize, reply_generation: u64, cap: CapId) {
        self.with_ipc_state_mut(|ipc| {
            if reply_index >= super::MAX_REPLY_CAPS {
                return;
            }
            if ipc.reply_cap_generations[reply_index] != reply_generation {
                return;
            }
            if let Some(record) = &mut ipc.reply_caps[reply_index] {
                record.waiter_cap_id = Some(cap);
                crate::yarm_log!(
                    "IPC_RECV_REPLY_CAP_WAITER_CAP_SET reply_index={} reply_gen={} cap={}",
                    reply_index, reply_generation, cap.0
                );
            }
        });
    }

    pub(crate) fn revoke_reply_caps_for_caller(&mut self, caller_tid: u64) -> usize {
        self.with_ipc_state_mut(|ipc| {
            let mut revoked = 0usize;
            for slot in ipc.reply_caps.iter_mut() {
                if slot.is_some_and(|record| record.caller_tid.0 == caller_tid) {
                    *slot = None;
                    revoked += 1;
                }
            }
            revoked
        })
    }

    fn clear_ipc_timeout_for_tid(&mut self, tid: u64) -> Result<(), KernelError> {
        self.with_tcbs_mut(|tcbs| {
            let tcb = tcbs
                .iter_mut()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .ok_or(KernelError::TaskMissing)?;
            tcb.ipc_timeout_deadline = None;
            tcb.ipc_timeout_fired = false;
            Ok::<_, KernelError>(())
        })
    }

    pub(crate) fn consume_ipc_timeout_fired_for_tid(
        &mut self,
        tid: u64,
    ) -> Result<bool, KernelError> {
        self.with_tcbs_mut(|tcbs| {
            let tcb = tcbs
                .iter_mut()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .ok_or(KernelError::TaskMissing)?;
            let fired = tcb.ipc_timeout_fired;
            tcb.ipc_timeout_fired = false;
            Ok::<_, KernelError>(fired)
        })
    }

    fn enqueue_sender_waiter(
        &mut self,
        endpoint_idx: usize,
        waiter: SenderWaiter,
    ) -> Result<(), KernelError> {
        self.with_ipc_state_mut(|ipc| {
            // Defence-in-depth: verify endpoint still exists.
            ipc.endpoints
                .get(endpoint_idx)
                .and_then(Option::as_ref)
                .ok_or(KernelError::WrongObject)?;
            let queue = &mut ipc.endpoint_sender_waiters[endpoint_idx];
            if let Some(slot) = queue[..MAX_ENDPOINT_SENDER_WAITERS].iter_mut().find(|s| s.is_none()) {
                *slot = Some(waiter);
                return Ok(());
            }
            Err(KernelError::EndpointQueueFull)
        })
    }

    fn resolve_send_cap_task_local(&self, send_cap: CapId) -> Result<Capability, KernelError> {
        let cnode = self.current_task_cnode().ok_or(KernelError::TaskMissing)?;
        self.capability_for_cnode_local(cnode, send_cap)
            .ok_or(KernelError::InvalidCapability)
    }

    fn resolve_recv_cap_task_local(&self, recv_cap: CapId) -> Result<Capability, KernelError> {
        let cnode = self.current_task_cnode().ok_or(KernelError::TaskMissing)?;
        self.capability_for_cnode_local(cnode, recv_cap)
            .ok_or(KernelError::InvalidCapability)
    }

    fn mint_capability_for_active_cnode(
        &mut self,
        capability: Capability,
    ) -> Result<CapId, KernelError> {
        let cnode = self.current_task_cnode().ok_or(KernelError::TaskMissing)?;
        self.mint_capability_in_cnode(cnode, capability)
    }

    fn block_current_on_receive_with_deadline(
        &mut self,
        endpoint_idx: usize,
        recv_cap: CapId,
        deadline: Option<u64>,
    ) -> Result<ThreadId, KernelError> {
        let blocked_tid = self.block_current_cpu().ok_or(KernelError::TaskMissing)?;
        crate::yarm_log!("SCHED_BLOCK tid={}", blocked_tid);
        self.with_tcbs_mut(|tcbs| {
            let tcb = tcbs
                .iter_mut()
                .flatten()
                .find(|tcb| tcb.tid.0 == blocked_tid)
                .ok_or(KernelError::TaskMissing)?;
            tcb.status = TaskStatus::Blocked(WaitReason::EndpointReceive(recv_cap));
            tcb.ipc_timeout_deadline = deadline;
            tcb.ipc_timeout_fired = false;
            Ok::<_, KernelError>(())
        })?;
        // Publish receiver waiter under ipc_state_lock (rank 3), AFTER TCB is marked
        // Blocked under task_state_lock (rank 2) above — sequential ordering guarantees
        // the waker sees a consistent Blocked TCB when it finds this slot.
        self.with_ipc_state_mut(|ipc| {
            ipc.endpoint_waiters[endpoint_idx] = Some(ThreadId(blocked_tid));
        });
        crate::yarm_log!(
            "IPC_RECV_BLOCK_REGISTER endpoint={} tid={}",
            endpoint_idx,
            blocked_tid
        );
        let _ = self.dispatch_next_task()?;
        Ok(ThreadId(blocked_tid))
    }

    fn block_current_on_send_with_deadline(
        &mut self,
        endpoint_idx: usize,
        send_cap: CapId,
        msg: Message,
        deadline: Option<u64>,
    ) -> Result<ThreadId, KernelError> {
        let blocked_tid = self.block_current_cpu().ok_or(KernelError::TaskMissing)?;
        crate::yarm_log!("SCHED_BLOCK tid={}", blocked_tid);
        self.with_tcbs_mut(|tcbs| {
            let tcb = tcbs
                .iter_mut()
                .flatten()
                .find(|tcb| tcb.tid.0 == blocked_tid)
                .ok_or(KernelError::TaskMissing)?;
            tcb.status = TaskStatus::Blocked(WaitReason::EndpointSend(send_cap));
            tcb.ipc_timeout_deadline = deadline;
            tcb.ipc_timeout_fired = false;
            Ok::<_, KernelError>(())
        })?;
        self.enqueue_sender_waiter(
            endpoint_idx,
            SenderWaiter {
                tid: ThreadId(blocked_tid),
                msg,
            },
        )?;
        let _ = self.dispatch_next_task()?;
        Ok(ThreadId(blocked_tid))
    }

    pub(crate) fn wake_waiter_for_endpoint(
        &mut self,
        endpoint_idx: usize,
    ) -> Result<(), KernelError> {
        let waiter_tid = self.with_ipc_state_mut(|ipc| ipc.endpoint_waiters[endpoint_idx].take());
        if let Some(waiter_tid) = waiter_tid {
            crate::yarm_log!("SCHED_WAKE tid={}", waiter_tid.0);
            self.wake_tid_to_runnable(waiter_tid)?;
        }
        Ok(())
    }

    /// Enqueue `msg` into the endpoint at `endpoint_idx` and wake any waiter.
    ///
    /// This is the canonical "supervisor notify" pattern used by fault reporting,
    /// task-exit, transfer-revoke, and TLB-shootdown escalation.  It enforces the
    /// lock ordering:
    ///
    /// - Enqueue under `ipc_state_lock` (rank 3) so the message is visible before
    ///   the lock is released.
    /// - Wake the waiter **after** releasing `ipc_state_lock` — `wake_tid_to_runnable`
    ///   acquires `task_state_lock` (rank 2), which ranks below IPC (rank 3) and
    ///   must therefore not be acquired while the IPC lock is held.
    pub(crate) fn send_message_to_endpoint_and_wake(
        &mut self,
        endpoint_idx: usize,
        msg: crate::kernel::ipc::Message,
    ) -> Result<(), KernelError> {
        self.with_ipc_state_mut(|ipc| {
            let Some(ep_storage) = ipc.endpoints.get_mut(endpoint_idx).and_then(Option::as_mut) else {
                return Err(KernelError::WrongObject);
            };
            kernel_mut(ep_storage).send(msg).map_err(|_| KernelError::EndpointQueueFull)?;
            Ok(())
        })?;
        let _ = self.wake_waiter_for_endpoint(endpoint_idx);
        Ok(())
    }

    pub(crate) fn process_ipc_timeout_deadlines(
        &mut self,
        now_tick: u64,
    ) -> Result<usize, KernelError> {
        let mut expired = [None; super::MAX_TASKS];
        let mut expired_count = 0usize;
        self.with_tcbs_mut(|tcbs| {
            for tcb in tcbs.iter_mut().flatten() {
                let Some(deadline) = tcb.ipc_timeout_deadline else {
                    continue;
                };
                let blocked_ipc = matches!(
                    tcb.status,
                    TaskStatus::Blocked(WaitReason::EndpointReceive(_))
                        | TaskStatus::Blocked(WaitReason::EndpointSend(_))
                );
                if !blocked_ipc {
                    continue;
                }
                if now_tick.wrapping_sub(deadline) > 0 || now_tick == deadline {
                    tcb.status = TaskStatus::Runnable;
                    tcb.ipc_timeout_deadline = None;
                    tcb.ipc_timeout_fired = true;
                    if expired_count < expired.len() {
                        expired[expired_count] = Some(tcb.tid);
                        expired_count += 1;
                    }
                }
            }
            Ok::<_, KernelError>(())
        })?;

        if expired_count == 0 {
            return Ok(0);
        }

        self.with_ipc_state_mut(|ipc| {
            for tid in expired.iter().flatten().copied() {
                for waiter in ipc.endpoint_waiters.iter_mut() {
                    if *waiter == Some(tid) {
                        *waiter = None;
                    }
                }
                for queue in ipc.endpoint_sender_waiters.iter_mut() {
                    for slot in queue.iter_mut() {
                        if slot.as_ref().is_some_and(|w| w.tid == tid) {
                            *slot = None;
                        }
                    }
                }
                for waiter in ipc.notification_waiters.iter_mut() {
                    if *waiter == Some(tid) {
                        *waiter = None;
                    }
                }
            }
        });

        for tid in expired.iter().flatten().copied() {
            let _ = self.enqueue_task(tid.0)?;
        }
        Ok(expired_count)
    }

    pub(crate) fn resolve_endpoint_index(&self, object: CapObject) -> Result<usize, KernelError> {
        let limits = self.runtime_capacity_config();
        match object {
            CapObject::Endpoint { index, generation } => self.with_ipc_state(|ipc| {
                if index >= limits.max_endpoints {
                    return Err(KernelError::WrongObject);
                }
                if ipc.endpoints[index].is_none() {
                    return Err(KernelError::WrongObject);
                }
                if ipc.endpoint_generations[index] != generation {
                    return Err(KernelError::StaleCapability);
                }
                Ok(index)
            }),
            CapObject::Kernel
            | CapObject::AddressSpace { .. }
            | CapObject::IovaSpace { .. }
            | CapObject::MemoryObject { .. }
            | CapObject::DmaRegion { .. }
            | CapObject::Notification { .. }
            | CapObject::Reply { .. }
            | CapObject::Irq { .. } => Err(KernelError::WrongObject),
        }
    }

    /// Stage 4C/4D endpoint-domain split receive helper.
    ///
    /// Preconditions: the caller has already validated the receive capability and
    /// endpoint generation. This helper mutates only the selected buffered endpoint
    /// queue under `ipc_state_lock`.
    ///
    /// Stage 4C: no sender waiter — dequeue the plain message and return Received.
    ///
    /// Stage 4D: plain sender waiter at queue head — two-phase refill:
    ///   Phase 1 (here): dequeue receiver message, dequeue first sender waiter,
    ///   enqueue sender's message into the newly-freed slot.
    ///   Phase 2 (caller, outside lock): wake sender via apply_split_sender_wake_plan.
    ///   Returns ReceivedWithSenderWake so the caller can apply the wake plan outside
    ///   any lock.
    ///
    /// Falls back (Ineligible) for every case that requires scheduler/TCB mutation,
    /// capability operations, user-memory access, TrapFrame writes, complex (flagged)
    /// messages, non-buffered endpoints, or sender waiters with complex messages.
    #[allow(dead_code)]
    pub(crate) fn ipc_try_recv_queued_plain_endpoint_only(
        &mut self,
        endpoint_idx: usize,
    ) -> IpcEndpointRecvResult {
        self.with_ipc_state_mut(|ipc| {
            if endpoint_idx >= ipc.endpoints.len() {
                return IpcEndpointRecvResult::Ineligible(
                    IpcEndpointSplitRejectReason::EndpointIndexOutOfRange,
                );
            }
            if ipc.endpoint_waiters[endpoint_idx].is_some() {
                return IpcEndpointRecvResult::Ineligible(
                    IpcEndpointSplitRejectReason::ReceiverWaiterPresent,
                );
            }

            // Stage 4D: peek at sender waiter queue head (position 0) before touching the
            // endpoint. Copies data so no reference is held across the endpoint borrow below.
            let head_waiter: Option<(ThreadId, Message)> =
                ipc.endpoint_sender_waiters[endpoint_idx][0].map(|w| (w.tid, w.msg));

            // If the queue is sparse (position 0 is None but later positions are Some), the
            // gap was left by a timed-out sender; fall back to the full path which handles
            // arbitrary queue state correctly.
            if head_waiter.is_none()
                && ipc.endpoint_sender_waiters[endpoint_idx]
                    .iter()
                    .any(Option::is_some)
            {
                return IpcEndpointRecvResult::Ineligible(
                    IpcEndpointSplitRejectReason::SenderWaiterPresent,
                );
            }

            // If a sender waiter exists at position 0, validate their message before
            // dequeuing anything — ensures we can commit to the full two-phase refill.
            if let Some((_, waiter_msg)) = head_waiter {
                let split_unsafe_flags = Message::FLAG_CAP_TRANSFER
                    | Message::FLAG_CAP_TRANSFER_PLAIN
                    | Message::FLAG_REPLY_CAP;
                if (waiter_msg.flags & split_unsafe_flags) != 0
                    || waiter_msg.transferred_cap().is_some()
                {
                    return IpcEndpointRecvResult::Ineligible(
                        IpcEndpointSplitRejectReason::SenderWaiterPresent,
                    );
                }
            }

            // Borrow endpoint, validate mode and message, then dequeue.
            // Scoped block so the &mut Endpoint borrow ends before the sender-waiter
            // refill accesses ipc.endpoint_sender_waiters and re-borrows ipc.endpoints.
            let received = {
                let Some(endpoint_storage) = ipc.endpoints[endpoint_idx].as_mut() else {
                    return IpcEndpointRecvResult::Ineligible(
                        IpcEndpointSplitRejectReason::EndpointMissing,
                    );
                };
                let endpoint = kernel_mut(endpoint_storage);
                if endpoint.mode() != EndpointMode::Buffered {
                    return IpcEndpointRecvResult::Ineligible(
                        IpcEndpointSplitRejectReason::NonBufferedEndpoint,
                    );
                }
                let Some(message) = endpoint.peek().copied() else {
                    return IpcEndpointRecvResult::Ineligible(
                        IpcEndpointSplitRejectReason::EmptyQueue,
                    );
                };
                let split_unsafe_flags = Message::FLAG_CAP_TRANSFER
                    | Message::FLAG_CAP_TRANSFER_PLAIN
                    | Message::FLAG_REPLY_CAP;
                if (message.flags & split_unsafe_flags) != 0
                    || message.transferred_cap().is_some()
                {
                    return IpcEndpointRecvResult::Ineligible(
                        IpcEndpointSplitRejectReason::TransferOrReplyCapMessage,
                    );
                }
                endpoint
                    .recv()
                    .expect("peeked plain endpoint message must remain queued")
            };

            // Stage 4D two-phase refill: only reached when head_waiter is Some with a
            // plain message (validated above). All mutations stay under ipc_state_lock.
            if let Some((waiter_tid, waiter_msg)) = head_waiter {
                // Phase 1a: remove first sender waiter and compact the queue.
                {
                    let queue = &mut ipc.endpoint_sender_waiters[endpoint_idx];
                    queue[0] = None;
                    for idx in 1..queue.len() {
                        queue[idx - 1] = queue[idx].take();
                    }
                }
                // Phase 1b: enqueue the sender's message into the slot freed by recv.
                {
                    let ep = kernel_mut(
                        ipc.endpoints[endpoint_idx]
                            .as_mut()
                            .expect("endpoint must remain present after recv dequeue"),
                    );
                    ep.send(waiter_msg)
                        .expect("one slot must be free after recv dequeue");
                }
                crate::yarm_log!(
                    "IPC_RECV_SPLIT_REFILL_QUEUED waiter_tid={}",
                    waiter_tid.0
                );
                return IpcEndpointRecvResult::ReceivedWithSenderWake(received, waiter_tid);
            }

            IpcEndpointRecvResult::Received(received)
        })
    }

    /// Apply a deferred scheduler wake plan returned by the Stage 4D split recv helper.
    ///
    /// Must be called outside `ipc_state_lock`. Wakes the sender whose message was
    /// already refilled into the endpoint queue under ipc_state_lock.
    pub(crate) fn apply_split_sender_wake_plan(
        &mut self,
        sender_tid: ThreadId,
    ) -> Result<(), KernelError> {
        crate::yarm_log!("IPC_RECV_SPLIT_REFILL_WAKE_APPLY tid={}", sender_tid.0);
        self.apply_scheduler_wake_plan(super::SchedulerWakePlan::Wake(sender_tid))
    }

    pub(crate) fn ipc_try_send_queued_plain_endpoint_only(
        &mut self,
        endpoint_idx: usize,
        msg: Message,
    ) -> IpcEndpointSendResult {
        self.with_ipc_state_mut(|ipc| {
            if endpoint_idx >= ipc.endpoints.len() {
                return IpcEndpointSendResult::Ineligible(
                    IpcEndpointSplitRejectReason::EndpointIndexOutOfRange,
                );
            }
            let receiver_waiter = ipc.endpoint_waiters[endpoint_idx];
            let has_sender_waiters = ipc.endpoint_sender_waiters[endpoint_idx]
                .iter()
                .any(Option::is_some);

            match (receiver_waiter, has_sender_waiters) {
                (Some(_), true) => {
                    // Both receiver and sender waiters present: complex ordering state.
                    // Fall back to the full IPC send path which handles this correctly.
                    return IpcEndpointSendResult::Ineligible(
                        IpcEndpointSplitRejectReason::SenderWaiterPresent,
                    );
                }
                (Some(receiver_tid), false) => {
                    // Receiver waiter present, no sender waiters.
                    // TID comes from a locked ipc_state_lock read — no unlocked access needed.
                    // Caller must check is_task_recv_v2_blocked (task_state_lock rank 3) before
                    // calling ipc_try_send_to_plain_receiver_endpoint_only (ipc_state_lock rank 4).
                    return IpcEndpointSendResult::ReceiverWaiterFound(receiver_tid);
                }
                (None, true) => {
                    return IpcEndpointSendResult::Ineligible(
                        IpcEndpointSplitRejectReason::SenderWaiterPresent,
                    );
                }
                (None, false) => {
                    // No waiters: fall through to Stage 4E queue-enqueue logic below.
                }
            }

            // Stage 4E: FLAG_REPLY_CAP messages carry a kernel reply-cap handle and
            // must use the full send path (reply-cap semantics require endpoint-side
            // tracking not present here).
            //
            // FLAG_CAP_TRANSFER and FLAG_CAP_TRANSFER_PLAIN are safe for Stage 4E:
            // stash_transfer_handle already moved the cap into the transfer-envelope
            // table before this call, so the message's transferred_cap field is merely
            // a numeric envelope handle.  For the no-receiver buffered-enqueue case,
            // ipc_send_with_optional_deadline does an identical endpoint.send(msg),
            // making Stage 4E a strict behavioural subset of the full path.
            // The receiver's ipc_recv (or ipc_recv_timeout) falls through to the full
            // path to materialise the cap, since Stage 4C/4D still rejects cap-transfer
            // messages on the recv side.
            if (msg.flags & Message::FLAG_REPLY_CAP) != 0 {
                return IpcEndpointSendResult::Ineligible(
                    IpcEndpointSplitRejectReason::TransferOrReplyCapMessage,
                );
            }

            let Some(endpoint_storage) = ipc.endpoints[endpoint_idx].as_mut() else {
                return IpcEndpointSendResult::Ineligible(
                    IpcEndpointSplitRejectReason::EndpointMissing,
                );
            };
            let endpoint = kernel_mut(endpoint_storage);
            if endpoint.mode() != EndpointMode::Buffered {
                return IpcEndpointSendResult::Ineligible(
                    IpcEndpointSplitRejectReason::NonBufferedEndpoint,
                );
            }

            if endpoint.send(msg).is_err() {
                return IpcEndpointSendResult::Ineligible(
                    IpcEndpointSplitRejectReason::EndpointQueueFull,
                );
            }

            IpcEndpointSendResult::Enqueued
        })
    }

    /// Return true if the task identified by `tid` is blocked on a recv-v2 operation.
    /// Acquires task_state_lock (rank 3). Must be called before ipc_state_lock (rank 4).
    pub(crate) fn is_task_recv_v2_blocked(&self, tid: u64) -> bool {
        self.with_tcbs(|tcbs| {
            tcbs.iter()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .and_then(|tcb| tcb.blocked_recv_state.as_ref())
                .is_some_and(|state| state.recv_abi == RecvAbiVariant::RecvV2)
        })
    }

    /// Stage 4F: plain send to a waiting legacy (non-recv-v2) receiver on a buffered endpoint.
    ///
    /// Preconditions (caller must verify before this call):
    ///   - `expected_receiver_tid` is not recv-v2 blocked (checked under task_state_lock rank 3)
    ///   - message has no cap-transfer or reply-cap flags
    ///
    /// Under ipc_state_lock:
    ///   - re-verifies receiver slot still holds expected_receiver_tid
    ///   - enqueues msg into the endpoint queue
    ///   - clears endpoint_waiters slot
    ///
    /// Returns EnqueuedWakeReceiver(tid) on success; caller must call
    /// apply_split_receiver_wake_plan outside the lock.
    pub(crate) fn ipc_try_send_to_plain_receiver_endpoint_only(
        &mut self,
        endpoint_idx: usize,
        expected_receiver_tid: ThreadId,
        msg: Message,
    ) -> IpcEndpointSendResult {
        self.with_ipc_state_mut(|ipc| {
            if endpoint_idx >= ipc.endpoints.len() {
                return IpcEndpointSendResult::Ineligible(
                    IpcEndpointSplitRejectReason::EndpointIndexOutOfRange,
                );
            }
            // Re-verify receiver slot: timeout may have cleared it between pre-check and lock.
            if ipc.endpoint_waiters[endpoint_idx] != Some(expected_receiver_tid) {
                return IpcEndpointSendResult::Ineligible(
                    IpcEndpointSplitRejectReason::ReceiverWaiterPresent,
                );
            }
            // Defense-in-depth: sender waiters should have been screened out by
            // ipc_try_send_queued_plain_endpoint_only returning ReceiverWaiterFound only when
            // no sender waiters are present. Re-check under lock for safety.
            if ipc.endpoint_sender_waiters[endpoint_idx]
                .iter()
                .any(Option::is_some)
            {
                return IpcEndpointSendResult::Ineligible(
                    IpcEndpointSplitRejectReason::SenderWaiterPresent,
                );
            }
            let split_unsafe_flags = Message::FLAG_CAP_TRANSFER
                | Message::FLAG_CAP_TRANSFER_PLAIN
                | Message::FLAG_REPLY_CAP;
            if (msg.flags & split_unsafe_flags) != 0 || msg.transferred_cap().is_some() {
                return IpcEndpointSendResult::Ineligible(
                    IpcEndpointSplitRejectReason::TransferOrReplyCapMessage,
                );
            }
            let Some(endpoint_storage) = ipc.endpoints[endpoint_idx].as_mut() else {
                return IpcEndpointSendResult::Ineligible(
                    IpcEndpointSplitRejectReason::EndpointMissing,
                );
            };
            let endpoint = kernel_mut(endpoint_storage);
            if endpoint.mode() != EndpointMode::Buffered {
                return IpcEndpointSendResult::Ineligible(
                    IpcEndpointSplitRejectReason::NonBufferedEndpoint,
                );
            }
            if endpoint.send(msg).is_err() {
                return IpcEndpointSendResult::Ineligible(
                    IpcEndpointSplitRejectReason::EndpointQueueFull,
                );
            }
            // Clear receiver from waiters; wake is applied outside lock.
            ipc.endpoint_waiters[endpoint_idx] = None;
            crate::yarm_log!(
                "IPC_SEND_SPLIT_ENQUEUED_WAKE_RECEIVER receiver_tid={}",
                expected_receiver_tid.0
            );
            IpcEndpointSendResult::EnqueuedWakeReceiver(expected_receiver_tid)
        })
    }

    /// Apply the deferred receiver-wake plan returned by ipc_try_send_to_plain_receiver_endpoint_only.
    /// Must be called outside ipc_state_lock.
    pub(crate) fn apply_split_receiver_wake_plan(
        &mut self,
        receiver_tid: ThreadId,
    ) -> Result<(), KernelError> {
        crate::yarm_log!("IPC_SEND_SPLIT_RECEIVER_WAKE_APPLY tid={}", receiver_tid.0);
        self.apply_scheduler_wake_plan(super::SchedulerWakePlan::Wake(receiver_tid))
    }

    /// Apply a general-purpose deferred wake plan produced by any kernel domain.
    ///
    /// Callers compute the plan while holding a domain-specific lock, then release
    /// that lock before calling this function.  The function itself acquires only
    /// scheduler-internal state (rank 1–2) which is below all IPC/task/capability
    /// locks, so no lock-ordering violation is possible.
    ///
    /// See `doc/KERNEL_LOCKING.md §SchedulerWakePlan` for the full protocol.
    pub(crate) fn apply_scheduler_wake_plan(
        &mut self,
        plan: super::SchedulerWakePlan,
    ) -> Result<(), KernelError> {
        match plan {
            super::SchedulerWakePlan::None => Ok(()),
            super::SchedulerWakePlan::Wake(tid) => self.wake_tid_to_runnable(tid),
        }
    }

    /// Apply a deferred cooperative-handoff plan.
    ///
    /// `YieldTo(tid)` calls `yield_current_to(tid)` — a one-shot preempt that removes
    /// `tid` from the run-queue and makes it current directly, bypassing FIFO order.
    /// Returns `true` when `tid` became the current task, `false` otherwise.
    ///
    /// Callers that guarantee `tid` was just enqueued (e.g. via `wake_waiter_for_endpoint`)
    /// will always get `true` back.
    ///
    /// Must be called outside all IPC/cap/VM/memory domain locks.
    pub(crate) fn apply_scheduler_handoff_plan(
        &mut self,
        plan: super::SchedulerHandoffPlan,
    ) -> Result<bool, KernelError> {
        match plan {
            super::SchedulerHandoffPlan::None => Ok(false),
            super::SchedulerHandoffPlan::YieldTo(tid) => self.yield_current_to(tid),
        }
    }

    fn resolve_reply_index(&self, object: CapObject) -> Result<usize, KernelError> {
        match object {
            CapObject::Reply { index, generation } => self.with_ipc_state(|ipc| {
                if index >= super::MAX_REPLY_CAPS {
                    return Err(KernelError::WrongObject);
                }
                if ipc.reply_caps[index].is_none() || ipc.reply_cap_generations[index] != generation
                {
                    return Err(KernelError::StaleCapability);
                }
                Ok(index)
            }),
            _ => Err(KernelError::WrongObject),
        }
    }

    pub fn create_reply_cap_for_caller(
        &mut self,
        caller_tid: ThreadId,
        caller_reply_recv_cap: CapId,
        responder_tid: Option<ThreadId>,
    ) -> Result<CapId, KernelError> {
        let reply_capability =
            self.resolve_capability_for_task(caller_tid.0, caller_reply_recv_cap)?;
        if !reply_capability.has_right(CapRights::RECEIVE) {
            return Err(KernelError::MissingRight);
        }
        let reply_endpoint = match reply_capability.object {
            CapObject::Endpoint { .. } => reply_capability.object,
            _ => return Err(KernelError::WrongObject),
        };

        crate::yarm_log!(
            "IPC_CALL_REPLY_CAP_ALLOC_BEGIN caller_tid={} responder={}",
            caller_tid.0,
            responder_tid.map(|t| t.0).unwrap_or(u64::MAX)
        );

        // Phase 1: Reserve a global reply slot with a placeholder caller_cap_id.
        // The real CapId is filled in after the mint succeeds (Phase 3).
        let (slot, generation) = self
            .with_ipc_state_mut(|ipc| {
                for idx in 0..super::MAX_REPLY_CAPS {
                    if ipc.reply_caps[idx].is_none() {
                        let mut next_generation = ipc.reply_cap_generations[idx].wrapping_add(1);
                        if next_generation == 0 {
                            next_generation = 1;
                        }
                        ipc.reply_cap_generations[idx] = next_generation;
                        ipc.reply_caps[idx] = Some(ReplyCapRecord {
                            caller_tid,
                            reply_endpoint,
                            responder_tid,
                            caller_cap_id: CapId(0), // placeholder; updated in Phase 3
                            waiter_cap_id: None,     // filled in when cap is materialized
                        });
                        return Ok::<_, KernelError>((idx, next_generation));
                    }
                }
                Err(KernelError::CapabilityFull)
            })
            .map_err(|err| {
                crate::yarm_log!(
                    "IPC_CALL_REPLY_RECORD_ALLOC_FAIL caller_tid={} err={:?}",
                    caller_tid.0,
                    err
                );
                err
            })?;

        // Phase 2: Mint the Reply cap into the caller's (current) cnode.
        let cap_id = match self.mint_capability_for_active_cnode(Capability::new(
            CapObject::Reply {
                index: slot,
                generation,
            },
            CapRights::SEND,
        )) {
            Ok(id) => id,
            Err(err) => {
                let active_cnode = self.current_task_cnode().map(|c| c.0).unwrap_or(u64::MAX);
                crate::yarm_log!(
                    "IPC_CALL_MINT_REPLY_CAP_FAIL caller_tid={} slot={} cnode={} err={:?}",
                    caller_tid.0,
                    slot,
                    active_cnode,
                    err
                );
                // Rollback: free the slot reserved in Phase 1 so it can be reused.
                self.with_ipc_state_mut(|ipc| {
                    ipc.reply_caps[slot] = None;
                });
                return Err(err);
            }
        };

        // Phase 3: Persist the minted CapId in the record so that ipc_reply can revoke
        // it from the caller's cnode when the reply is eventually delivered.
        self.with_ipc_state_mut(|ipc| {
            if let Some(record) = &mut ipc.reply_caps[slot] {
                record.caller_cap_id = cap_id;
            }
        });

        crate::yarm_log!(
            "IPC_CALL_REPLY_CAP_ALLOC_DONE caller_tid={} slot={} cap={}",
            caller_tid.0,
            slot,
            cap_id.0
        );
        Ok(cap_id)
    }

    /// Return the reply endpoint recorded in the `ReplyCapRecord` for `reply_cap`
    /// (resolved in the **current** task's cnode) without consuming or modifying the record.
    ///
    /// Used by `handle_ipc_reply` in syscall.rs to look up the reply endpoint so
    /// it can call `stash_transfer_handle` before committing to the reply.  The
    /// `ipc_reply` call itself would also validate and then consume the record, so
    /// we can safely peek here without any TOCTOU concern (both paths run single-
    /// threaded inside the kernel lock).
    pub fn reply_cap_peek_endpoint(&self, reply_cap: CapId) -> Result<CapObject, KernelError> {
        let capability = self.resolve_send_cap_task_local(reply_cap)?;
        if !capability.has_right(CapRights::SEND) {
            return Err(KernelError::MissingRight);
        }
        let slot = self.resolve_reply_index(capability.object)?;
        self.with_ipc_state(|ipc| {
            ipc.reply_caps[slot]
                .ok_or(KernelError::StaleCapability)
                .map(|rec| rec.reply_endpoint)
        })
    }

    pub fn ipc_reply(&mut self, reply_cap: CapId, msg: Message) -> Result<(), KernelError> {
        let current_tid = self.current_tid().ok_or(KernelError::TaskMissing)?;
        crate::yarm_log!("IPC_REPLY_ENTER tid={}", current_tid);
        let capability = self.resolve_send_cap_task_local(reply_cap)?;
        if !capability.has_right(CapRights::SEND) {
            return Err(KernelError::MissingRight);
        }
        let slot = match self.resolve_reply_index(capability.object) {
            Ok(slot) => slot,
            Err(err) => return Err(err),
        };
        let replier_tid = ThreadId(self.current_tid().ok_or(KernelError::TaskMissing)?);
        let allowed = self.with_ipc_state(|ipc| {
            let rec = ipc.reply_caps[slot].ok_or(KernelError::StaleCapability)?;
            Ok::<_, KernelError>(rec.responder_tid.is_none_or(|tid| tid == replier_tid))
        })?;
        if !allowed {
            return Err(KernelError::MissingRight);
        }
        let record = self.with_ipc_state_mut(|ipc| {
            let rec = ipc.reply_caps[slot].ok_or(KernelError::StaleCapability)?;
            ipc.reply_caps[slot] = None;
            Ok::<_, KernelError>(rec)
        })?;

        // ── No-alloc reply-cap cleanup ─────────────────────────────────────────
        // Replaces the previous revoke_capability_in_cnode() calls which allocated
        // up to 81920 bytes (Box<[Option<DelegatedCapabilityLink>; 2048]>) inside
        // collect_delegated_descendants() — causing a panic on freestanding AArch64
        // when the kernel heap was exhausted.
        //
        // The narrow fast-revoke path:
        // - performs no heap allocation
        // - does not traverse delegation trees (Reply caps are never delegated)
        // - clears exactly one cnode slot and bumps the generation
        // - returns bool for diagnostics only
        //
        // Failures are non-fatal: the global ReplyCapRecord is already consumed so
        // the reply is irrevocably committed. The worst case of a false return is a
        // leaked cnode slot, not a safety violation.
        let reply_object = capability.object;

        // Revoke the Reply cap from the replier's (current task's) cnode.
        //
        // Prefer `record.waiter_cap_id` (kernel-controlled CapId set during
        // materialization) over the user-supplied `reply_cap` argument. Both
        // should refer to the same slot, but using the kernel-recorded value
        // is more robust against a misbehaving replier passing a different CapId
        // that happens to resolve to the same Reply object.
        //
        // Fall back to `reply_cap` when `waiter_cap_id` is not set (e.g. the
        // receiver consumed the message via the legacy non-v2 recv path that
        // does not populate this field, or the materialization path was not the
        // direct-mint path).
        let replier_cap_to_revoke = record.waiter_cap_id.unwrap_or(reply_cap);
        let replier_ok = if let Some(replier_cnode) = self.current_task_cnode() {
            self.fast_revoke_reply_cap_in_cnode(replier_cnode, replier_cap_to_revoke, reply_object)
        } else {
            false
        };
        crate::yarm_log!(
            "IPC_REPLY_REPLIER_CAP_FAST_REVOKE caller_tid={} replier_tid={} cap={} waiter_cap={} expected={:?} ok={}",
            record.caller_tid.0,
            replier_tid.0,
            reply_cap.0,
            replier_cap_to_revoke.0,
            reply_object,
            replier_ok
        );
        if !replier_ok {
            // If waiter_cap_id revoke failed, attempt with user-supplied reply_cap as
            // a best-effort fallback (covers the case where waiter_cap_id wasn't set).
            if record.waiter_cap_id.is_some() && replier_cap_to_revoke.0 != reply_cap.0 {
                let fallback_ok = if let Some(replier_cnode) = self.current_task_cnode() {
                    self.fast_revoke_reply_cap_in_cnode(replier_cnode, reply_cap, reply_object)
                } else {
                    false
                };
                if fallback_ok {
                    crate::yarm_log!(
                        "IPC_REPLY_FAST_REVOKE_FAIL reason=waiter_cap_mismatch_used_fallback_reply_cap ok={}",
                        fallback_ok
                    );
                } else {
                    crate::yarm_log!(
                        "IPC_REPLY_FAST_REVOKE_FAIL reason=replier_cap_not_found_or_mismatch"
                    );
                }
            } else {
                crate::yarm_log!(
                    "IPC_REPLY_FAST_REVOKE_FAIL reason=replier_cap_not_found_or_mismatch"
                );
            }
        }

        // Revoke the Reply cap that create_reply_cap_for_caller minted into the
        // caller's cnode. record.caller_cap_id == 0 is the "not yet set" sentinel
        // (should never occur in practice; Phase-3 of create_reply_cap_for_caller
        // always updates it before returning to userspace).
        if record.caller_cap_id.0 != 0 {
            let caller_ok = if let Some(caller_cnode) = self.task_cnode(record.caller_tid.0) {
                self.fast_revoke_reply_cap_in_cnode(
                    caller_cnode,
                    record.caller_cap_id,
                    reply_object,
                )
            } else {
                false
            };
            crate::yarm_log!(
                "IPC_REPLY_CALLER_CAP_FAST_REVOKE caller_tid={} cap={} expected={:?} ok={}",
                record.caller_tid.0,
                record.caller_cap_id.0,
                reply_object,
                caller_ok
            );
            if !caller_ok {
                crate::yarm_log!(
                    "IPC_REPLY_FAST_REVOKE_FAIL reason=caller_cap_not_found_or_mismatch"
                );
            }
        }

        // ── Deliver the reply ──────────────────────────────────────────────────
        let endpoint_idx = match self.resolve_endpoint_index(record.reply_endpoint) {
            Ok(idx) => idx,
            Err(err) => return Err(err),
        };
        if let CapObject::Reply { index, generation } = capability.object {
            crate::yarm_log!(
                "IPC_REPLY_OBJECT_OK tid={} cap={} reply_index={} generation={} target_endpoint={}",
                current_tid,
                reply_cap.0,
                index,
                generation,
                endpoint_idx
            );
        }
        // Phase 1: snapshot waiter TID under ipc_state_lock — consistent with the
        // Stage 4K/4L Phase 1–5 discipline.  The lock is released before Phase 2–5
        // so it is never held across user-memory copy, cap ops, or scheduler mutation.
        let opt_waiter_tid: Option<ThreadId> =
            self.with_ipc_state(|ipc| ipc.endpoint_waiters[endpoint_idx]);
        if let Some(waiter_tid) = opt_waiter_tid {
            crate::yarm_log!(
                "IPC_REPLY_DELIVER_TO_WAITER tid={} endpoint={} len={}",
                waiter_tid.0,
                endpoint_idx,
                msg.len
            );
            // Phase 2: confirm recv-v2 under task_state_lock (rank 3) before
            // re-acquiring ipc_state_lock (rank 4) in Phase 4.
            let waiter_recv_v2_blocked = self.with_tcbs(|tcbs| {
                tcbs.iter()
                    .flatten()
                    .find(|tcb| tcb.tid.0 == waiter_tid.0)
                    .and_then(|tcb| tcb.blocked_recv_state.as_ref())
                    .is_some_and(|state| state.recv_abi == RecvAbiVariant::RecvV2)
            });
            if waiter_recv_v2_blocked {
                // Phase 3: complete delivery outside all locks.
                match complete_blocked_recv_for_waiter(self, waiter_tid.0, &msg) {
                    Ok(()) => {
                        crate::yarm_log!(
                            "IPC_REPLY_DELIVER_TO_WAITER_CONSUMED tid={} endpoint={}",
                            waiter_tid.0,
                            endpoint_idx
                        );
                        self.note_ipc_reply_split_delivery();
                        // Phase 4: clear waiter slot under ipc_state_lock.
                        self.ipc_clear_plain_receiver_waiter_only(endpoint_idx, waiter_tid);
                        // Phase 5: wake receiver outside locks.
                        self.apply_scheduler_wake_plan(super::SchedulerWakePlan::Wake(waiter_tid))?;
                        return Ok(());
                    }
                    Err(err) => {
                        crate::yarm_log!(
                            "IPC_RECV_BLOCKED_COMPLETE_FAILED tid={} err={:?}",
                            waiter_tid.0,
                            err
                        );
                        return Err(KernelError::UserMemoryFault);
                    }
                }
            }
            crate::yarm_log!("IPC_REPLY_WAKE_CALLER tid={}", waiter_tid.0);
        }
        // Phase 3: enqueue reply and atomically snapshot receiver waiter under
        // ipc_state_lock (rank 3).  Lock released before Phase 4 scheduler mutation.
        let wake_plan = self.with_ipc_state_mut(|ipc| {
            let ep_storage = ipc.endpoints[endpoint_idx]
                .as_mut()
                .ok_or(KernelError::WrongObject)?;
            kernel_mut(ep_storage)
                .send(msg)
                .map_err(|_| KernelError::EndpointQueueFull)?;
            Ok::<_, KernelError>(
                ipc.endpoint_waiters[endpoint_idx]
                    .take()
                    .map(super::SchedulerWakePlan::Wake)
                    .unwrap_or(super::SchedulerWakePlan::None),
            )
        })?;
        // Phase 4: wake receiver outside all locks.
        self.apply_scheduler_wake_plan(wake_plan)?;
        Ok(())
    }

    pub fn destroy_endpoint(&mut self, endpoint_idx: usize) -> Result<(), KernelError> {
        let limits = self.runtime_capacity_config();
        self.with_ipc_state_mut(|ipc| {
            if endpoint_idx >= limits.max_endpoints || ipc.endpoints[endpoint_idx].is_none() {
                return Err(KernelError::WrongObject);
            }
            ipc.endpoints[endpoint_idx] = None;
            ipc.endpoint_waiters[endpoint_idx] = None;
            ipc.endpoint_sender_waiters[endpoint_idx] = [None; MAX_ENDPOINT_SENDER_WAITERS];
            let mut next_generation = ipc.endpoint_generations[endpoint_idx].wrapping_add(1);
            if next_generation == 0 {
                next_generation = 1;
            }
            ipc.endpoint_generations[endpoint_idx] = next_generation;
            Ok(())
        })?;
        self.with_fault_state_mut(|faults| {
            if faults.fault_handler_endpoint == Some(endpoint_idx) {
                faults.fault_handler_endpoint = None;
            }
        });
        Ok(())
    }

    pub fn create_endpoint(
        &mut self,
        max_depth: usize,
    ) -> Result<(usize, CapId, CapId), KernelError> {
        self.create_endpoint_with_mode(max_depth, EndpointMode::Buffered)
    }

    pub fn create_endpoint_with_mode(
        &mut self,
        max_depth: usize,
        mode: EndpointMode,
    ) -> Result<(usize, CapId, CapId), KernelError> {
        let max_endpoints = self.runtime_capacity_config().max_endpoints;
        // Atomic under ipc_state_lock: find free slot, assign generation, store endpoint.
        // Capability minting happens outside the lock (capability rank 4 > ipc rank 3).
        let (endpoint_idx, generation) = self.with_ipc_state_mut(|ipc| {
            let slot = ipc
                .endpoints
                .iter()
                .take(max_endpoints)
                .position(Option::is_none)
                .ok_or(KernelError::EndpointFull)?;
            let mut next_gen = ipc.endpoint_generations[slot].wrapping_add(1);
            if next_gen == 0 {
                next_gen = 1;
            }
            ipc.endpoint_generations[slot] = next_gen;
            ipc.endpoints[slot] = Some(super::store_kernel_value(
                Endpoint::new_with_mode(max_depth, mode).map_err(map_ipc_error)?,
            ));
            Ok::<_, KernelError>((slot, next_gen))
        })?;
        let send_cap = self.mint_capability_for_active_cnode(Capability::new(
            CapObject::Endpoint { index: endpoint_idx, generation },
            CapRights::SEND,
        ))?;
        let recv_cap = self.mint_capability_for_active_cnode(Capability::new(
            CapObject::Endpoint { index: endpoint_idx, generation },
            CapRights::RECEIVE,
        ))?;
        Ok((endpoint_idx, send_cap, recv_cap))
    }

    pub fn create_notification(
        &mut self,
        max_depth: usize,
    ) -> Result<(usize, CapId, CapId), KernelError> {
        let max_notifications = self.runtime_capacity_config().max_notifications;
        // Slot selection, generation bump, and object storage are atomic under
        // ipc_state_lock. Capability minting happens outside the lock (cap rank
        // 4 > ipc rank 3; acquiring both simultaneously would invert lock order).
        let (notification_idx, generation) = self.with_ipc_state_mut(|ipc| {
            let slot = ipc
                .notifications
                .iter()
                .take(max_notifications)
                .position(Option::is_none)
                .ok_or(KernelError::EndpointFull)?;
            let mut next_gen = ipc.notification_generations[slot].wrapping_add(1);
            if next_gen == 0 {
                next_gen = 1;
            }
            ipc.notification_generations[slot] = next_gen;
            ipc.notifications[slot] = Some(NotificationObject::new(max_depth)?);
            Ok::<_, KernelError>((slot, next_gen))
        })?;

        let notification_cap = self.mint_capability_for_active_cnode(Capability::new(
            CapObject::Notification {
                index: notification_idx,
                generation,
            },
            CapRights::SIGNAL,
        ))?;

        let recv_cap = self.mint_capability_for_active_cnode(Capability::new(
            CapObject::Notification {
                index: notification_idx,
                generation,
            },
            CapRights::RECEIVE,
        ))?;

        Ok((notification_idx, notification_cap, recv_cap))
    }

    fn resolve_notification_index(&self, object: CapObject) -> Result<usize, KernelError> {
        let limits = self.runtime_capacity_config();
        match object {
            CapObject::Notification { index, generation } => self.with_ipc_state(|ipc| {
                if index >= limits.max_notifications || ipc.notifications[index].is_none() {
                    return Err(KernelError::WrongObject);
                }
                if ipc.notification_generations[index] != generation {
                    return Err(KernelError::StaleCapability);
                }
                Ok(index)
            }),
            _ => Err(KernelError::WrongObject),
        }
    }

    pub fn bind_irq_notification(
        &mut self,
        irq_line: u16,
        notification_cap: CapId,
    ) -> Result<(), KernelError> {
        let capability = self
            .capability_service()
            .resolve_current_task_capability(notification_cap)
            .ok_or(KernelError::InvalidCapability)?;
        if !capability.has_right(CapRights::SIGNAL) {
            return Err(KernelError::MissingRight);
        }

        let notif_idx = self.resolve_notification_index(capability.object)?;
        let irq_idx = irq_line as usize;
        if irq_idx >= MAX_IRQ_LINES {
            return Err(KernelError::WrongObject);
        }
        self.with_ipc_state_mut(|ipc| {
            ipc.irq_routes[irq_idx] = Some(notif_idx);
        });
        Ok(())
    }

    fn signal_notification(
        &mut self,
        notification_idx: usize,
        irq_line: u16,
    ) -> Result<(), KernelError> {
        // Phase 1: send signal and snapshot waiter TID under ipc_state_lock.
        // Lock released before Phase 2 (task_state_lock, rank 2) to preserve ordering.
        let opt_waiter_tid = self.with_ipc_state_mut(|ipc| {
            let notif = ipc.notifications[notification_idx]
                .as_mut()
                .ok_or(KernelError::WrongObject)?;
            notif.send_irq(irq_line)?;
            Ok::<_, KernelError>(ipc.notification_waiters[notification_idx].take())
        })?;
        // Phase 2: wake waiter outside ipc_state_lock.
        if let Some(waiter_tid) = opt_waiter_tid {
            self.with_tcbs_mut(|tcbs| {
                let tcb = tcbs
                    .iter_mut()
                    .flatten()
                    .find(|tcb| tcb.tid.0 == waiter_tid.0)
                    .ok_or(KernelError::TaskMissing)?;
                tcb.status = TaskStatus::Runnable;
                Ok::<_, KernelError>(())
            })?;
            self.enqueue_task(waiter_tid.0)?;
        }
        Ok(())
    }

    pub fn route_external_irq(&mut self, irq_line: u16) -> Result<(), KernelError> {
        let irq_idx = irq_line as usize;
        let notification_idx =
            self.with_ipc_state(|ipc| ipc.irq_routes.get(irq_idx).copied().flatten());
        let Some(notification_idx) = notification_idx else {
            return Ok(());
        };
        self.signal_notification(notification_idx, irq_line)
    }

    pub fn ipc_send(&mut self, send_cap: CapId, msg: Message) -> Result<(), KernelError> {
        self.ipc_send_with_optional_deadline(send_cap, msg, None)
    }

    pub fn ipc_send_with_deadline(
        &mut self,
        send_cap: CapId,
        msg: Message,
        timeout_ticks: u64,
    ) -> Result<(), KernelError> {
        let deadline = if timeout_ticks == 0 {
            None
        } else {
            Some(self.scheduler_tick_now().wrapping_add(timeout_ticks))
        };
        self.ipc_send_with_optional_deadline(send_cap, msg, deadline)
    }

    fn ipc_send_with_optional_deadline(
        &mut self,
        send_cap: CapId,
        msg: Message,
        deadline: Option<u64>,
    ) -> Result<(), KernelError> {
        let capability = self.resolve_send_cap_task_local(send_cap)?;
        if !capability.has_right(CapRights::SEND) {
            return Err(KernelError::MissingRight);
        }
        if capability.object == CapObject::Kernel {
            return self.handle_restart_control_kernel_ipc(msg);
        }

        let endpoint_idx = self.resolve_endpoint_index(capability.object)?;

        // Phase 0: snapshot endpoint mode under ipc_state_lock.
        let endpoint_mode = self
            .with_ipc_state(|ipc| {
                ipc.endpoints
                    .get(endpoint_idx)
                    .and_then(Option::as_ref)
                    .map(|e| e.mode())
            })
            .ok_or(KernelError::WrongObject)?;

        if endpoint_mode == EndpointMode::Synchronous {
            // Phase 1: snapshot waiter TID under ipc_state_lock.  Lock released before
            // Phase 2 (task_state_lock, rank 3) to preserve lock-rank ordering.
            let opt_waiter_tid: Option<ThreadId> =
                self.with_ipc_state(|ipc| ipc.endpoint_waiters[endpoint_idx]);
            if let Some(waiter_tid) = opt_waiter_tid {
                crate::yarm_log!(
                    "IPC_SEND_SYNC_WAITER endpoint={} waiter_tid={}",
                    endpoint_idx,
                    waiter_tid.0
                );
                self.with_ipc_state_mut(|ipc| {
                    ipc.telemetry.fastpath_attempts =
                        ipc.telemetry.fastpath_attempts.saturating_add(1);
                });
                // Phase 2: check recv-v2 under task_state_lock (rank 3), outside
                // ipc_state_lock (rank 4), to preserve lock-rank ordering.
                let waiter_recv_v2_blocked = self.with_tcbs(|tcbs| {
                    tcbs.iter()
                        .flatten()
                        .find(|tcb| tcb.tid.0 == waiter_tid.0)
                        .and_then(|tcb| tcb.blocked_recv_state.as_ref())
                        .is_some_and(|state| state.recv_abi == RecvAbiVariant::RecvV2)
                });
                if waiter_recv_v2_blocked {
                    // Phase 3: complete delivery outside all locks (TrapFrame/user-memory write).
                    crate::yarm_log!(
                        "IPC_RECV_DELIVER_TO_WAITER tid={} endpoint={} len={} reply_cap={}",
                        waiter_tid.0,
                        endpoint_idx,
                        msg.len,
                        msg.transferred_cap().map(|c| c.0).unwrap_or(u64::MAX)
                    );
                    match complete_blocked_recv_for_waiter(self, waiter_tid.0, &msg) {
                        Ok(()) => {
                            crate::yarm_log!(
                                "IPC_RECV_DELIVER_TO_WAITER_CONSUMED tid={} endpoint={}",
                                waiter_tid.0,
                                endpoint_idx
                            );
                            crate::yarm_log!(
                                "IPC_RECV_ENQUEUE_SKIPPED_WAITER_COMPLETED endpoint={}",
                                endpoint_idx
                            );
                        }
                        Err(err) => {
                            crate::yarm_log!(
                                "IPC_RECV_BLOCKED_COMPLETE_FAILED tid={} err={:?}",
                                waiter_tid.0,
                                err
                            );
                            return Err(KernelError::UserMemoryFault);
                        }
                    }
                }
                // Phase 4: under ipc_state_lock — for legacy path enqueue message, then
                // clear waiter slot and bump telemetry.  Re-verifies waiter slot for
                // defence-in-depth.
                let wake_plan = self.ipc_try_send_sync_endpoint_only(
                    endpoint_idx,
                    waiter_tid,
                    msg,
                    waiter_recv_v2_blocked,
                )?;
                crate::yarm_log!("IPC_SEND_SYNC_WAKE_DONE waiter_tid={}", waiter_tid.0);
                // Phase 5: wake receiver outside all locks.
                self.apply_scheduler_wake_plan(wake_plan)?;
                // Phase 6: cooperative handoff outside all locks.
                let switched = self.apply_scheduler_handoff_plan(
                    super::SchedulerHandoffPlan::YieldTo(waiter_tid),
                )?;
                if switched {
                    self.with_ipc_state_mut(|ipc| {
                        ipc.telemetry.fastpath_switches =
                            ipc.telemetry.fastpath_switches.saturating_add(1);
                        ipc.telemetry.scheduler_fastpath_handoffs =
                            ipc.telemetry.scheduler_fastpath_handoffs.saturating_add(1);
                    });
                }
                crate::yarm_log!("IPC_SEND_SYNC_SWITCH_DONE waiter_tid={}", waiter_tid.0);
                return Ok(());
            }

            crate::yarm_log!("IPC_SEND_SYNC_NO_WAITER endpoint={}", endpoint_idx);
            let _ =
                self.block_current_on_send_with_deadline(endpoint_idx, send_cap, msg, deadline)?;
            self.with_ipc_state_mut(|ipc| {
                ipc.telemetry.blocked_sends = ipc.telemetry.blocked_sends.saturating_add(1);
            });
            return Err(KernelError::WouldBlock);
        }

        if let Some(waiter_tid) = self.with_ipc_state(|ipc| ipc.endpoint_waiters[endpoint_idx]) {
            crate::yarm_log!(
                "IPC_RECV_DELIVER_TO_WAITER tid={} endpoint={} len={} reply_cap={}",
                waiter_tid.0,
                endpoint_idx,
                msg.len,
                msg.transferred_cap().map(|c| c.0).unwrap_or(u64::MAX)
            );
            let waiter_recv_v2_blocked = self.with_tcbs(|tcbs| {
                tcbs.iter()
                    .flatten()
                    .find(|tcb| tcb.tid.0 == waiter_tid.0)
                    .and_then(|tcb| tcb.blocked_recv_state.as_ref())
                    .is_some_and(|state| state.recv_abi == RecvAbiVariant::RecvV2)
            });
            if waiter_recv_v2_blocked {
                match complete_blocked_recv_for_waiter(self, waiter_tid.0, &msg) {
                    Ok(()) => {
                        crate::yarm_log!(
                            "IPC_RECV_DELIVER_TO_WAITER_CONSUMED tid={} endpoint={}",
                            waiter_tid.0,
                            endpoint_idx
                        );
                        crate::yarm_log!(
                            "IPC_RECV_ENQUEUE_SKIPPED_WAITER_COMPLETED endpoint={}",
                            endpoint_idx
                        );
                        self.wake_waiter_for_endpoint(endpoint_idx)?;
                        return Ok(());
                    }
                    Err(err) => {
                        crate::yarm_log!(
                            "IPC_RECV_BLOCKED_COMPLETE_FAILED tid={} err={:?}",
                            waiter_tid.0,
                            err
                        );
                        return Err(KernelError::UserMemoryFault);
                    }
                }
            }
        }
        let queued = self.with_ipc_state_mut(|ipc| {
            let Some(endpoint_storage) = ipc.endpoints.get_mut(endpoint_idx).and_then(Option::as_mut) else {
                return Err(KernelError::WrongObject);
            };
            let endpoint = kernel_mut(endpoint_storage);
            Ok(endpoint.send(msg).is_ok())
        })?;
        if !queued {
            crate::yarm_log!("IPC_SEND_SYNC_NO_WAITER endpoint={}", endpoint_idx);
            let _ =
                self.block_current_on_send_with_deadline(endpoint_idx, send_cap, msg, deadline)?;
            self.with_ipc_state_mut(|ipc| {
                ipc.telemetry.blocked_sends = ipc.telemetry.blocked_sends.saturating_add(1);
            });
            return Err(KernelError::WouldBlock);
        }

        self.with_ipc_state_mut(|ipc| {
            ipc.telemetry.queued_sends = ipc.telemetry.queued_sends.saturating_add(1);
        });
        self.wake_waiter_for_endpoint(endpoint_idx)?;
        Ok(())
    }

    pub(crate) fn note_endpoint_only_queued_send_split(&mut self) {
        self.with_ipc_state_mut(|ipc| {
            ipc.telemetry.queued_sends = ipc.telemetry.queued_sends.saturating_add(1);
        });
    }

    pub(crate) fn note_endpoint_only_queued_recv_split(&mut self) {
        self.with_ipc_state_mut(|ipc| {
            ipc.telemetry.queued_recvs = ipc.telemetry.queued_recvs.saturating_add(1);
        });
    }

    /// Stage 4K: clear `endpoint_waiters[endpoint_idx]` under `ipc_state_lock` if and only if
    /// the slot still holds `expected_receiver_tid`.  Called after `complete_blocked_recv_for_waiter`
    /// succeeds to finalise the three-phase recv-v2 split delivery.  Under the global kernel lock
    /// this always matches; the conditional re-verify is a defence-in-depth check.
    pub(crate) fn ipc_clear_plain_receiver_waiter_only(
        &mut self,
        endpoint_idx: usize,
        expected_receiver_tid: ThreadId,
    ) {
        self.with_ipc_state_mut(|ipc| {
            if ipc.endpoint_waiters.get(endpoint_idx).copied().flatten()
                == Some(expected_receiver_tid)
            {
                ipc.endpoint_waiters[endpoint_idx] = None;
                crate::yarm_log!(
                    "IPC_SEND_SPLIT_RECV_V2_CLEAR_WAITER receiver_tid={}",
                    expected_receiver_tid.0
                );
            }
        });
    }

    /// Stage 4Q: synchronous-endpoint send — clear waiter slot and (for legacy receivers)
    /// enqueue message, all under `ipc_state_lock`.
    ///
    /// Preconditions (caller must verify before this call):
    ///   - Phase 2 recv-v2 check done under task_state_lock (rank 3), outside ipc_state_lock
    ///   - if `recv_v2_completed` is true, message was already written to receiver's TrapFrame
    ///     via `complete_blocked_recv_for_waiter` outside all locks (Phase 3)
    ///
    /// Under ipc_state_lock:
    ///   - re-verifies receiver slot still holds `expected_receiver_tid` (defence-in-depth)
    ///   - for legacy (non-recv-v2): enqueues `msg` into the endpoint queue
    ///   - clears `endpoint_waiters` slot
    ///   - bumps `rendezvous_handoffs` telemetry
    ///
    /// Returns `SchedulerWakePlan::Wake(tid)` on success; caller must apply outside the lock
    /// via `apply_scheduler_wake_plan`, then optionally `apply_scheduler_handoff_plan`.
    pub(crate) fn ipc_try_send_sync_endpoint_only(
        &mut self,
        endpoint_idx: usize,
        expected_receiver_tid: ThreadId,
        msg: Message,
        recv_v2_completed: bool,
    ) -> Result<super::SchedulerWakePlan, KernelError> {
        self.with_ipc_state_mut(|ipc| {
            // Re-verify waiter slot: defence-in-depth under the global kernel lock.
            if ipc.endpoint_waiters.get(endpoint_idx).copied().flatten()
                != Some(expected_receiver_tid)
            {
                return Err(KernelError::WrongObject);
            }
            if !recv_v2_completed {
                // Legacy path: deliver message via endpoint queue.
                let Some(endpoint_storage) = ipc.endpoints.get_mut(endpoint_idx).and_then(Option::as_mut) else {
                    return Err(KernelError::WrongObject);
                };
                let endpoint = kernel_mut(endpoint_storage);
                if endpoint.mode() != EndpointMode::Synchronous {
                    return Err(KernelError::WrongObject);
                }
                crate::yarm_log!(
                    "IPC_RECV_DELIVER_TO_WAITER tid={} endpoint={}",
                    expected_receiver_tid.0,
                    endpoint_idx
                );
                endpoint.send(msg).map_err(|_| KernelError::EndpointQueueFull)?;
            }
            ipc.endpoint_waiters[endpoint_idx] = None;
            ipc.telemetry.rendezvous_handoffs =
                ipc.telemetry.rendezvous_handoffs.saturating_add(1);
            crate::yarm_log!(
                "IPC_SEND_SYNC_CLEAR_WAITER receiver_tid={}",
                expected_receiver_tid.0
            );
            Ok(super::SchedulerWakePlan::Wake(expected_receiver_tid))
        })
    }

    pub(crate) fn note_split_recv_v2_delivery(&mut self) {
        self.with_ipc_state_mut(|ipc| {
            ipc.telemetry.split_recv_v2_deliveries =
                ipc.telemetry.split_recv_v2_deliveries.saturating_add(1);
        });
    }

    pub(crate) fn note_ipc_call_split_delivery(&mut self) {
        self.with_ipc_state_mut(|ipc| {
            ipc.telemetry.ipc_call_split_deliveries =
                ipc.telemetry.ipc_call_split_deliveries.saturating_add(1);
        });
    }

    pub(crate) fn note_ipc_reply_split_delivery(&mut self) {
        self.with_ipc_state_mut(|ipc| {
            ipc.telemetry.ipc_reply_split_deliveries =
                ipc.telemetry.ipc_reply_split_deliveries.saturating_add(1);
        });
    }

    pub(crate) fn note_cap_transfer_recv_v2_delivery(&mut self) {
        self.with_ipc_state_mut(|ipc| {
            ipc.telemetry.cap_transfer_recv_v2_deliveries =
                ipc.telemetry.cap_transfer_recv_v2_deliveries.saturating_add(1);
        });
    }

    pub(crate) fn note_cap_transfer_stage4e_enqueued(&mut self) {
        self.with_ipc_state_mut(|ipc| {
            ipc.telemetry.cap_transfer_stage4e_enqueued =
                ipc.telemetry.cap_transfer_stage4e_enqueued.saturating_add(1);
        });
    }

    fn handle_restart_control_kernel_ipc(&mut self, msg: Message) -> Result<(), KernelError> {
        if msg.opcode != PROC_OP_EXECUTE_RESTART {
            return Err(KernelError::WrongObject);
        }
        let args = ExecuteRestartRequest::decode(msg.as_slice())
            .map_err(|_| KernelError::UserMemoryFault)?;
        let status = match self.restart_task(args.tid, args.restart_token) {
            Ok(()) => ExecuteRestartReply::STATUS_OK,
            Err(KernelError::TaskMissing) => ExecuteRestartReply::STATUS_NOT_FOUND,
            Err(KernelError::WrongObject) => ExecuteRestartReply::STATUS_TOKEN_MISMATCH,
            Err(KernelError::MissingRight) => ExecuteRestartReply::STATUS_PERMISSION_DENIED,
            Err(_) => ExecuteRestartReply::STATUS_INTERNAL_UNSUPPORTED,
        };
        let reply_cap = msg.transferred_cap().ok_or(KernelError::MissingRight)?;
        let reply = Message::with_header(
            0,
            PROC_OP_EXECUTE_RESTART,
            0,
            None,
            &ExecuteRestartReply::new(status).encode(),
        )
        .map_err(|_| KernelError::UserMemoryFault)?;
        self.ipc_reply(CapId(reply_cap.0), reply)
    }

    pub fn ipc_send_fastpath(
        &mut self,
        send_cap: CapId,
        msg: Message,
    ) -> Result<IpcFastpathResult, KernelError> {
        let capability = self.resolve_send_cap_task_local(send_cap)?;
        if !capability.has_right(CapRights::SEND) {
            return Err(KernelError::MissingRight);
        }

        let endpoint_idx = self.resolve_endpoint_index(capability.object)?;
        let (endpoint_mode, waiter_tid) = self
            .with_ipc_state(|ipc| {
                let mode = ipc
                    .endpoints
                    .get(endpoint_idx)
                    .and_then(Option::as_ref)
                    .map(|e| e.mode());
                let waiter = ipc.endpoint_waiters[endpoint_idx];
                (mode, waiter)
            });
        let endpoint_mode = endpoint_mode.ok_or(KernelError::WrongObject)?;
        let inline_sync_handoff =
            endpoint_mode == EndpointMode::Synchronous && waiter_tid.is_some();
        if !inline_sync_handoff {
            self.with_ipc_state_mut(|ipc| {
                ipc.telemetry.fastpath_attempts =
                    ipc.telemetry.fastpath_attempts.saturating_add(1);
            });
        }

        self.ipc_send(send_cap, msg)?;

        let switched = if inline_sync_handoff {
            true
        } else if waiter_tid.is_some() {
            self.apply_scheduler_handoff_plan(
                super::SchedulerHandoffPlan::YieldTo(waiter_tid.expect("checked is_some")),
            )?
        } else {
            false
        };

        if switched && !inline_sync_handoff {
            self.with_ipc_state_mut(|ipc| {
                ipc.telemetry.fastpath_switches =
                    ipc.telemetry.fastpath_switches.saturating_add(1);
                ipc.telemetry.scheduler_fastpath_handoffs =
                    ipc.telemetry.scheduler_fastpath_handoffs.saturating_add(1);
            });
        }

        Ok(IpcFastpathResult {
            switched_to_waiter: switched,
        })
    }

    pub fn ipc_send_with_cap_transfer(
        &mut self,
        send_cap: CapId,
        sender_tid: ThreadId,
        opcode: u16,
        transfer_cap: CapId,
        payload: &[u8],
    ) -> Result<(), KernelError> {
        // Resolve all capabilities in the sender's cspace to keep authorization
        // task-local even for kernel-internal transfer staging paths.
        let _ = self.resolve_capability_for_task(sender_tid.0, transfer_cap)?;
        let send_capability = self.resolve_capability_for_task(sender_tid.0, send_cap)?;
        if !send_capability.has_right(CapRights::SEND) {
            return Err(KernelError::MissingRight);
        }
        let endpoint_idx = self.resolve_endpoint_index(send_capability.object)?;
        let waiter_tid = self
            .with_ipc_state(|ipc| ipc.endpoint_waiters[endpoint_idx])
            .ok_or(KernelError::WouldBlock)?;
        let transfer_handle = self
            .stash_transfer_envelope(
                sender_tid,
                transfer_cap,
                send_capability.object,
                Some(waiter_tid),
                None,
            )
            .ok_or(KernelError::EndpointQueueFull)?;
        let msg = Message::with_header(
            sender_tid.0,
            opcode,
            Message::FLAG_CAP_TRANSFER,
            Some(transfer_handle),
            payload,
        )
        .map_err(map_ipc_error)?;
        if let Err(err) = self.ipc_send(send_cap, msg) {
            let _ =
                self.take_transfer_envelope(transfer_handle, send_capability.object, sender_tid);
            return Err(err);
        }
        Ok(())
    }

    /// Atomically dequeue one message from the endpoint and compact sender-waiter queue.
    ///
    /// All mutations happen under `ipc_state_lock` (rank 3).  Returns the message (if any)
    /// and a deferred wake plan to apply after the lock is released.
    ///
    /// Protocol:
    ///   1a. Dequeue from endpoint queue (scoped borrow released before step 1b).
    ///   1b. Compact sender-waiter queue: take head, shift remainder left.
    ///   2.  If message + waiter: refill endpoint with waiter's message → WakeSender.
    ///       If message, no waiter: return message → None wake.
    ///       If no message + waiter: direct delivery of waiter's message → WakeSender.
    ///       If neither: return None → None wake.
    pub(crate) fn ipc_recv_endpoint_take(
        &mut self,
        endpoint_idx: usize,
    ) -> Result<(Option<Message>, super::SchedulerWakePlan), KernelError> {
        self.with_ipc_state_mut(|ipc| {
            // 1a: Dequeue from endpoint queue; scoped block releases the borrow.
            let opt_msg = {
                let Some(ep_storage) = ipc.endpoints[endpoint_idx].as_mut() else {
                    return Err(KernelError::WrongObject);
                };
                kernel_mut(ep_storage).recv()
            };
            // 1b: Compact sender-waiter queue.
            //
            // Scan for the first live sender rather than always taking slot[0]:
            // process_ipc_timeout_deadlines nulls expired slots in-place without
            // compacting, creating sparse queues ([None, Some(B), ...]). Taking
            // only slot[0] would miss live senders at positions > 0, permanently
            // stranding them.  Full left-compaction after the take keeps the queue
            // dense for subsequent operations.
            let opt_waiter = {
                let queue = &mut ipc.endpoint_sender_waiters[endpoint_idx];
                if let Some(idx) = queue.iter().position(Option::is_some) {
                    let head = queue[idx].take().expect("position guarantees Some");
                    // Full left-compact: move remaining Some entries to the front.
                    let mut write = 0;
                    for read in 0..queue.len() {
                        if queue[read].is_some() {
                            queue[write] = queue[read].take();
                            write += 1;
                        }
                    }
                    Some(head)
                } else {
                    None
                }
            };
            match (opt_msg, opt_waiter) {
                (Some(msg), Some(waiter)) => {
                    // Endpoint slot just freed: refill with waiter's message.
                    let ep_storage = ipc.endpoints[endpoint_idx]
                        .as_mut()
                        .expect("endpoint must exist after recv");
                    kernel_mut(ep_storage)
                        .send(waiter.msg)
                        .map_err(|_| KernelError::EndpointQueueFull)?;
                    Ok((Some(msg), super::SchedulerWakePlan::Wake(waiter.tid)))
                }
                (Some(msg), None) => Ok((Some(msg), super::SchedulerWakePlan::None)),
                (None, Some(waiter)) => {
                    // Direct delivery: bypass endpoint queue.
                    Ok((Some(waiter.msg), super::SchedulerWakePlan::Wake(waiter.tid)))
                }
                (None, None) => Ok((None, super::SchedulerWakePlan::None)),
            }
        })
    }

    pub fn try_ipc_recv(&mut self, recv_cap: CapId) -> Result<Option<Message>, KernelError> {
        // Probe path resolves receive capability in the current task cspace.
        let capability = self.resolve_recv_cap_task_local(recv_cap)?;
        if !capability.has_right(CapRights::RECEIVE) {
            return Err(KernelError::MissingRight);
        }
        if let CapObject::Notification { .. } = capability.object {
            let notif_idx = self.resolve_notification_index(capability.object)?;
            let result = self.with_ipc_state_mut(|ipc| {
                ipc.notifications[notif_idx].as_mut().map(|n| n.recv())
            });
            return result.ok_or(KernelError::WrongObject);
        }

        let endpoint_idx = self.resolve_endpoint_index(capability.object)?;
        let (msg, wake_plan) = self.ipc_recv_endpoint_take(endpoint_idx)?;
        self.apply_scheduler_wake_plan(wake_plan)?;
        Ok(msg)
    }

    pub fn ipc_recv(&mut self, recv_cap: CapId) -> Result<Option<Message>, KernelError> {
        self.ipc_recv_with_optional_deadline(recv_cap, None)
    }

    pub fn ipc_recv_with_deadline(
        &mut self,
        recv_cap: CapId,
        timeout_ticks: u64,
    ) -> Result<Option<Message>, KernelError> {
        let deadline = if timeout_ticks == 0 {
            None
        } else {
            Some(self.scheduler_tick_now().wrapping_add(timeout_ticks))
        };
        self.ipc_recv_with_optional_deadline(recv_cap, deadline)
    }

    pub fn ipc_recv_until_deadline(
        &mut self,
        recv_cap: CapId,
        deadline_tick: u64,
    ) -> Result<Option<Message>, KernelError> {
        self.ipc_recv_with_optional_deadline(recv_cap, Some(deadline_tick))
    }

    fn ipc_recv_with_optional_deadline(
        &mut self,
        recv_cap: CapId,
        deadline: Option<u64>,
    ) -> Result<Option<Message>, KernelError> {
        let capability = self.resolve_recv_cap_task_local(recv_cap)?;
        if !capability.has_right(CapRights::RECEIVE) {
            return Err(KernelError::MissingRight);
        }
        if let CapObject::Notification { .. } = capability.object {
            let notif_idx = self.resolve_notification_index(capability.object)?;
            // Try immediate recv under ipc_state_lock.
            let immediate = self.with_ipc_state_mut(|ipc| {
                ipc.notifications[notif_idx].as_mut().map(|n| n.recv())
            });
            match immediate {
                None => return Err(KernelError::WrongObject),
                Some(Some(msg)) => return Ok(Some(msg)),
                Some(None) => {}
            }
            // Nothing available — block.
            let blocked_tid = self.block_current_cpu().ok_or(KernelError::TaskMissing)?;
            self.with_tcbs_mut(|tcbs| {
                let tcb = tcbs
                    .iter_mut()
                    .flatten()
                    .find(|tcb| tcb.tid.0 == blocked_tid)
                    .ok_or(KernelError::TaskMissing)?;
                tcb.status = TaskStatus::Blocked(WaitReason::EndpointReceive(recv_cap));
                tcb.ipc_timeout_deadline = deadline;
                tcb.ipc_timeout_fired = false;
                Ok::<_, KernelError>(())
            })?;
            // Publish waiter under ipc_state_lock after TCB is Blocked.
            self.with_ipc_state_mut(|ipc| {
                ipc.notification_waiters[notif_idx] = Some(ThreadId(blocked_tid));
            });
            let _ = self.dispatch_next_task()?;
            return Ok(None);
        }

        let endpoint_idx = self.resolve_endpoint_index(capability.object)?;
        // Phase 1: try immediate recv under ipc_state_lock.
        let (msg, wake_plan) = self.ipc_recv_endpoint_take(endpoint_idx)?;
        self.apply_scheduler_wake_plan(wake_plan)?;
        if msg.is_some() {
            return Ok(msg);
        }

        let blocked_tid =
            self.block_current_on_receive_with_deadline(endpoint_idx, recv_cap, deadline)?;
        let timed_out = self.consume_ipc_timeout_fired_for_tid(blocked_tid.0)?;
        if timed_out {
            return Ok(None);
        }
        // Phase 2: post-wake recv under ipc_state_lock (sender may have delivered directly).
        let (msg, wake_plan) = self.ipc_recv_endpoint_take(endpoint_idx)?;
        self.apply_scheduler_wake_plan(wake_plan)?;
        Ok(msg)
    }
}
