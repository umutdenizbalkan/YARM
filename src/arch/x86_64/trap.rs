// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use crate::arch::trap::{FaultAccess, FaultInfo, TrapEvent};
use crate::kernel::boot::{FaultBookkeepingMode, KernelState, TrapHandleError};
use crate::kernel::scheduler::{CpuId, MAX_CPUS};
use crate::kernel::trapframe::TrapFrame;
use crate::kernel::vm::VirtAddr;
use core::sync::atomic::{AtomicUsize, Ordering};

const VEC_SYSCALL: u8 = 0x80;
const VEC_TIMER: u8 = 0x20;
const VEC_EXTERNAL_BASE: u8 = 0x20;
const VEC_EXTERNAL_LIMIT: u8 =
    VEC_EXTERNAL_BASE + crate::arch::platform_constants::MAX_IRQ_LINES as u8;
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

pub(crate) fn restore_arch_thread_state(
    kernel: &mut KernelState,
    cpu: CpuId,
    frame: Option<&mut TrapFrame>,
) -> Result<(), TrapHandleError> {
    let Some(frame) = frame else {
        return Ok(());
    };
    let tls = match kernel.resume_current_thread_with_frame(frame) {
        Ok(tls) => tls,
        Err(crate::kernel::boot::KernelError::TaskMissing) => {
            // No user task scheduled yet (normal during early boot).
            // Skip frame restore and return cleanly so DEPTH resets to 0.
            return Ok(());
        }
        Err(e) => {
            return Err(TrapHandleError::Syscall(
                crate::kernel::syscall::SyscallError::from(e),
            ));
        }
    };
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
    frame: Option<&mut TrapFrame>,
) -> Result<(), TrapHandleError> {
    handle_trap_entry_with_fault_bookkeeping_mode(
        kernel,
        cpu,
        context,
        frame,
        FaultBookkeepingMode::RecordInHandleTrapEvent,
    )
}

pub(crate) fn handle_trap_entry_with_fault_bookkeeping_mode(
    kernel: &mut KernelState,
    cpu: CpuId,
    context: X86TrapContext,
    mut frame: Option<&mut TrapFrame>,
    fault_bookkeeping_mode: FaultBookkeepingMode,
) -> Result<(), TrapHandleError> {
    super::descriptor_tables::ensure_boot_descriptor_tables_scaffolded();
    // Stage 132: one-shot post-cleanup #PF diagnostic, armed after CLEANUP_DONE.
    #[cfg(not(feature = "hosted-dev"))]
    {
        let cpu_idx = cpu.0 as usize;
        if cpu_idx < crate::kernel::scheduler::MAX_CPUS
            && crate::kernel::boot::D6_POST_CLEANUP_DIAG_PENDING[cpu_idx]
                .swap(false, core::sync::atomic::Ordering::AcqRel)
        {
            d6_emit_post_cleanup_first_trap_diag(kernel, cpu, context);
        }
    }
    // NOTE(arch/x86_64): Architecture-specific IDT setup and assembly trap stubs
    // funnel hardware entries into this Rust dispatcher. Tests may still construct
    // synthetic contexts directly, but real trap/interrupt/syscall vectors now use
    // the same decode/dispatch path through descriptor_tables' stubs.
    let _ = kernel.set_current_cpu(cpu);
    let _ = kernel.process_cross_cpu_work_for_cpu(cpu);
    // Save the entering task's register context (PC, SP, GPRs) to its TCB before
    // dispatching.  This is essential for x86_64 where the IPC blocking path does
    // not call sync_current_thread_from_frame; without this, a blocked task's TCB
    // retains its spawn-time PC and would restart from scratch on every resume.
    //
    // Skipped for tid==0 (supervisor/idle) — the kernel never returns to user-mode
    // supervisor code via iretq, so there is nothing meaningful to save.
    if let (Some(f), Some(tid)) = (frame.as_deref(), kernel.current_tid()) {
        if tid != 0 {
            let _ = kernel.sync_current_thread_from_frame(f);
        }
    }
    kernel.handle_trap_event_with_fault_bookkeeping_mode(
        decode_trap_context(context),
        frame.as_deref_mut(),
        fault_bookkeeping_mode,
    )?;
    // Stage 117: skip restore_arch_thread_state when a global-lock-drop plan
    // is stashed for this CPU. The restore will be called post-switch in
    // `handle_trap_entry_shared` after `switch_frames` runs outside the lock.
    let cpu_idx = cpu.0 as usize;
    let switch_pending = cpu_idx < crate::kernel::scheduler::MAX_CPUS
        && unsafe { crate::kernel::boot::DISPATCH_SWITCH_PLAN_STASH[cpu_idx].has_plan() };
    if !switch_pending {
        restore_arch_thread_state(kernel, cpu, frame)?;
    }
    Ok(())
}

