// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use crate::arch::trap::TrapEvent;
use crate::kernel::boot::{FaultBookkeepingMode, KernelState, TrapHandleError};
use crate::kernel::scheduler::CpuId;
use crate::kernel::trapframe::TrapFrame;

#[cfg(target_arch = "riscv64")]
pub type ArchTrapContext = super::riscv64::trap::Riscv64TrapContext;
#[cfg(target_arch = "riscv64")]
pub fn decode_trap_context(context: ArchTrapContext) -> TrapEvent {
    super::riscv64::trap::decode_trap_context(context)
}
#[cfg(target_arch = "riscv64")]
pub fn handle_trap_entry(
    kernel: &mut KernelState,
    cpu: CpuId,
    context: ArchTrapContext,
    frame: Option<&mut TrapFrame>,
) -> Result<(), TrapHandleError> {
    super::riscv64::trap::handle_trap_entry(kernel, cpu, context, frame)
}
#[cfg(target_arch = "riscv64")]
pub(crate) fn handle_trap_entry_with_fault_bookkeeping_mode(
    kernel: &mut KernelState,
    cpu: CpuId,
    context: ArchTrapContext,
    frame: Option<&mut TrapFrame>,
    fault_bookkeeping_mode: FaultBookkeepingMode,
) -> Result<(), TrapHandleError> {
    super::riscv64::trap::handle_trap_entry_with_fault_bookkeeping_mode(
        kernel,
        cpu,
        context,
        frame,
        fault_bookkeeping_mode,
    )
}

#[cfg(target_arch = "x86_64")]
pub type ArchTrapContext = super::x86_64::trap::X86TrapContext;
#[cfg(target_arch = "x86_64")]
pub fn decode_trap_context(context: ArchTrapContext) -> TrapEvent {
    super::x86_64::trap::decode_trap_context(context)
}
#[cfg(target_arch = "x86_64")]
pub fn handle_trap_entry(
    kernel: &mut KernelState,
    cpu: CpuId,
    context: ArchTrapContext,
    frame: Option<&mut TrapFrame>,
) -> Result<(), TrapHandleError> {
    super::x86_64::trap::handle_trap_entry(kernel, cpu, context, frame)
}
#[cfg(target_arch = "x86_64")]
pub(crate) fn handle_trap_entry_with_fault_bookkeeping_mode(
    kernel: &mut KernelState,
    cpu: CpuId,
    context: ArchTrapContext,
    frame: Option<&mut TrapFrame>,
    fault_bookkeeping_mode: FaultBookkeepingMode,
) -> Result<(), TrapHandleError> {
    super::x86_64::trap::handle_trap_entry_with_fault_bookkeeping_mode(
        kernel,
        cpu,
        context,
        frame,
        fault_bookkeeping_mode,
    )
}

#[cfg(target_arch = "aarch64")]
pub type ArchTrapContext = super::aarch64::trap::Aarch64TrapContext;
#[cfg(target_arch = "aarch64")]
pub fn decode_trap_context(context: ArchTrapContext) -> TrapEvent {
    super::aarch64::trap::decode_trap_context(context)
}
#[cfg(target_arch = "aarch64")]
pub fn handle_trap_entry(
    kernel: &mut KernelState,
    cpu: CpuId,
    context: ArchTrapContext,
    frame: Option<&mut TrapFrame>,
) -> Result<(), TrapHandleError> {
    super::aarch64::trap::handle_trap_entry(kernel, cpu, context, frame)
}
#[cfg(target_arch = "aarch64")]
pub(crate) fn handle_trap_entry_with_fault_bookkeeping_mode(
    kernel: &mut KernelState,
    cpu: CpuId,
    context: ArchTrapContext,
    frame: Option<&mut TrapFrame>,
    fault_bookkeeping_mode: FaultBookkeepingMode,
) -> Result<(), TrapHandleError> {
    super::aarch64::trap::handle_trap_entry_with_fault_bookkeeping_mode(
        kernel,
        cpu,
        context,
        frame,
        fault_bookkeeping_mode,
    )
}

