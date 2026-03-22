use super::{KernelError, KernelState, RestartTelemetry, map_ipc_error};
use crate::kernel::ipc::Message;
use crate::kernel::task::{RestartToken, TaskClass, TaskStatus, ThreadDetachState};
use crate::kernel::time::{TickDuration, TickInstant};

impl KernelState {
    pub fn report_task_exit_to_supervisor(
        &mut self,
        tid: u64,
        code: u64,
    ) -> Result<(), KernelError> {
        let Some(endpoint_idx) = self.faults.supervisor_endpoint else {
            return Ok(());
        };
        let mut payload = [0u8; 16];
        payload[..8].copy_from_slice(&tid.to_le_bytes());
        payload[8..16].copy_from_slice(&code.to_le_bytes());
        let msg = Message::with_header(0, 0xEE, 0, None, &payload).map_err(map_ipc_error)?;
        let endpoint = self
            .ipc
            .endpoints
            .get_mut(endpoint_idx)
            .and_then(Option::as_mut)
            .ok_or(KernelError::WrongObject)?;
        endpoint
            .send(msg)
            .map_err(|_| KernelError::EndpointQueueFull)?;
        let _ = self.wake_waiter_for_endpoint(endpoint_idx);
        Ok(())
    }

    fn report_restart_denial_to_supervisor(
        &mut self,
        tid: u64,
        denied_count: u32,
    ) -> Result<(), KernelError> {
        let Some(endpoint_idx) = self.faults.supervisor_endpoint else {
            return Ok(());
        };
        let mut payload = [0u8; 16];
        payload[..8].copy_from_slice(&tid.to_le_bytes());
        payload[8..12].copy_from_slice(&denied_count.to_le_bytes());
        let msg = Message::with_header(0, 0xEF, 0, None, &payload).map_err(map_ipc_error)?;
        let endpoint = self
            .ipc
            .endpoints
            .get_mut(endpoint_idx)
            .and_then(Option::as_mut)
            .ok_or(KernelError::WrongObject)?;
        endpoint
            .send(msg)
            .map_err(|_| KernelError::EndpointQueueFull)?;
        let _ = self.wake_waiter_for_endpoint(endpoint_idx);
        Ok(())
    }

    pub fn set_task_restart_policy(
        &mut self,
        tid: u64,
        budget: u8,
        backoff_ticks: u64,
    ) -> Result<(), KernelError> {
        let tcb = self.tcb_mut(tid).ok_or(KernelError::TaskMissing)?;
        tcb.restart.budget = budget;
        tcb.restart.backoff = TickDuration(backoff_ticks);
        Ok(())
    }

    pub fn task_restart_telemetry(&self, tid: u64) -> Result<RestartTelemetry, KernelError> {
        let tcb = self
            .tcbs
            .iter()
            .flatten()
            .find(|tcb| tcb.tid.0 == tid)
            .ok_or(KernelError::TaskMissing)?;
        Ok(RestartTelemetry {
            budget_remaining: tcb.restart.budget,
            backoff_ticks: tcb.restart.backoff.0,
            available_at_tick: tcb.restart.available_at.0,
            token_outstanding: tcb.restart.token.is_some(),
            denied_count: tcb.restart.denied_count,
            escalation_count: tcb.restart.escalation_count,
            last_exit_code: tcb.last_exit_code,
        })
    }

    pub fn exit_task(&mut self, tid: u64, code: u64) -> Result<u64, KernelError> {
        let token = self.restart.next_restart_token;
        self.restart.next_restart_token =
            self.restart.next_restart_token.checked_add(1).unwrap_or(1);

        let robust = self.robust_futex_state(tid);
        let detached = self.thread_detach_state(tid) == Some(ThreadDetachState::Detached);
        let tcb = self.tcb_mut(tid).ok_or(KernelError::TaskMissing)?;
        tcb.status = TaskStatus::Exited;
        tcb.restart.token = Some(RestartToken(token));
        tcb.last_exit_code = Some(code);
        self.report_task_exit_to_supervisor(tid, code)?;
        if let Some(robust) = robust {
            let stride = core::mem::size_of::<usize>();
            let mut offset = 0usize;
            while offset < robust.len {
                let addr = robust.head.saturating_add(offset.saturating_mul(stride));
                let _ = self.futex_wake(addr, u32::MAX);
                offset += 1;
            }
        }
        let _ = self.wake_joiners_for(tid)?;

        if self.scheduler.current_tid() == Some(tid) {
            let _ = self.scheduler.block_current();
            let _ = self.dispatch_next_task()?;
        }
        if detached {
            self.reap_if_detached(tid)?;
        }

        Ok(token)
    }

    pub fn restart_task(&mut self, tid: u64, token: u64) -> Result<(), KernelError> {
        let now_tick = TickInstant(self.timer.current_ticks().0);
        let (app_threshold, driver_threshold, system_threshold) = (
            self.restart.app_escalation_threshold,
            self.restart.driver_escalation_threshold,
            self.restart.system_escalation_threshold,
        );

        let mut should_notify = None;
        let err = {
            let tcb = self.tcb_mut(tid).ok_or(KernelError::TaskMissing)?;
            let mut denied = false;
            let err = if tcb.restart.token != Some(RestartToken(token)) {
                denied = true;
                Some(KernelError::WrongObject)
            } else if tcb.restart.budget == 0 {
                denied = true;
                Some(KernelError::WouldBlock)
            } else if now_tick < tcb.restart.available_at {
                denied = true;
                Some(KernelError::WouldBlock)
            } else {
                None
            };

            if denied {
                tcb.restart.denied_count = tcb.restart.denied_count.saturating_add(1);
                let threshold = match tcb.class {
                    TaskClass::App => app_threshold,
                    TaskClass::Driver => driver_threshold,
                    TaskClass::SystemServer => system_threshold,
                };
                if tcb.restart.denied_count.is_multiple_of(threshold) {
                    tcb.restart.escalation_count = tcb.restart.escalation_count.saturating_add(1);
                    should_notify = Some(tcb.restart.denied_count);
                }
            }
            err
        };

        if let Some(count) = should_notify {
            self.report_restart_denial_to_supervisor(tid, count)?;
        }

        if let Some(err) = err {
            return Err(err);
        }

        let _ = self.revoke_driver_runtime_caps(tid);

        let tcb = self.tcb_mut(tid).ok_or(KernelError::TaskMissing)?;
        tcb.restart.budget = tcb.restart.budget.saturating_sub(1);
        tcb.restart.available_at = TickInstant(now_tick.0.saturating_add(tcb.restart.backoff.0));
        tcb.restart.token = None;
        tcb.status = TaskStatus::Runnable;
        self.enqueue_task(tid).map(|_| ())
    }

    pub fn mark_task_dead(&mut self, tid: u64) -> Result<(), KernelError> {
        {
            let tcb = self.tcb_mut(tid).ok_or(KernelError::TaskMissing)?;
            tcb.status = TaskStatus::Dead;
            tcb.restart.token = None;
        }
        let _ = self.revoke_driver_runtime_caps(tid);
        Ok(())
    }
}
