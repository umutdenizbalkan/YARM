use super::vm::VirtAddr;

pub type IrqNumber = u16;

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
    RouteIrq,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrapEvent {
    Syscall,
    PageFault(FaultInfo),
    TimerInterrupt,
    ExternalInterrupt(IrqNumber),
}

impl TrapEvent {
    pub const fn syscall() -> Self {
        Self::Syscall
    }

    pub const fn page_fault(fault: FaultInfo) -> Self {
        Self::PageFault(fault)
    }

    pub const fn timer_interrupt() -> Self {
        Self::TimerInterrupt
    }

    pub const fn external_interrupt(irq: IrqNumber) -> Self {
        Self::ExternalInterrupt(irq)
    }

    pub const fn trap(&self) -> Trap {
        match self {
            Self::Syscall => Trap::Syscall,
            Self::PageFault(_) => Trap::PageFault,
            Self::TimerInterrupt => Trap::TimerInterrupt,
            Self::ExternalInterrupt(_) => Trap::ExternalInterrupt,
        }
    }

    pub const fn fault(&self) -> Option<FaultInfo> {
        match self {
            Self::PageFault(fault) => Some(*fault),
            _ => None,
        }
    }

    pub const fn irq(&self) -> Option<IrqNumber> {
        match self {
            Self::ExternalInterrupt(irq) => Some(*irq),
            _ => None,
        }
    }
}

/// Routing is currently 1:1 with trap kind. Kept as a separate action enum so
/// future policy can map one trap kind to richer action flows.
pub fn route_trap(event: &TrapEvent) -> TrapAction {
    match event {
        TrapEvent::Syscall => TrapAction::DispatchSyscall,
        TrapEvent::PageFault(_) => TrapAction::HandlePageFault,
        TrapEvent::TimerInterrupt => TrapAction::TickScheduler,
        TrapEvent::ExternalInterrupt(_) => TrapAction::RouteIrq,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trap_router_maps_syscall() {
        assert_eq!(route_trap(&TrapEvent::syscall()), TrapAction::DispatchSyscall);
    }

    #[test]
    fn trap_event_can_carry_irq_number() {
        let event = TrapEvent::external_interrupt(7);
        assert_eq!(event.irq(), Some(7));
        assert_eq!(event.fault(), None);
    }

    #[test]
    fn router_covers_all_traps() {
        assert_eq!(route_trap(&TrapEvent::syscall()), TrapAction::DispatchSyscall);
        assert_eq!(
            route_trap(&TrapEvent::page_fault(FaultInfo {
                addr: VirtAddr(0x1000),
                access: FaultAccess::Read,
            })),
            TrapAction::HandlePageFault
        );
        assert_eq!(route_trap(&TrapEvent::timer_interrupt()), TrapAction::TickScheduler);
        assert_eq!(
            route_trap(&TrapEvent::external_interrupt(1)),
            TrapAction::RouteIrq
        );
    }
}
