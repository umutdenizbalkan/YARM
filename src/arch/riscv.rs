use crate::kernel::trap::{FaultAccess, FaultInfo, Trap, TrapEvent};

const INTERRUPT_BIT: usize = 1usize << (usize::BITS as usize - 1);

const SCAUSE_EXCEPTION_MASK: usize = !INTERRUPT_BIT;

const EXC_USER_ECALL: usize = 8;
const EXC_LOAD_PAGE_FAULT: usize = 13;
const EXC_STORE_PAGE_FAULT: usize = 15;

const IRQ_SUPERVISOR_TIMER: usize = 5;
const IRQ_SUPERVISOR_EXTERNAL: usize = 9;

pub fn decode_trap(scause: usize, stval: usize) -> TrapEvent {
    let is_interrupt = (scause & INTERRUPT_BIT) != 0;
    let code = scause & SCAUSE_EXCEPTION_MASK;

    if is_interrupt {
        return match code {
            IRQ_SUPERVISOR_TIMER => TrapEvent::new(Trap::TimerInterrupt),
            IRQ_SUPERVISOR_EXTERNAL => TrapEvent::new(Trap::ExternalInterrupt),
            _ => TrapEvent::new(Trap::ExternalInterrupt),
        };
    }

    match code {
        EXC_USER_ECALL => TrapEvent::new(Trap::Syscall),
        EXC_LOAD_PAGE_FAULT => TrapEvent::with_fault(
            Trap::PageFault,
            FaultInfo {
                addr: stval,
                access: FaultAccess::Read,
            },
        ),
        EXC_STORE_PAGE_FAULT => TrapEvent::with_fault(
            Trap::PageFault,
            FaultInfo {
                addr: stval,
                access: FaultAccess::Write,
            },
        ),
        _ => TrapEvent::new(Trap::ExternalInterrupt),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_user_ecall_to_syscall() {
        let event = decode_trap(EXC_USER_ECALL, 0);
        assert_eq!(event.trap, Trap::Syscall);
        assert_eq!(event.fault, None);
    }

    #[test]
    fn decode_timer_irq_to_timer_interrupt() {
        let event = decode_trap(INTERRUPT_BIT | IRQ_SUPERVISOR_TIMER, 0);
        assert_eq!(event.trap, Trap::TimerInterrupt);
    }

    #[test]
    fn decode_page_fault_carries_fault_info() {
        let event = decode_trap(EXC_STORE_PAGE_FAULT, 0xDEAD_BEEF);
        assert_eq!(event.trap, Trap::PageFault);
        assert_eq!(
            event.fault,
            Some(FaultInfo {
                addr: 0xDEAD_BEEF,
                access: FaultAccess::Write,
            })
        );
    }
}
