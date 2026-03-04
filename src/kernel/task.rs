use super::vm::Asid;
use crate::kernel::bootstrap::FaultPolicy;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitReason {
    EndpointReceive(usize),
    EndpointSend(usize),
    Poll,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    Runnable,
    Running,
    Blocked(WaitReason),
    Faulted,
    Exited(u64),
    Dead,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThreadControlBlock {
    pub tid: u64,
    pub status: TaskStatus,
    pub asid: Option<Asid>,
    pub fault_policy_override: Option<FaultPolicy>,
    pub brk_base: Option<usize>,
    pub brk_end: Option<usize>,
    pub restart_token: Option<u64>,
}
