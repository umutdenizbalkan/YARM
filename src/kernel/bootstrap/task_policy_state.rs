use super::{KernelError, KernelState, MAX_TASKS};
use crate::kernel::ipc::ThreadId;
use crate::kernel::task::{
    RestartState, TaskClass, TaskStatus, ThreadControlBlock, ThreadDetachState, ThreadGroupId,
    UserRegisterContext,
};

impl KernelState {
    pub fn register_task_with_class(
        &mut self,
        tid: u64,
        class: TaskClass,
    ) -> Result<(), KernelError> {
        if self.task_status(tid).is_some() {
            return Ok(());
        }
        if let Some(slot) = self.tcbs.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(ThreadControlBlock {
                tid: ThreadId(tid),
                thread_group_id: ThreadGroupId(tid),
                class,
                status: TaskStatus::Runnable,
                asid: None,
                tls_ptr: None,
                user_entry: None,
                user_stack_top: None,
                user_context: UserRegisterContext::default(),
                detach_state: ThreadDetachState::Joinable,
                fault_policy_override: None,
                restart: RestartState::default(),
                last_exit_code: None,
            });
            Ok(())
        } else {
            Err(KernelError::TaskTableFull)
        }
    }

    pub fn register_task(&mut self, tid: u64) -> Result<(), KernelError> {
        self.register_task_with_class(tid, TaskClass::App)
    }

    pub fn allocate_thread_id(&mut self) -> Result<u64, KernelError> {
        let mut candidate = self.next_dynamic_tid;
        for _ in 0..MAX_TASKS.saturating_mul(4) {
            self.next_dynamic_tid = self.next_dynamic_tid.saturating_add(1);
            if self.task_status(candidate).is_none() {
                return Ok(candidate);
            }
            candidate = self.next_dynamic_tid;
        }
        Err(KernelError::TaskTableFull)
    }
}
