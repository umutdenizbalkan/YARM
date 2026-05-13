// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::{
    IpcFastpathResult, KernelError, KernelState, MAX_ENDPOINT_SENDER_WAITERS, MAX_IRQ_LINES,
    NotificationObject, ReplyCapRecord, SenderWaiter, kernel_ref, map_ipc_error,
};
use crate::kernel::capabilities::{CapId, CapObject, CapRights, Capability};
use crate::kernel::ipc::{Endpoint, EndpointMode, Message, ThreadId};
use crate::kernel::task::{TaskStatus, WaitReason};
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

    fn endpoint_sender_waiter_limit(&self, endpoint_idx: usize) -> Result<usize, KernelError> {
        self.ipc
            .endpoints
            .get(endpoint_idx)
            .and_then(Option::as_ref)
            .ok_or(KernelError::WrongObject)?;
        Ok(MAX_ENDPOINT_SENDER_WAITERS)
    }

    fn enqueue_sender_waiter(
        &mut self,
        endpoint_idx: usize,
        waiter: SenderWaiter,
    ) -> Result<(), KernelError> {
        let limit = self.endpoint_sender_waiter_limit(endpoint_idx)?;
        let queue = &mut self.ipc.endpoint_sender_waiters[endpoint_idx];
        if let Some(slot) = queue[..limit].iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(waiter);
            return Ok(());
        }
        Err(KernelError::EndpointQueueFull)
    }

    fn dequeue_sender_waiter(&mut self, endpoint_idx: usize) -> Option<SenderWaiter> {
        let queue = &mut self.ipc.endpoint_sender_waiters[endpoint_idx];
        let head = queue[0].take()?;
        for idx in 1..queue.len() {
            queue[idx - 1] = queue[idx].take();
        }
        queue[queue.len() - 1] = None;
        Some(head)
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
        self.ipc.endpoint_waiters[endpoint_idx] = Some(ThreadId(blocked_tid));
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
        if let Some(waiter_tid) = self.ipc.endpoint_waiters[endpoint_idx].take() {
            crate::yarm_log!("SCHED_WAKE tid={}", waiter_tid.0);
            self.wake_tid_to_runnable(waiter_tid)?;
        }
        Ok(())
    }

    fn wake_sender_waiter(&mut self, sender_tid: ThreadId) -> Result<(), KernelError> {
        crate::yarm_log!("SCHED_WAKE tid={}", sender_tid.0);
        self.wake_tid_to_runnable(sender_tid)
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

        let (slot, generation) = self.with_ipc_state_mut(|ipc| {
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
                    });
                    return Ok::<_, KernelError>((idx, next_generation));
                }
            }
            Err(KernelError::CapabilityFull)
        })?;

        self.mint_capability_for_active_cnode(Capability::new(
            CapObject::Reply {
                index: slot,
                generation,
            },
            CapRights::SEND,
        ))
    }

    pub fn ipc_reply(&mut self, reply_cap: CapId, msg: Message) -> Result<(), KernelError> {
        let current_tid = self.current_tid().ok_or(KernelError::TaskMissing)?;
        crate::yarm_log!("IPC_REPLY_ENTER tid={}", current_tid);
        let capability = self.resolve_send_cap_task_local(reply_cap)?;
        if !capability.has_right(CapRights::SEND) {
            return Err(KernelError::MissingRight);
        }
        let slot = self.resolve_reply_index(capability.object)?;
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

        let endpoint_idx = self.resolve_endpoint_index(record.reply_endpoint)?;
        let endpoint = self
            .ipc
            .endpoints
            .get_mut(endpoint_idx)
            .and_then(Option::as_mut)
            .ok_or(KernelError::WrongObject)?;
        endpoint
            .send(msg)
            .map_err(|_| KernelError::EndpointQueueFull)?;
        if let Some(waiter_tid) = self.ipc.endpoint_waiters[endpoint_idx] {
            crate::yarm_log!("IPC_REPLY_WAKE_CALLER tid={}", waiter_tid.0);
        }
        self.wake_waiter_for_endpoint(endpoint_idx)?;
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
        let limits = self.runtime_capacity_config();
        let mut slot_index = None;
        for (idx, slot) in self
            .ipc
            .endpoints
            .iter()
            .take(limits.max_endpoints)
            .enumerate()
        {
            if slot.is_none() {
                slot_index = Some(idx);
                break;
            }
        }

        let endpoint_idx = slot_index.ok_or(KernelError::EndpointFull)?;
        let mut next_generation = self.ipc.endpoint_generations[endpoint_idx].wrapping_add(1);
        if next_generation == 0 {
            next_generation = 1;
        }
        self.ipc.endpoint_generations[endpoint_idx] = next_generation;
        self.ipc.endpoints[endpoint_idx] = Some(super::store_kernel_value(
            Endpoint::new_with_mode(max_depth, mode).map_err(map_ipc_error)?,
        ));

        let send_cap = self.mint_capability_for_active_cnode(Capability::new(
            CapObject::Endpoint {
                index: endpoint_idx,
                generation: self.ipc.endpoint_generations[endpoint_idx],
            },
            CapRights::SEND,
        ))?;

        let recv_cap = self.mint_capability_for_active_cnode(Capability::new(
            CapObject::Endpoint {
                index: endpoint_idx,
                generation: self.ipc.endpoint_generations[endpoint_idx],
            },
            CapRights::RECEIVE,
        ))?;

        Ok((endpoint_idx, send_cap, recv_cap))
    }

    pub fn create_notification(
        &mut self,
        max_depth: usize,
    ) -> Result<(usize, CapId, CapId), KernelError> {
        let limits = self.runtime_capacity_config();

        let mut slot_index = None;
        for (idx, slot) in self
            .ipc
            .notifications
            .iter()
            .take(limits.max_notifications)
            .enumerate()
        {
            if slot.is_none() {
                slot_index = Some(idx);
                break;
            }
        }

        let notification_idx = slot_index.ok_or(KernelError::EndpointFull)?;
        let mut next_generation =
            self.ipc.notification_generations[notification_idx].wrapping_add(1);
        if next_generation == 0 {
            next_generation = 1;
        }
        self.ipc.notification_generations[notification_idx] = next_generation;
        self.ipc.notifications[notification_idx] = Some(NotificationObject::new(max_depth)?);

        let notification_cap = self.mint_capability_for_active_cnode(Capability::new(
            CapObject::Notification {
                index: notification_idx,
                generation: self.ipc.notification_generations[notification_idx],
            },
            CapRights::SIGNAL,
        ))?;

        let recv_cap = self.mint_capability_for_active_cnode(Capability::new(
            CapObject::Notification {
                index: notification_idx,
                generation: self.ipc.notification_generations[notification_idx],
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
        let notif = self.ipc.notifications[notification_idx]
            .as_mut()
            .ok_or(KernelError::WrongObject)?;
        notif.send_irq(irq_line)?;
        if let Some(waiter_tid) = self.ipc.notification_waiters[notification_idx].take() {
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

        let endpoint_mode = self
            .ipc
            .endpoints
            .get(endpoint_idx)
            .and_then(Option::as_ref)
            .ok_or(KernelError::WrongObject)?
            .mode();

        if endpoint_mode == EndpointMode::Synchronous {
            if let Some(waiter_tid) = self.ipc.endpoint_waiters[endpoint_idx] {
                crate::yarm_log!(
                    "IPC_SEND_SYNC_WAITER endpoint={} waiter_tid={}",
                    endpoint_idx,
                    waiter_tid.0
                );
                self.ipc.telemetry.fastpath_attempts =
                    self.ipc.telemetry.fastpath_attempts.saturating_add(1);
                let endpoint = self
                    .ipc
                    .endpoints
                    .get_mut(endpoint_idx)
                    .and_then(Option::as_mut)
                    .ok_or(KernelError::WrongObject)?;
                endpoint
                    .send(msg)
                    .map_err(|_| KernelError::EndpointQueueFull)?;
                self.ipc.telemetry.rendezvous_handoffs =
                    self.ipc.telemetry.rendezvous_handoffs.saturating_add(1);
                self.wake_waiter_for_endpoint(endpoint_idx)?;
                crate::yarm_log!("IPC_SEND_SYNC_WAKE_DONE waiter_tid={}", waiter_tid.0);
                let switched = self.switch_to_runnable_tid(waiter_tid)?;
                if switched {
                    self.ipc.telemetry.fastpath_switches =
                        self.ipc.telemetry.fastpath_switches.saturating_add(1);
                    self.ipc.telemetry.scheduler_fastpath_handoffs = self
                        .ipc
                        .telemetry
                        .scheduler_fastpath_handoffs
                        .saturating_add(1);
                }
                crate::yarm_log!("IPC_SEND_SYNC_SWITCH_DONE waiter_tid={}", waiter_tid.0);
                return Ok(());
            }

            crate::yarm_log!("IPC_SEND_SYNC_NO_WAITER endpoint={}", endpoint_idx);
            let _ =
                self.block_current_on_send_with_deadline(endpoint_idx, send_cap, msg, deadline)?;
            self.ipc.telemetry.blocked_sends = self.ipc.telemetry.blocked_sends.saturating_add(1);
            return Err(KernelError::WouldBlock);
        }

        let endpoint = self
            .ipc
            .endpoints
            .get_mut(endpoint_idx)
            .and_then(Option::as_mut)
            .ok_or(KernelError::WrongObject)?;
        if endpoint.send(msg).is_err() {
            crate::yarm_log!("IPC_SEND_SYNC_NO_WAITER endpoint={}", endpoint_idx);
            let _ =
                self.block_current_on_send_with_deadline(endpoint_idx, send_cap, msg, deadline)?;
            self.ipc.telemetry.blocked_sends = self.ipc.telemetry.blocked_sends.saturating_add(1);
            return Err(KernelError::WouldBlock);
        }

        self.ipc.telemetry.queued_sends = self.ipc.telemetry.queued_sends.saturating_add(1);
        self.wake_waiter_for_endpoint(endpoint_idx)?;
        Ok(())
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
        let endpoint_mode = self
            .ipc
            .endpoints
            .get(endpoint_idx)
            .and_then(Option::as_ref)
            .ok_or(KernelError::WrongObject)?
            .mode();
        let waiter_tid = self.ipc.endpoint_waiters[endpoint_idx];
        let inline_sync_handoff =
            endpoint_mode == EndpointMode::Synchronous && waiter_tid.is_some();
        if !inline_sync_handoff {
            self.ipc.telemetry.fastpath_attempts =
                self.ipc.telemetry.fastpath_attempts.saturating_add(1);
        }

        self.ipc_send(send_cap, msg)?;

        let switched = if inline_sync_handoff {
            true
        } else if waiter_tid.is_some() {
            self.switch_to_runnable_tid(waiter_tid.expect("checked is_some"))?
        } else {
            false
        };

        if switched && !inline_sync_handoff {
            self.ipc.telemetry.fastpath_switches =
                self.ipc.telemetry.fastpath_switches.saturating_add(1);
            self.ipc.telemetry.scheduler_fastpath_handoffs = self
                .ipc
                .telemetry
                .scheduler_fastpath_handoffs
                .saturating_add(1);
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
        let waiter_tid = self.ipc.endpoint_waiters[endpoint_idx].ok_or(KernelError::WouldBlock)?;
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

    pub fn try_ipc_recv(&mut self, recv_cap: CapId) -> Result<Option<Message>, KernelError> {
        // Probe path resolves receive capability in the current task cspace.
        let capability = self.resolve_recv_cap_task_local(recv_cap)?;
        if !capability.has_right(CapRights::RECEIVE) {
            return Err(KernelError::MissingRight);
        }
        if let CapObject::Notification { .. } = capability.object {
            let notif_idx = self.resolve_notification_index(capability.object)?;
            let notif = self.ipc.notifications[notif_idx]
                .as_mut()
                .ok_or(KernelError::WrongObject)?;
            return Ok(notif.recv());
        }

        let endpoint_idx = self.resolve_endpoint_index(capability.object)?;

        let dequeued = self
            .ipc
            .endpoints
            .get_mut(endpoint_idx)
            .and_then(Option::as_mut)
            .ok_or(KernelError::WrongObject)?
            .recv();

        if let Some(msg) = dequeued {
            if let Some(waiter) = self.dequeue_sender_waiter(endpoint_idx) {
                self.ipc
                    .endpoints
                    .get_mut(endpoint_idx)
                    .and_then(Option::as_mut)
                    .ok_or(KernelError::WrongObject)?
                    .send(waiter.msg)
                    .map_err(|_| KernelError::EndpointQueueFull)?;
                self.wake_sender_waiter(waiter.tid)?;
            }
            return Ok(Some(msg));
        }

        if let Some(waiter) = self.dequeue_sender_waiter(endpoint_idx) {
            self.wake_sender_waiter(waiter.tid)?;
            return Ok(Some(waiter.msg));
        }

        Ok(None)
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
            let notif = self.ipc.notifications[notif_idx]
                .as_mut()
                .ok_or(KernelError::WrongObject)?;
            if let Some(msg) = notif.recv() {
                return Ok(Some(msg));
            }
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
            self.ipc.notification_waiters[notif_idx] = Some(ThreadId(blocked_tid));
            let _ = self.dispatch_next_task()?;
            return Ok(None);
        }

        let endpoint_idx = self.resolve_endpoint_index(capability.object)?;

        let dequeued = self
            .ipc
            .endpoints
            .get_mut(endpoint_idx)
            .and_then(Option::as_mut)
            .ok_or(KernelError::WrongObject)?
            .recv();

        if let Some(msg) = dequeued {
            if let Some(waiter) = self.dequeue_sender_waiter(endpoint_idx) {
                self.ipc
                    .endpoints
                    .get_mut(endpoint_idx)
                    .and_then(Option::as_mut)
                    .ok_or(KernelError::WrongObject)?
                    .send(waiter.msg)
                    .map_err(|_| KernelError::EndpointQueueFull)?;
                self.wake_sender_waiter(waiter.tid)?;
            }
            return Ok(Some(msg));
        }

        if let Some(waiter) = self.dequeue_sender_waiter(endpoint_idx) {
            self.wake_sender_waiter(waiter.tid)?;
            return Ok(Some(waiter.msg));
        }

        let _ = self.block_current_on_receive_with_deadline(endpoint_idx, recv_cap, deadline)?;
        Ok(None)
    }
}
