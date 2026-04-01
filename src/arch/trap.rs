use crate::kernel::vm::VirtAddr;

pub type IrqNumber = u16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultAccess {
    Read,
    Write,
    /// Instruction fetch from a non-executable page.
    /// RISC-V: instruction page fault / fetch-side fault classification from
    /// `scause`.
    /// x86-64: `#PF` with the instruction-fetch bit set in the error code.
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
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrapAction {
    DispatchSyscall,
    HandlePageFault,
    TickScheduler,
    RouteIrq,
    Unhandled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrapEvent {
    Syscall,
    PageFault(FaultInfo),
    TimerInterrupt,
    ExternalInterrupt(IrqNumber),
    Unknown { arch_code: u64 },
}

impl TrapEvent {
    pub const fn trap(&self) -> Trap {
        match self {
            Self::Syscall => Trap::Syscall,
            Self::PageFault(_) => Trap::PageFault,
            Self::TimerInterrupt => Trap::TimerInterrupt,
            Self::ExternalInterrupt(_) => Trap::ExternalInterrupt,
            Self::Unknown { .. } => Trap::Unknown,
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

    pub const fn unknown_code(&self) -> Option<u64> {
        match self {
            Self::Unknown { arch_code } => Some(*arch_code),
            _ => None,
        }
    }
}

/// Routing is currently 1:1 with trap kind. Kept as a separate action enum for
/// future non-trivial mappings.
pub fn route_trap(event: &TrapEvent) -> TrapAction {
    match event {
        TrapEvent::Syscall => TrapAction::DispatchSyscall,
        TrapEvent::PageFault(_) => TrapAction::HandlePageFault,
        TrapEvent::TimerInterrupt => TrapAction::TickScheduler,
        TrapEvent::ExternalInterrupt(_) => TrapAction::RouteIrq,
        TrapEvent::Unknown { .. } => TrapAction::Unhandled,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trap_router_maps_syscall() {
        assert_eq!(route_trap(&TrapEvent::Syscall), TrapAction::DispatchSyscall);
    }

    #[test]
    fn trap_event_can_carry_irq_number() {
        let event = TrapEvent::ExternalInterrupt(7);
        assert_eq!(event.irq(), Some(7));
        assert_eq!(event.fault(), None);
    }
}
