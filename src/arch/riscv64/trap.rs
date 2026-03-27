use crate::kernel::boot::{KernelState, TrapHandleError};
use crate::kernel::scheduler::{CpuId, MAX_CPUS};
use crate::kernel::trap::{FaultAccess, FaultInfo, TrapEvent};
use crate::kernel::trapframe::TrapFrame;
use crate::kernel::vm::VirtAddr;
use core::sync::atomic::{AtomicUsize, Ordering};

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

static LAST_RESTORED_TLS_BASE: [AtomicUsize; MAX_CPUS] = [const { AtomicUsize::new(0) }; MAX_CPUS];

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
    let idx = cpu.0 as usize;
    if idx < MAX_CPUS {
        LAST_RESTORED_TLS_BASE[idx].store(tls.unwrap_or(0), Ordering::Relaxed);
    }
    Ok(())
}

pub fn decode_trap_context(context: Riscv64TrapContext) -> TrapEvent {
    let is_interrupt = (context.scause & INTERRUPT_BIT) != 0;
    let code = context.scause & SCAUSE_EXCEPTION_MASK;

    if is_interrupt {
        return match code {
            IRQ_SUPERVISOR_TIMER => TrapEvent::TimerInterrupt,
            IRQ_SUPERVISOR_EXTERNAL => TrapEvent::ExternalInterrupt(context.stval as u16),
            _ => TrapEvent::Unknown {
                arch_code: context.scause as u64,
            },
        };
    }

    match code {
        EXC_USER_ECALL => TrapEvent::Syscall,
        EXC_LOAD_PAGE_FAULT => TrapEvent::PageFault(FaultInfo {
            addr: VirtAddr(context.stval as u64),
            access: FaultAccess::Read,
        }),
        EXC_STORE_PAGE_FAULT => TrapEvent::PageFault(FaultInfo {
            addr: VirtAddr(context.stval as u64),
            access: FaultAccess::Write,
        }),
        _ => TrapEvent::Unknown {
            arch_code: context.scause as u64,
        },
    }
}

pub fn handle_trap_entry(
    kernel: &mut KernelState,
    cpu: CpuId,
    context: Riscv64TrapContext,
    mut frame: Option<&mut TrapFrame>,
) -> Result<(), TrapHandleError> {
    let _ = kernel.set_current_cpu(cpu);
    let _ = kernel.process_cross_cpu_work_for_cpu(cpu);
    kernel.handle_trap_event(decode_trap_context(context), frame.as_deref_mut())?;
    restore_arch_thread_state(kernel, cpu, frame)?;
    Ok(())
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
        assert_eq!(event.trap(), Trap::Syscall);
    }

    #[test]
    fn trap_entry_sets_cpu_and_handles_timer() {
        use crate::kernel::boot::Bootstrap;

        let mut state = crate::std::boxed::Box::new(Bootstrap::init().expect("init"));
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

        assert_eq!(state.current_cpu(), CpuId(1));
    }

    #[test]
    fn decode_unknown_scause_maps_to_unknown_event() {
        let event = decode_trap_context(Riscv64TrapContext {
            scause: INTERRUPT_BIT | 0x3f,
            stval: 0,
        });
        assert_eq!(event.trap(), Trap::Unknown);
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
            CpuId(1),
            Riscv64TrapContext {
                scause: INTERRUPT_BIT | IRQ_SUPERVISOR_TIMER,
                stval: 0,
            },
            Some(&mut frame),
        )
        .expect("trap");
        assert_eq!(last_restored_tls_base(CpuId(1)), Some(0xCAFE_0000));
    }

    #[test]
    fn tls_restore_slots_are_isolated_per_cpu() {
        use crate::kernel::boot::{Bootstrap, UserImageSpec};
        use crate::kernel::task::TaskClass;

        let mut state = crate::std::boxed::Box::new(Bootstrap::init().expect("init"));
        state.bring_up_cpu(CpuId(1)).expect("cpu1");
        let (asid, _aspace_cap) = state.create_user_address_space().expect("asid");
        state
            .spawn_user_task_from_image(UserImageSpec {
                tid: 60,
                entry: 0x4000,
                asid: Some(asid),
                class: TaskClass::App,
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
            Riscv64TrapContext {
                scause: INTERRUPT_BIT | IRQ_SUPERVISOR_TIMER,
                stval: 0,
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
            Riscv64TrapContext {
                scause: INTERRUPT_BIT | IRQ_SUPERVISOR_TIMER,
                stval: 0,
            },
            Some(&mut frame_b),
        )
        .expect("trap b");

        assert_eq!(last_restored_tls_base(CpuId(1)), Some(0xAAA0_0000));
        assert_eq!(last_restored_tls_base(CpuId(0)), Some(0xBBB0_0000));
    }
}
