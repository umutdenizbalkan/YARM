use super::vm::VirtAddr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultAccess {
    Read,
    Write,
    Execute,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FaultInfo {
    pub addr: VirtAddr,
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
    pub const fn syscall() -> Self {
        Self {
            trap: Trap::Syscall,
            fault: None,
            irq: None,
        }
    }

    pub const fn page_fault(fault: FaultInfo) -> Self {
        Self {
            trap: Trap::PageFault,
            fault: Some(fault),
            irq: None,
        }
    }

    pub const fn timer_interrupt() -> Self {
        Self {
            trap: Trap::TimerInterrupt,
            fault: None,
            irq: None,
        }
    }

    pub const fn external_interrupt(irq: u16) -> Self {
        Self {
            trap: Trap::ExternalInterrupt,
            fault: None,
            irq: Some(irq),
        }
    }
}

/// Routing is currently 1:1 with trap kind. Kept as a separate action enum so
/// future policy can map one trap kind to richer action flows.
pub fn route_trap(event: TrapEvent) -> TrapAction {
    match event.trap {
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
        assert_eq!(
            route_trap(TrapEvent::syscall()),
            TrapAction::DispatchSyscall
        );
    }

    #[test]
    fn trap_event_can_carry_irq_number() {
        let event = TrapEvent::external_interrupt(7);
        assert_eq!(event.irq, Some(7));
        assert_eq!(event.fault, None);
    }

    #[test]
    fn router_covers_all_traps() {
        assert_eq!(
            route_trap(TrapEvent::syscall()),
            TrapAction::DispatchSyscall
        );
        assert_eq!(
            route_trap(TrapEvent::page_fault(FaultInfo {
                addr: VirtAddr(0x1000),
                access: FaultAccess::Read,
            })),
            TrapAction::HandlePageFault
        );
        assert_eq!(
            route_trap(TrapEvent::timer_interrupt()),
            TrapAction::TickScheduler
        );
        assert_eq!(
            route_trap(TrapEvent::external_interrupt(1)),
            TrapAction::HandleDeviceInterrupt
        );
    }
}
