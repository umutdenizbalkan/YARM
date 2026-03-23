use super::{
    IpcFastpathResult, KernelError, KernelState, MAX_ENDPOINTS, MAX_IRQ_LINES, MAX_NOTIFICATIONS,
    NotificationObject, map_ipc_error,
};
use crate::kernel::capabilities::{CapId, CapObject, CapRights, Capability};
use crate::kernel::ipc::{Endpoint, EndpointMode, Message, ThreadId};
use crate::kernel::task::{TaskStatus, WaitReason};

impl KernelState {
    fn block_current_on_receive(
        &mut self,
        endpoint_idx: usize,
        recv_cap: CapId,
    ) -> Result<(), KernelError> {
        let blocked_tid = self.block_current_cpu().ok_or(KernelError::TaskMissing)?;
        let tcb = self.tcb_mut(blocked_tid).ok_or(KernelError::TaskMissing)?;
        tcb.status = TaskStatus::Blocked(WaitReason::EndpointReceive(recv_cap));
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
        if self.ipc.endpoint_sender_waiters[endpoint_idx].is_some() {
            return Err(KernelError::EndpointQueueFull);
        }

        let blocked_tid = self.block_current_cpu().ok_or(KernelError::TaskMissing)?;
        let tcb = self.tcb_mut(blocked_tid).ok_or(KernelError::TaskMissing)?;
        tcb.status = TaskStatus::Blocked(WaitReason::EndpointSend(send_cap));
        self.ipc.endpoint_sender_waiters[endpoint_idx] = Some((ThreadId(blocked_tid), msg, true));
        let _ = self.dispatch_next_task()?;
        Ok(())
    }

    pub(crate) fn wake_waiter_for_endpoint(
        &mut self,
        endpoint_idx: usize,
    ) -> Result<(), KernelError> {
        if let Some(waiter_tid) = self.ipc.endpoint_waiters[endpoint_idx].take() {
            {
                let tcb = self.tcb_mut(waiter_tid.0).ok_or(KernelError::TaskMissing)?;
                tcb.status = TaskStatus::Runnable;
            }
            self.enqueue_task(waiter_tid.0)?;
        }
        Ok(())
    }

    fn wake_sender_waiter(&mut self, sender_tid: ThreadId) -> Result<(), KernelError> {
        {
            let tcb = self.tcb_mut(sender_tid.0).ok_or(KernelError::TaskMissing)?;
            tcb.status = TaskStatus::Runnable;
        }
        self.enqueue_task(sender_tid.0).map(|_| ())
    }

