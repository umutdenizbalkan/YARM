use super::{KernelError, KernelState};
use crate::kernel::supervisor_abi::{
    TaskExitedEvent, TransferRevokedEvent, task_exited_message, transfer_revoked_message,
};
use crate::kernel::task::{RestartToken, TaskStatus, ThreadDetachState};

impl KernelState {
    pub fn report_task_exit_to_supervisor(
        &mut self,
        tid: u64,
        code: u64,
        restart_token: u64,
    ) -> Result<(), KernelError> {
        let Some(endpoint_idx) = self.with_fault_state(|faults| faults.supervisor_endpoint) else {
            return Ok(());
        };
        let msg = task_exited_message(
            0,
            TaskExitedEvent {
                tid,
                exit_code: code,
                restart_token,
            },
        )
        .map_err(|_| KernelError::WrongObject)?;
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

    pub fn report_transfer_revoke_to_supervisor(
        &mut self,
        owner_pid: u64,
        cap: u64,
        base: u64,
        len: u64,
    ) -> Result<(), KernelError> {
        let Some(endpoint_idx) = self.with_fault_state(|faults| faults.supervisor_endpoint) else {
            return Ok(());
        };
        let msg = transfer_revoked_message(
            0,
            TransferRevokedEvent {
                owner_pid,
                cap,
                base,
                len,
            },
        )
        .map_err(|_| KernelError::WrongObject)?;
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
        self.with_tcbs_mut(|tcbs| {
            let tcb = tcbs
                .iter_mut()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .ok_or(KernelError::TaskMissing)?;
            tcb.status = TaskStatus::Exited(code);
            tcb.restart.token = Some(RestartToken(token));
            Ok::<_, KernelError>(())
        })?;
        self.report_task_exit_to_supervisor(tid, code, token)?;
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
        let token_matches = self.with_tcbs(|tcbs| {
            tcbs.iter()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .map(|tcb| tcb.restart.token == Some(RestartToken(token)))
        });
        let token_matches = token_matches.ok_or(KernelError::TaskMissing)?;
        if !token_matches {
            return Err(KernelError::WrongObject);
        }

        let _ = self.revoke_driver_runtime_caps(tid);

        self.with_tcbs_mut(|tcbs| {
            let tcb = tcbs
                .iter_mut()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .ok_or(KernelError::TaskMissing)?;
            tcb.restart.token = None;
            tcb.status = TaskStatus::Runnable;
            Ok::<_, KernelError>(())
        })?;
        self.enqueue_task(tid).map(|_| ())
    }

    pub fn mark_task_dead(&mut self, tid: u64) -> Result<(), KernelError> {
        let process_pid = self
            .thread_group_id(tid)
            .map(|group| group.0)
            .ok_or(KernelError::TaskMissing)?;
        self.with_tcbs_mut(|tcbs| {
            let tcb = tcbs
                .iter_mut()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .ok_or(KernelError::TaskMissing)?;
            tcb.status = TaskStatus::Dead;
            tcb.restart.token = None;
            Ok::<_, KernelError>(())
        })?;
        let _ = self.release_kernel_context(tid);
        let _ = self.revoke_driver_runtime_caps(tid);
        self.maybe_cleanup_process_cnode_for_pid(process_pid);
        Ok(())
    }
}