/// Stage 117: arch-specific post-switch restore, called after `switch_frames`
/// in the incoming task's context under a re-acquired global lock. Restores
/// the incoming task's user-mode register state to its trap frame.
#[cfg(target_arch = "x86_64")]
fn post_switch_restore_arch_thread_state(
    kernel: &mut KernelState,
    cpu: CpuId,
    frame: Option<&mut TrapFrame>,
) -> Result<(), TrapHandleError> {
    super::x86_64::trap::restore_arch_thread_state(kernel, cpu, frame)
}

#[cfg(target_arch = "aarch64")]
fn post_switch_restore_arch_thread_state(
    kernel: &mut KernelState,
    cpu: CpuId,
    frame: Option<&mut TrapFrame>,
) -> Result<(), TrapHandleError> {
    super::aarch64::trap::restore_arch_thread_state_post_switch(kernel, cpu, frame)
}

#[cfg(target_arch = "riscv64")]
fn post_switch_restore_arch_thread_state(
    _kernel: &mut KernelState,
    _cpu: CpuId,
    _frame: Option<&mut TrapFrame>,
) -> Result<(), TrapHandleError> {
    // RISC-V uses a raw-pointer trap path (no `with_cpu`). The stash is never
    // populated on RISC-V, so this function is never called on that arch.
    Ok(())
}

