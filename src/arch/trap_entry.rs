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
pub(crate) fn post_switch_restore_arch_thread_state(
    kernel: &mut KernelState,
    cpu: CpuId,
    frame: Option<&mut TrapFrame>,
) -> Result<(), TrapHandleError> {
    super::x86_64::trap::restore_arch_thread_state(kernel, cpu, frame)
}

#[cfg(target_arch = "aarch64")]
pub(crate) fn post_switch_restore_arch_thread_state(
    kernel: &mut KernelState,
    cpu: CpuId,
    frame: Option<&mut TrapFrame>,
) -> Result<(), TrapHandleError> {
    super::aarch64::trap::restore_arch_thread_state_post_switch(kernel, cpu, frame)
}

#[cfg(target_arch = "riscv64")]
pub(crate) fn post_switch_restore_arch_thread_state(
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
            // Stage 160C: import the decoded syscall ABI into the frame BEFORE the
            // split dispatch inspects it (AArch64-only, proof-knob-gated; no-op on
            // x86_64/riscv64). Without this the AArch64 split dispatch sees nr=0
            // and always falls back (Stage 160B).
            pre_split_import_syscall_abi(frame);
            if let Some(result) =
                crate::kernel::syscall_split::try_split_dispatch_into_frame(shared, cpu, frame)
            {
                match result {
                    Ok(()) => {
                        // Stage 160C: a HANDLED split syscall must return to
                        // userspace via the arch syscall-return ABI (export results
                        // + advance past the trap instruction). AArch64-only and
                        // proof-knob-gated; no-op on x86_64/riscv64, whose trap
                        // return already does this from the ret lanes.
                        finalize_split_handled_syscall(shared, cpu, frame);
                        crate::yarm_log!(
                            "YARM_LOCK_SPLIT_DISPATCH nr={} cpu={} result=ok",
                            frame.syscall_num(),
                            cpu.0,
                        );
                        // task_switched == false (no scheduler interaction); skip
                        // the global lock entirely.
                        return Ok(());
                    }
                    // Stage 159BC/D parity fix: a NORMAL syscall error produced on
                    // the split fast path (e.g. the recv-v2 queued-split rollback
                    // returning InvalidArgs after an undersized writeback, having
                    // already rolled the materialized cap back) must be encoded
                    // into the trap frame and returned to userspace — exactly as
                    // the global-lock path does in `KernelState::handle_trap`
                    // (boot/fault_state.rs). All three arch entry points treat an
                    // `Err(TrapHandleError)` return as a FATAL kernel halt, so
                    // propagating a normal syscall error here turned an expected
                    // user-visible error into a fatal trap dump. The split path
                    // stashes no switch plan, so returning `Ok` here is complete.
                    //
                    // PageFault is encoded as an error code (conservative,
                    // non-fatal) rather than killing the task; the global-lock
                    // path retains the genuine task-fault semantics.
                    Err(TrapHandleError::Syscall(e)) => {
                        frame.set_err(e.code());
                        // Stage 160C: same arch syscall-return ABI as the success
                        // arm — the error code must reach userspace (AArch64 via the
                        // user GPR lanes) and the SVC must advance (this is a
                        // completed syscall, not a WouldBlock retry).
                        finalize_split_handled_syscall(shared, cpu, frame);
                        crate::yarm_log!(
                            "YARM_LOCK_SPLIT_DISPATCH nr={} cpu={} result=handled_err code={}",
                            frame.syscall_num(),
                            cpu.0,
                            e.code(),
                        );
                        return Ok(());
                    }
                    // MissingTrapFrame (and any future non-syscall variant) is a
                    // genuine kernel-side failure; propagate it unchanged.
                    Err(other) => {
                        crate::yarm_log!(
                            "YARM_LOCK_SPLIT_DISPATCH nr={} cpu={} result=err",
                            frame.syscall_num(),
                            cpu.0,
                        );
                        return Err(other);
                    }
                }
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
            // Stage 120: diagnostic-only x86_64 proof hook. Default-off and
            // one-shot; when enabled it stashes a normal DispatchSwitchPlan
            // before regular trap handling, so the existing Stage 117 drain
            // below proves the unlocked switch_frames path without changing
            // scheduler policy or syscall ABI.
            #[cfg(target_arch = "x86_64")]
            kernel
                .maybe_run_d6_controlled_switch_proof()
                .map_err(|err| {
                    TrapHandleError::Syscall(crate::kernel::syscall::SyscallError::from(err))
                })?;
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

    // Stage 188A: dispatch-return delivery channel drain. With the broad
    // `&mut KernelState` borrow dropped above, execute any post-boundary work a
    // handler stashed under the broad borrow, through `&SharedKernel` seams.
    // Infrastructure only in Stage 188A: no live handler stashes work, so the
    // stash is empty on every production trap and this is a no-op (one-shot
    // `DISPATCH_RETURN_CHANNEL_READY mode=helper_only`). Placed FIRST among the
    // post-`with_cpu` drains so a future blocked-waiter delivery completes before
    // any context-switch drain.
    shared.drain_dispatch_post_work(cpu)?;

    // Stage 167 (D6-GENUINE-A): first LIVE production use of the rank-1
    // scheduler split seam. With the global `SpinLock<KernelState>` guard from
    // `with_cpu` already dropped above, run one genuine `local_dispatch_step_split`
    // observation through `SharedKernel::with_scheduler_split_mut`, holding ONLY
    // the scheduler lock. Default-off behind `yarm.d6_genuine=1`; mutually
    // exclusive with the proof/switch-a knobs so those paths stay intact. The
    // observation is non-mutating, so it cannot double-advance the run queue;
    // the authoritative dispatch decision was already taken by the in-lock
    // `local_dispatch_step_split` inside `with_cpu` (the preserved fallback).
    // Stage 168B/169: capture the D2 recv/send deferral state once — the drains
    // below clear it, and the D6 block must know a D2 drain ran so it does not
    // also run a spurious observation this cycle.
    #[cfg(target_arch = "x86_64")]
    let d2_recv_was_deferred = crate::kernel::boot::d2_recv_dispatch_is_deferred(cpu_idx);
    #[cfg(target_arch = "x86_64")]
    let d2_send_was_deferred = crate::kernel::boot::d2_send_dispatch_is_deferred(cpu_idx);
    // Stage 192A: capture the FutexWait queue-advancing dispatch deferral state (set by the
    // in-lock `futex_wait_current`); its drain below clears it.
    #[cfg(target_arch = "x86_64")]
    let futex_wait_was_deferred = crate::kernel::boot::futex_wait_dispatch_is_deferred(cpu_idx);
    // Stage 192B: capture the Yield queue-advancing dispatch deferral state (set by the
    // in-lock `yield_current`); its drain below clears it.
    #[cfg(target_arch = "x86_64")]
    let yield_was_deferred = crate::kernel::boot::yield_dispatch_is_deferred(cpu_idx);

    // Stage 169 (D2-GENUINE-SEND): drain the deferred blocking-SEND queue-
    // advancing dispatch OUTSIDE the global lock (mirrors the recv drain below).
    #[cfg(target_arch = "x86_64")]
    if !crate::kernel::boot::d6_controlled_switch_proof_enabled()
        && !crate::kernel::boot::d6_switch_a_enabled()
        && d2_send_was_deferred
    {
        crate::yarm_log!("D2_SEND_GENUINE_GLOBAL_DROPPED cpu={}", cpu.0);
        let outgoing = crate::kernel::boot::d2_send_dispatch_outgoing(cpu_idx);
        // Re-verify the deferred sender is still Blocked(EndpointSend).
        let reverify_ok = outgoing
            .map(|t| shared.d2_send_reverify_blocked(t))
            .unwrap_or(false);
        if reverify_ok {
            if let Some(t) = outgoing {
                crate::yarm_log!("D2_SEND_GENUINE_DISPATCH_REVERIFY_OK tid={}", t);
            }
            crate::yarm_log!("D2_SEND_GENUINE_DISPATCH_ENTER cpu={}", cpu.0);
            let incoming = shared.d2_send_dispatch_step_mut(cpu);
            if let Some(inc) = incoming {
                shared.d6_genuine_mark_running_via_task_seam(incoming);
                // Dormant kernel-thread switch_frames variant (user-task sender
                // resumes via trap-frame restore + syscall restart).
                if shared.d2_recv_incoming_has_kernel_switch_ctx(inc) {
                    crate::yarm_log!(
                        "D2_SEND_GENUINE_SWITCH_STASHED outgoing={:?} incoming={}",
                        outgoing,
                        inc
                    );
                    crate::yarm_log!(
                        "D2_SEND_GENUINE_SWITCH_ENTER outgoing={:?} incoming={}",
                        outgoing,
                        inc
                    );
                    crate::yarm_log!("D2_SEND_GENUINE_FIRST_RESUME incoming={}", inc);
                }
                let restore = shared
                    .with_cpu(cpu, |kernel| {
                        kernel.d2_recv_switch_incoming_asid(inc);
                        post_switch_restore_arch_thread_state(kernel, cpu, frame.as_deref_mut())
                    })
                    .map_err(|err| TrapHandleError::Syscall(err.into()));
                crate::kernel::boot::d2_send_dispatch_clear(cpu_idx);
                restore??;
                let n = crate::kernel::boot::D2_SEND_DISPATCH_COUNT
                    .fetch_add(1, core::sync::atomic::Ordering::Relaxed)
                    + 1;
                crate::yarm_log!(
                    "D2_SEND_GENUINE_DISPATCH_DONE result=switch cpu={} incoming={:?} count={}",
                    cpu.0,
                    incoming,
                    n
                );
            } else {
                crate::kernel::boot::d2_send_dispatch_clear(cpu_idx);
                crate::yarm_log!("D2_SEND_GENUINE_DISPATCH_DONE result=idle cpu={}", cpu.0);
            }
        } else {
            crate::yarm_log!(
                "D2_SEND_GENUINE_FALLBACK reason=state_changed cpu={}",
                cpu.0
            );
            crate::kernel::boot::d2_send_dispatch_clear(cpu_idx);
        }
    }
    #[cfg(target_arch = "x86_64")]
    if !crate::kernel::boot::d6_controlled_switch_proof_enabled()
        && !crate::kernel::boot::d6_switch_a_enabled()
        && d2_recv_was_deferred
    {
        // Stage 168B (D2-GENUINE-RECV completion): drain the deferred
        // blocking-recv queue-advancing dispatch OUTSIDE the global lock. The
        // in-lock `block_current_on_receive_with_deadline` published the waiter
        // and marked the recv task `Blocked`, then declined to dispatch in-lock
        // (D2_RECV_GENUINE_NO_INLOCK_DISPATCH). We now run the single
        // authoritative `dispatch_next_on` under ONLY the rank-1 scheduler seam
        // (global lock genuinely dropped) and perform the arch thread-state
        // restore via the hardened D6-SWITCH-A post-switch re-acquire.
        crate::yarm_log!("D2_RECV_GENUINE_GLOBAL_DROPPED cpu={}", cpu.0);
        let outgoing = crate::kernel::boot::d2_recv_dispatch_outgoing(cpu_idx);
        // Re-verify the deferred recv task is still Blocked(EndpointReceive).
        let reverify_ok = outgoing
            .map(|t| shared.d2_recv_reverify_blocked(t))
            .unwrap_or(false);
        if reverify_ok {
            if let Some(t) = outgoing {
                crate::yarm_log!("D2_RECV_GENUINE_DISPATCH_REVERIFY_OK tid={}", t);
            }
            crate::yarm_log!("D2_RECV_GENUINE_DISPATCH_ENTER cpu={}", cpu.0);
            let incoming = shared.d2_recv_dispatch_step_mut(cpu);
            if let Some(inc) = incoming {
                // Commit the selected task Running via the rank-2 task seam.
                shared.d6_genuine_mark_running_via_task_seam(incoming);
                // Dormant kernel-thread switch_frames variant (user-task recv
                // resumes via trap-frame restore + syscall restart, so this does
                // not fire for the recv workload).
                if shared.d2_recv_incoming_has_kernel_switch_ctx(inc) {
                    crate::yarm_log!(
                        "D2_RECV_GENUINE_SWITCH_STASHED outgoing={:?} incoming={}",
                        outgoing,
                        inc
                    );
                    crate::yarm_log!(
                        "D2_RECV_GENUINE_SWITCH_ENTER outgoing={:?} incoming={}",
                        outgoing,
                        inc
                    );
                    crate::yarm_log!("D2_RECV_GENUINE_FIRST_RESUME incoming={}", inc);
                }
                // Restore the incoming task's arch thread state (frame + CR3).
                // The dispatch above already ran lock-free; this brief re-acquire
                // only performs the arch restore, exactly as D6-SWITCH-A does.
                let restore = shared
                    .with_cpu(cpu, |kernel| {
                        kernel.d2_recv_switch_incoming_asid(inc);
                        post_switch_restore_arch_thread_state(kernel, cpu, frame.as_deref_mut())
                    })
                    .map_err(|err| TrapHandleError::Syscall(err.into()));
                crate::kernel::boot::d2_recv_dispatch_clear(cpu_idx);
                restore??;
                let n = crate::kernel::boot::D2_RECV_DISPATCH_COUNT
                    .fetch_add(1, core::sync::atomic::Ordering::Relaxed)
                    + 1;
                crate::yarm_log!(
                    "D2_RECV_GENUINE_DISPATCH_DONE result=switch cpu={} incoming={:?} count={}",
                    cpu.0,
                    incoming,
                    n
                );
            } else {
                crate::kernel::boot::d2_recv_dispatch_clear(cpu_idx);
                crate::yarm_log!("D2_RECV_GENUINE_DISPATCH_DONE result=idle cpu={}", cpu.0);
            }
        } else {
            crate::yarm_log!(
                "D2_RECV_GENUINE_FALLBACK reason=state_changed cpu={}",
                cpu.0
            );
            crate::kernel::boot::d2_recv_dispatch_clear(cpu_idx);
        }
    }

    // Stage 192A (FUTEXWAIT QUEUE-ADVANCING DISPATCH): drain the deferred FutexWait
    // queue-advancing dispatch OUTSIDE the global lock — the direct analogue of the
    // D2-GENUINE recv drain above. The in-lock `futex_wait_current` published
    // `Blocked(Futex)` + `block_current` (removing the waiter from `current`) and declined
    // the in-lock dispatch, so `dispatch_next_on` here genuinely dequeues the next runnable
    // task (or idles). We re-verify the waiter is still `Blocked(Futex)`, run the
    // authoritative dispatch under only the rank-1 scheduler seam, mark the incoming task
    // Running (rank-2), then a brief `with_cpu` re-acquire performs ONLY the arch restore
    // (incoming ASID/CR3 switch + trap-frame restore) via the hardened D6-SWITCH-A path.
    #[cfg(target_arch = "x86_64")]
    if !crate::kernel::boot::d6_controlled_switch_proof_enabled()
        && !crate::kernel::boot::d6_switch_a_enabled()
        && futex_wait_was_deferred
    {
        crate::yarm_log!("QUEUE_ADVANCING_DISPATCH_BEGIN cpu={}", cpu.0);
        let outgoing = crate::kernel::boot::futex_wait_dispatch_outgoing(cpu_idx);
        let reverify_ok = outgoing
            .map(|t| shared.futex_wait_reverify_blocked(t))
            .unwrap_or(false);
        if reverify_ok {
            // Queue-advancing dequeue (emits QUEUE_ADVANCING_DISPATCH_DEQUEUE_OK).
            let incoming = shared.futex_wait_dispatch_step_mut(cpu);
            if let Some(inc) = incoming {
                shared.d6_genuine_mark_running_via_task_seam(incoming);
                crate::yarm_log!(
                    "QUEUE_ADVANCING_DISPATCH_CURRENT_SET_OK cpu={} tid={}",
                    cpu.0,
                    inc
                );
                let restore = shared
                    .with_cpu(cpu, |kernel| {
                        kernel.d2_recv_switch_incoming_asid(inc);
                        post_switch_restore_arch_thread_state(kernel, cpu, frame.as_deref_mut())
                    })
                    .map_err(|err| TrapHandleError::Syscall(err.into()));
                crate::kernel::boot::futex_wait_dispatch_clear(cpu_idx);
                restore??;
                crate::yarm_log!(
                    "QUEUE_ADVANCING_DISPATCH_FRAME_OK cpu={} tid={}",
                    cpu.0,
                    inc
                );
                let n = crate::kernel::boot::FUTEX_WAIT_DISPATCH_COUNT
                    .fetch_add(1, core::sync::atomic::Ordering::Relaxed)
                    + 1;
                crate::yarm_log!(
                    "FUTEX_WAIT_SPLIT_DISPATCH_OK cpu={} incoming={} count={}",
                    cpu.0,
                    inc,
                    n
                );
                crate::yarm_log!("QUEUE_ADVANCING_DISPATCH_DONE result=ok");
                crate::yarm_log!("FUTEX_WAIT_SPLIT_DONE result=blocked");
                crate::kernel::boot::maybe_log_futex_wait_retired();
            } else {
                // Nothing else runnable ⇒ idle (same as the D2 recv idle branch).
                crate::kernel::boot::futex_wait_dispatch_clear(cpu_idx);
                crate::yarm_log!("FUTEX_WAIT_SPLIT_DISPATCH_OK cpu={} incoming=idle", cpu.0);
                crate::yarm_log!("QUEUE_ADVANCING_DISPATCH_DONE result=ok");
                crate::yarm_log!("FUTEX_WAIT_SPLIT_DONE result=blocked");
                crate::kernel::boot::maybe_log_futex_wait_retired();
            }
        } else {
            // A FutexWake (or in-lock fallback) already changed the waiter's state — do NOT
            // dispatch it away; fall through so the trap returns to the re-runnable task.
            crate::yarm_log!(
                "QUEUE_ADVANCING_DISPATCH_DEFERRED reason=state_changed cpu={}",
                cpu.0
            );
            crate::kernel::boot::futex_wait_dispatch_clear(cpu_idx);
        }
    }

    // Stage 192B (YIELD QUEUE-ADVANCING DISPATCH): drain the deferred Yield queue-advancing
    // dispatch OUTSIDE the global lock — the preempt sibling of the FutexWait drain above.
    // The in-lock `yield_current` set the caller Runnable, RE-ENQUEUED it, and cleared
    // `current`, so `dispatch_next_on` here genuinely dequeues the next runnable task (the
    // FIFO head — the re-enqueued caller itself when alone). We re-verify `current` is still
    // cleared, run the authoritative dispatch under only the rank-1 scheduler seam, mark the
    // incoming task Running (rank-2), then a brief `with_cpu` re-acquire performs ONLY the
    // arch restore (incoming ASID/CR3 switch + trap-frame restore) via the D6-SWITCH-A path.
    #[cfg(target_arch = "x86_64")]
    if !crate::kernel::boot::d6_controlled_switch_proof_enabled()
        && !crate::kernel::boot::d6_switch_a_enabled()
        && yield_was_deferred
    {
        crate::yarm_log!("YIELD_DISPATCH_DEFER_BEGIN cpu={} drain=1", cpu.0);
        let reverify_ok = shared.yield_reverify_ready(cpu);
        if reverify_ok {
            let incoming = shared.yield_dispatch_step_mut(cpu);
            if let Some(inc) = incoming {
                shared.d6_genuine_mark_running_via_task_seam(incoming);
                crate::yarm_log!("YIELD_DISPATCH_CURRENT_SET_OK cpu={} tid={}", cpu.0, inc);
                let restore = shared
                    .with_cpu(cpu, |kernel| {
                        kernel.d2_recv_switch_incoming_asid(inc);
                        post_switch_restore_arch_thread_state(kernel, cpu, frame.as_deref_mut())
                    })
                    .map_err(|err| TrapHandleError::Syscall(err.into()));
                crate::kernel::boot::yield_dispatch_clear(cpu_idx);
                restore??;
                crate::yarm_log!("YIELD_DISPATCH_FRAME_OK cpu={} tid={}", cpu.0, inc);
                let n = crate::kernel::boot::YIELD_DISPATCH_COUNT
                    .fetch_add(1, core::sync::atomic::Ordering::Relaxed)
                    + 1;
                crate::yarm_log!(
                    "YIELD_DISPATCH_DONE result=ok cpu={} incoming={} count={}",
                    cpu.0,
                    inc,
                    n
                );
                crate::kernel::boot::maybe_log_yield_retired();
            } else {
                // Unreachable in practice (the re-enqueued caller is always a candidate),
                // but handle idle defensively.
                crate::kernel::boot::yield_dispatch_clear(cpu_idx);
                crate::yarm_log!("YIELD_DISPATCH_DONE result=ok cpu={} incoming=idle", cpu.0);
                crate::kernel::boot::maybe_log_yield_retired();
            }
        } else {
            // An in-lock fallback already dispatched — do NOT double-dispatch.
            crate::yarm_log!("YIELD_DISPATCH_DEFERRED reason=state_changed cpu={}", cpu.0);
            crate::kernel::boot::yield_dispatch_clear(cpu_idx);
        }
    }

    #[cfg(target_arch = "x86_64")]
    {
        let d6_genuine_mode = crate::kernel::boot::d6_genuine_enabled()
            && !crate::kernel::boot::d6_controlled_switch_proof_enabled()
            && !crate::kernel::boot::d6_switch_a_enabled()
            && !d2_recv_was_deferred
            && !d2_send_was_deferred
            // Stage 192A: a FutexWait drain ran the authoritative dispatch this cycle;
            // skip the spurious d6 observation (mirrors the D2 recv/send exclusion).
            && !futex_wait_was_deferred
            // Stage 192B: same exclusion for a Yield drain cycle.
            && !yield_was_deferred;
        if d6_genuine_mode {
            if crate::kernel::boot::d6_genuine_dispatch_is_deferred(cpu_idx) {
                // Stage 168 (D6-GENUINE-B): the in-lock `dispatch_next_task`
                // declined to perform the authoritative mutating dispatch for
                // this eligible, queue-neutral cycle. Perform it now through the
                // rank-1 scheduler seam with the global lock genuinely dropped —
                // this is the single authoritative `local_dispatch_step_split`
                // for the cycle.
                crate::yarm_log!("D6_GENUINE_MUT_DISPATCH_GLOBAL_DROPPED cpu={}", cpu.0);
                // Re-verify queue-neutrality out of lock (single-CPU, IRQ-off ⇒
                // unchanged unless an in-lock fallback superseded the deferral).
                if shared.d6_genuine_dispatch_queue_neutral(cpu) {
                    crate::yarm_log!("D6_GENUINE_MUT_DISPATCH_ENTER cpu={}", cpu.0);
                    let incoming = shared.d6_genuine_local_dispatch_step_mut(cpu);
                    // Deferred Phase B (idempotent for the same running task).
                    shared.d6_genuine_mark_running_via_task_seam(incoming);
                    let n = crate::kernel::boot::D6_GENUINE_MUT_DISPATCH_COUNT
                        .fetch_add(1, core::sync::atomic::Ordering::Relaxed)
                        + 1;
                    crate::yarm_log!(
                        "D6_GENUINE_MUT_DISPATCH_DONE cpu={} incoming={:?}",
                        cpu.0,
                        incoming
                    );
                    crate::yarm_log!("D6_GENUINE_MUT_DISPATCH_COUNT value={}", n);
                } else {
                    crate::yarm_log!(
                        "D6_GENUINE_MUT_DISPATCH_FALLBACK reason=state_changed cpu={}",
                        cpu.0
                    );
                }
                crate::kernel::boot::d6_genuine_dispatch_clear_deferred(cpu_idx);
            } else {
                // Stage 167 observation: no dispatch was deferred this cycle;
                // prove the scheduler seam still executes live outside the
                // global lock (non-mutating).
                crate::yarm_log!("D6_LOCAL_DISPATCH_SEAM_CANDIDATE cpu={}", cpu.0);
                let eligible = shared.online_cpu_count_split_read() <= 1
                    && cpu_idx < crate::kernel::scheduler::MAX_CPUS;
                if eligible {
                    crate::yarm_log!("D6_LOCAL_DISPATCH_SEAM_ENTER cpu={}", cpu.0);
                    crate::yarm_log!("D6_LOCAL_DISPATCH_SEAM_LOCK_SCOPE_DROPPED cpu={}", cpu.0);
                    let observed = shared.d6_genuine_local_dispatch_observe(cpu);
                    let n = crate::kernel::boot::D6_GENUINE_SEAM_COUNT[cpu_idx]
                        .fetch_add(1, core::sync::atomic::Ordering::Relaxed)
                        + 1;
                    crate::yarm_log!(
                        "D6_LOCAL_DISPATCH_SEAM_COUNT cpu={} n={} tid={:?}",
                        cpu.0,
                        n,
                        observed
                    );
                    crate::yarm_log!("D6_LOCAL_DISPATCH_SEAM_DONE cpu={}", cpu.0);
                } else {
                    crate::yarm_log!("D6_LOCAL_DISPATCH_SEAM_FALLBACK cpu={}", cpu.0);
                }
            }
        }
    }

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
            // Stage 166 (D6-SWITCH-A): tag this as a real production unlocked
            // switch when driven by `yarm.d6_switch_a=1` (proof knob off).
            #[cfg(target_arch = "x86_64")]
            let d6_switch_a_mode = crate::kernel::boot::d6_switch_a_enabled()
                && !crate::kernel::boot::d6_controlled_switch_proof_enabled();
            #[cfg(not(target_arch = "x86_64"))]
            let d6_switch_a_mode = false;
            crate::yarm_log!(
                "D6_GLOBAL_LOCK_DROPPED_BEFORE_SWITCH outgoing={} incoming={}",
                plan.outgoing_tid,
                plan.incoming_tid
            );
            if d6_switch_a_mode {
                crate::yarm_log!(
                    "D6_SWITCH_A_LOCK_DROPPED outgoing={} incoming={}",
                    plan.outgoing_tid,
                    plan.incoming_tid
                );
            }
            crate::yarm_log!(
                "D6_SWITCH_FRAMES_ENTER_UNLOCKED outgoing={} incoming={}",
                plan.outgoing_tid,
                plan.incoming_tid
            );
            if d6_switch_a_mode {
                crate::yarm_log!(
                    "D6_SWITCH_A_SWITCH_ENTER outgoing={} incoming={}",
                    plan.outgoing_tid,
                    plan.incoming_tid
                );
            }
            // Stage 118 Part D: detect first-resume path (x86_64 only).
            // If the incoming frame's RIP points to the trampoline, stash a
            // FirstResumeContext so the trampoline can switch back after
            // calling post_switch_restore_arch_thread_state.
            #[cfg(target_arch = "x86_64")]
            {
                unsafe extern "C" {
                    fn yarm_kernel_thread_switch_trampoline() -> !;
                }
                let trampoline_ip = yarm_kernel_thread_switch_trampoline as *const () as usize;
                // SAFETY: incoming_frame_ptr is stable (KernelState::tcbs fixed array).
                let incoming_ip = unsafe { (*plan.incoming_frame_ptr).instruction_ptr() };
                if incoming_ip == trampoline_ip {
                    let ctx = crate::kernel::boot::FirstResumeContext {
                        cpu_id: cpu,
                        incoming_tid: plan.incoming_tid,
                        outgoing_frame_ptr: plan.outgoing_frame_ptr as *const _,
                        incoming_frame_ptr: plan.incoming_frame_ptr,
                        outgoing_stack_top: plan.outgoing_stack_top,
                    };
                    // SAFETY: single CPU, interrupts disabled.
                    unsafe {
                        crate::kernel::boot::FIRST_RESUME_STASH[cpu_idx].store(ctx);
                    }
                }
            }
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
            // POINT 2: execution resumes here when the outgoing task is switched
            // back in (either by the normal scheduler or by the first-resume
            // trampoline switching back after post_switch_restore).
            crate::yarm_log!(
                "D6_SWITCH_FRAMES_RETURNED_UNLOCKED outgoing={} incoming={}",
                plan.outgoing_tid,
                plan.incoming_tid
            );
            if d6_switch_a_mode {
                crate::yarm_log!(
                    "D6_SWITCH_A_RETURNED outgoing={} incoming={}",
                    plan.outgoing_tid,
                    plan.incoming_tid
                );
            }
            // Stage 139: hardware CR3 snapshot at POINT 2, before proof cleanup
            // restores the correct address space.  The proof path does not touch
            // CR3 in switch_frames or the trampoline, so this captures any
            // divergence introduced by the proof's lock-drop switch.
            #[cfg(all(target_arch = "x86_64", not(feature = "hosted-dev")))]
            {
                let hw_cr3 = crate::arch::x86_64::page_table::read_hw_cr3();
                crate::yarm_log!("D6_PROOF_CR3_AFTER_SWITCH_BACK cr3=0x{:016x}", hw_cr3);
            }
            let is_proof_done =
                if crate::kernel::boot::d6_controlled_switch_proof_take_pending_done() {
                    crate::kernel::boot::d6_controlled_switch_proof_mark_done();
                    crate::yarm_log!("D6_CONTROLLED_SWITCH_PROOF_DONE");
                    crate::yarm_log!("D6_CONTROLLED_SWITCH_PROOF_CLEANUP_BEGIN");
                    // Dispatch stash was consumed by take() above — re-verify empty.
                    let dispatch_clear = unsafe {
                        !crate::kernel::boot::DISPATCH_SWITCH_PLAN_STASH[cpu_idx].has_plan()
                    };
                    // First-resume stash was consumed by the trampoline — verify empty.
                    let resume_clear = unsafe {
                        crate::kernel::boot::FIRST_RESUME_STASH[cpu_idx]
                            .take()
                            .is_none()
                    };
                    if dispatch_clear && resume_clear {
                        crate::yarm_log!("D6_CONTROLLED_SWITCH_PROOF_STASH_CLEAR_OK");
                    }
                    // PENDING_DONE was swapped to false by take_pending_done; verify.
                    let pending_clear =
                        !crate::kernel::boot::D6_CONTROLLED_SWITCH_PROOF_PENDING_DONE
                            .load(core::sync::atomic::Ordering::Acquire);
                    // GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE was cleared before the stash drain.
                    let trap_path_clear = cpu_idx >= crate::kernel::scheduler::MAX_CPUS
                        || !crate::kernel::boot::GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE[cpu_idx]
                            .load(core::sync::atomic::Ordering::Relaxed);
                    if pending_clear && trap_path_clear {
                        crate::yarm_log!("D6_CONTROLLED_SWITCH_PROOF_STATE_CLEAR_OK");
                    }
                    true
                } else {
                    false
                };
            // Re-acquire the global lock to restore the incoming task's arch thread
            // state (populate its trap frame with its user-mode register context).
            shared
                .with_cpu(cpu, |kernel| {
                    let result =
                        post_switch_restore_arch_thread_state(kernel, cpu, frame.as_deref_mut());
                    if is_proof_done {
                        #[cfg(target_arch = "x86_64")]
                        kernel.d6_emit_proof_cleanup_arch_markers();
                        // Stage 133: verify ASID 1 maps the fault page before emitting DONE.
                        #[cfg(target_arch = "x86_64")]
                        kernel.d6_check_asid1_stack_page_mapped();
                        // Stage 165D: the proof restored CR3 to asid 1, but normal
                        // scheduling/trap/idle can land a post-cleanup trap on
                        // another task's kernel stack (observed: tid=3) while asid 1
                        // is active — and per-task kernel stacks are mapped only in
                        // their own root.  Share every live task's kernel stack
                        // pages into the active root and all task roots so no
                        // post-cleanup trap faults on a supervisor stack write.
                        #[cfg(all(target_arch = "x86_64", not(feature = "hosted-dev")))]
                        if let Err(err) = kernel.d6_ensure_post_cleanup_task_stacks_mapped() {
                            crate::yarm_log!("D6_POST_CLEANUP_STACK_MAP_FAILED err={:?}", err);
                        }
                        crate::yarm_log!("D6_CONTROLLED_SWITCH_PROOF_CLEANUP_DONE");
                        if d6_switch_a_mode {
                            crate::yarm_log!("D6_SWITCH_A_DONE");
                        }
                    }
                    result
                })
                .map_err(|err| TrapHandleError::Syscall(err.into()))??;
            // Stage 132: arm the first post-cleanup trap diagnostic.
            if is_proof_done {
                let cpu_idx_set = cpu.0 as usize;
                if cpu_idx_set < crate::kernel::scheduler::MAX_CPUS {
                    crate::kernel::boot::D6_POST_CLEANUP_DIAG_PENDING[cpu_idx_set]
                        .store(true, core::sync::atomic::Ordering::Release);
                    // Stage 133: arm the pre-lock #PF register diagnostic.
                    #[cfg(target_arch = "x86_64")]
                    crate::kernel::boot::D6_PRE_LOCK_PF_DIAG_PENDING[cpu_idx_set]
                        .store(true, core::sync::atomic::Ordering::Release);
                }
            }
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

// Stage 160C: AArch64 trap-ABI bracketing hooks for the pre-global-lock split
// dispatch. Gated behind the IPC recv oracle proof knob so the newly-enabled
// AArch64 split-dispatch path is exercised ONLY during oracle proof validation;
// a normal boot leaves the knob off, the import is skipped, the split dispatch
// keeps seeing `syscall_num=0` and falls back to the global path exactly as
// before (byte-identical). x86_64 / riscv64 are no-ops: x86_64's trap stub
// already populates the decoded ABI and returns results via the ret lanes, and
// riscv64 does not enter `handle_trap_entry_shared`.
#[cfg(target_arch = "aarch64")]
fn pre_split_import_syscall_abi(frame: &mut TrapFrame) {
    if crate::kernel::boot::ipc_recv_oracle_proof_enabled() {
        super::aarch64::trap::split_import_syscall_abi(frame);
    }
}
#[cfg(not(target_arch = "aarch64"))]
fn pre_split_import_syscall_abi(_frame: &mut TrapFrame) {}

#[cfg(target_arch = "aarch64")]
fn finalize_split_handled_syscall(
    shared: &crate::runtime::SharedKernel,
    cpu: CpuId,
    frame: &mut TrapFrame,
) {
    if crate::kernel::boot::ipc_recv_oracle_proof_enabled() {
        let _ = shared.with_cpu(cpu, |kernel| {
            super::aarch64::trap::split_finalize_handled_syscall(kernel, cpu, frame)
        });
    }
}
#[cfg(not(target_arch = "aarch64"))]
fn finalize_split_handled_syscall(
    _shared: &crate::runtime::SharedKernel,
    _cpu: CpuId,
    _frame: &mut TrapFrame,
) {
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
