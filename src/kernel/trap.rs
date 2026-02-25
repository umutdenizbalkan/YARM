#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultAccess {
    Read,
    Write,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FaultInfo {
    pub addr: usize,
    pub access: FaultAccess,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trap {
    Syscall,
    PageFault,
    TimerInterrupt,
    ExternalInterrupt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrapAction {
    DispatchSyscall,
    HandlePageFault,
    TickScheduler,
    HandleDeviceInterrupt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrapEvent {
    pub trap: Trap,
    pub fault: Option<FaultInfo>,
}

impl TrapEvent {
    pub const fn new(trap: Trap) -> Self {
        Self { trap, fault: None }
    }

    pub const fn with_fault(trap: Trap, fault: FaultInfo) -> Self {
        Self {
            trap,
            fault: Some(fault),
        }
    }
}

pub fn route_trap(trap: Trap) -> TrapAction {
    match trap {
        Trap::Syscall => TrapAction::DispatchSyscall,
        Trap::PageFault => TrapAction::HandlePageFault,
        Trap::TimerInterrupt => TrapAction::TickScheduler,
        Trap::ExternalInterrupt => TrapAction::HandleDeviceInterrupt,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trap_router_maps_syscall() {
        assert_eq!(route_trap(Trap::Syscall), TrapAction::DispatchSyscall);
    }
}