pub fn handle_trap_entry_shared(
    shared: &crate::runtime::SharedKernel,
    cpu: CpuId,
    context: ArchTrapContext,
    mut frame: Option<&mut TrapFrame>,
) -> Result<(), TrapHandleError> {
    // Stage 29: pre-global-lock split-dispatch seam (whitelist-only, default-deny).
    //
    // For a syscall trap whose number is on the `syscall_split` whitelist (today
    // ONLY `ControlPlaneSetCnodeSlots` / NR 8), service it via per-domain split
    // helpers WITHOUT taking the global `with_cpu` lock, writing the result into
    // the frame here (`set_ok(slots, pid, 0)`). The split path never blocks,
    // yields, schedules, or switches tasks, so `task_switched` stays `false` for
    // the arch return-register writeback exactly as on the global-lock path.
    //
    // Every other syscall (and any classification/precondition miss, or an absent
    // requester TID) returns `None` and falls through to the UNCHANGED global-lock
    // dispatch below. This is gated on the trap being a syscall so non-syscall
    // events (page faults, timer/external IRQs) never enter the seam.
    if matches!(decode_trap_context(context), TrapEvent::Syscall) {
        if let Some(frame) = frame.as_deref_mut() {
            if let Some(result) =
                crate::kernel::syscall_split::try_split_dispatch_into_frame(shared, cpu, frame)
            {
                crate::yarm_log!(
                    "YARM_LOCK_SPLIT_DISPATCH nr={} cpu={} result={}",
                    frame.syscall_num(),
                    cpu.0,
                    if result.is_ok() { "ok" } else { "err" }
                );
                // task_switched == false (no scheduler interaction); skip the
                // global lock entirely.
                return result;
            }
        }
    }

    // Stage L4A: architecture-neutral recv-timeout split-read staging for trap
    // paths that enter through SharedKernel-owned dispatch.
    //
    // We pre-read scheduler tick under the scheduler lock before taking the
    // global SharedKernel lock and stage a per-CPU deadline slot consumed by
    // handle_ipc_recv_timeout. Non-shared/raw trap paths are unchanged.
    if let Some((syscall_nr, timeout_ticks, arch_name)) =
        shared_recv_timeout_staging_info(context, frame.as_deref())
    {
        if syscall_nr == crate::kernel::syscall::SYSCALL_IPC_RECV_TIMEOUT_NR && timeout_ticks != 0 {
            crate::yarm_log!(
                "YARM_LOCK_SPLIT_RECV_TIMEOUT path=shared_bridge arch={}",
                arch_name
            );
            let now = shared.scheduler_tick_now_split_read();
            let deadline = now.wrapping_add(timeout_ticks);
            let cpu_idx = cpu.0 as usize;
            if cpu_idx < crate::kernel::scheduler::MAX_CPUS && deadline != 0 {
                crate::kernel::scheduler::SPLIT_RECV_TIMEOUT_DEADLINE[cpu_idx]
                    .store(deadline, core::sync::atomic::Ordering::Release);
            }
        }
    }
    // Stage 3B-E: SharedKernel trap paths pre-record only diagnostic page-fault
    // bookkeeping under fault_state_lock before taking the global SharedKernel
    // lock. All real trap behavior still runs in shared.with_cpu below; raw
    // paths keep recording inside KernelState::handle_trap_event.
    let fault_bookkeeping_mode = if let TrapEvent::PageFault(fault) = decode_trap_context(context) {
        shared.record_fault_split_mut(fault);
        if let Some(frame) = frame.as_deref() {
            shared.record_fault_frame_snapshot_split_mut(frame);
        }
        FaultBookkeepingMode::AlreadyRecordedBySharedSeam
    } else {
        FaultBookkeepingMode::RecordInHandleTrapEvent
    };

    // Stage 117: signal to `maybe_switch_kernel_context` that this CPU is in
    // the `handle_trap_entry_shared` path and the stash WILL be drained after
    // `with_cpu` returns. Without this flag, direct-call paths (tests) would
    // stash a plan with no external drainer, losing the context switch.
    let cpu_idx = cpu.0 as usize;
    if cpu_idx < crate::kernel::scheduler::MAX_CPUS {
        crate::kernel::boot::GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE[cpu_idx]
            .store(true, core::sync::atomic::Ordering::Relaxed);
    }

    // Stage 117: pass `frame.as_deref_mut()` (reborrow) so that `frame` remains
    // available after `with_cpu` returns for the stash drain below.
    let inner_result = shared
        .with_cpu(cpu, |kernel| {
            handle_trap_entry_with_fault_bookkeeping_mode(
                kernel,
                cpu,
                context,
                frame.as_deref_mut(),
                fault_bookkeeping_mode,
            )
        })
        .map_err(|err| TrapHandleError::Syscall(err.into()));

    // Clear the trap-path-active flag; the stash drain below handles whatever
    // was stashed during the `with_cpu` call.
    if cpu_idx < crate::kernel::scheduler::MAX_CPUS {
        crate::kernel::boot::GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE[cpu_idx]
            .store(false, core::sync::atomic::Ordering::Relaxed);
    }

    let inner_result = inner_result?;
    // `with_cpu` has returned; the outer `SpinLock<KernelState>` guard is dropped.
    // `inner_result: Result<(), TrapHandleError>` from the arch handler.

    // Stage 117: drain the per-CPU switch plan stash.
    //
    // If `maybe_switch_kernel_context` stashed a `DispatchSwitchPlan` (single-CPU
    // x86_64/aarch64 path), call `switch_frames` here with NO global lock held.
    // Phase A safety: interrupts remain disabled because hardware disabled them
    // on trap entry and `SpinLock<KernelState>` does not save/restore IRQ state.
    //
    // After `switch_frames` the execution context has switched to the INCOMING
    // task's kernel stack. All local variables below (`frame`, `shared`, `cpu`)
    // are now the INCOMING task's versions, which were on its own kernel stack
    // when it was last suspended at this exact code location.
    let cpu_idx = cpu.0 as usize;
    if cpu_idx < crate::kernel::scheduler::MAX_CPUS {
        // SAFETY: single CPU, interrupts disabled, no concurrent accessor.
        let plan = unsafe { crate::kernel::boot::DISPATCH_SWITCH_PLAN_STASH[cpu_idx].take() };
        if let Some(plan) = plan {
            crate::yarm_log!("D6_GLOBAL_LOCK_DROPPED_BEFORE_SWITCH");
            crate::yarm_log!(
                "D6_SWITCH_FRAMES_ENTER_UNLOCKED outgoing={} incoming={}",
                plan.outgoing_tid,
                plan.incoming_tid
            );
            // SAFETY: pointers derived from stable KernelState::tcbs storage under
            // task_state_lock; valid because KernelState is alive for the program
            // lifetime, the array is fixed-size (no reallocation), and the system is
            // single-CPU with interrupts disabled (no concurrent modification).
            // The dereferences are non-aliasing: outgoing and incoming indices were
            // verified distinct in `maybe_switch_kernel_context`.
            unsafe {
                crate::arch::selected_isa::context_switch::switch_frames(
                    &mut *plan.outgoing_frame_ptr,
                    &*plan.incoming_frame_ptr,
                    plan.incoming_stack_top,
                );
            }
            // POINT 2: execution resumes here in the INCOMING task's context.
            // The INCOMING task's `frame` (on its own kernel stack) is now `frame`.
            crate::yarm_log!("D6_SWITCH_FRAMES_RETURNED_UNLOCKED");
            // Re-acquire the global lock to restore the incoming task's arch thread
            // state (populate its trap frame with its user-mode register context).
            shared
                .with_cpu(cpu, |kernel| {
                    post_switch_restore_arch_thread_state(kernel, cpu, frame.as_deref_mut())
                })
                .map_err(|err| TrapHandleError::Syscall(err.into()))??;
        }
    }

    inner_result
}

