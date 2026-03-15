use crate::kernel::bootstrap::{KernelState, TrapHandleError};
use crate::kernel::scheduler::CpuId;
use crate::kernel::trap::{FaultAccess, FaultInfo, TrapEvent};
use crate::kernel::trapframe::TrapFrame;
use crate::kernel::vm::VirtAddr;

const INTERRUPT_BIT: usize = 1usize << (usize::BITS as usize - 1);
const SCAUSE_EXCEPTION_MASK: usize = !INTERRUPT_BIT;

const EXC_USER_ECALL: usize = 8;
const EXC_LOAD_PAGE_FAULT: usize = 13;
const EXC_STORE_PAGE_FAULT: usize = 15;

const IRQ_SUPERVISOR_TIMER: usize = 5;
const IRQ_SUPERVISOR_EXTERNAL: usize = 9;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Riscv64TrapContext {
    pub scause: usize,
    pub stval: usize,
}

pub fn decode_trap_context(context: Riscv64TrapContext) -> TrapEvent {
    let is_interrupt = (context.scause & INTERRUPT_BIT) != 0;
    let code = context.scause & SCAUSE_EXCEPTION_MASK;

    if is_interrupt {
        return match code {
            IRQ_SUPERVISOR_TIMER => TrapEvent::timer_interrupt(),
            IRQ_SUPERVISOR_EXTERNAL => TrapEvent::external_interrupt(context.stval as u16),
            _ => TrapEvent::external_interrupt(0),
        };
    }

    match code {
        EXC_USER_ECALL => TrapEvent::syscall(),
        EXC_LOAD_PAGE_FAULT => TrapEvent::page_fault(FaultInfo {
            addr: VirtAddr(context.stval as u64),
            access: FaultAccess::Read,
        }),
        EXC_STORE_PAGE_FAULT => TrapEvent::page_fault(FaultInfo {
            addr: VirtAddr(context.stval as u64),
            access: FaultAccess::Write,
        }),
        _ => TrapEvent::external_interrupt(0),
    }
}

pub fn handle_trap_entry(
    kernel: &mut KernelState,
    cpu: CpuId,
    context: Riscv64TrapContext,
    frame: Option<&mut TrapFrame>,
) -> Result<(), TrapHandleError> {
    let _ = kernel.set_current_cpu(cpu);
    let _ = kernel.process_cross_cpu_work_for_cpu(cpu);
    kernel.handle_trap_event(decode_trap_context(context), frame)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::trap::Trap;

    #[test]
    fn decode_user_ecall_to_syscall() {
        let event = decode_trap_context(Riscv64TrapContext {
            scause: EXC_USER_ECALL,
            stval: 0,
        });
        assert_eq!(event.trap, Trap::Syscall);
    }

    #[test]
    fn trap_entry_sets_cpu_and_handles_timer() {
        use crate::kernel::bootstrap::Bootstrap;

        let mut state = Bootstrap::init().expect("init");
        state.bring_up_cpu(CpuId(1)).expect("cpu1");

        handle_trap_entry(
            &mut state,
            CpuId(1),
            Riscv64TrapContext {
                scause: INTERRUPT_BIT | IRQ_SUPERVISOR_TIMER,
                stval: 0,
            },
            None,
        )
        .expect("timer");

        assert_eq!(state.scheduler.current_cpu(), CpuId(1));
    }
}
