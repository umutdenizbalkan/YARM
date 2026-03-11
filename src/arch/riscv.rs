use crate::kernel::bootstrap::{KernelState, TrapHandleError};
use crate::kernel::scheduler::CpuId;
use crate::kernel::trap::{FaultAccess, FaultInfo, Trap, TrapEvent};
use crate::kernel::trapframe::TrapFrame;

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
            IRQ_SUPERVISOR_EXTERNAL => TrapEvent::with_irq(Trap::ExternalInterrupt, stval as u16),
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

pub fn handle_trap_from_scause(
    kernel: &mut KernelState,
    scause: usize,
    stval: usize,
    frame: Option<&mut TrapFrame>,
) -> Result<(), TrapHandleError> {
    let event = decode_trap(scause, stval);
    kernel.handle_trap_event(event, frame)
}

pub fn handle_trap_entry(
    kernel: &mut KernelState,
    cpu: CpuId,
    scause: usize,
    stval: usize,
    frame: Option<&mut TrapFrame>,
) -> Result<(), TrapHandleError> {
    let _ = kernel.set_current_cpu(cpu);
    let _ = kernel.process_cross_cpu_work_for_cpu(cpu);
    handle_trap_from_scause(kernel, scause, stval, frame)
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
    fn decode_external_irq_carries_irq_line() {
        let event = decode_trap(INTERRUPT_BIT | IRQ_SUPERVISOR_EXTERNAL, 11);
        assert_eq!(event.trap, Trap::ExternalInterrupt);
        assert_eq!(event.irq, Some(11));
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

    #[test]
    fn handle_timer_trap_from_scause_routes_to_kernel() {
        use crate::kernel::bootstrap::Bootstrap;

        let mut state = Bootstrap::init().expect("init");
        state.timer = crate::kernel::timer::Timer::new(1);
        state.register_task(1).expect("task1");
        state.scheduler.enqueue(1).expect("enqueue");

        handle_trap_from_scause(&mut state, INTERRUPT_BIT | IRQ_SUPERVISOR_TIMER, 0, None)
            .expect("timer trap");
        assert_eq!(state.scheduler.current_tid(), Some(1));
    }

    #[test]
    fn trap_entry_sets_cpu_and_processes_cpu_work() {
        use crate::kernel::bootstrap::Bootstrap;
        use crate::kernel::smp::WorkItem;
        use crate::kernel::vm::Asid;

        let mut state = Bootstrap::init().expect("init");
        state.bring_up_cpu(CpuId(1)).expect("cpu1");
        state
            .submit_cross_cpu_work(WorkItem::TlbShootdown {
                target_cpu: CpuId(1),
                asid: Asid(1),
            })
            .expect("submit");

        handle_trap_entry(
            &mut state,
            CpuId(1),
            INTERRUPT_BIT | IRQ_SUPERVISOR_TIMER,
            0,
            None,
        )
        .expect("handle");
        assert_eq!(state.scheduler.current_cpu(), CpuId(1));
        assert_eq!(state.tlb_shootdown_count(), 1);
    }
}
