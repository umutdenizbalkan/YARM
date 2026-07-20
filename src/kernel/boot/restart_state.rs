// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::{KernelError, KernelState};
use crate::kernel::ipc::Message;
use crate::kernel::task::{RestartToken, TaskStatus, ThreadDetachState};
use yarm_ipc_abi::process_abi::{KERNEL_OP_PM_TASK_EXITED, KernelPmTaskExitedPayload};
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
        crate::yarm_log!("TASK_EXITED_REPORT_BEGIN tid={}", tid);
        let Some(endpoint_idx) = self.with_fault_state(|faults| faults.supervisor_endpoint) else {
            crate::yarm_log!(
                "TASK_EXITED_REPORT_FAIL tid={} reason=no-supervisor-endpoint",
                tid
            );
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
        match self.send_message_to_endpoint_and_wake(endpoint_idx, msg) {
            Ok(()) => {
                crate::yarm_log!("TASK_EXITED_REPORT_SENT tid={} target=supervisor", tid);
                Ok(())
            }
            Err(err) => {
                crate::yarm_log!("TASK_EXITED_REPORT_FAIL tid={} reason={:?}", tid, err);
                Err(err)
            }
        }
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

    /// Stage 77+78: deliver a task-exit notification to PM's `pm_task_exit_endpoint`.
    ///
    /// Silent no-op when `pm_task_exit_endpoint` is `None` (not yet registered).
    /// Sends `KERNEL_OP_PM_TASK_EXITED` with a 16-byte LE `KernelPmTaskExitedPayload`.
    pub fn report_task_exit_to_pm(&mut self, tid: u64, code: u64) -> Result<(), KernelError> {
        let Some(endpoint_idx) = self.with_fault_state(|faults| faults.pm_task_exit_endpoint)
        else {
            return Ok(());
        };
        let payload = KernelPmTaskExitedPayload::new(tid, code).encode();
        let msg = Message::with_header(0, KERNEL_OP_PM_TASK_EXITED, 0, None, &payload)
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
        // Stage 173 (CAP-CNODE): default-off on-exit cap-revoke markers. Diagnostic
        // only — the reply-cap sweep + waiter cleanup below is UNCHANGED.
        let cap_cnode = crate::kernel::boot::cap_cnode_enabled();
        if cap_cnode {
            let count = self
                .snapshot_live_capabilities_for_task(tid)
                .map(|v| v.len())
                .unwrap_or(0);
            crate::yarm_log!("CAP_CNODE_REVOKE_ON_EXIT tid={} count={}", tid, count);
        }
        // Stage 199A2B1: capture the exiting task's AUTHORITATIVE identity while its
        // TCB is still live (this is the exiting incarnation), then clean up reply
        // records by that exact `{tid, asid}` — never by a numeric TID re-resolved
        // later, so a replacement task reusing the numeric TID is untouched.
        let exit_identity = crate::kernel::boot::ReceiverWaiterIdentity::new(
            crate::kernel::ipc::ThreadId(tid),
            self.task_asid(tid).unwrap_or(crate::kernel::vm::Asid(0)),
        );
        let _ = self.revoke_reply_caps_for_caller_identity(exit_identity);
        let _ = self.revoke_reply_caps_for_replier_identity(exit_identity);
        if cap_cnode {
            crate::yarm_log!("CAP_CNODE_REVOKE_ON_EXIT_OK tid={}", tid);
        }
        // Stage 174 (FAULT-DELIVERY): default-off cleanup markers around the IPC
        // waiter sweep for an exiting (possibly faulted) task. The sweep itself is
        // UNCHANGED — this only exposes that a faulting task's queued/waiting IPC
        // references are cleared so no dangling fault-channel reference remains.
        let fault_delivery = crate::kernel::boot::fault_delivery_enabled();
        if fault_delivery {
            crate::yarm_log!("FAULT_DELIVERY_TASK_CLEANUP_BEGIN tid={}", tid);
        }
        self.clear_ipc_waiters_for_tid(tid);
        if fault_delivery {
            crate::yarm_log!("FAULT_DELIVERY_TASK_CLEANUP_OK tid={}", tid);
        }
        self.report_task_exit_to_supervisor(tid, code, token)?;
        self.report_task_exit_to_pm(tid, code)?;
        if let Some(robust) = robust {
            // Use futex_wake_on_exit: the addresses come from the task's own
            // robust list and are trusted user-space, but current_tid() may be
            // a different task (e.g. supervisor) when exit is externally driven.
            let stride = core::mem::size_of::<usize>();
            let mut offset = 0usize;
            while offset < robust.len {
                let addr = robust.head.saturating_add(offset.saturating_mul(stride));
                let _ = self.futex_wake_on_exit(addr);
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

        // Stage 174 (FAULT-DELIVERY): default-off supervisor-restart markers. The
        // restart sequence (cap revoke → token clear → runnable → re-enqueue) is
        // UNCHANGED. The token was already validated above, so the fault channel
        // (endpoint index/generation) remains valid across the restart — the
        // restarted task rebinds to the same channel without a stale sender/reply
        // cap or orphaned waiter.
        let fault_delivery = crate::kernel::boot::fault_delivery_enabled();
        if fault_delivery {
            crate::yarm_log!(
                "FAULT_DELIVERY_SUPERVISOR_RESTART_BEGIN old_tid={} new_tid={}",
                tid,
                tid
            );
            crate::yarm_log!(
                "FAULT_DELIVERY_RESTART_TOKEN_OK tid={} token={}",
                tid,
                token
            );
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
        // Stage 199A2B1: restart keeps the same TCB/ASID; capture the authoritative
        // identity and clear any caller-side reply records by exact `{tid, asid}`.
        let restart_identity = crate::kernel::boot::ReceiverWaiterIdentity::new(
            crate::kernel::ipc::ThreadId(tid),
            self.task_asid(tid).unwrap_or(crate::kernel::vm::Asid(0)),
        );
        let _ = self.revoke_reply_caps_for_caller_identity(restart_identity);
        let result = match self.enqueue_task(tid) {
            Ok(_) | Err(KernelError::WouldBlock) => Ok(()),
            Err(err) => Err(err),
        };
        if fault_delivery && result.is_ok() {
            crate::yarm_log!("FAULT_DELIVERY_CHANNEL_REBIND_OK tid={}", tid);
            crate::yarm_log!("FAULT_DELIVERY_SUPERVISOR_RESTART_OK");
        }
        result
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
        // Stage 199A2B1: authoritative-identity reply-record cleanup at task death.
        let dead_identity = crate::kernel::boot::ReceiverWaiterIdentity::new(
            crate::kernel::ipc::ThreadId(tid),
            self.task_asid(tid).unwrap_or(crate::kernel::vm::Asid(0)),
        );
        let _ = self.revoke_reply_caps_for_caller_identity(dead_identity);
        let _ = self.revoke_reply_caps_for_replier_identity(dead_identity);
        self.clear_ipc_waiters_for_tid(tid);
        let _ = self.release_kernel_context(tid);
        let _ = self.revoke_driver_runtime_caps(tid);
        self.maybe_cleanup_process_cnode_for_pid(process_pid);
        Ok(())
    }

    pub fn reap_faulted_task_noalloc_cleanup(&mut self, tid: u64) -> Result<(), KernelError> {
        let process_pid = self
            .thread_group_id(tid)
            .map(|group| group.0)
            .ok_or(KernelError::TaskMissing)?;
        crate::yarm_log!("TASK_REAP_FAULTED_NOALLOC_CLEANUP_BEGIN target_tid={}", tid);
        crate::yarm_log!(
            "TASK_REAP_FAULTED_CLEANUP_STEP target_tid={} step=mark_dead_clear_restart",
            tid
        );
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
        crate::yarm_log!(
            "TASK_REAP_FAULTED_CLEANUP_STEP target_tid={} step=reply_caps",
            tid
        );
        // Stage 199A2B1: authoritative-identity reply-record cleanup at faulted reap.
        let reap_identity = crate::kernel::boot::ReceiverWaiterIdentity::new(
            crate::kernel::ipc::ThreadId(tid),
            self.task_asid(tid).unwrap_or(crate::kernel::vm::Asid(0)),
        );
        let _ = self.revoke_reply_caps_for_caller_identity(reap_identity);
        let _ = self.revoke_reply_caps_for_replier_identity(reap_identity);
        crate::yarm_log!(
            "TASK_REAP_FAULTED_CLEANUP_STEP target_tid={} step=ipc_waiters",
            tid
        );
        self.clear_ipc_waiters_for_tid(tid);
        crate::yarm_log!(
            "TASK_REAP_FAULTED_CLEANUP_STEP target_tid={} step=kernel_context",
            tid
        );
        let _ = self.release_kernel_context(tid);
        crate::yarm_log!(
            "TASK_REAP_FAULTED_CLEANUP_STEP target_tid={} step=process_cnode_noalloc",
            tid
        );
        let _ = self.maybe_cleanup_process_cnode_for_pid_noalloc_reap(process_pid);
        crate::yarm_log!("TASK_REAP_FAULTED_NOALLOC_CLEANUP_OK target_tid={}", tid);
        Ok(())
    }
}