#[cfg(target_arch = "aarch64")]
fn shared_recv_timeout_staging_info(
    context: ArchTrapContext,
    frame: Option<&TrapFrame>,
) -> Option<(usize, u64, &'static str)> {
    const ESR_EC_SVC64: u32 = 0x15;
    let esr_ec = (context.esr_el1 >> 26) & 0x3f;
    if esr_ec != ESR_EC_SVC64 {
        return None;
    }
    let frame = frame?;
    // At this seam the AArch64 trap frame mirrors vector GPRs directly.
    // `syscall_num`/`args` are populated later by aarch64::trap::handle_trap_entry,
    // so staging must decode from architectural syscall ABI registers.
    Some((
        frame.user_gpr(crate::arch::aarch64::syscall_abi::REG_X8),
        frame.user_gpr(crate::arch::aarch64::syscall_abi::REG_X3) as u64,
        "aarch64",
    ))
}

#[cfg(target_arch = "x86_64")]
fn shared_recv_timeout_staging_info(
    context: ArchTrapContext,
    frame: Option<&TrapFrame>,
) -> Option<(usize, u64, &'static str)> {
    const VEC_SYSCALL: u8 = 0x80;
    if context.vector != VEC_SYSCALL {
        return None;
    }
    let frame = frame?;
    Some((frame.syscall_num(), frame.arg(3) as u64, "x86_64"))
}

#[cfg(target_arch = "riscv64")]
fn shared_recv_timeout_staging_info(
    _context: ArchTrapContext,
    _frame: Option<&TrapFrame>,
) -> Option<(usize, u64, &'static str)> {
    None
}

pub fn dispatch_trap_entry_with_shared_kernel(
    shared: &crate::runtime::SharedKernel,
    cpu: CpuId,
    context: ArchTrapContext,
    frame: Option<&mut TrapFrame>,
) -> Result<(), TrapHandleError> {
    handle_trap_entry_shared(shared, cpu, context, frame)
}

#[cfg(not(any(
    target_arch = "riscv64",
    target_arch = "x86_64",
    target_arch = "aarch64"
)))]
compile_error!("unsupported target_arch for arch::trap_entry");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_arch_decoder_is_callable() {
        let _ = decode_trap_context;
        let _ = handle_trap_entry;
    }
}
