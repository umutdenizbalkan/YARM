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
    pub irq: Option<u16>,
}

impl TrapEvent {
    pub const fn new(trap: Trap) -> Self {
        Self {
            trap,
            fault: None,
            irq: None,
        }
    }

    pub const fn with_fault(trap: Trap, fault: FaultInfo) -> Self {
        Self {
            trap,
            fault: Some(fault),
            irq: None,
        }
    }

    pub const fn with_irq(trap: Trap, irq: u16) -> Self {
        Self {
            trap,
            fault: None,
            irq: Some(irq),
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

    #[test]
    fn trap_event_can_carry_irq_number() {
        let event = TrapEvent::with_irq(Trap::ExternalInterrupt, 7);
        assert_eq!(event.irq, Some(7));
        assert_eq!(event.fault, None);
    }
}
