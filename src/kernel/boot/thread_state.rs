use super::{KernelError, KernelState};
use crate::kernel::ipc::ThreadId;
use crate::kernel::task::{
    RobustFutexState, TaskStatus, ThreadDetachState, ThreadGroupId, UserRegisterContext, WaitReason,
};
use crate::kernel::trapframe::TrapFrame;

impl KernelState {
    pub fn thread_group_id(&self, tid: u64) -> Option<ThreadGroupId> {
        self.tcbs
            .iter()
            .flatten()
            .find(|tcb| tcb.tid.0 == tid)
            .map(|tcb| tcb.thread_group_id)
    }

    pub fn thread_tls_base(&self, tid: u64) -> Option<usize> {
        self.tcbs
            .iter()
            .flatten()
            .find(|tcb| tcb.tid.0 == tid)
            .map(|tcb| tcb.tls_ptr.map(|ptr| ptr.0 as usize))
            .flatten()
    }

    pub fn process_id(&self, tid: u64) -> Option<u64> {
        self.thread_group_id(tid).map(|group_id| group_id.0)
    }

    pub fn is_thread_group_leader(&self, tid: u64) -> bool {
        self.thread_group_id(tid) == Some(ThreadGroupId(tid))
    }

    pub fn thread_user_context(&self, tid: u64) -> Option<UserRegisterContext> {
        self.tcbs
            .iter()
            .flatten()
            .find(|tcb| tcb.tid.0 == tid)
            .map(|tcb| tcb.user_context)
    }

    pub fn set_thread_user_context(
        &mut self,
        tid: u64,
        context: UserRegisterContext,
    ) -> Result<(), KernelError> {
        let tcb = self.tcb_mut(tid).ok_or(KernelError::TaskMissing)?;
        tcb.user_context = context;
        Ok(())
    }

    pub fn tls_restore_pending(&self, tid: u64) -> Option<bool> {
        self.tcbs
            .iter()
            .flatten()
            .find(|tcb| tcb.tid.0 == tid)
            .map(|tcb| {
                self.tls_restore_pending
                    .iter()
                    .flatten()
                    .any(|pending_tid| *pending_tid == tcb.tid)
            })
    }

    pub fn take_tls_restore_request(&mut self, tid: u64) -> Result<Option<usize>, KernelError> {
        let idx = self
            .tls_restore_pending
            .iter()
            .position(|slot| slot.is_some_and(|pending_tid| pending_tid.0 == tid));
        let Some(idx) = idx else {
            return Ok(None);
        };
        self.tls_restore_pending[idx] = None;
        Ok(self.thread_tls_base(tid))
    }

    pub fn mark_thread_detached(&mut self, tid: u64) -> Result<(), KernelError> {
        let tcb = self.tcb_mut(tid).ok_or(KernelError::TaskMissing)?;
        tcb.detach_state = ThreadDetachState::Detached;
        Ok(())
    }

    pub fn thread_detach_state(&self, tid: u64) -> Option<ThreadDetachState> {
        self.tcbs
            .iter()
            .flatten()
            .find(|tcb| tcb.tid.0 == tid)
            .map(|tcb| tcb.detach_state)
    }

    pub fn join_thread(&mut self, tid: u64) -> Result<Option<u64>, KernelError> {
        let tcb = self.tcb_mut(tid).ok_or(KernelError::TaskMissing)?;
        if tcb.detach_state == ThreadDetachState::Detached {
            return Err(KernelError::WrongObject);
        }
        let TaskStatus::Exited(exit_code) = tcb.status else {
            let current_tid = self.current_tid();
            if let Some(joiner_tid) = current_tid.filter(|joiner| *joiner != tid) {
                let joiner_pid = self
                    .process_id(joiner_tid)
                    .ok_or(KernelError::TaskMissing)?;
                let target_pid = self.process_id(tid).ok_or(KernelError::TaskMissing)?;
                if joiner_pid != target_pid {
                    return Err(KernelError::WrongObject);
                }
            }
            if let Some(joiner_tid) = current_tid.filter(|joiner| *joiner != tid) {
                let joiner = self.tcb_mut(joiner_tid).ok_or(KernelError::TaskMissing)?;
                joiner.status = TaskStatus::Blocked(WaitReason::Join(ThreadId(tid)));
                let _ = self.block_current_cpu();
                self.dispatch_next_task()?;
            }
            return Ok(None);
        };
        tcb.status = TaskStatus::Dead;
        Ok(Some(exit_code))
    }

    pub fn set_robust_futex_head(
        &mut self,
        tid: u64,
        head: usize,
        len: usize,
    ) -> Result<(), KernelError> {
        if head == 0 || len == 0 {
            return Err(KernelError::WrongObject);
        }
        let _ = self.tcb_mut(tid).ok_or(KernelError::TaskMissing)?;
        if let Some(slot) = self
            .robust_futex
            .iter_mut()
            .find(|slot| slot.is_some_and(|entry| entry.tid == ThreadId(tid)) || slot.is_none())
        {
            *slot = Some(super::RobustFutexRecord {
                tid: ThreadId(tid),
                state: RobustFutexState { head, len },
            });
            Ok(())
        } else {
            Err(KernelError::TaskTableFull)
        }
    }

