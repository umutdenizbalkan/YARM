use super::capabilities::CapId;
use super::ipc::ThreadId;
use super::vm::{Asid, VirtAddr};
use crate::kernel::bootstrap::FaultPolicy;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RestartToken(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ThreadGroupId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitReason {
    EndpointReceive(CapId),
    EndpointSend(CapId),
    Futex(VirtAddr),
    Join(ThreadId),
    Poll,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskClass {
    App,
    Driver,
    SystemServer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    Runnable,
    Running,
    Blocked(WaitReason),
    Faulted,
    /// Task exited; inspect `ThreadControlBlock::last_exit_code` for status.
    Exited,
    Dead,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadDetachState {
    Joinable,
    Detached,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UserRegisterContext {
    pub instruction_ptr: usize,
    pub stack_ptr: usize,
    pub arg0: usize,
    pub arg1: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RobustFutexState {
    pub head: usize,
    pub len: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RestartState {
    pub token: Option<RestartToken>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadControlBlock {
    pub tid: ThreadId,
    pub thread_group_id: ThreadGroupId,
    pub class: TaskClass,
    pub status: TaskStatus,
    pub asid: Option<Asid>,
    pub tls_ptr: Option<VirtAddr>,
    pub user_entry: Option<usize>,
    pub user_stack_top: Option<usize>,
    pub user_context: UserRegisterContext,
    pub detach_state: ThreadDetachState,
    /// `None` means fallback to kernel/class policy in `KernelState`.
    pub fault_policy_override: Option<FaultPolicy>,
    pub restart: RestartState,
    pub last_exit_code: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::bootstrap::FaultPolicy;

    #[test]
    fn task_status_variants_construct() {
        let _ = TaskStatus::Runnable;
        let _ = TaskStatus::Running;
        let _ = TaskStatus::Blocked(WaitReason::Poll);
        let _ = TaskStatus::Blocked(WaitReason::Futex(VirtAddr(0x1000)));
        let _ = TaskStatus::Blocked(WaitReason::Join(ThreadId(7)));
        let _ = TaskStatus::Faulted;
        let _ = TaskStatus::Exited;
        let _ = TaskStatus::Dead;
    }

    #[test]
    fn tcb_uses_typed_fields() {
        let tcb = ThreadControlBlock {
            tid: ThreadId(7),
            thread_group_id: ThreadGroupId(7),
            class: TaskClass::App,
            status: TaskStatus::Runnable,
            asid: Some(Asid(1)),
            tls_ptr: Some(VirtAddr(0xDEAD_BEEF)),
            user_entry: Some(0x4000),
            user_stack_top: Some(0x8000),
            user_context: UserRegisterContext {
                instruction_ptr: 0x4000,
                stack_ptr: 0x8000,
                arg0: 1,
                arg1: 2,
            },
            detach_state: ThreadDetachState::Joinable,
            fault_policy_override: Some(FaultPolicy::KillTask),
            restart: RestartState {
                token: Some(RestartToken(9)),
            },
            last_exit_code: Some(0),
        };

        assert_eq!(tcb.tid, ThreadId(7));
        assert_eq!(tcb.restart.token, Some(RestartToken(9)));
        assert_eq!(tcb.thread_group_id, ThreadGroupId(7));
        assert_eq!(tcb.tls_ptr, Some(VirtAddr(0xDEAD_BEEF)));
        assert_eq!(tcb.user_context.instruction_ptr, 0x4000);
        assert_eq!(tcb.detach_state, ThreadDetachState::Joinable);
        assert_eq!(tcb.status, TaskStatus::Runnable);
    }
}
