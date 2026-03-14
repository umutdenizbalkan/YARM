use super::capabilities::CapId;
use super::ipc::ThreadId;
use super::vm::{Asid, VirtAddr};
use crate::kernel::bootstrap::FaultPolicy;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RestartToken(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TickDuration(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TickInstant(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitReason {
    EndpointReceive(CapId),
    EndpointSend(CapId),
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

/// Restart/backoff state tracked in scheduler ticks.
///
/// `available_at` is an absolute tick instant in the same clock domain as
/// `Timer::current_ticks`, while `backoff` is a relative duration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestartState {
    pub token: Option<RestartToken>,
    pub budget: u8,
    pub backoff: TickDuration,
    pub available_at: TickInstant,
    pub denied_count: u32,
    pub escalation_count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThreadControlBlock {
    pub tid: ThreadId,
    pub class: TaskClass,
    pub status: TaskStatus,
    pub asid: Option<Asid>,
    /// `None` means fallback to kernel/class policy in `KernelState`.
    pub fault_policy_override: Option<FaultPolicy>,
    pub brk_base: Option<VirtAddr>,
    pub brk_end: Option<VirtAddr>,
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
        let _ = TaskStatus::Faulted;
        let _ = TaskStatus::Exited;
        let _ = TaskStatus::Dead;
    }

    #[test]
    fn tcb_uses_typed_fields() {
        let tcb = ThreadControlBlock {
            tid: ThreadId(7),
            class: TaskClass::App,
            status: TaskStatus::Runnable,
            asid: Some(Asid(1)),
            fault_policy_override: Some(FaultPolicy::KillTask),
            brk_base: Some(VirtAddr(0x1000)),
            brk_end: Some(VirtAddr(0x2000)),
            restart: RestartState {
                token: Some(RestartToken(9)),
                budget: 3,
                backoff: TickDuration(10),
                available_at: TickInstant(20),
                denied_count: 1,
                escalation_count: 0,
            },
            last_exit_code: Some(0),
        };

        assert_eq!(tcb.tid, ThreadId(7));
        assert_eq!(tcb.restart.backoff, TickDuration(10));
        assert_eq!(tcb.status, TaskStatus::Runnable);
    }
}