    pub(crate) fn resolve_endpoint_index(&self, object: CapObject) -> Result<usize, KernelError> {
        match object {
            CapObject::Endpoint { index, generation } => {
                if index >= MAX_ENDPOINTS {
                    return Err(KernelError::WrongObject);
                }
                if self.ipc.endpoints[index].is_none() {
                    return Err(KernelError::WrongObject);
                }
                if self.ipc.endpoint_generations[index] != generation {
                    return Err(KernelError::StaleCapability);
                }
                Ok(index)
            }
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
        if endpoint_idx >= MAX_ENDPOINTS || self.ipc.endpoints[endpoint_idx].is_none() {
            return Err(KernelError::WrongObject);
        }
        self.ipc.endpoints[endpoint_idx] = None;
        if self.faults.fault_handler_endpoint == Some(endpoint_idx) {
            self.faults.fault_handler_endpoint = None;
        }
        self.ipc.endpoint_waiters[endpoint_idx] = None;
        self.ipc.endpoint_sender_waiters[endpoint_idx] = None;
        let mut next_generation = self.ipc.endpoint_generations[endpoint_idx].wrapping_add(1);
        if next_generation == 0 {
            next_generation = 1;
        }
        self.ipc.endpoint_generations[endpoint_idx] = next_generation;
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
        let mut slot_index = None;
        for (idx, slot) in self.ipc.endpoints.iter().enumerate() {
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
        self.ipc.endpoints[endpoint_idx] =
            Some(Endpoint::new_with_mode(max_depth, mode).map_err(map_ipc_error)?);

        let send_cap = self
            .cspace
            .mint(Capability::new(
                CapObject::Endpoint {
                    index: endpoint_idx,
                    generation: self.ipc.endpoint_generations[endpoint_idx],
                },
                CapRights::SEND,
            ))
            .map_err(|_| KernelError::CapabilityFull)?;

        let recv_cap = self
            .cspace
            .mint(Capability::new(
                CapObject::Endpoint {
                    index: endpoint_idx,
                    generation: self.ipc.endpoint_generations[endpoint_idx],
                },
                CapRights::RECEIVE,
            ))
            .map_err(|_| KernelError::CapabilityFull)?;

        Ok((endpoint_idx, send_cap, recv_cap))
    }

    pub fn create_notification(
        &mut self,
        max_depth: usize,
    ) -> Result<(usize, CapId, CapId), KernelError> {
        let (endpoint_idx, notif_send_cap, recv_cap) =
            self.create_endpoint_with_mode(max_depth, EndpointMode::Buffered)?;

        let mut slot_index = None;
        for (idx, slot) in self.ipc.notifications.iter().enumerate() {
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
        self.ipc.notifications[notification_idx] = Some(NotificationObject { endpoint_idx });

        let notification_cap = self
            .cspace
            .mint(Capability::new(
                CapObject::Notification {
                    index: notification_idx,
                    generation: self.ipc.notification_generations[notification_idx],
                },
                CapRights::SIGNAL,
            ))
            .map_err(|_| KernelError::CapabilityFull)?;

        let _ = notif_send_cap;
        Ok((notification_idx, notification_cap, recv_cap))
    }

    fn resolve_notification_index(&self, object: CapObject) -> Result<usize, KernelError> {
        match object {
            CapObject::Notification { index, generation } => {
                if index >= MAX_NOTIFICATIONS || self.ipc.notifications[index].is_none() {
                    return Err(KernelError::WrongObject);
                }
                if self.ipc.notification_generations[index] != generation {
                    return Err(KernelError::StaleCapability);
                }
                Ok(index)
            }
            _ => Err(KernelError::WrongObject),
        }
    }

    pub fn bind_irq_notification(
        &mut self,
        irq_line: u16,
        notification_cap: CapId,
    ) -> Result<(), KernelError> {
        let capability = self
            .cspace
            .get(notification_cap)
            .ok_or(KernelError::InvalidCapability)?;
        if !capability.has_right(CapRights::SIGNAL) {
            return Err(KernelError::MissingRight);
        }

        let notif_idx = self.resolve_notification_index(capability.object)?;
        let irq_idx = irq_line as usize;
        if irq_idx >= MAX_IRQ_LINES {
            return Err(KernelError::WrongObject);
        }
        self.ipc.irq_routes[irq_idx] = Some(notif_idx);
        Ok(())
    }

    fn signal_notification(
        &mut self,
        notification_idx: usize,
        irq_line: u16,
    ) -> Result<(), KernelError> {
        let notif = self.ipc.notifications[notification_idx].ok_or(KernelError::WrongObject)?;
        let payload = irq_line.to_le_bytes();
        let msg = Message::with_header(0, irq_line, 0, None, &payload).map_err(map_ipc_error)?;
        if let Some(endpoint) = self.ipc.endpoints[notif.endpoint_idx].as_mut() {
            endpoint
                .send(msg)
                .map_err(|_| KernelError::EndpointQueueFull)?;
            let _ = self.wake_waiter_for_endpoint(notif.endpoint_idx);
            Ok(())
        } else {
            Err(KernelError::WrongObject)
        }
    }

    pub fn route_external_irq(&mut self, irq_line: u16) -> Result<(), KernelError> {
        let irq_idx = irq_line as usize;
        let Some(notification_idx) = self.ipc.irq_routes.get(irq_idx).copied().flatten() else {
            return Ok(());
        };
        self.signal_notification(notification_idx, irq_line)
    }

    pub fn ipc_send(&mut self, send_cap: CapId, msg: Message) -> Result<(), KernelError> {
        let capability = self
            .cspace
            .get(send_cap)
            .ok_or(KernelError::InvalidCapability)?;
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
        let capability = self
            .cspace
            .get(send_cap)
            .ok_or(KernelError::InvalidCapability)?;
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
        if self.cspace.get(transfer_cap).is_none() {
            return Err(KernelError::InvalidCapability);
        }
        let msg = Message::with_header(
            sender_tid.0,
            opcode,
            Message::FLAG_CAP_TRANSFER,
            Some(transfer_cap.0),
            payload,
        )
        .map_err(map_ipc_error)?;
        self.ipc_send(send_cap, msg)
    }

    pub fn ipc_recv(&mut self, recv_cap: CapId) -> Result<Option<Message>, KernelError> {
        let capability = self
            .cspace
            .get(recv_cap)
            .ok_or(KernelError::InvalidCapability)?;
        if !capability.has_right(CapRights::RECEIVE) {
            return Err(KernelError::MissingRight);
        }

        let endpoint_idx = self.resolve_endpoint_index(capability.object)?;

        let endpoint = self
            .ipc
            .endpoints
            .get_mut(endpoint_idx)
            .and_then(Option::as_mut)
            .ok_or(KernelError::WrongObject)?;

        if let Some(msg) = endpoint.recv() {
            if let Some((sender_tid, pending_msg, sender_blocked)) =
                self.ipc.endpoint_sender_waiters[endpoint_idx].take()
            {
                endpoint
                    .send(pending_msg)
                    .map_err(|_| KernelError::EndpointQueueFull)?;
                if sender_blocked {
                    self.wake_sender_waiter(sender_tid)?;
                }
            }
            return Ok(Some(msg));
        }

        if let Some((sender_tid, pending_msg, sender_blocked)) =
            self.ipc.endpoint_sender_waiters[endpoint_idx].take()
        {
            if sender_blocked {
                self.wake_sender_waiter(sender_tid)?;
            }
            return Ok(Some(pending_msg));
        }

        self.block_current_on_receive(endpoint_idx, recv_cap)?;
        Ok(None)
    }
}
