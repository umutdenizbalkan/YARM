use super::{KernelError, KernelState, SpawnedUserTask, UserImageSpec};
use crate::arch::hal::Hal;
use crate::kernel::task::{TaskStatus, ThreadGroupId, WaitReason};
use crate::kernel::vm::VirtAddr;

impl KernelState {
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
        let tcb = self.tcb_mut(tid).ok_or(KernelError::TaskMissing)?;
        tcb.status = TaskStatus::Blocked(WaitReason::Futex(VirtAddr(addr as u64)));
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
        let mut woken = 0u32;
        for idx in 0..self.tcbs.len() {
            if woken >= max_wake {
                break;
            }
            let wake_tid = {
                let Some(tcb) = self.tcbs[idx].as_mut() else {
                    continue;
                };
                if tcb.status != TaskStatus::Blocked(WaitReason::Futex(VirtAddr(addr as u64))) {
                    continue;
                }
                tcb.status = TaskStatus::Runnable;
                tcb.tid.0
            };
            self.enqueue_task(wake_tid)?;
            woken += 1;
        }
        Ok(woken)
    }

    pub fn spawn_user_task_from_image(
        &mut self,
        spec: UserImageSpec,
    ) -> Result<SpawnedUserTask, KernelError> {
        self.register_task_with_class(spec.tid, spec.class)?;
        let cnode = self.task_cnode(spec.tid).ok_or(KernelError::TaskMissing)?;
        self.set_process_cnode_for_pid(spec.tid, cnode)?;
        if let Some(tcb) = self.tcb_mut(spec.tid) {
            tcb.thread_group_id = ThreadGroupId(spec.tid);
            tcb.cnode = cnode;
            tcb.asid = spec.asid;
            tcb.user_entry = Some(VirtAddr(spec.entry as u64));
            tcb.user_context.instruction_ptr = VirtAddr(spec.entry as u64);
            tcb.status = TaskStatus::Runnable;
        }
        Ok(SpawnedUserTask {
            tid: spec.tid,
            entry: spec.entry,
            asid: spec.asid,
        })
    }

    pub(crate) fn dispatch_next_task(&mut self) -> Result<Option<u64>, KernelError> {
        let outgoing_asid = self.current_tid().and_then(|tid| self.task_asid(tid));
        let next = self.dispatch_next_current_cpu();
        if let Some(tid) = next {
            let incoming_asid = self.task_asid(tid);
            if let Some(asid) = incoming_asid
                && incoming_asid != outgoing_asid
            {
                self.hal.switch_address_space(asid);
            }
            let tcb = self.tcb_mut(tid).ok_or(KernelError::TaskMissing)?;
            tcb.status = TaskStatus::Running;
        }
        Ok(next)
    }

    pub fn yield_current(&mut self) -> Result<(), KernelError> {
        let outgoing_asid = self.current_tid().and_then(|tid| self.task_asid(tid));
        if let Some(tid) = self.current_tid() {
            let tcb = self.tcb_mut(tid).ok_or(KernelError::TaskMissing)?;
            tcb.status = TaskStatus::Runnable;
        }

        let next_tid = self.on_preempt_current_cpu();
        if let Some(tid) = next_tid {
            let incoming_asid = self.task_asid(tid);
            if let Some(asid) = incoming_asid
                && incoming_asid != outgoing_asid
            {
                self.hal.switch_address_space(asid);
            }
            let tcb = self.tcb_mut(tid).ok_or(KernelError::TaskMissing)?;
            tcb.status = TaskStatus::Running;
        }
        Ok(())
    }
}
