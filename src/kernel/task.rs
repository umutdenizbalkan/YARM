use super::vm::Asid;
use crate::kernel::bootstrap::FaultPolicy;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitReason {
    EndpointReceive(usize),
    EndpointSend(usize),
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
    Exited(u64),
    Dead,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThreadControlBlock {
    pub tid: u64,
    pub class: TaskClass,
    pub status: TaskStatus,
    pub asid: Option<Asid>,
    pub fault_policy_override: Option<FaultPolicy>,
    pub brk_base: Option<usize>,
    pub brk_end: Option<usize>,
    pub restart_token: Option<u64>,
    pub restart_budget: u8,
    pub restart_backoff_ticks: u64,
    pub restart_available_at_tick: u64,
    pub restart_denied_count: u32,
    pub restart_escalation_count: u32,
    pub last_exit_code: Option<u64>,
}
