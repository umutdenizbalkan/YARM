use super::{KernelError, KernelState, map_ipc_error};
use crate::kernel::ipc::Message;
use crate::kernel::task::{RestartToken, TaskStatus, ThreadDetachState};

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

        if self.current_tid() == Some(tid) {
            let _ = self.block_current_cpu();
            let _ = self.dispatch_next_task()?;
        }
        if detached {
            self.reap_if_detached(tid)?;
        }

        Ok(token)
    }

    pub fn restart_task(&mut self, tid: u64, token: u64) -> Result<(), KernelError> {
        let token_matches = {
            let tcb = self.tcb_mut(tid).ok_or(KernelError::TaskMissing)?;
            tcb.restart.token == Some(RestartToken(token))
        };
        if !token_matches {
            return Err(KernelError::WrongObject);
        }

        let _ = self.revoke_driver_runtime_caps(tid);

        let tcb = self.tcb_mut(tid).ok_or(KernelError::TaskMissing)?;
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
