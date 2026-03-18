use crate::kernel::bootstrap::{KernelState, TrapHandleError};
use crate::kernel::scheduler::CpuId;
use crate::kernel::trap::{FaultAccess, FaultInfo, TrapEvent};
use crate::kernel::trapframe::TrapFrame;
use crate::kernel::vm::VirtAddr;

const ESR_EC_SVC64: u32 = 0x15;
const ESR_EC_IABT_LOW: u32 = 0x20;
const ESR_EC_DABT_LOW: u32 = 0x24;
const ESR_EC_MASK: u32 = 0x3F;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Aarch64TrapContext {
    pub esr_el1: u32,
    pub far_el1: u64,
    pub irq_line: Option<u16>,
    pub is_timer_irq: bool,
}

pub fn decode_trap_context(context: Aarch64TrapContext) -> TrapEvent {
    if context.is_timer_irq {
        return TrapEvent::timer_interrupt();
    }
    if let Some(irq) = context.irq_line {
        return TrapEvent::external_interrupt(irq);
    }

    match (context.esr_el1 >> 26) & ESR_EC_MASK {
        ESR_EC_SVC64 => TrapEvent::syscall(),
        ESR_EC_IABT_LOW => TrapEvent::page_fault(FaultInfo {
            addr: VirtAddr(context.far_el1),
            access: FaultAccess::Execute,
        }),
        ESR_EC_DABT_LOW => {
            let is_write = ((context.esr_el1 >> 6) & 1) != 0;
            TrapEvent::page_fault(FaultInfo {
                addr: VirtAddr(context.far_el1),
                access: if is_write {
                    FaultAccess::Write
                } else {
                    FaultAccess::Read
                },
            })
        }
        _ => TrapEvent::external_interrupt(0),
    }
}

pub fn handle_trap_entry(
    kernel: &mut KernelState,
    cpu: CpuId,
    context: Aarch64TrapContext,
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
    fn decode_svc64_syscall() {
        let ev = decode_trap_context(Aarch64TrapContext {
            esr_el1: ESR_EC_SVC64 << 26,
            far_el1: 0,
            irq_line: None,
            is_timer_irq: false,
        });
        assert_eq!(ev.trap, Trap::Syscall);
    }

    #[test]
    fn decode_timer_irq() {
        let ev = decode_trap_context(Aarch64TrapContext {
            esr_el1: 0,
            far_el1: 0,
            irq_line: None,
            is_timer_irq: true,
        });
        assert_eq!(ev.trap, Trap::TimerInterrupt);
    }

    #[test]
    fn decode_external_irq() {
        let ev = decode_trap_context(Aarch64TrapContext {
            esr_el1: 0,
            far_el1: 0,
            irq_line: Some(44),
            is_timer_irq: false,
        });
        assert_eq!(ev.trap, Trap::ExternalInterrupt);
        assert_eq!(ev.irq, Some(44));
    }

    #[test]
    fn decode_data_abort_write_fault() {
        let ev = decode_trap_context(Aarch64TrapContext {
            esr_el1: (ESR_EC_DABT_LOW << 26) | (1 << 6),
            far_el1: 0xABCD_4000,
            irq_line: None,
            is_timer_irq: false,
        });
        assert_eq!(ev.trap, Trap::PageFault);
        assert_eq!(
            ev.fault,
            Some(FaultInfo {
                addr: VirtAddr(0xABCD_4000),
                access: FaultAccess::Write,
            })
        );
    }

    #[test]
    fn trap_entry_sets_cpu_and_handles_irq() {
        use crate::kernel::bootstrap::Bootstrap;

        let mut state = Bootstrap::init().expect("init");
        state.bring_up_cpu(CpuId(2)).expect("cpu2");

        handle_trap_entry(
            &mut state,
            CpuId(2),
            Aarch64TrapContext {
                esr_el1: 0,
                far_el1: 0,
                irq_line: Some(11),
                is_timer_irq: false,
            },
            None,
        )
        .expect("irq");

        assert_eq!(state.scheduler.current_cpu(), CpuId(2));
    }
}
