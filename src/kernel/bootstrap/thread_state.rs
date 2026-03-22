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
            .and_then(|tcb| tcb.linux.tls_base)
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
            .map(|tcb| tcb.linux.tls_restore_pending)
    }

    pub fn take_tls_restore_request(&mut self, tid: u64) -> Result<Option<usize>, KernelError> {
        let tcb = self.tcb_mut(tid).ok_or(KernelError::TaskMissing)?;
        if !tcb.linux.tls_restore_pending {
            return Ok(None);
        }
        tcb.linux.tls_restore_pending = false;
        Ok(tcb.linux.tls_base)
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
        if tcb.status != TaskStatus::Exited {
            let current_tid = self.scheduler.current_tid();
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
                let _ = self.scheduler.block_current();
                self.dispatch_next_task()?;
            }
            return Ok(None);
        }
        let exit_code = tcb.last_exit_code;
        tcb.status = TaskStatus::Dead;
        Ok(exit_code)
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
        let tcb = self.tcb_mut(tid).ok_or(KernelError::TaskMissing)?;
        tcb.linux.robust_futex = Some(RobustFutexState { head, len });
        Ok(())
    }

    pub fn robust_futex_state(&self, tid: u64) -> Option<RobustFutexState> {
        self.tcbs
            .iter()
            .flatten()
            .find(|tcb| tcb.tid.0 == tid)
            .and_then(|tcb| tcb.linux.robust_futex)
    }

    pub(crate) fn sync_current_thread_from_frame(
        &mut self,
        frame: &TrapFrame,
    ) -> Result<(), KernelError> {
        let tid = self
            .scheduler
            .current_tid()
            .ok_or(KernelError::TaskMissing)?;
        let tcb = self.tcb_mut(tid).ok_or(KernelError::TaskMissing)?;
        tcb.user_context = frame.capture_user_context();
        Ok(())
    }

    fn apply_current_thread_to_frame(&mut self, frame: &mut TrapFrame) -> Result<(), KernelError> {
        let tid = self
            .scheduler
            .current_tid()
            .ok_or(KernelError::TaskMissing)?;
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
        let tid = self
            .scheduler
            .current_tid()
            .ok_or(KernelError::TaskMissing)?;
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
        tcb.linux.tls_base = Some(tls_base);
        tcb.linux.tls_restore_pending = true;
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
        let tid = self.allocate_thread_id()?;
        self.register_task_with_class(tid, parent.class)?;
        if let Some(tcb) = self.tcb_mut(tid) {
            tcb.thread_group_id = parent.thread_group_id;
            tcb.asid = parent.asid;
            tcb.linux.tls_base = Some(tls_base);
            tcb.linux.tls_restore_pending = true;
            tcb.user_entry = Some(user_entry);
            tcb.user_stack_top = Some(user_stack_top);
            tcb.user_context = UserRegisterContext {
                instruction_ptr: user_entry,
                stack_ptr: user_stack_top,
                arg0: 0,
                arg1: 0,
            };
            tcb.status = TaskStatus::Runnable;
        }
        let _ = self.enqueue_task(tid)?;
        Ok(tid)
    }
}
