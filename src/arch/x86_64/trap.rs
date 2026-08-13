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

/// U3 (203C) — the broad-lock-free twin of [`ensure_user_return_cr3`].
///
/// Every step of the rare repair path is preserved verbatim: the ACTUAL hardware CR3 read, the
/// target task's CR3 lookup, the live RIP/RSP sampling on mismatch, the scan of all TCB kernel
/// stacks for the one containing RSP (through the rank-2 seam here), the identical fallback
/// bounds, `ensure_kernel_return_context_mapped_for_asid`, and the CR3 write ONLY once that
/// mapping is proven. A page-table failure is never weakened into success — it still takes the
/// `USER_CR3_PRE_IRET_SKIP` path and leaves hardware CR3 untouched. The markers are the same
/// ones, with the same fields.
///
/// `active_asid` was derived as "the current task's ASID, else the target's". The caller has
/// already established that the scheduler names `tid` as current and that `task_asid` is the
/// incarnation still bound to it, so that derivation and `task_asid` are the same value here —
/// the diagnostic is unchanged, not approximated.
pub(crate) fn ensure_user_return_cr3_split(
    shared: &crate::runtime::SharedKernel,
    tid: u64,
    task_asid: crate::kernel::vm::Asid,
) {
    #[cfg(not(feature = "hosted-dev"))]
    {
        let task_cr3 = match crate::arch::x86_64::page_table::cr3_for_asid(task_asid) {
            Some(c) => c,
            None => return,
        };
        let hw_cr3 = crate::arch::x86_64::page_table::read_hw_cr3();
        let active_asid = task_asid;
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
            // Stage 143: the correct LIVE kernel stack, not the target task's own (which may
            // be idle) — the same scan, through the rank-2 task seam.
            let (stack_base, stack_top, owner_tid) =
                find_kernel_stack_bounds_containing_rsp_split(shared, rsp);
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
    let _ = (shared, tid, task_asid);
}

/// U3 (203C) — truthful divergence when an exact-token post-lock resume refuses.
///
/// The x86 analogue of the established AArch64 `enter_post_lock_dispatch_fatal`. A refusal
/// means the dispatch already mutated the scheduler and the identity then failed to hold, so
/// returning would IRET through the OUTGOING task's frame. Instead the caller rolls the
/// dequeue back with its exact authority and diverges here, into the same wake-capable
/// `idle_halt_loop` the ordinary idle outcome uses — never a spin that masks the fault, and
/// never a resume of a replacement incarnation.
pub(crate) fn enter_post_lock_dispatch_fatal(cpu: CpuId, incoming: u64, rolled_back: bool) -> ! {
    crate::yarm_log!(
        "X86_POST_LOCK_DISPATCH_FATAL cpu={} incoming={} rolled_back={} reason=partial_dispatch_unrecoverable",
        cpu.0,
        incoming,
        rolled_back as u32
    );
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
    crate::arch::x86_64::descriptor_tables::idle_halt_loop();
    #[cfg(not(all(not(feature = "hosted-dev"), target_arch = "x86_64")))]
    panic!("x86_64 post-lock dispatch refused after marking: partial dispatch unrecoverable");
}

/// Why an exact-token post-lock resume refused. Each variant names the step that refused, so a
/// caller's diagnostic and its rollback decision are driven by evidence rather than inference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum X86ResumeRefusal {
    /// The rank-1 scheduler no longer names the token's incoming TID on the token's CPU.
    SchedulerCurrent,
    /// The token's exact ASID is no longer bound to that TID (a replacement incarnation), or
    /// the token names the idle task, which has no address space to activate.
    Asid,
    /// The token's exact incarnation has no restorable saved context.
    Context,
}

