// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use crate::arch::trap::{FaultAccess, FaultInfo, TrapEvent};
use crate::kernel::boot::{KernelState, TrapHandleError};
use crate::kernel::scheduler::CpuId;
#[cfg(test)]
use crate::kernel::scheduler::MAX_CPUS;
use crate::kernel::trapframe::TrapFrame;
use crate::kernel::vm::VirtAddr;

const ESR_EC_SVC64: u32 = 0x15;
const ESR_EC_IABT_LOW: u32 = 0x20;
const ESR_EC_IABT_CUR: u32 = 0x21;
const ESR_EC_DABT_LOW: u32 = 0x24;
const ESR_EC_DABT_CUR: u32 = 0x25;
const ESR_EC_MASK: u32 = 0x3F;
const ARCH_TIMER_PPI_IRQ: u16 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Aarch64TrapContext {
    pub esr_el1: u32,
    pub far_el1: u64,
    pub irq_line: Option<u16>,
    pub is_timer_irq: bool,
}

#[cfg(test)]
use core::sync::atomic::{AtomicUsize, Ordering};

#[cfg(test)]
static LAST_RESTORED_TLS_BASE: [AtomicUsize; MAX_CPUS] = [const { AtomicUsize::new(0) }; MAX_CPUS];

#[cfg(test)]
pub fn last_restored_tls_base(cpu: CpuId) -> Option<usize> {
    let idx = cpu.0 as usize;
    if idx >= MAX_CPUS {
        return None;
    }
    let value = LAST_RESTORED_TLS_BASE[idx].load(Ordering::Relaxed);
    (value != 0).then_some(value)
}

fn restore_arch_thread_state(
    kernel: &mut KernelState,
    cpu: CpuId,
    frame: Option<&mut TrapFrame>,
) -> Result<(), TrapHandleError> {
    let Some(frame) = frame else {
        return Ok(());
    };
    let tls = kernel
        .resume_current_thread_with_frame(frame)
        .map_err(crate::kernel::syscall::SyscallError::from)
        .map_err(TrapHandleError::Syscall)?;
    frame.set_user_gpr(
        crate::arch::aarch64::syscall_abi::REG_X18_TLS,
        tls.unwrap_or(0),
    );
    #[cfg(test)]
    {
        let idx = cpu.0 as usize;
        if idx < MAX_CPUS {
            LAST_RESTORED_TLS_BASE[idx].store(tls.unwrap_or(0), Ordering::Relaxed);
        }
    }
    #[cfg(not(test))]
    let _ = (cpu, tls);
    Ok(())
}

fn import_syscall_abi_from_user_gprs(frame: &mut TrapFrame) {
    frame.set_syscall_num(frame.user_gpr(crate::arch::aarch64::syscall_abi::REG_X8));
    frame.set_arg(0, frame.user_gpr(crate::arch::aarch64::syscall_abi::REG_X0));
    frame.set_arg(1, frame.user_gpr(crate::arch::aarch64::syscall_abi::REG_X1));
    frame.set_arg(2, frame.user_gpr(crate::arch::aarch64::syscall_abi::REG_X2));
    frame.set_arg(3, frame.user_gpr(crate::arch::aarch64::syscall_abi::REG_X3));
    frame.set_arg(4, frame.user_gpr(crate::arch::aarch64::syscall_abi::REG_X4));
    frame.set_arg(5, frame.user_gpr(crate::arch::aarch64::syscall_abi::REG_X5));
}

fn export_syscall_result_to_user_gprs(frame: &mut TrapFrame) {
    if let Some(error) = frame.error_code() {
        frame.set_user_gpr(crate::arch::aarch64::syscall_abi::REG_X0, error);
        frame.set_user_gpr(crate::arch::aarch64::syscall_abi::REG_X1, 0);
        frame.set_user_gpr(crate::arch::aarch64::syscall_abi::REG_X2, 0);
    } else {
        frame.set_user_gpr(crate::arch::aarch64::syscall_abi::REG_X0, frame.ret0());
        frame.set_user_gpr(crate::arch::aarch64::syscall_abi::REG_X1, frame.ret1());
        frame.set_user_gpr(crate::arch::aarch64::syscall_abi::REG_X2, frame.ret2());
    }
}

