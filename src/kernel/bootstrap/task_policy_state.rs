use super::{ClassPolicySnapshot, KernelError, KernelState, MAX_TASKS, RestartPolicy};
use crate::kernel::ipc::ThreadId;
use crate::kernel::task::{
    LinuxThreadState, RestartState, TaskClass, TaskStatus, ThreadControlBlock, ThreadDetachState,
    ThreadGroupId, UserRegisterContext,
};
use crate::kernel::time::{TickDuration, TickInstant};

impl KernelState {
    pub fn set_class_escalation_threshold(&mut self, class: TaskClass, threshold: u32) {
        let bounded = threshold.max(1);
        match class {
            TaskClass::App => self.restart.app_escalation_threshold = bounded,
            TaskClass::Driver => self.restart.driver_escalation_threshold = bounded,
            TaskClass::SystemServer => self.restart.system_escalation_threshold = bounded,
        }
    }

    fn restart_policy_for_class(&self, class: TaskClass) -> RestartPolicy {
        match class {
            TaskClass::App => self.restart.app_restart_policy,
            TaskClass::Driver => self.restart.driver_restart_policy,
            TaskClass::SystemServer => self.restart.system_restart_policy,
        }
    }

    pub fn class_policy_snapshot(&self, class: TaskClass) -> ClassPolicySnapshot {
        let policy = self.restart_policy_for_class(class);
        let escalation_threshold = match class {
            TaskClass::App => self.restart.app_escalation_threshold,
            TaskClass::Driver => self.restart.driver_escalation_threshold,
            TaskClass::SystemServer => self.restart.system_escalation_threshold,
        };
        ClassPolicySnapshot {
            class,
            restart_budget: policy.budget,
            restart_backoff_ticks: policy.backoff_ticks,
            escalation_threshold,
        }
    }

    pub fn set_class_restart_policy(&mut self, class: TaskClass, budget: u8, backoff_ticks: u64) {
        let policy = RestartPolicy {
            budget,
            backoff_ticks,
        };
        match class {
            TaskClass::App => self.restart.app_restart_policy = policy,
            TaskClass::Driver => self.restart.driver_restart_policy = policy,
            TaskClass::SystemServer => self.restart.system_restart_policy = policy,
        }
    }

    pub fn register_task_with_class(
        &mut self,
        tid: u64,
        class: TaskClass,
    ) -> Result<(), KernelError> {
        if self.task_status(tid).is_some() {
            return Ok(());
        }
        let policy = self.restart_policy_for_class(class);
        if let Some(slot) = self.tcbs.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(ThreadControlBlock {
                tid: ThreadId(tid),
                thread_group_id: ThreadGroupId(tid),
                class,
                status: TaskStatus::Runnable,
                asid: None,
                linux: LinuxThreadState::default(),
                user_entry: None,
                user_stack_top: None,
                user_context: UserRegisterContext::default(),
                detach_state: ThreadDetachState::Joinable,
                fault_policy_override: None,
                restart: RestartState {
                    token: None,
                    budget: policy.budget,
                    backoff: TickDuration(policy.backoff_ticks),
                    available_at: TickInstant(0),
                    denied_count: 0,
                    escalation_count: 0,
                },
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
