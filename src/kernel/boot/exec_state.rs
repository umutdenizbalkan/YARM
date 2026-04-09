// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::{KernelError, KernelState, SpawnedUserTask, UserImageSpec};
use crate::arch::hal::Hal;
use crate::kernel::task::{TaskStatus, ThreadGroupId, WaitReason};
use crate::kernel::vm::VirtAddr;

impl KernelState {
    fn maybe_switch_kernel_context(
        &mut self,
        outgoing_tid: Option<u64>,
        incoming_tid: u64,
    ) -> Result<(), KernelError> {
        let Some(outgoing_tid) = outgoing_tid else {
            return Ok(());
        };
        if outgoing_tid == incoming_tid {
            return Ok(());
        }

        let outgoing_idx = self
            .with_tcbs(|tcbs| {
                tcbs.iter()
                    .position(|slot| slot.as_ref().is_some_and(|tcb| tcb.tid.0 == outgoing_tid))
            })
            .ok_or(KernelError::TaskMissing)?;
        let incoming_idx = self
            .with_tcbs(|tcbs| {
                tcbs.iter()
                    .position(|slot| slot.as_ref().is_some_and(|tcb| tcb.tid.0 == incoming_tid))
            })
            .ok_or(KernelError::TaskMissing)?;

        if outgoing_idx == incoming_idx {
            return Ok(());
        }

        self.with_tcbs_mut(|tcbs| {
            let (outgoing_tcb, incoming_tcb) = if outgoing_idx < incoming_idx {
                let (left, right) = tcbs.split_at_mut(incoming_idx);
                (
                    left[outgoing_idx]
                        .as_mut()
                        .ok_or(KernelError::TaskMissing)?,
                    right[0].as_mut().ok_or(KernelError::TaskMissing)?,
                )
            } else {
                let (left, right) = tcbs.split_at_mut(outgoing_idx);
                (
                    right[0].as_mut().ok_or(KernelError::TaskMissing)?,
                    left[incoming_idx]
                        .as_mut()
                        .ok_or(KernelError::TaskMissing)?,
                )
            };

            if !outgoing_tcb.kernel_context.initialized || !incoming_tcb.kernel_context.initialized
            {
                return Ok(());
            }

            crate::arch::selected_isa::context_switch::switch_frames(
                &mut outgoing_tcb.kernel_context.frame,
                &incoming_tcb.kernel_context.frame,
                incoming_tcb.kernel_context.stack_top.map(|top| top.0),
            );
            Ok(())
        })
    }

    pub fn futex_wait_current(
        &mut self,
        addr: usize,
        expected: u32,
        observed: u32,
    ) -> Result<bool, KernelError> {
        if addr == 0 {
            return Err(KernelError::WrongObject);
        }
        if expected != observed {
            return Ok(false);
        }
        let tid = self.current_tid().ok_or(KernelError::TaskMissing)?;
        self.with_tcbs_mut(|tcbs| {
            let tcb = tcbs
                .iter_mut()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .ok_or(KernelError::TaskMissing)?;
            tcb.status = TaskStatus::Blocked(WaitReason::Futex(VirtAddr(addr as u64)));
            Ok::<_, KernelError>(())
        })?;
        let _ = self.block_current_cpu();
        self.dispatch_next_task()?;
        Ok(true)
    }

    pub fn futex_wake(&mut self, addr: usize, max_wake: u32) -> Result<u32, KernelError> {
        if addr == 0 {
            return Err(KernelError::WrongObject);
        }
        if max_wake == 0 {
            return Ok(0);
        }
        let (wake_tids, wake_count) = self.with_tcbs_mut(|tcbs| {
            let mut wake_tids = [None; super::MAX_TASKS];
            let mut wake_count = 0usize;
            for tcb in tcbs.iter_mut().flatten() {
                if wake_count >= max_wake as usize {
                    break;
                }
                if tcb.status != TaskStatus::Blocked(WaitReason::Futex(VirtAddr(addr as u64))) {
                    continue;
                }
                tcb.status = TaskStatus::Runnable;
                wake_tids[wake_count] = Some(tcb.tid.0);
                wake_count += 1;
            }
            (wake_tids, wake_count)
        });
        for wake_tid in wake_tids.iter().take(wake_count).flatten() {
            self.enqueue_task(*wake_tid)?;
        }
        Ok(wake_count as u32)
    }

