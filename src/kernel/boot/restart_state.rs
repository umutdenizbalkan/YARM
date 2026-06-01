// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::{KernelError, KernelState};
use crate::kernel::ipc::Message;
use crate::kernel::task::{RestartToken, TaskStatus, ThreadDetachState};
use yarm_ipc_abi::supervisor_abi::{
    SUPERVISOR_OP_TASK_EXITED, SUPERVISOR_OP_TRANSFER_REVOKED, encode_task_exited_event,
    encode_transfer_revoked_event,
};

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
        let msg = Message::with_header(
            0,
            SUPERVISOR_OP_TASK_EXITED,
            0,
            None,
            &encode_task_exited_event(tid, code, restart_token),
        )
        .map_err(|_| KernelError::WrongObject)?;
        self.send_message_to_endpoint_and_wake(endpoint_idx, msg)
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
        let msg = Message::with_header(
            0,
            SUPERVISOR_OP_TRANSFER_REVOKED,
            0,
            None,
            &encode_transfer_revoked_event(owner_pid, cap, base, len),
        )
        .map_err(|_| KernelError::WrongObject)?;
        self.send_message_to_endpoint_and_wake(endpoint_idx, msg)
    }

    pub fn exit_task(&mut self, tid: u64, code: u64) -> Result<u64, KernelError> {
        let token = self.with_restart_state_mut(|restart| {
            let token = restart.next_restart_token;
            restart.next_restart_token = restart.next_restart_token.checked_add(1).unwrap_or(1);
            token
        });

        let robust = self.robust_futex_state(tid);
        let detached = self.thread_detach_state(tid) == Some(ThreadDetachState::Detached);
        self.with_tcbs_mut(|tcbs| {
            let tcb = tcbs
                .iter_mut()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .ok_or(KernelError::TaskMissing)?;
            if tcb.blocked_recv_state.take().is_some() {
                crate::yarm_log!("IPC_RECV_BLOCKED_STATE_CLEAR tid={} reason=cancel", tid);
            }
            tcb.status = TaskStatus::Exited(code);
            tcb.restart.token = Some(RestartToken(token));
            Ok::<_, KernelError>(())
        })?;
        let _ = self.revoke_reply_caps_for_caller(tid);
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
        let _ = self.revoke_reply_caps_for_caller(tid);
        match self.enqueue_task(tid) {
            Ok(_) | Err(KernelError::WouldBlock) => Ok(()),
            Err(err) => Err(err),
        }
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
        let _ = self.revoke_reply_caps_for_caller(tid);
        let _ = self.release_kernel_context(tid);
        let _ = self.revoke_driver_runtime_caps(tid);
        self.maybe_cleanup_process_cnode_for_pid(process_pid);
        Ok(())
    }
}