/// U3 (203C) — the x86_64 post-lock resume of a MARKED incoming task, without a broad lock.
///
/// This is the shared, class-neutral replacement for the two D2 switch-success bodies that
/// re-acquired `with_cpu` to run `d2_recv_switch_incoming_asid(inc)` +
/// `post_switch_restore_arch_thread_state(...)`. It reproduces that operation's observable
/// effects in the same order, reaching every piece of state through the exact-token seams:
///
/// 1. **Scheduler re-validation** — the rank-1 seam must still name the token's incoming TID
///    on the token's own authenticated CPU. Checked FIRST, so a divergence refuses before the
///    frame is touched at all.
/// 2. **Exact ASID activation** — `direct_dispatch_activate_asid_split`, which on x86_64 runs
///    the identical primitive pair the broad path ran (`hal_adapters::switch_address_space`
///    then `note_address_space_activated`), refusing unless the TCB still carries the token's
///    ASID.
/// 3. **Exact context + TLS** — `direct_dispatch_restore_context_split` reads the same
///    `tcb.user_context` `apply_current_thread_to_frame` read and takes the same pending
///    TLS-restore request `take_tls_restore_request` took, in ONE rank-2 acquisition, resolved
///    by exact `{tid, asid}` rather than by re-reading `current`.
/// 4. **Frame application**, then the FS base and `LAST_RESTORED_TLS_BASE` update, unchanged.
/// 5. **The pre-IRET CR3 invariant**, including its rare repair path — see
///    [`ensure_user_return_cr3_split`]. ASID activation does NOT make this redundant: it is a
///    check against the ACTUAL hardware CR3, which a D6 proof switch can still have left
///    disagreeing.
///
/// **`frame == None` is preserved exactly.** The old body activated the ASID and then called a
/// restore hook whose first statement was `let Some(frame) = frame else { return Ok(()) }` — so
/// activation happened and nothing else did. That is reproduced here, and an absent frame is
/// NOT an error.
///
/// **The idle token refuses.** A `Marked` token carries no ASID only for the idle TID, and the
/// old path skipped both activation (`task_asid(0)` was `None`) and the CR3 block (`tid != 0`).
/// Resuming idle through a user-frame restore is the very thing the x86 trap epilogue already
/// refuses to do, so this returns `Asid` rather than restoring one. Class-neutral: no D2, IPC
/// or direct-dispatch telemetry is emitted here.
pub(crate) fn x86_post_lock_resume_marked_incoming(
    shared: &crate::runtime::SharedKernel,
    token: crate::runtime::DispatchMarkToken,
    frame: Option<&mut TrapFrame>,
) -> Result<(), X86ResumeRefusal> {
    let incoming = token.tid();
    let cpu = token.cpu();
    if shared.current_tid_split_read(cpu) != Some(incoming) {
        return Err(X86ResumeRefusal::SchedulerCurrent);
    }
    let asid = token.expect_asid().ok_or(X86ResumeRefusal::Asid)?;
    shared
        .direct_dispatch_activate_asid_split(token)
        .ok_or(X86ResumeRefusal::Asid)?;
    // The old restore hook returned `Ok(())` for an absent frame, AFTER the activation above.
    let Some(frame) = frame else {
        return Ok(());
    };
    let (context, tls) = shared
        .direct_dispatch_restore_context_split(token)
        .ok_or(X86ResumeRefusal::Context)?;
    frame.apply_user_context(context);
    // U6 §8 — the BLOCKED-SEND completion boundary on x86_64.
    //
    // Placed immediately after `apply_user_context`, which reloads the sender's pre-block
    // register snapshot: writing the result before that call would restore over it. x86_64's
    // blocked recv installs its result into the saved frame at completion time and never
    // consumes a parked record; a blocked SEND cannot do that, because the U6 commit happens
    // off-lock after the producer has already returned, so the saved frame still carries the
    // pre-block state. The parked completion is what makes the resumed sender's result true.
    //
    // ABI: the same lanes an ordinary `ipc_send` return uses — success is `set_ok(0, 0, 0)`,
    // an error is the canonical code in the error lane. RIP is untouched.
    if let Some(done) = shared.direct_dispatch_take_send_completion_split(token) {
        if done.result == 0 {
            frame.set_ok(0, 0, 0);
        } else {
            frame.set_err(done.result as usize);
        }
        crate::yarm_log!(
            "X86_BLOCKED_SEND_COMPLETION_CONSUMED tid={} class={} result={} blocked_generation={} result=ok",
            incoming,
            done.syscall_class.slug(),
            done.result,
            done.blocked_generation
        );
    }
    let tls = tls.unwrap_or(0);
    restore_fs_base_if_needed(tls);
    let idx = cpu.0 as usize;
    if idx < MAX_CPUS {
        LAST_RESTORED_TLS_BASE[idx].store(tls, Ordering::Relaxed);
    }
    // Stage 140, unchanged: enforce hw CR3 == task_cr3 before the assembly stub does IRET.
    // The old guards were `current_tid()` non-zero and the task having an ASID; both are
    // answered by the token, which is strictly more exact than re-reading `current`.
    if incoming != 0 {
        ensure_user_return_cr3_split(shared, incoming, asid);
    }
    Ok(())
}