    pub fn robust_futex_state(&self, tid: u64) -> Option<RobustFutexState> {
        self.robust_futex
            .iter()
            .flatten()
            .find(|entry| entry.tid.0 == tid)
            .map(|entry| entry.state)
    }

    pub(crate) fn sync_current_thread_from_frame(
        &mut self,
        frame: &TrapFrame,
    ) -> Result<(), KernelError> {
        let tid = self.current_tid().ok_or(KernelError::TaskMissing)?;
        let tcb = self.tcb_mut(tid).ok_or(KernelError::TaskMissing)?;
        tcb.user_context = frame.capture_user_context();
        Ok(())
    }

    fn apply_current_thread_to_frame(&mut self, frame: &mut TrapFrame) -> Result<(), KernelError> {
        let tid = self.current_tid().ok_or(KernelError::TaskMissing)?;
        let context = self
            .thread_user_context(tid)
            .ok_or(KernelError::TaskMissing)?;
        frame.apply_user_context(context);
        Ok(())
    }

    pub fn resume_current_thread_with_frame(
        &mut self,
        frame: &mut TrapFrame,
    ) -> Result<Option<usize>, KernelError> {
        self.apply_current_thread_to_frame(frame)?;
        let tid = self.current_tid().ok_or(KernelError::TaskMissing)?;
        self.take_tls_restore_request(tid)
    }

    pub(crate) fn wake_joiners_for(&mut self, target_tid: u64) -> Result<u32, KernelError> {
        let mut woken = 0u32;
        for idx in 0..self.tcbs.len() {
            let wake_tid = {
                let Some(tcb) = self.tcbs[idx].as_mut() else {
                    continue;
                };
                if tcb.status != TaskStatus::Blocked(WaitReason::Join(ThreadId(target_tid))) {
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

    pub(crate) fn reap_if_detached(&mut self, tid: u64) -> Result<(), KernelError> {
        let detached = self
            .thread_detach_state(tid)
            .ok_or(KernelError::TaskMissing)?
            == ThreadDetachState::Detached;
        if detached {
            self.mark_task_dead(tid)?;
        }
        Ok(())
    }

    pub fn set_thread_tls_base(&mut self, tid: u64, tls_base: usize) -> Result<(), KernelError> {
        if tls_base == 0 {
            return Err(KernelError::WrongObject);
        }
        let tcb = self.tcb_mut(tid).ok_or(KernelError::TaskMissing)?;
        tcb.tls_ptr = Some(crate::kernel::vm::VirtAddr(tls_base as u64));
        if let Some(slot) = self
            .tls_restore_pending
            .iter_mut()
            .find(|slot| slot.is_some_and(|pending_tid| pending_tid.0 == tid) || slot.is_none())
        {
            *slot = Some(ThreadId(tid));
        }
        Ok(())
    }

    pub fn spawn_user_thread(
        &mut self,
        parent_tid: u64,
        tls_base: usize,
        user_stack_top: usize,
        user_entry: usize,
    ) -> Result<u64, KernelError> {
        if tls_base == 0 || user_stack_top == 0 || user_entry == 0 {
            return Err(KernelError::WrongObject);
        }
        let parent = self
            .tcbs
            .iter()
            .flatten()
            .find(|tcb| tcb.tid.0 == parent_tid)
            .cloned()
            .ok_or(KernelError::TaskMissing)?;
        let parent_cnode = self.task_cnode(parent_tid).ok_or(KernelError::TaskMissing)?;
        let tid = self.allocate_thread_id()?;
        self.register_task_with_class_in_process(tid, parent.class, parent.thread_group_id.0)?;
        if let Some(tcb) = self.tcb_mut(tid) {
            tcb.thread_group_id = parent.thread_group_id;
            tcb.asid = parent.asid;
            tcb.cnode = parent_cnode;
            tcb.tls_ptr = Some(crate::kernel::vm::VirtAddr(tls_base as u64));
            tcb.user_entry = Some(crate::kernel::vm::VirtAddr(user_entry as u64));
            tcb.user_stack_top = Some(crate::kernel::vm::VirtAddr(user_stack_top as u64));
            tcb.user_context = UserRegisterContext {
                instruction_ptr: crate::kernel::vm::VirtAddr(user_entry as u64),
                stack_ptr: crate::kernel::vm::VirtAddr(user_stack_top as u64),
                arg0: 0,
                arg1: 0,
            };
            tcb.status = TaskStatus::Runnable;
        }
        if let Some(slot) = self
            .tls_restore_pending
            .iter_mut()
            .find(|slot| slot.is_some_and(|pending_tid| pending_tid.0 == tid) || slot.is_none())
        {
            *slot = Some(ThreadId(tid));
        }
        let _ = self.enqueue_task(tid)?;
        Ok(tid)
    }
}