pub fn decode_trap_context(context: Aarch64TrapContext) -> TrapEvent {
    if context.is_timer_irq {
        return TrapEvent::TimerInterrupt;
    }
    if context.irq_line == Some(ARCH_TIMER_PPI_IRQ) {
        return TrapEvent::TimerInterrupt;
    }
    if let Some(irq) = context.irq_line {
        return TrapEvent::ExternalInterrupt(irq);
    }

    match (context.esr_el1 >> 26) & ESR_EC_MASK {
        ESR_EC_SVC64 => TrapEvent::Syscall,
        ESR_EC_IABT_LOW | ESR_EC_IABT_CUR => TrapEvent::PageFault(FaultInfo {
            addr: VirtAddr(context.far_el1),
            access: FaultAccess::Execute,
        }),
        ESR_EC_DABT_LOW | ESR_EC_DABT_CUR => {
            let is_write = ((context.esr_el1 >> 6) & 1) != 0;
            TrapEvent::PageFault(FaultInfo {
                addr: VirtAddr(context.far_el1),
                access: if is_write {
                    FaultAccess::Write
                } else {
                    FaultAccess::Read
                },
            })
        }
        _ => TrapEvent::Unknown {
            arch_code: context.esr_el1 as u64,
        },
    }
}

pub fn handle_trap_entry(
    kernel: &mut KernelState,
    cpu: CpuId,
    context: Aarch64TrapContext,
    mut frame: Option<&mut TrapFrame>,
) -> Result<(), TrapHandleError> {
    let event = decode_trap_context(context);
    let entering_tid = kernel.current_tid();
    let original_saved_pc = frame.as_ref().map(|f| f.saved_pc());
    if matches!(event, TrapEvent::Syscall) {
        if let Some(trapframe) = frame.as_deref_mut() {
            import_syscall_abi_from_user_gprs(trapframe);
        }
    }
    let _ = kernel.set_current_cpu(cpu);
    let _ = kernel.process_cross_cpu_work_for_cpu(cpu);
    kernel.handle_trap_event(event, frame.as_deref_mut())?;
    if matches!(event, TrapEvent::Syscall) {
        if let Some(trapframe) = frame.as_deref_mut() {
            export_syscall_result_to_user_gprs(trapframe);
        }
    }
    restore_arch_thread_state(kernel, cpu, frame.as_deref_mut())?;
    if matches!(event, TrapEvent::Syscall) {
        let exiting_tid = kernel.current_tid();
        if entering_tid == exiting_tid
            && let (Some(saved_pc), Some(trapframe)) = (original_saved_pc, frame.as_deref_mut())
        {
            trapframe.set_saved_pc(saved_pc);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arch::trap::Trap;

    #[test]
    fn decode_svc64_syscall() {
        let ev = decode_trap_context(Aarch64TrapContext {
            esr_el1: ESR_EC_SVC64 << 26,
            far_el1: 0,
            irq_line: None,
            is_timer_irq: false,
        });
        assert_eq!(ev.trap(), Trap::Syscall);
    }

    #[test]
    fn decode_timer_irq() {
        let ev = decode_trap_context(Aarch64TrapContext {
            esr_el1: 0,
            far_el1: 0,
            irq_line: None,
            is_timer_irq: true,
        });
        assert_eq!(ev.trap(), Trap::TimerInterrupt);
    }

    #[test]
    fn decode_arch_timer_ppi_irq_as_timer() {
        let ev = decode_trap_context(Aarch64TrapContext {
            esr_el1: 0,
            far_el1: 0,
            irq_line: Some(30),
            is_timer_irq: false,
        });
        assert_eq!(ev.trap(), Trap::TimerInterrupt);
    }

    #[test]
    fn decode_external_irq() {
        let ev = decode_trap_context(Aarch64TrapContext {
            esr_el1: 0,
            far_el1: 0,
            irq_line: Some(44),
            is_timer_irq: false,
        });
        assert_eq!(ev.trap(), Trap::ExternalInterrupt);
        assert_eq!(ev.irq(), Some(44));
    }

    #[test]
    fn syscall_abi_imports_x_register_arguments() {
        let mut frame = TrapFrame::new(0, [0; 6]);
        frame.set_user_gpr(crate::arch::aarch64::syscall_abi::REG_X8, 42);
        frame.set_user_gpr(crate::arch::aarch64::syscall_abi::REG_X0, 10);
        frame.set_user_gpr(crate::arch::aarch64::syscall_abi::REG_X1, 11);
        frame.set_user_gpr(crate::arch::aarch64::syscall_abi::REG_X2, 12);
        frame.set_user_gpr(crate::arch::aarch64::syscall_abi::REG_X3, 13);
        frame.set_user_gpr(crate::arch::aarch64::syscall_abi::REG_X4, 14);
        frame.set_user_gpr(crate::arch::aarch64::syscall_abi::REG_X5, 15);

        import_syscall_abi_from_user_gprs(&mut frame);

        assert_eq!(frame.syscall_num(), 42);
        assert_eq!(frame.arg(0), 10);
        assert_eq!(frame.arg(1), 11);
        assert_eq!(frame.arg(2), 12);
        assert_eq!(frame.arg(3), 13);
        assert_eq!(frame.arg(4), 14);
        assert_eq!(frame.arg(5), 15);
    }

    #[test]
    fn syscall_abi_exports_return_registers() {
        let mut frame = TrapFrame::new(0, [0; 6]);
        frame.set_ok(7, 8, 9);
        export_syscall_result_to_user_gprs(&mut frame);
        assert_eq!(frame.user_gpr(crate::arch::aarch64::syscall_abi::REG_X0), 7);
        assert_eq!(frame.user_gpr(crate::arch::aarch64::syscall_abi::REG_X1), 8);
        assert_eq!(frame.user_gpr(crate::arch::aarch64::syscall_abi::REG_X2), 9);

        frame.set_err(5);
        export_syscall_result_to_user_gprs(&mut frame);
        assert_eq!(frame.user_gpr(crate::arch::aarch64::syscall_abi::REG_X0), 5);
        assert_eq!(frame.user_gpr(crate::arch::aarch64::syscall_abi::REG_X1), 0);
        assert_eq!(frame.user_gpr(crate::arch::aarch64::syscall_abi::REG_X2), 0);
    }

    #[test]
    fn decode_data_abort_write_fault() {
        let ev = decode_trap_context(Aarch64TrapContext {
            esr_el1: (ESR_EC_DABT_LOW << 26) | (1 << 6),
            far_el1: 0xABCD_4000,
            irq_line: None,
            is_timer_irq: false,
        });
        assert_eq!(ev.trap(), Trap::PageFault);
        assert_eq!(
            ev.fault(),
            Some(FaultInfo {
                addr: VirtAddr(0xABCD_4000),
                access: FaultAccess::Write,
            })
        );
    }

    #[test]
    fn decode_data_abort_current_el_is_page_fault() {
        let ev = decode_trap_context(Aarch64TrapContext {
            esr_el1: ESR_EC_DABT_CUR << 26,
            far_el1: 0x6000,
            irq_line: None,
            is_timer_irq: false,
        });
        assert_eq!(
            ev,
            TrapEvent::PageFault(FaultInfo {
                addr: VirtAddr(0x6000),
                access: FaultAccess::Read,
            })
        );
    }

    #[test]
    fn decode_unknown_exception_class_is_unknown_event() {
        let ev = decode_trap_context(Aarch64TrapContext {
            esr_el1: 0x3F << 26,
            far_el1: 0,
            irq_line: None,
            is_timer_irq: false,
        });
        assert_eq!(ev.trap(), Trap::Unknown);
    }

    #[test]
    fn trap_entry_sets_cpu_and_handles_irq() {
        use crate::kernel::boot::Bootstrap;

        let mut state = crate::std::boxed::Box::new(Bootstrap::init().expect("init"));
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

        assert_eq!(state.current_cpu(), CpuId(2));
    }

    #[test]
    fn trap_entry_restores_tls_for_resumed_thread() {
        use crate::kernel::boot::{Bootstrap, UserImageSpec};
        use crate::kernel::task::TaskClass;

        let mut state = crate::std::boxed::Box::new(Bootstrap::init().expect("init"));
        let (asid, _aspace_cap) = state.create_user_address_space().expect("asid");
        state
            .spawn_user_task_from_image(UserImageSpec {
                tid: 50,
                entry: 0x4000,
                asid: Some(asid),
                class: TaskClass::App,
                startup_args: UserImageSpec::DEFAULT_STARTUP_ARGS,
            })
            .expect("leader");
        let tid = state
            .spawn_user_thread(50, 0xCAFE_0000, 0x8100_0000, 0x4010)
            .expect("thread");
        state.yield_current().expect("switch");
        assert_eq!(state.current_tid(), Some(tid));

        let mut frame = TrapFrame::new(0, [0; 6]);
        handle_trap_entry(
            &mut state,
            CpuId(2),
            Aarch64TrapContext {
                esr_el1: 0,
                far_el1: 0,
                irq_line: None,
                is_timer_irq: true,
            },
            Some(&mut frame),
        )
        .expect("trap");
        assert_eq!(last_restored_tls_base(CpuId(2)), Some(0xCAFE_0000));
        assert_eq!(
            frame.user_gpr(crate::arch::aarch64::syscall_abi::REG_X18_TLS),
            0xCAFE_0000
        );
    }

    #[test]
    fn tls_restore_slots_are_isolated_per_cpu() {
        use crate::kernel::boot::{Bootstrap, UserImageSpec};
        use crate::kernel::task::TaskClass;

        let mut state = crate::std::boxed::Box::new(Bootstrap::init().expect("init"));
        state.bring_up_cpu(CpuId(1)).expect("cpu1");
        state.bring_up_cpu(CpuId(2)).expect("cpu2");
        let (asid, _aspace_cap) = state.create_user_address_space().expect("asid");
        state
            .spawn_user_task_from_image(UserImageSpec {
                tid: 60,
                entry: 0x4000,
                asid: Some(asid),
                class: TaskClass::App,
                startup_args: UserImageSpec::DEFAULT_STARTUP_ARGS,
            })
            .expect("leader");
        let tid_a = state
            .spawn_user_thread(60, 0xAAA0_0000, 0x8200_0000, 0x4010)
            .expect("thread a");
        state.set_current_cpu(CpuId(1)).expect("switch cpu1");
        let _ = state.dispatch_next_task().expect("dispatch a");
        assert_eq!(state.current_tid(), Some(tid_a));
        let mut frame_a = TrapFrame::new(0, [0; 6]);
        handle_trap_entry(
            &mut state,
            CpuId(1),
            Aarch64TrapContext {
                esr_el1: 0,
                far_el1: 0,
                irq_line: None,
                is_timer_irq: true,
            },
            Some(&mut frame_a),
        )
        .expect("trap a");

        state
            .set_thread_tls_base(0, 0xBBB0_0000)
            .expect("set tls boot");
        state.set_current_cpu(CpuId(0)).expect("switch cpu0");
        let mut frame_b = TrapFrame::new(0, [0; 6]);
        handle_trap_entry(
            &mut state,
            CpuId(0),
            Aarch64TrapContext {
                esr_el1: 0,
                far_el1: 0,
                irq_line: None,
                is_timer_irq: true,
            },
            Some(&mut frame_b),
        )
        .expect("trap b");

        assert_eq!(last_restored_tls_base(CpuId(1)), Some(0xAAA0_0000));
        assert_eq!(last_restored_tls_base(CpuId(0)), Some(0xBBB0_0000));
    }
}