    pub fn spawn_user_task_from_image(
        &mut self,
        spec: UserImageSpec,
    ) -> Result<SpawnedUserTask, KernelError> {
        self.register_task_with_class(spec.tid, spec.class)?;
        let cnode = self.task_cnode(spec.tid).ok_or(KernelError::TaskMissing)?;
        self.set_process_cnode_for_pid(spec.tid, cnode)?;
        self.with_tcbs_mut(|tcbs| {
            let tcb = tcbs
                .iter_mut()
                .flatten()
                .find(|tcb| tcb.tid.0 == spec.tid)
                .ok_or(KernelError::TaskMissing)?;
            tcb.thread_group_id = ThreadGroupId(spec.tid);
            tcb.asid = spec.asid;
            tcb.user_entry = Some(VirtAddr(spec.entry as u64));
            tcb.user_context.instruction_ptr = VirtAddr(spec.entry as u64);
            tcb.status = TaskStatus::Runnable;
            Ok::<_, KernelError>(())
        })?;
        if spec.asid.is_some() {
            let stack_top = self.allocate_user_stack_with_guard(spec.tid, 64)?;
            self.with_tcbs_mut(|tcbs| {
                let tcb = tcbs
                    .iter_mut()
                    .flatten()
                    .find(|tcb| tcb.tid.0 == spec.tid)
                    .ok_or(KernelError::TaskMissing)?;
                tcb.user_stack_top = Some(stack_top);
                tcb.user_context.stack_ptr = stack_top;
                Ok::<_, KernelError>(())
            })?;
        }
        Ok(SpawnedUserTask {
            tid: spec.tid,
            entry: spec.entry,
            asid: spec.asid,
        })
    }

    pub(crate) fn dispatch_next_task(&mut self) -> Result<Option<u64>, KernelError> {
        self.with_ipc_state_mut(|ipc| {
            ipc.telemetry.scheduler_dispatch_calls =
                ipc.telemetry.scheduler_dispatch_calls.saturating_add(1);
        });
        let outgoing_tid = self.current_tid();
        let next = self.dispatch_next_current_cpu();
        if let Some(tid) = next {
            let incoming_asid = self.task_asid(tid);
            if let Some(asid) = incoming_asid {
                self.hal.switch_address_space(asid);
            }
            self.maybe_switch_kernel_context(outgoing_tid, tid)?;
            if outgoing_tid != Some(tid) {
                self.with_ipc_state_mut(|ipc| {
                    ipc.telemetry.scheduler_context_switches =
                        ipc.telemetry.scheduler_context_switches.saturating_add(1);
                });
            }
            self.with_tcbs_mut(|tcbs| {
                let tcb = tcbs
                    .iter_mut()
                    .flatten()
                    .find(|tcb| tcb.tid.0 == tid)
                    .ok_or(KernelError::TaskMissing)?;
                tcb.status = TaskStatus::Running;
                Ok::<_, KernelError>(())
            })?;
        }
        Ok(next)
    }

    pub fn dispatch_ready_task(&mut self) -> Result<Option<u64>, KernelError> {
        self.dispatch_next_task()
    }

    pub fn yield_current(&mut self) -> Result<(), KernelError> {
        self.with_ipc_state_mut(|ipc| {
            ipc.telemetry.scheduler_yield_calls =
                ipc.telemetry.scheduler_yield_calls.saturating_add(1);
        });
        let outgoing_tid = self.current_tid();
        if let Some(tid) = outgoing_tid {
            self.with_tcbs_mut(|tcbs| {
                let tcb = tcbs
                    .iter_mut()
                    .flatten()
                    .find(|tcb| tcb.tid.0 == tid)
                    .ok_or(KernelError::TaskMissing)?;
                tcb.status = TaskStatus::Runnable;
                Ok::<_, KernelError>(())
            })?;
        }

        let next_tid = self.on_preempt_current_cpu();
        if let Some(tid) = next_tid {
            let incoming_asid = self.task_asid(tid);
            if let Some(asid) = incoming_asid {
                self.hal.switch_address_space(asid);
            }
            self.maybe_switch_kernel_context(outgoing_tid, tid)?;
            if outgoing_tid != Some(tid) {
                self.with_ipc_state_mut(|ipc| {
                    ipc.telemetry.scheduler_context_switches =
                        ipc.telemetry.scheduler_context_switches.saturating_add(1);
                });
            }
            self.with_tcbs_mut(|tcbs| {
                let tcb = tcbs
                    .iter_mut()
                    .flatten()
                    .find(|tcb| tcb.tid.0 == tid)
                    .ok_or(KernelError::TaskMissing)?;
                tcb.status = TaskStatus::Running;
                Ok::<_, KernelError>(())
            })?;
        }
        Ok(())
    }
}
