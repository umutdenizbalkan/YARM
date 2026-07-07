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
    // Stage 140: enforce hw CR3 == task_cr3 before the assembly stub does IRET.
    if let Some(tid) = kernel.current_tid() {
        if tid != 0 {
            if let Some(task_asid) = kernel.task_asid(tid) {
                ensure_user_return_cr3(kernel, tid, task_asid);
            }
        }
    }
    Ok(())
}

/// Enforce hardware CR3 == task_cr3 immediately before a ring-3 return.
/// Reads the actual hardware CR3 (not HAL bookkeeping) and force-writes it when
/// there is a mismatch. No-op in normal runs; repairs the invariant that D6
/// proof switches can break.
pub(crate) fn ensure_user_return_cr3(
    kernel: &KernelState,
    tid: u64,
    task_asid: crate::kernel::vm::Asid,
) {
    #[cfg(not(feature = "hosted-dev"))]
    {
        let task_cr3 = match crate::arch::x86_64::page_table::cr3_for_asid(task_asid) {
            Some(c) => c,
            None => return,
        };
        // Stage 189C — per-CPU-correct return authority. The active root is THIS
        // CPU's ACTUAL hardware CR3, never the global HAL "active ASID"
        // (`d6_diag_active_asid_num`), which is a single BSP-centric value and is
        // wrong on an AP. We reverse-derive the active ASID from the executing
        // CPU's current task (`current_tid()` is set from the trapping CPU's APIC
        // id at entry), so nothing global leaks into an AP's return reasoning. The
        // switch decision below already keys off `hw_cr3`, so this changes only the
        // diagnostic derivation, not the switch — BSP behavior is unchanged.
        let hw_cr3 = crate::arch::x86_64::page_table::read_hw_cr3();
        let active_asid = kernel
            .current_tid()
            .and_then(|cur| kernel.task_asid(cur))
            .unwrap_or(task_asid);
        let active_cr3 =
            crate::arch::x86_64::page_table::cr3_for_asid(active_asid).unwrap_or(hw_cr3);
        crate::yarm_log!(
            "USER_CR3_PRE_IRET_CHECK tid={} task_asid={} task_cr3=0x{:016x} active_asid={} active_cr3=0x{:016x} hw_cr3=0x{:016x}",
            tid,
            task_asid.0,
            task_cr3,
            active_asid.0,
            active_cr3,
            hw_cr3,
        );
        if hw_cr3 != task_cr3 {
            // Stage 141/142: repair the kernel return context before force-writing CR3.
            let mut rip: u64 = 0;
            let mut rsp: u64 = 0;
            unsafe {
                core::arch::asm!("lea {}, [rip + 0]", out(reg) rip, options(nostack, preserves_flags));
                core::arch::asm!("mov {}, rsp", out(reg) rsp, options(nostack, preserves_flags));
            }
            // Stage 143: scan ALL TCBs for the one whose stack contains the
            // sampled RSP, so the correct live kernel stack is mapped rather
            // than the target task's own TCB stack (which may be idle).
            let (stack_base, stack_top, owner_tid) =
                find_kernel_stack_bounds_containing_rsp(kernel, rsp);
            crate::yarm_log!(
                "USER_CR3_RETURN_STACK_SELECT rsp=0x{:x} base=0x{:x} top=0x{:x} owner_tid={}",
                rsp,
                stack_base,
                stack_top,
                owner_tid,
            );
            let ctx_mapped =
                crate::arch::x86_64::page_table::ensure_kernel_return_context_mapped_for_asid(
                    task_asid, rip, rsp, stack_base, stack_top,
                );
            crate::yarm_log!(
                "USER_CR3_PRE_IRET_SWITCH tid={} from=0x{:016x} to=0x{:016x} ctx_mapped={}",
                tid,
                hw_cr3,
                task_cr3,
                ctx_mapped,
            );
            if ctx_mapped {
                // Guarded force path: return-context mapping proven, safe to write.
                crate::arch::x86_64::page_table::write_cr3_for_asid(task_asid);
            } else {
                // Do not switch into a root that lacks the live kernel stack;
                // that is the exact #PF Stage 140 caused. Leave hw CR3 as-is.
                crate::yarm_log!(
                    "USER_CR3_PRE_IRET_SKIP tid={} reason=return_ctx_unmapped",
                    tid
                );
            }
        }
        let final_cr3 = crate::arch::x86_64::page_table::read_hw_cr3();
        crate::yarm_log!(
            "USER_CR3_PRE_IRET_OK tid={} hw_cr3=0x{:016x}",
            tid,
            final_cr3
        );
    }
    #[cfg(feature = "hosted-dev")]
    let _ = (kernel, tid, task_asid);
}

