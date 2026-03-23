use super::vm::VirtAddr;

pub type IrqNumber = u16;

// NOTE: `TrapEvent` and `Trap` currently live in the kernel layer for prototype
// simplicity. When multi-arch support expands, `TrapEvent` should move to
// `crate::arch::trap` so each architecture entry path produces a typed event
// and the kernel layer only consumes `TrapAction`.
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

/// Routing is currently 1:1 with trap kind. Kept as a separate action enum for
/// future non-trivial mappings, for example:
///   - `PageFault` during a kernel copy-from-user -> `HandleKernelCopyFault`
///     instead of suspending the current user task.
///   - `ExternalInterrupt` on a known timer line on secondary CPUs ->
///     `TickScheduler` rather than generic IRQ routing.
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
        assert_eq!(route_trap(&TrapEvent::Syscall), TrapAction::DispatchSyscall);
    }

    #[test]
    fn trap_event_can_carry_irq_number() {
        let event = TrapEvent::ExternalInterrupt(7);
        assert_eq!(event.irq(), Some(7));
        assert_eq!(event.fault(), None);
    }

    #[test]
    fn router_covers_all_traps() {
        assert_eq!(route_trap(&TrapEvent::Syscall), TrapAction::DispatchSyscall);
        assert_eq!(
            route_trap(&TrapEvent::PageFault(FaultInfo {
                addr: VirtAddr(0x1000),
                access: FaultAccess::Read,
            })),
            TrapAction::HandlePageFault
        );
        assert_eq!(
            route_trap(&TrapEvent::TimerInterrupt),
            TrapAction::TickScheduler
        );
        assert_eq!(
            route_trap(&TrapEvent::ExternalInterrupt(1)),
            TrapAction::RouteIrq
        );
    }
}
