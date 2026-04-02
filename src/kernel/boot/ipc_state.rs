// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::{
    IpcFastpathResult, KernelError, KernelState, MAX_ENDPOINT_SENDER_WAITERS, MAX_IRQ_LINES,
    NotificationObject, SenderWaiter, map_ipc_error,
};
use crate::kernel::capabilities::{CapId, CapObject, CapRights, Capability};
use crate::kernel::ipc::{Endpoint, EndpointMode, Message, ThreadId};
use crate::kernel::task::{TaskStatus, WaitReason};

impl KernelState {
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

    fn block_current_on_receive(
        &mut self,
        endpoint_idx: usize,
        recv_cap: CapId,
    ) -> Result<(), KernelError> {
        let blocked_tid = self.block_current_cpu().ok_or(KernelError::TaskMissing)?;
        self.with_tcbs_mut(|tcbs| {
            let tcb = tcbs
                .iter_mut()
                .flatten()
                .find(|tcb| tcb.tid.0 == blocked_tid)
                .ok_or(KernelError::TaskMissing)?;
            tcb.status = TaskStatus::Blocked(WaitReason::EndpointReceive(recv_cap));
            Ok::<_, KernelError>(())
        })?;
        self.ipc.endpoint_waiters[endpoint_idx] = Some(ThreadId(blocked_tid));
        let _ = self.dispatch_next_task()?;
        Ok(())
    }

    fn block_current_on_send(
        &mut self,
        endpoint_idx: usize,
        send_cap: CapId,
        msg: Message,
    ) -> Result<(), KernelError> {
        let blocked_tid = self.block_current_cpu().ok_or(KernelError::TaskMissing)?;
        self.with_tcbs_mut(|tcbs| {
            let tcb = tcbs
                .iter_mut()
                .flatten()
                .find(|tcb| tcb.tid.0 == blocked_tid)
                .ok_or(KernelError::TaskMissing)?;
            tcb.status = TaskStatus::Blocked(WaitReason::EndpointSend(send_cap));
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
        Ok(())
    }

    pub(crate) fn wake_waiter_for_endpoint(
        &mut self,
        endpoint_idx: usize,
    ) -> Result<(), KernelError> {
        if let Some(waiter_tid) = self.ipc.endpoint_waiters[endpoint_idx].take() {
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

    fn wake_sender_waiter(&mut self, sender_tid: ThreadId) -> Result<(), KernelError> {
        self.with_tcbs_mut(|tcbs| {
            let tcb = tcbs
                .iter_mut()
                .flatten()
                .find(|tcb| tcb.tid.0 == sender_tid.0)
                .ok_or(KernelError::TaskMissing)?;
            tcb.status = TaskStatus::Runnable;
            Ok::<_, KernelError>(())
        })?;
        self.enqueue_task(sender_tid.0).map(|_| ())
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
            | CapObject::Irq { .. } => Err(KernelError::WrongObject),
        }
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
        let payload = irq_line.to_le_bytes();
        let msg = Message::with_header(0, irq_line, 0, None, &payload).map_err(map_ipc_error)?;
        notif.send(msg)?;
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

        if endpoint_mode == EndpointMode::Synchronous {
            if let Some(waiter_tid) = self.ipc.endpoint_waiters[endpoint_idx] {
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
                if self.switch_to_runnable_tid(waiter_tid)? {
                    self.ipc.telemetry.fastpath_switches =
                        self.ipc.telemetry.fastpath_switches.saturating_add(1);
                    self.ipc.telemetry.scheduler_fastpath_handoffs = self
                        .ipc
                        .telemetry
                        .scheduler_fastpath_handoffs
                        .saturating_add(1);
                }
                return Ok(());
            }

            self.block_current_on_send(endpoint_idx, send_cap, msg)?;
            self.ipc.telemetry.blocked_sends = self.ipc.telemetry.blocked_sends.saturating_add(1);
            return Err(KernelError::WouldBlock);
        }

        let endpoint = self
            .ipc
            .endpoints
            .get_mut(endpoint_idx)
            .and_then(Option::as_mut)
            .ok_or(KernelError::WrongObject)?;

        endpoint
            .send(msg)
            .map_err(|_| KernelError::EndpointQueueFull)?;

        self.ipc.telemetry.queued_sends = self.ipc.telemetry.queued_sends.saturating_add(1);
        self.wake_waiter_for_endpoint(endpoint_idx)?;
        Ok(())
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

        self.block_current_on_receive(endpoint_idx, recv_cap)?;
        Ok(None)
    }
}
