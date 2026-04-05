// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use crate::arch::trap::{FaultAccess, FaultInfo, TrapEvent};
use crate::kernel::boot::{KernelState, TrapHandleError};
use crate::kernel::scheduler::{CpuId, MAX_CPUS};
use crate::kernel::trapframe::TrapFrame;
use crate::kernel::vm::VirtAddr;
use core::sync::atomic::{AtomicUsize, Ordering};

const VEC_SYSCALL: u8 = 0x80;
const VEC_TIMER: u8 = 0x20;
const VEC_EXTERNAL_BASE: u8 = 0x20;
const VEC_EXTERNAL_LIMIT: u8 = 0x30;
const VEC_PAGE_FAULT: u8 = 14;
#[cfg(not(feature = "hosted-dev"))]
const MSR_FS_BASE: u32 = 0xC000_0100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct X86TrapContext {
    pub vector: u8,
    pub error_code: u64,
    pub fault_addr: u64,
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
    restore_fs_base_if_needed(tls.unwrap_or(0));
    let idx = cpu.0 as usize;
    if idx < MAX_CPUS {
        LAST_RESTORED_TLS_BASE[idx].store(tls.unwrap_or(0), Ordering::Relaxed);
    }
    Ok(())
}

#[cfg(not(feature = "hosted-dev"))]
fn restore_fs_base_if_needed(target: usize) {
    let current = read_msr(MSR_FS_BASE);
    let target = target as u64;
    if current != target {
        write_msr(MSR_FS_BASE, target);
    }
}

#[cfg(feature = "hosted-dev")]
fn restore_fs_base_if_needed(_target: usize) {}

#[cfg(not(feature = "hosted-dev"))]
fn read_msr(msr: u32) -> u64 {
    let low: u32;
    let high: u32;
    unsafe {
        core::arch::asm!(
            "rdmsr",
            in("ecx") msr,
            out("eax") low,
            out("edx") high,
            options(nomem, nostack)
        );
    }
    ((high as u64) << 32) | (low as u64)
}

#[cfg(not(feature = "hosted-dev"))]
fn write_msr(msr: u32, value: u64) {
    let low = value as u32;
    let high = (value >> 32) as u32;
    unsafe {
        core::arch::asm!(
            "wrmsr",
            in("ecx") msr,
            in("eax") low,
            in("edx") high,
            options(nomem, nostack)
        );
    }
}

pub fn decode_trap_context(context: X86TrapContext) -> TrapEvent {
    match context.vector {
        VEC_SYSCALL => TrapEvent::Syscall,
        VEC_TIMER => TrapEvent::TimerInterrupt,
        VEC_PAGE_FAULT => {
            let access = if (context.error_code & (1 << 1)) != 0 {
                FaultAccess::Write
            } else if (context.error_code & (1 << 4)) != 0 {
                FaultAccess::Execute
            } else {
                FaultAccess::Read
            };
            TrapEvent::PageFault(FaultInfo {
                addr: VirtAddr(context.fault_addr),
                access,
            })
        }
        v if (VEC_EXTERNAL_BASE..VEC_EXTERNAL_LIMIT).contains(&v) => {
            TrapEvent::ExternalInterrupt((v - VEC_EXTERNAL_BASE) as u16)
        }
        _ => TrapEvent::Unknown {
            arch_code: context.vector as u64,
        },
    }
}

pub fn handle_trap_entry(
    kernel: &mut KernelState,
    cpu: CpuId,
    context: X86TrapContext,
    mut frame: Option<&mut TrapFrame>,
) -> Result<(), TrapHandleError> {
    super::descriptor_tables::ensure_boot_descriptor_tables_scaffolded();
    // NOTE(arch/x86_64): Architecture-specific IDT setup and assembly trap stubs
    // funnel hardware entries into this Rust dispatcher. Tests may still construct
    // synthetic contexts directly, but real trap/interrupt/syscall vectors now use
    // the same decode/dispatch path through descriptor_tables' stubs.
    let _ = kernel.set_current_cpu(cpu);
    let _ = kernel.process_cross_cpu_work_for_cpu(cpu);
    kernel.handle_trap_event(decode_trap_context(context), frame.as_deref_mut())?;
    restore_arch_thread_state(kernel, cpu, frame)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arch::trap::Trap;

    #[test]
    fn decode_syscall_vector() {
        let ev = decode_trap_context(X86TrapContext {
            vector: VEC_SYSCALL,
            error_code: 0,
            fault_addr: 0,
        });
        assert_eq!(ev.trap(), Trap::Syscall);
    }

    #[test]
    fn decode_timer_vector() {
        let ev = decode_trap_context(X86TrapContext {
            vector: VEC_TIMER,
            error_code: 0,
            fault_addr: 0,
        });
        assert_eq!(ev.trap(), Trap::TimerInterrupt);
    }

    #[test]
    fn decode_external_vector_maps_irq_line() {
        let ev = decode_trap_context(X86TrapContext {
            vector: VEC_EXTERNAL_BASE + 7,
            error_code: 0,
            fault_addr: 0,
        });
        assert_eq!(ev.trap(), Trap::ExternalInterrupt);
        assert_eq!(ev.irq(), Some(7));
    }

    #[test]
    fn decode_external_vector_limit_is_exclusive() {
        let ev = decode_trap_context(X86TrapContext {
            vector: VEC_EXTERNAL_LIMIT,
            error_code: 0,
            fault_addr: 0,
        });
        assert_eq!(ev.trap(), Trap::Unknown);
    }

    #[test]
    fn decode_page_fault_uses_cr2_and_access_bits() {
        let ev = decode_trap_context(X86TrapContext {
            vector: VEC_PAGE_FAULT,
            error_code: 0b10,
            fault_addr: 0xFACE_1000,
        });
        assert_eq!(ev.trap(), Trap::PageFault);
        assert_eq!(
            ev.fault(),
            Some(FaultInfo {
                addr: VirtAddr(0xFACE_1000),
                access: FaultAccess::Write,
            })
        );
    }

    #[test]
    fn decode_unknown_vector_is_unknown_event() {
        let ev = decode_trap_context(X86TrapContext {
            vector: 0x7F,
            error_code: 0,
            fault_addr: 0,
        });
        assert_eq!(ev.trap(), Trap::Unknown);
    }

    #[test]
    fn trap_entry_sets_cpu_and_handles_timer() {
        use crate::kernel::boot::Bootstrap;

        let mut state = crate::std::boxed::Box::new(Bootstrap::init().expect("init"));
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
        assert_eq!(state.current_cpu(), CpuId(1));
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
            X86TrapContext {
                vector: VEC_TIMER,
                error_code: 0,
                fault_addr: 0,
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
            X86TrapContext {
                vector: VEC_TIMER,
                error_code: 0,
                fault_addr: 0,
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
            X86TrapContext {
                vector: VEC_TIMER,
                error_code: 0,
                fault_addr: 0,
            },
            Some(&mut frame_b),
        )
        .expect("trap b");

        assert_eq!(last_restored_tls_base(CpuId(1)), Some(0xAAA0_0000));
        assert_eq!(last_restored_tls_base(CpuId(0)), Some(0xBBB0_0000));
    }
}