#[cfg(not(feature = "hosted-dev"))]
fn find_kernel_stack_bounds_containing_rsp(kernel: &KernelState, rsp: u64) -> (u64, u64, u64) {
    let found = kernel.with_tcbs(|tcbs| {
        tcbs.iter().flatten().find_map(|tcb| {
            let base = tcb.kernel_context.stack_base.map_or(0u64, |v| v.0);
            let top = tcb.kernel_context.stack_top.map_or(0u64, |v| v.0);
            if base != 0 && top != 0 && rsp >= base && rsp < top {
                Some((base, top, tcb.tid.0))
            } else {
                None
            }
        })
    });
    found.unwrap_or_else(|| {
        const PAGE_SZ: u64 = 4096;
        const STACK_FLOOR: u64 = 0xFFFF_8000_0000_1000;
        // Stage 165J: x86_64 per-task kernel stacks are 128 KiB (124 KiB usable
        // above the guard page), so the fallback estimate spans 124 KiB.
        let top = (rsp & !(PAGE_SZ - 1)) + PAGE_SZ;
        let base = (rsp & !(PAGE_SZ - 1))
            .saturating_sub(124 * 1024)
            .max(STACK_FLOOR);
        (base, top, 0)
    })
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
    // Stage 137: log raw hardware fault context before any KernelState mutation.
    // frame_rip = hardware interrupt-frame RIP (the true faulting PC).
    if context.vector == VEC_PAGE_FAULT {
        let tid = kernel.current_tid().unwrap_or(u64::MAX);
        let error = context.error_code;
        let frame_rip = frame.as_ref().map(|f| f.saved_pc).unwrap_or(0);
        let frame_rsp = frame.as_ref().map(|f| f.saved_sp).unwrap_or(0);
        let frame_rax = frame.as_ref().map(|f| f.user_gpr(0)).unwrap_or(0);
        let frame_rcx = frame.as_ref().map(|f| f.user_gpr(2)).unwrap_or(0);
        let frame_rdi = frame.as_ref().map(|f| f.user_gpr(5)).unwrap_or(0);
        let frame_rsi = frame.as_ref().map(|f| f.user_gpr(4)).unwrap_or(0);
        crate::yarm_log!(
            "PAGE_FAULT_RAW tid={} vector=0x{:x} error=0x{:x} cr2=0x{:x} frame_rip=0x{:x} frame_rsp=0x{:x} rax=0x{:x} rcx=0x{:x} rdi=0x{:x} rsi=0x{:x}",
            tid,
            context.vector,
            error,
            context.fault_addr,
            frame_rip,
            frame_rsp,
            frame_rax,
            frame_rcx,
            frame_rdi,
            frame_rsi,
        );
        crate::yarm_log!(
            "PAGE_FAULT_X86_ERROR raw=0x{:x} present={} write={} user={} instr={} reserved={}",
            error,
            (error >> 0) & 1,
            (error >> 1) & 1,
            (error >> 2) & 1,
            (error >> 4) & 1,
            (error >> 3) & 1,
        );
    }
    // Stage 138: compare hardware CR3 against HAL-tracked active CR3 and the
    // task's expected CR3.  A mismatch here explains why software VM says the
    // page is present while the CPU keeps taking not-present faults: the
    // hardware is walking a different page table than the software resolves.
    #[cfg(not(feature = "hosted-dev"))]
    if context.vector == VEC_PAGE_FAULT {
        let mut hw_cr3: u64;
        unsafe {
            core::arch::asm!(
                "mov {}, cr3",
                out(reg) hw_cr3,
                options(nostack, preserves_flags),
            );
        }
        let active_asid_num = kernel.d6_diag_active_asid_num();
        let active_asid = crate::kernel::vm::Asid(active_asid_num as u16);
        let active_cr3 =
            crate::arch::x86_64::page_table::cr3_for_asid(active_asid).unwrap_or(u64::MAX);
        let tid = kernel.current_tid().unwrap_or(u64::MAX);
        let task_asid = kernel.task_asid(tid).unwrap_or(crate::kernel::vm::Asid(0));
        let task_cr3 = crate::arch::x86_64::page_table::cr3_for_asid(task_asid).unwrap_or(u64::MAX);
        crate::yarm_log!(
            "PAGE_FAULT_CR3_COMPARE hw_cr3=0x{:016x} active_asid={} active_cr3=0x{:016x} task_asid={} task_cr3=0x{:016x}",
            hw_cr3,
            active_asid.0,
            active_cr3,
            task_asid.0,
            task_cr3,
        );
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
    // Stage 134: stack watermark — cr2 is the fault address and serves as
    // an approximate lower bound on where RSP was (RSP <= cr2 + small offset).
    let stack_used = stack_top.saturating_sub(cr2);
    let stack_limit = stack_top.saturating_sub(stack_base);
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
    crate::yarm_log!(
        "KERNEL_STACK_WATERMARK tid={} rsp=0x{:x} used={} limit={}",
        current_tid,
        cr2,
        stack_used,
        stack_limit
    );
    if cr2 < stack_base {
        crate::yarm_log!(
            "KERNEL_STACK_OVERFLOW_DETECTED tid={} rsp=0x{:x} base=0x{:x}",
            current_tid,
            cr2,
            stack_base
        );
    }
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