/// U3 (canonical 203C) — the arch half of the owner-revalidation transaction, with NO lock
/// held. The scheduler (rank 1) and task (rank 2) guards are both released before this runs.
///
/// This is the tail of [`restore_arch_thread_state`] for the one caller whose broad
/// re-acquisition was retired, reproduced step for step from the already-captured snapshot:
///
/// * `frame.apply_user_context(context)` — what `apply_current_thread_to_frame` did with the
///   value `thread_user_context` returned;
/// * `restore_fs_base_if_needed(tls.unwrap_or(0))` and the per-CPU `LAST_RESTORED_TLS_BASE`
///   store, both against the value `take_tls_restore_request` yielded — including `None`,
///   which still stores 0 exactly as `tls.unwrap_or(0)` did;
/// * the pre-IRET CR3 invariant through [`ensure_user_return_cr3_split`], which carries the
///   rare stack-bound lookup and repair and the fail-safe skip that leaves hardware CR3
///   untouched when the return context cannot be mapped.
///
/// The two legacy guards on the CR3 block are preserved with the same meaning: `tid != 0`,
/// and an ASID actually bound to the task. A task with no ASID restores and skips the block —
/// `if let Some(task_asid) = kernel.task_asid(tid)` was never a refusal, and is not made one.
///
/// Unlike [`x86_post_lock_resume_marked_incoming`] this performs **no ASID activation** and
/// takes **no `DispatchMarkToken`**: the body it replaces activated nothing, and inventing an
/// activation or an incarnation proof here would change behavior rather than preserve it.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn x86_apply_owner_revalidation_restore(
    shared: &crate::runtime::SharedKernel,
    cpu: CpuId,
    tid: u64,
    snapshot: crate::runtime::OwnerRevalidationSnapshot,
    frame: &mut TrapFrame,
) {
    frame.apply_user_context(snapshot.context);
    let tls = snapshot.tls.unwrap_or(0);
    restore_fs_base_if_needed(tls);
    let idx = cpu.0 as usize;
    if idx < MAX_CPUS {
        LAST_RESTORED_TLS_BASE[idx].store(tls, Ordering::Relaxed);
    }
    if tid != 0
        && let Some(task_asid) = snapshot.asid
    {
        ensure_user_return_cr3_split(shared, tid, task_asid);
    }
}

/// The estimate used when no TCB owns a kernel stack containing `rsp`.
///
/// U3 (203C): lifted out of [`find_kernel_stack_bounds_containing_rsp`] so the broad-lock
/// lookup and the rank-2 split lookup share ONE definition. "Identical fallback bounds" is
/// then a property of the code rather than an assertion about two copies that could drift.
pub(crate) fn fallback_kernel_stack_bounds(rsp: u64) -> (u64, u64, u64) {
    const PAGE_SZ: u64 = 4096;
    const STACK_FLOOR: u64 = 0xFFFF_8000_0000_1000;
    // Stage 165J: x86_64 per-task kernel stacks are 128 KiB (124 KiB usable
    // above the guard page), so the fallback estimate spans 124 KiB.
    let top = (rsp & !(PAGE_SZ - 1)) + PAGE_SZ;
    let base = (rsp & !(PAGE_SZ - 1))
        .saturating_sub(124 * 1024)
        .max(STACK_FLOOR);
    (base, top, 0)
}