#[cfg(not(feature = "hosted-dev"))]
fn d6_emit_post_cleanup_first_trap_diag(
    kernel: &mut KernelState,
    _cpu: CpuId,
    context: X86TrapContext,
) {
    let vector = context.vector;
    let error_code = context.error_code;
    let cr2 = context.fault_addr;
    let rsp_derived = cr2.wrapping_add(8);
    let kernel_ptr = kernel as *const _ as usize as u64;
    let current_tid = kernel.current_tid().unwrap_or(u64::MAX);
    let active_asid_num = kernel.d6_diag_active_asid_num();
    let tss_rsp0 = super::descriptor_tables::read_boot_tss_rsp0();
    let (stack_base, stack_top) = kernel.with_tcbs(|tcbs| {
        tcbs.iter()
            .flatten()
            .find(|tcb| tcb.tid.0 == current_tid)
            .map(|tcb| {
                (
                    tcb.kernel_context.stack_base.map_or(0u64, |v| v.0),
                    tcb.kernel_context.stack_top.map_or(0u64, |v| v.0),
                )
            })
            .unwrap_or((0u64, 0u64))
    });
    let mapped_page_bottom = stack_top.saturating_sub(4096u64);
    let cr2_eq_rsp_m8 = cr2 == rsp_derived.wrapping_sub(8);
    let in_full_stack = cr2 >= stack_base && cr2 < stack_top;
    let in_mapped_page = cr2 >= mapped_page_bottom && cr2 < stack_top;
    let stack_class = if cr2_eq_rsp_m8 && in_full_stack && !in_mapped_page {
        "cr2_below_mapped_stack"
    } else if cr2_eq_rsp_m8 && in_mapped_page {
        "cr2_inside_mapped_stack"
    } else if cr2 < stack_base {
        "cr2_below_expected_stack_page"
    } else if cr2 >= stack_top {
        "rsp_above_expected_stack_top"
    } else {
        "unknown"
    };
    crate::yarm_log!("D6_POST_CLEANUP_FIRST_TRAP_BEGIN");
    crate::yarm_log!("D6_POST_CLEANUP_FIRST_TRAP_VECTOR value=0x{:x}", vector);
    crate::yarm_log!("D6_POST_CLEANUP_FIRST_TRAP_ERROR value=0x{:x}", error_code);
    crate::yarm_log!("D6_POST_CLEANUP_FIRST_TRAP_CR2 value=0x{:x}", cr2);
    crate::yarm_log!(
        "D6_POST_CLEANUP_FIRST_TRAP_RIP value=unknown_kernel_mode tid={}",
        current_tid
    );
    crate::yarm_log!("D6_POST_CLEANUP_FIRST_TRAP_RSP value=0x{:x}", rsp_derived);
    crate::yarm_log!(
        "D6_POST_CLEANUP_FIRST_TRAP_R14 value=kernel_ptr=0x{:x}",
        kernel_ptr
    );
    crate::yarm_log!("D6_POST_CLEANUP_FIRST_TRAP_CURRENT tid={}", current_tid);
    crate::yarm_log!(
        "D6_POST_CLEANUP_FIRST_TRAP_ASID value=0x{:x}",
        active_asid_num
    );
    crate::yarm_log!(
        "D6_POST_CLEANUP_FIRST_TRAP_CR3 value=asid=0x{:x}",
        active_asid_num
    );
    crate::yarm_log!("D6_POST_CLEANUP_FIRST_TRAP_TSS_RSP0 value=0x{:x}", tss_rsp0);
    crate::yarm_log!(
        "D6_POST_CLEANUP_FIRST_TRAP_CR2_EQUALS_RSP_MINUS_8 {}",
        if cr2_eq_rsp_m8 { "yes" } else { "no" }
    );
    crate::yarm_log!(
        "D6_POST_CLEANUP_FIRST_TRAP_STACK_CLASS class={}",
        stack_class
    );
    crate::yarm_log!("D6_POST_CLEANUP_FIRST_TRAP_DONE");
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
    fn decode_external_vector_maps_highest_configured_irq_line() {
        let highest = crate::arch::platform_constants::MAX_IRQ_LINES as u8 - 1;
        let ev = decode_trap_context(X86TrapContext {
            vector: VEC_EXTERNAL_BASE + highest,
            error_code: 0,
            fault_addr: 0,
        });
        assert_eq!(ev.trap(), Trap::ExternalInterrupt);
        assert_eq!(ev.irq(), Some(highest as u16));
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
        // KernelState is large; use an 8 MiB thread stack to avoid overflow.
        std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
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
            })
            .expect("spawn")
            .join()
            .expect("join");
    }

    #[test]
    fn trap_entry_restores_tls_for_resumed_thread() {
        // KernelState is large; use an 8 MiB thread stack to avoid overflow.
        std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
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
                        ..Default::default()
                    })
                    .expect("leader");
                let tid = state
                    .spawn_user_thread(50, 0xCAFE_0000, 0x8100_0000, 0x4010)
                    .expect("thread");
                // spawn_user_task_from_image enqueues the leader (tid 50) before
                // spawn_user_thread enqueues the thread; yield until on the spawned thread.
                for _ in 0..5 {
                    if state.current_tid() == Some(tid) {
                        break;
                    }
                    state.yield_current().expect("switch");
                }
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
            })
            .expect("spawn")
            .join()
            .expect("join");
    }

    #[test]
    fn tls_restore_slots_are_isolated_per_cpu() {
        // KernelState is large; use an 8 MiB thread stack to avoid overflow.
        std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                use crate::kernel::boot::Bootstrap;
                // Register a bare task for CPU 1 with TLS=0xAAA0.  Avoid
                // spawn_user_task_from_image + spawn_user_thread because those use the
                // balanced scheduler which may place tasks on either CPU.
                let mut state = crate::std::boxed::Box::new(Bootstrap::init().expect("init"));
                state.bring_up_cpu(CpuId(1)).expect("cpu1");
                let tid_a = 61u64;
                state.register_task(tid_a).expect("register thread a");
                state
                    .set_thread_tls_base(tid_a, 0xAAA0_0000)
                    .expect("set tls a");
                state
                    .enqueue_on_cpu(CpuId(1), tid_a)
                    .expect("enqueue a on cpu1");
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
            })
            .expect("spawn")
            .join()
            .expect("join");
    }
}
