use super::capabilities::{CNodeId, CapId};
use super::ipc::ThreadId;
use super::vm::{Asid, VirtAddr};

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
pub enum FaultPolicy {
    KillTask,
    NotifyAndContinue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    Runnable,
    /// Set only by `KernelState::dispatch_next_task()` / yield scheduling paths.
    /// Do not assign directly outside scheduler-mediated transitions.
    Running,
    Blocked(WaitReason),
    Faulted,
    Exited(u64),
    Dead,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadDetachState {
    Joinable,
    Detached,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserRegisterContext {
    pub instruction_ptr: VirtAddr,
    pub stack_ptr: VirtAddr,
    pub arg0: usize,
    pub arg1: usize,
}

impl Default for UserRegisterContext {
    fn default() -> Self {
        Self {
            instruction_ptr: VirtAddr(0),
            stack_ptr: VirtAddr(0),
            arg0: 0,
            arg1: 0,
        }
    }
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
    pub cnode: CNodeId,
    pub thread_group_id: ThreadGroupId,
    pub class: TaskClass,
    pub status: TaskStatus,
    pub asid: Option<Asid>,
    pub tls_ptr: Option<VirtAddr>,
    pub user_entry: Option<VirtAddr>,
    pub user_stack_top: Option<VirtAddr>,
    pub user_context: UserRegisterContext,
    pub detach_state: ThreadDetachState,
    /// `None` means fallback to kernel/class policy in `KernelState`.
    pub fault_policy_override: Option<FaultPolicy>,
    pub restart: RestartState,
}

impl ThreadControlBlock {
    pub fn new(tid: ThreadId, cnode: CNodeId, class: TaskClass, asid: Option<Asid>) -> Self {
        Self {
            tid,
            cnode,
            thread_group_id: ThreadGroupId(tid.0),
            class,
            status: TaskStatus::Runnable,
            asid,
            tls_ptr: None,
            user_entry: None,
            user_stack_top: None,
            user_context: UserRegisterContext::default(),
            detach_state: ThreadDetachState::Joinable,
            fault_policy_override: None,
            restart: RestartState::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_status_variants_construct() {
        let _ = TaskStatus::Runnable;
        let _ = TaskStatus::Running;
        let _ = TaskStatus::Blocked(WaitReason::Poll);
        let _ = TaskStatus::Blocked(WaitReason::Futex(VirtAddr(0x1000)));
        let _ = TaskStatus::Blocked(WaitReason::Join(ThreadId(7)));
        let _ = TaskStatus::Faulted;
        let _ = TaskStatus::Exited(0);
        let _ = TaskStatus::Dead;
    }

    #[test]
    fn tcb_constructor_uses_typed_fields() {
        let mut tcb = ThreadControlBlock::new(ThreadId(7), CNodeId(7), TaskClass::App, Some(Asid(1)));
        tcb.tls_ptr = Some(VirtAddr(0xDEAD_BEEF));
        tcb.user_entry = Some(VirtAddr(0x4000));
        tcb.user_stack_top = Some(VirtAddr(0x8000));
        tcb.user_context = UserRegisterContext {
            instruction_ptr: VirtAddr(0x4000),
            stack_ptr: VirtAddr(0x8000),
            arg0: 1,
            arg1: 2,
        };
        tcb.fault_policy_override = Some(FaultPolicy::KillTask);
        tcb.restart = RestartState {
            token: Some(RestartToken(9)),
        };

        assert_eq!(tcb.tid, ThreadId(7));
        assert_eq!(tcb.cnode, CNodeId(7));
        assert_eq!(tcb.restart.token, Some(RestartToken(9)));
        assert_eq!(tcb.thread_group_id, ThreadGroupId(7));
        assert_eq!(tcb.tls_ptr, Some(VirtAddr(0xDEAD_BEEF)));
        assert_eq!(tcb.user_context.instruction_ptr, VirtAddr(0x4000));
        assert_eq!(tcb.detach_state, ThreadDetachState::Joinable);
        assert_eq!(tcb.status, TaskStatus::Runnable);
    }

    #[test]
    fn tcb_constructor_does_not_truncate_large_tid_for_cnode() {
        let tid = ThreadId(70_000);
        let tcb = ThreadControlBlock::new(tid, CNodeId(70_000), TaskClass::App, None);

        assert_eq!(tcb.cnode, CNodeId(70_000));
        assert_eq!(tcb.thread_group_id, ThreadGroupId(70_000));
    }
}
