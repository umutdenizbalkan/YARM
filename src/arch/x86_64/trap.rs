use crate::kernel::bootstrap::{KernelState, TrapHandleError};
use crate::kernel::scheduler::CpuId;
use crate::kernel::trap::{FaultAccess, FaultInfo, TrapEvent};
use crate::kernel::trapframe::TrapFrame;
use crate::kernel::vm::VirtAddr;

const VEC_SYSCALL: u8 = 0x80;
const VEC_TIMER: u8 = 0x20;
const VEC_EXTERNAL_BASE: u8 = 0x20;
const VEC_EXTERNAL_LIMIT: u8 = 0x30;
const VEC_PAGE_FAULT: u8 = 14;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct X86TrapContext {
    pub vector: u8,
    pub error_code: u64,
    pub fault_addr: u64,
}

pub fn decode_trap_context(context: X86TrapContext) -> TrapEvent {
    match context.vector {
        VEC_SYSCALL => TrapEvent::syscall(),
        VEC_TIMER => TrapEvent::timer_interrupt(),
        VEC_PAGE_FAULT => {
            let access = if (context.error_code & (1 << 1)) != 0 {
                FaultAccess::Write
            } else if (context.error_code & (1 << 4)) != 0 {
                FaultAccess::Execute
            } else {
                FaultAccess::Read
            };
            TrapEvent::page_fault(FaultInfo {
                addr: VirtAddr(context.fault_addr),
                access,
            })
        }
        v if (VEC_EXTERNAL_BASE..VEC_EXTERNAL_LIMIT).contains(&v) => {
            TrapEvent::external_interrupt((v - VEC_EXTERNAL_BASE) as u16)
        }
        _ => TrapEvent::external_interrupt(0),
    }
}

pub fn handle_trap_entry(
    kernel: &mut KernelState,
    cpu: CpuId,
    context: X86TrapContext,
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
    fn decode_syscall_vector() {
        let ev = decode_trap_context(X86TrapContext {
            vector: VEC_SYSCALL,
            error_code: 0,
            fault_addr: 0,
        });
        assert_eq!(ev.trap, Trap::Syscall);
    }

    #[test]
    fn decode_timer_vector() {
        let ev = decode_trap_context(X86TrapContext {
            vector: VEC_TIMER,
            error_code: 0,
            fault_addr: 0,
        });
        assert_eq!(ev.trap, Trap::TimerInterrupt);
    }

    #[test]
    fn decode_external_vector_maps_irq_line() {
        let ev = decode_trap_context(X86TrapContext {
            vector: VEC_EXTERNAL_BASE + 7,
            error_code: 0,
            fault_addr: 0,
        });
        assert_eq!(ev.trap, Trap::ExternalInterrupt);
        assert_eq!(ev.irq, Some(7));
    }

    #[test]
    fn decode_page_fault_uses_cr2_and_access_bits() {
        let ev = decode_trap_context(X86TrapContext {
            vector: VEC_PAGE_FAULT,
            error_code: 0b10,
            fault_addr: 0xFACE_1000,
        });
        assert_eq!(ev.trap, Trap::PageFault);
        assert_eq!(
            ev.fault,
            Some(FaultInfo {
                addr: VirtAddr(0xFACE_1000),
                access: FaultAccess::Write,
            })
        );
    }

    #[test]
    fn trap_entry_sets_cpu_and_handles_timer() {
        use crate::kernel::bootstrap::Bootstrap;

        let mut state = Bootstrap::init().expect("init");
        state.bring_up_cpu(CpuId(1)).expect("cpu1");

        handle_trap_entry(
            &mut state,
            CpuId(1),
            X86TrapContext {
                vector: VEC_TIMER,
                error_code: 0,
                fault_addr: 0,
            },
            None,
        )
        .expect("timer");
        assert_eq!(state.scheduler.current_cpu(), CpuId(1));
    }
}