/// Does `tcb`'s kernel stack contain `rsp`? The single predicate both lookups apply, so the
/// split scan cannot drift from the broad-lock scan it replaces.
pub(crate) fn kernel_stack_bounds_if_containing(
    tcb: &crate::kernel::task::ThreadControlBlock,
    rsp: u64,
) -> Option<(u64, u64, u64)> {
    let base = tcb.kernel_context.stack_base.map_or(0u64, |v| v.0);
    let top = tcb.kernel_context.stack_top.map_or(0u64, |v| v.0);
    (base != 0 && top != 0 && rsp >= base && rsp < top).then_some((base, top, tcb.tid.0))
}

#[cfg(not(feature = "hosted-dev"))]
fn find_kernel_stack_bounds_containing_rsp(kernel: &KernelState, rsp: u64) -> (u64, u64, u64) {
    let found = kernel.with_tcbs(|tcbs| {
        tcbs.iter()
            .flatten()
            .find_map(|tcb| kernel_stack_bounds_if_containing(tcb, rsp))
    });
    found.unwrap_or_else(|| fallback_kernel_stack_bounds(rsp))
}

/// U3 (203C) — the rank-2 split twin of [`find_kernel_stack_bounds_containing_rsp`].
///
/// Scans the SAME TCB fields with the SAME predicate and the SAME fallback, through the
/// task-domain seam instead of the broad `KernelState` lock. Nothing is mutated.
pub(crate) fn find_kernel_stack_bounds_containing_rsp_split(
    shared: &crate::runtime::SharedKernel,
    rsp: u64,
) -> (u64, u64, u64) {
    let found = shared.with_task_tcbs_split_mut(|tcbs| {
        tcbs.iter()
            .flatten()
            .find_map(|tcb| kernel_stack_bounds_if_containing(tcb, rsp))
    });
    found.unwrap_or_else(|| fallback_kernel_stack_bounds(rsp))
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
    // ── Stage 200D-0B3: the x86_64 CurrentTaskExited consumer (in-lock, corrected) ───
    //
    // THE single production consumer of the Stage 200D-0A disposition.
    //
    // CORRECTION (supersedes the Stage 200D-0B1 text sealed by Stage 200D-0B2). The previous
    // comment here claimed `handle_trap_event` had "released the broad `SpinLock<KernelState>`"
    // and that the post-lock deferred work had drained. Both were false, and the markers built
    // on them were false with them. `handle_trap_entry_with_fault_bookkeeping_mode` runs inside
    // `SharedKernel::with_cpu`, which holds the broad guard across this entire body; the shared
    // post-lock drains do not run until `with_cpu` returns, which is after this function exits.
    //
    // The consumer STAYS here, because in-lock is where it belongs — this is where the exiting
    // task's identity is still coherent and where the outgoing owner is selected. What changed
    // is that it now says so. Its whole in-lock job is:
    //
    //   * take exactly one typed disposition;
    //   * validate the exact {tid, asid} incarnation against live TCB state;
    //   * confirm the exiting task is terminal, is not current, and is not the restore owner;
    //   * name the PREPARED restore owner (replacement or idle);
    //   * arm one bounded attestation latch so the two later frames — the post-lock section and
    //     the vector epilogue — can attest what THEY actually do.
    //
    // It performs NO teardown, NO PeerDeath or timeout terminal claim, NO caller-result
    // publication, NO scheduler enqueue, NO userspace copy, NO reply-record scan, NO frame
    // write and NO trap-depth write. Those six production side effects are exactly what must
    // not happen under the broad lock, and `c0b3_no_production_side_effects_under_broad_lock`
    // guards each of them at zero.
    //
    // "Prepared" is not "restored". `restore_arch_thread_state` below writes the replacement's
    // context into the kernel-side `TrapFrame` only. The hardware iret frame is not touched
    // until `flush_trap_context_to_iret_frame` in the vector epilogue, which runs after
    // `with_cpu` returns AND after every post-lock drain. The exiting task cannot be restored
    // because it is no longer the restore owner, and no user return happens from here at all.
    if let crate::kernel::boot::PostLockTrapDisposition::CurrentTaskExited { tid, asid } =
        crate::kernel::boot::take_post_lock_trap_disposition(cpu.0 as usize)
    {
        crate::yarm_log!(
            "EXIT_TASK_DISPOSITION_CONSUMED arch=x86_64 tid={} asid={} cpu={} broad_lock=1 result=ok",
            tid,
            asid.0,
            cpu.0
        );
        // (a) The exiting incarnation must no longer be current. If it were, the epilogue
        // would flush a dead task's context into the iret frame — fail closed through the
        // existing fatal path.
        if kernel.current_tid() == Some(tid) {
            crate::yarm_log!(
                "EXIT_TASK_EXITING_STILL_CURRENT arch=x86_64 tid={} result=fail",
                tid
            );
            return Err(TrapHandleError::Syscall(
                crate::kernel::syscall::SyscallError::Internal,
            ));
        }
        // (b) Identity is the FULL incarnation. A numeric TID match alone would let a
        // restarted task satisfy a stale disposition, so the ASID recorded at publication
        // must still be the one bound to that TID — or the TCB must be gone entirely.
        let identity_ok = match kernel.task_asid(tid) {
            Some(current_asid) => current_asid == asid,
            None => true, // fully reaped: nothing can impersonate it
        };
        let terminal = matches!(
            kernel.task_status(tid),
            Some(crate::kernel::task::TaskStatus::Exited(_))
                | Some(crate::kernel::task::TaskStatus::Dead)
                | None
        );
        // (c) Absence is not merely "off this CPU's queue": the exiting incarnation must be
        // present in NO runqueue on ANY CPU, so it can never be re-selected.
        let in_runqueue = kernel.task_present_in_any_runqueue(tid);
        if !identity_ok || !terminal || in_runqueue {
            crate::yarm_log!(
                "EXIT_TASK_WRONG_IDENTITY arch=x86_64 tid={} asid={} identity_ok={} terminal={} in_runqueue={} result=fail",
                tid,
                asid.0,
                u32::from(identity_ok),
                u32::from(terminal),
                u32::from(in_runqueue)
            );
            return Err(TrapHandleError::Syscall(
                crate::kernel::syscall::SyscallError::Internal,
            ));
        }
        crate::yarm_log!(
            "EXIT_TASK_EXITING_NOT_CURRENT arch=x86_64 tid={} asid={} cpu={} broad_lock=1 result=ok",
            tid,
            asid.0,
            cpu.0
        );
        crate::yarm_log!(
            "EXIT_TASK_ABSENCE_VALIDATED arch=x86_64 tid={} asid={} current=0 runqueue=0 restore_owner=0 identity=tid_asid broad_lock=1 result=ok",
            tid,
            asid.0
        );
        // (d) Name the PREPARED restore owner. It is never the exiting task. This selects the
        // owner; it does not restore anything to userspace.
        match kernel.current_tid() {
            Some(next) if next != 0 => {
                if next == tid {
                    crate::yarm_log!(
                        "EXIT_TASK_RESELECTED_EXITING_TASK arch=x86_64 tid={} cpu={} result=fail",
                        tid,
                        cpu.0
                    );
                    return Err(TrapHandleError::Syscall(
                        crate::kernel::syscall::SyscallError::Internal,
                    ));
                }
                crate::yarm_log!(
                    "EXIT_TASK_RESTORE_OWNER_PREPARED arch=x86_64 owner=replacement exiting_tid={} next_tid={} cpu={} broad_lock=1 result=ok",
                    tid,
                    next,
                    cpu.0
                );
            }
            _ => crate::yarm_log!(
                "EXIT_TASK_RESTORE_OWNER_PREPARED arch=x86_64 owner=idle exiting_tid={} cpu={} broad_lock=1 result=ok",
                tid,
                cpu.0
            ),
        }
        // (e) Arm the bounded latch. It drives markers only — no scheduling, teardown, frame
        // selection or terminal-claim decision reads it.
        if !crate::kernel::boot::arm_exit_attestation(cpu.0 as usize, tid, asid) {
            crate::yarm_log!(
                "EXIT_TASK_DUPLICATE_DISPOSITION arch=x86_64 tid={} cpu={} result=fail",
                tid,
                cpu.0
            );
            return Err(TrapHandleError::Syscall(
                crate::kernel::syscall::SyscallError::Internal,
            ));
        }
    }

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
                    .reserve_and_spawn_user_task_from_image_for_test(UserImageSpec {
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
