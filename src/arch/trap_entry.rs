// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use crate::arch::trap::TrapEvent;
use crate::kernel::boot::{FaultBookkeepingMode, KernelState, TrapHandleError};
use crate::kernel::scheduler::CpuId;
use crate::kernel::trapframe::TrapFrame;
// Stage 199D-WA3A-R2-SEAL (item E): every dispatch-mark consumer in this file matches all five
// outcomes explicitly; `RefusedTorn` reaches `dispatch_torn_fatal` and never returns.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
use crate::runtime::{DispatchMarkOutcome as Mark, dispatch_torn_fatal};

/// Stage 197 (FIRST-COHORT SEAL): arch tag for the `YARM_LOCK_SPLIT_DISPATCH`
/// marker — canonical `arch=<arch> ` on every architecture (x86_64 normalized from
/// the historical untagged text). RISC-V's split dispatch runs through its own
/// shared wrapper (which emits `arch=riscv64` directly), so this const is only
/// live on x86_64/AArch64, but it is defined per-arch for correctness.
#[cfg(target_arch = "aarch64")]
const SPLIT_DISPATCH_ARCH_TAG: &str = "arch=aarch64 ";
#[cfg(target_arch = "x86_64")]
const SPLIT_DISPATCH_ARCH_TAG: &str = "arch=x86_64 ";
#[cfg(target_arch = "riscv64")]
const SPLIT_DISPATCH_ARCH_TAG: &str = "arch=riscv64 ";

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
    kernel: &mut KernelState,
    cpu: CpuId,
    frame: Option<&mut TrapFrame>,
) -> Result<(), TrapHandleError> {
    // Stage 196A (Part 5): RISC-V now enters the shared trap path
    // (`handle_riscv_trap_entry_shared`), but no queue-advancing retirement class
    // is enabled yet, so the switch-plan stash is still never populated on RISC-V
    // and this remains uncalled in production. It is no longer a silent no-op: it
    // delegates to the documented `restore_arch_thread_state_post_switch`
    // FOUNDATION, so a future switch drain has a real, exercisable frame-restore
    // API (incoming sepc/sstatus/GPR/TLS). The incoming task's SATP/ASID
    // activation (with `sfence.vma`) is performed by the trap bridge today; a
    // future genuine cross-task switch drain would pair that with this restore.
    super::riscv64::trap::restore_arch_thread_state_post_switch(kernel, cpu, frame)
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
            // Stage 199D: capture the EXACT entering task identity {tid, asid} BEFORE the
            // split dispatch runs. The AArch64 return path must commit this syscall's
            // register state into THIS incarnation, never into whatever task an unqualified
            // "current" lookup would find afterwards — a direct NR6/NR7 transaction wakes and
            // enqueues another task, so that lookup is not a safe question to ask after it.
            let entering = split_return_identity(shared, cpu);
            if let Some(result) =
                crate::kernel::syscall_split::try_split_dispatch_into_frame(shared, cpu, frame)
            {
                match result {
                    Ok(()) => {
                        // Stage 160C: a HANDLED split syscall must return to
                        // userspace via the arch syscall-return ABI (export results
                        // + advance past the trap instruction). AArch64-only;
                        // no-op on x86_64/riscv64, whose trap return already does
                        // this from the ret lanes.
                        finalize_split_handled_syscall(shared, cpu, entering, frame);
                        crate::yarm_log!(
                            "YARM_LOCK_SPLIT_DISPATCH {}nr={} cpu={} result=ok",
                            SPLIT_DISPATCH_ARCH_TAG,
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
                        finalize_split_handled_syscall(shared, cpu, entering, frame);
                        crate::yarm_log!(
                            "YARM_LOCK_SPLIT_DISPATCH {}nr={} cpu={} result=handled_err code={}",
                            SPLIT_DISPATCH_ARCH_TAG,
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

    // Stage 200D-0B3: the x86_64 broad-lock-release attestation, emitted HERE — the first
    // statement after `with_cpu` returned — and nowhere else. Stage 200D-0B1 emitted this from
    // inside the arch handler, where the guard was still held; that is the false claim this
    // stage removes. A strict no-op unless the in-lock consumer armed the latch on this CPU,
    // so no ordinary trap pays for it.
    #[cfg(target_arch = "x86_64")]
    if let Some((exit_tid, exit_asid)) = crate::kernel::boot::advance_exit_attestation(
        cpu_idx,
        crate::kernel::boot::EXIT_ATTEST_CONSUMED,
        crate::kernel::boot::EXIT_ATTEST_LOCK_RELEASED,
    ) {
        crate::yarm_log!(
            "EXIT_TASK_BROAD_LOCK_RELEASED arch=x86_64 tid={} asid={} cpu={} broad_lock=0 holder=with_cpu result=ok",
            exit_tid,
            exit_asid.0,
            cpu.0
        );
    }

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
            let dispatch = shared.d2_send_dispatch_step_mut(cpu);
            // Stage 199D-WA3A-R2-SEAL (item E): all five outcomes are matched explicitly, each
            // with its own evidence. `RefusedTorn` is fatal — it may never fall through to a
            // resume, an ordinary fallback dispatch, an idle halt or a return to userspace.
            let marked = match shared.d6_genuine_mark_running_via_task_seam(dispatch) {
                Mark::Marked(token) => Some(token),
                Mark::Idle => {
                    crate::yarm_log!(
                        "D2_SEND_GENUINE_DISPATCH_DECLINED cpu={} reason=idle",
                        cpu.0
                    );
                    None
                }
                Mark::RefusedRolledBack => {
                    crate::yarm_log!(
                        "D2_SEND_GENUINE_DISPATCH_DECLINED cpu={} reason=refused_dequeue_undone",
                        cpu.0
                    );
                    None
                }
                Mark::RefusedNoSchedulerChange => {
                    crate::yarm_log!(
                        "D2_SEND_GENUINE_DISPATCH_DECLINED cpu={} reason=refused_scheduler_untouched",
                        cpu.0
                    );
                    None
                }
                Mark::RefusedTorn => dispatch_torn_fatal(
                    cpu,
                    dispatch.tid().map(|t| t.0).unwrap_or(u64::MAX),
                    "d2_send_genuine_dispatch",
                ),
            };
            if let Some(token) = marked {
                let inc = token.tid();
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
                    "D2_SEND_GENUINE_DISPATCH_DONE result=switch cpu={} incoming={} count={}",
                    cpu.0,
                    inc,
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
            let dispatch = shared.d2_recv_dispatch_step_mut(cpu);
            // Stage 199D-WA3A-R2-SEAL (item E): all five outcomes are matched explicitly, each
            // with its own evidence. `RefusedTorn` is fatal — it may never fall through to a
            // resume, an ordinary fallback dispatch, an idle halt or a return to userspace.
            let marked = match shared.d6_genuine_mark_running_via_task_seam(dispatch) {
                Mark::Marked(token) => Some(token),
                Mark::Idle => {
                    crate::yarm_log!(
                        "D2_RECV_GENUINE_DISPATCH_DECLINED cpu={} reason=idle",
                        cpu.0
                    );
                    None
                }
                Mark::RefusedRolledBack => {
                    crate::yarm_log!(
                        "D2_RECV_GENUINE_DISPATCH_DECLINED cpu={} reason=refused_dequeue_undone",
                        cpu.0
                    );
                    None
                }
                Mark::RefusedNoSchedulerChange => {
                    crate::yarm_log!(
                        "D2_RECV_GENUINE_DISPATCH_DECLINED cpu={} reason=refused_scheduler_untouched",
                        cpu.0
                    );
                    None
                }
                Mark::RefusedTorn => dispatch_torn_fatal(
                    cpu,
                    dispatch.tid().map(|t| t.0).unwrap_or(u64::MAX),
                    "d2_recv_genuine_dispatch",
                ),
            };
            if let Some(token) = marked {
                let inc = token.tid();
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
                    "D2_RECV_GENUINE_DISPATCH_DONE result=switch cpu={} incoming={} count={}",
                    cpu.0,
                    inc,
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

    // ── Stage 199D (AARCH64 BLOCKER 3): the post-lock DIRECT dispatch drain ──────────────────
    //
    // Closes the last AArch64 NR6/NR7 readiness blocker: the authoritative queue-advancing
    // dispatch used to be compile-time x86_64-only, so an AArch64 wake finished under the broad
    // lock even when the direct transaction itself had not taken it.
    //
    // What makes this drain different from the FutexWait/Yield drains directly below is not the
    // scheduler policy — it deliberately reuses the SAME rank-1 dequeue
    // (`futex_wait_dispatch_step_mut`) and the SAME rank-2 mark-Running seam, so there is one
    // scheduler policy in the tree, not two. What differs is that it takes NO broad lock at any
    // point: the ASID activation and the EL0 frame/TLS restore, which those drains obtain from a
    // brief `with_cpu` re-acquire, are obtained here from bounded rank-2 task seams.
    //
    // ## Settlement is a DEBT, not a choice
    //
    // The commit that published the work item removed the caller from `current`. From that
    // instant this CPU is running NOTHING, and the trap frame we would `eret` through belongs to
    // a task the scheduler has parked. So once a live item is taken, this drain MUST settle —
    // resume exactly one task, or enter the idle primitive. There is no "declined" path back to
    // userspace, because that path would resume a parked task through a stale frame.
    //
    // In particular, observing that a reply or timeout made the outgoing caller `Runnable` does
    // NOT cancel the debt: that caller is simply back on the run queue, and the authoritative
    // dequeue may select it like any other candidate. The observation is logged and nothing
    // more.
    //
    // The one case that legitimately owes nothing is a SUPERSEDED lease: a later `current`-clear
    // on this CPU opened a newer lease and took over settlement, so this item's cycle is already
    // closed.
    #[cfg(target_arch = "aarch64")]
    if crate::kernel::direct_dispatch::is_pending(cpu_idx) {
        use crate::kernel::direct_dispatch::{self as dd, DrainOutcome, Settlement};
        // The slot state machine makes the take destructive and exclusive: exactly one drain can
        // claim a READY item, so a published debt is settled once and only once.
        let outcome = match dd::take(cpu_idx) {
            None => DrainOutcome::NoDebtNothingPublished,
            Some(work) => {
                crate::yarm_log!(
                    "AARCH64_DIRECT_DISPATCH_BEGIN cpu={} outgoing={} lease={} class={:?}",
                    cpu.0,
                    work.outgoing_tid,
                    work.lease,
                    work.class
                );
                if !dd::lease_is_current(cpu, work.lease) {
                    // A later current-clear owns settlement; this item's cycle is closed. This is
                    // the ONLY way out of the drain without settling, and it is sound precisely
                    // because the newer cycle is the one now holding the debt.
                    crate::yarm_log!(
                        "AARCH64_DIRECT_DISPATCH_SUPERSEDED cpu={} outgoing={} item_lease={} current_lease={}",
                        cpu.0,
                        work.outgoing_tid,
                        work.lease,
                        dd::current_lease(cpu)
                    );
                    DrainOutcome::NoDebtSupersededLease
                } else {
                    // Pre-mutation observation — DIAGNOSTICS ONLY. Whatever the outgoing task
                    // has become, this CPU still owes a dispatch.
                    let observed = shared.direct_dispatch_observe_outgoing_split(work);
                    crate::yarm_log!(
                        "AARCH64_DIRECT_DISPATCH_OUTGOING tid={} asid={} observed={:?} debt=owed",
                        work.outgoing_tid,
                        work.outgoing_asid,
                        observed
                    );
                    // (2) ONE authoritative dequeue under the rank-1 scheduler seam. This is the
                    //     single queue-advancing step for the cycle. It may legitimately select
                    //     the outgoing caller itself, if a reply re-queued it.
                    let dispatch = shared.futex_wait_dispatch_step_mut(cpu);
                    match dispatch.tid().map(|t| t.0) {
                        None => {
                            // SETTLE (idle). The run queue is empty: the outgoing caller stays
                            // parked, `current` stays clear, no frame is restored and no incoming
                            // task is fabricated. A success, not a failure.
                            crate::yarm_log!("AARCH64_DIRECT_DISPATCH_NO_INCOMING cpu={}", cpu.0);
                            dd::note_outcome(DrainOutcome::Settled(Settlement::Idle));
                            crate::yarm_log!(
                                "AARCH64_DIRECT_DISPATCH_DONE result=idle settled=1 broad_lock=0"
                            );
                            // The established idle primitive, broad guard already dropped.
                            // Never returns.
                            super::aarch64::trap::enter_post_lock_idle_after_direct_dispatch(
                                cpu,
                                work.outgoing_tid,
                            );
                        }
                        Some(inc) => {
                            // From here the scheduler is MUTATED: `inc` was dequeued and made
                            // current. Any later failure must roll that back exactly and take
                            // the explicit fatal path — never return with it half-committed.
                            //
                            // Stage 199D-WA3A-R2-SEAL (item E): all five mark outcomes are
                            // matched explicitly. `RefusedTorn` never reaches the rollback or
                            // the idle settle — the scheduler and the task table already
                            // disagree, so there is nothing left to roll back.
                            let marked = match shared
                                .d6_genuine_mark_running_via_task_seam(dispatch)
                            {
                                Mark::Marked(token) => Some(token),
                                Mark::Idle => {
                                    crate::yarm_log!(
                                        "AARCH64_DIRECT_DISPATCH_DECLINED cpu={} reason=idle",
                                        cpu.0
                                    );
                                    None
                                }
                                Mark::RefusedRolledBack => {
                                    crate::yarm_log!(
                                        "AARCH64_DIRECT_DISPATCH_DECLINED cpu={} reason=refused_dequeue_undone",
                                        cpu.0
                                    );
                                    None
                                }
                                Mark::RefusedNoSchedulerChange => {
                                    crate::yarm_log!(
                                        "AARCH64_DIRECT_DISPATCH_DECLINED cpu={} reason=refused_scheduler_untouched",
                                        cpu.0
                                    );
                                    None
                                }
                                Mark::RefusedTorn => {
                                    dispatch_torn_fatal(cpu, inc, "aarch64_direct_dispatch")
                                }
                            };
                            let agrees = marked.is_some()
                                && shared.direct_dispatch_current_agrees_split_read(cpu, inc);
                            // Stage 199D-WA3A-R2-SEAL (item F): the resume runs off the mark
                            // TOKEN's exact incarnation, not the bare numeric TID, so a
                            // replacement task that reused the TID cannot have its address
                            // space activated or its context copied into the frame.
                            let resumed = agrees
                                && match (marked, frame.as_deref_mut()) {
                                    (Some(token), Some(f)) => {
                                        super::aarch64::trap::direct_dispatch_resume_incoming(
                                            shared, token, f,
                                        )
                                    }
                                    _ => false,
                                };
                            if resumed {
                                crate::yarm_log!(
                                    "AARCH64_DIRECT_DISPATCH_CURRENT_SET_OK cpu={} tid={}",
                                    cpu.0,
                                    inc
                                );
                                crate::yarm_log!("AARCH64_DIRECT_DISPATCH_RUNNING_OK tid={}", inc);
                                crate::yarm_log!("AARCH64_DIRECT_DISPATCH_FRAME_OK tid={}", inc);
                                DrainOutcome::Settled(Settlement::Dispatched { incoming: inc })
                            } else {
                                // Either the dequeue and `current` disagree, or the selected task
                                // has no saved context to resume. Both are kernel-invariant
                                // violations, not races. Undo the scheduler mutation EXACTLY,
                                // then halt: returning "declined" here would leave the scheduler
                                // believing `inc` is running while we eret through another
                                // task's frame.
                                // Stage 199D-WA3A-R2-SEAL (item C): only a token whose
                                // provenance is a genuine dequeue OF THIS TID can authorize
                                // undoing a dequeue. A `ContinuedCurrent` mark narrows to
                                // `None` here, so the rollback is unrepresentable rather than
                                // refused at the mutation site.
                                let rolled_back = marked
                                    .and_then(|t| t.into_dequeued_authority())
                                    .is_some_and(|a| shared.direct_dispatch_rollback_split(a));
                                crate::yarm_log!(
                                    "AARCH64_DIRECT_DISPATCH_ROLLBACK cpu={} incoming={} reason={} rolled_back={}",
                                    cpu.0,
                                    inc,
                                    if agrees {
                                        "no_saved_frame"
                                    } else {
                                        "current_disagreement"
                                    },
                                    rolled_back as u32
                                );
                                dd::note_outcome(DrainOutcome::RolledBackFatal);
                                super::aarch64::trap::enter_post_lock_dispatch_fatal(
                                    cpu,
                                    inc,
                                    rolled_back,
                                );
                            }
                        }
                    }
                }
            }
        };
        dd::note_outcome(outcome);
        // Return through the EXISTING eret model: the frame this drain restored is the one the
        // AArch64 vector epilogue erets from. No second return path is introduced.
        //
        // Reaching here means either the debt was settled by a dispatch, or there was no debt to
        // settle. The idle and fatal settlements never return at all.
        debug_assert!(outcome.debt_is_discharged());
        crate::yarm_log!(
            "AARCH64_DIRECT_DISPATCH_DONE result={} settled={} broad_lock=0",
            match outcome {
                DrainOutcome::Settled(Settlement::Dispatched { .. }) => "ok",
                DrainOutcome::NoDebtSupersededLease => "superseded",
                _ => "no_work",
            },
            outcome.owed_a_debt() as u32
        );
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
            let dispatch = shared.futex_wait_dispatch_step_mut(cpu);
            // Stage 199D-WA3A-R2-SEAL (item E): all five outcomes are matched explicitly, each
            // with its own evidence. `RefusedTorn` is fatal — it may never fall through to a
            // resume, an ordinary fallback dispatch, an idle halt or a return to userspace.
            let marked = match shared.d6_genuine_mark_running_via_task_seam(dispatch) {
                Mark::Marked(token) => Some(token),
                Mark::Idle => {
                    crate::yarm_log!(
                        "QUEUE_ADVANCING_DISPATCH_DECLINED cpu={} reason=idle",
                        cpu.0
                    );
                    None
                }
                Mark::RefusedRolledBack => {
                    crate::yarm_log!(
                        "QUEUE_ADVANCING_DISPATCH_DECLINED cpu={} reason=refused_dequeue_undone",
                        cpu.0
                    );
                    None
                }
                Mark::RefusedNoSchedulerChange => {
                    crate::yarm_log!(
                        "QUEUE_ADVANCING_DISPATCH_DECLINED cpu={} reason=refused_scheduler_untouched",
                        cpu.0
                    );
                    None
                }
                Mark::RefusedTorn => dispatch_torn_fatal(
                    cpu,
                    dispatch.tid().map(|t| t.0).unwrap_or(u64::MAX),
                    "futex_wait_queue_advancing_dispatch",
                ),
            };
            if let Some(token) = marked {
                let inc = token.tid();
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

    // Stage 195E (AARCH64 FUTEXWAIT QUEUE-ADVANCING DISPATCH): the AArch64 port of the 192A
    // drain. With the broad `with_cpu` guard dropped above, the in-lock `futex_wait_current`
    // published `Blocked(Futex)` + cleared `current` and declined the in-lock dispatch (the
    // caller returned through the AArch64 handler bypass). We re-verify the waiter is still
    // `Blocked(Futex)`, run the authoritative queue-advancing dispatch through ONLY the rank-1
    // scheduler seam, mark the incoming task Running (rank-2), then a brief `with_cpu`
    // re-acquire performs ONLY the AArch64 arch restore: incoming TTBR0_EL1/ASID switch (via
    // the generic HAL hook `switch_address_space`, which carries the DSB/ISB/TLBI ordering) +
    // EL0 SPSR/ELR/GPR frame restore (`restore_arch_thread_state_post_switch`). NO x86_64 CR3
    // logic is used. The generic seams (deferral ownership, reverify, dequeue/current, mark
    // Running, cleanup) are shared with x86_64; only the arch restore differs.
    #[cfg(target_arch = "aarch64")]
    {
        let futex_wait_was_deferred = cpu_idx < crate::kernel::scheduler::MAX_CPUS
            && crate::kernel::boot::futex_wait_dispatch_is_deferred(cpu_idx);
        if futex_wait_was_deferred {
            let outgoing = crate::kernel::boot::futex_wait_dispatch_outgoing(cpu_idx);
            let reverify_ok = outgoing
                .map(|t| shared.futex_wait_reverify_blocked(t))
                .unwrap_or(false);
            if reverify_ok {
                if let Some(t) = outgoing {
                    crate::yarm_log!("AARCH64_FUTEX_WAIT_DISPATCH_REVERIFY_OK tid={}", t);
                }
                // Queue-advancing dequeue + current assignment (rank-1 scheduler seam).
                let dispatch = shared.futex_wait_dispatch_step_mut(cpu);
                // Stage 199D-WA3A-R2-SEAL (item E): mark Running through the exact rank-2
                // transition FIRST, then match all five outcomes explicitly. A refusal has
                // already undone exactly what the selection did, so the drain resumes nothing
                // and the unchanged clear/fallback tail below runs — except for `RefusedTorn`,
                // which is fatal and never returns.
                let marked = match shared.d6_genuine_mark_running_via_task_seam(dispatch) {
                    Mark::Marked(token) => Some(token),
                    Mark::Idle => {
                        crate::yarm_log!(
                            "AARCH64_FUTEX_WAIT_DISPATCH_DECLINED cpu={} reason=idle",
                            cpu.0
                        );
                        None
                    }
                    Mark::RefusedRolledBack => {
                        crate::yarm_log!(
                            "AARCH64_FUTEX_WAIT_DISPATCH_DECLINED cpu={} reason=refused_dequeue_undone",
                            cpu.0
                        );
                        None
                    }
                    Mark::RefusedNoSchedulerChange => {
                        crate::yarm_log!(
                            "AARCH64_FUTEX_WAIT_DISPATCH_DECLINED cpu={} reason=refused_scheduler_untouched",
                            cpu.0
                        );
                        None
                    }
                    Mark::RefusedTorn => dispatch_torn_fatal(
                        cpu,
                        dispatch.tid().map(|t| t.0).unwrap_or(u64::MAX),
                        "aarch64_futex_wait_dispatch",
                    ),
                };
                if let Some(token) = marked {
                    let inc = token.tid();
                    crate::yarm_log!(
                        "AARCH64_FUTEX_WAIT_DISPATCH_DEQUEUE_OK cpu={} tid={}",
                        cpu.0,
                        inc
                    );
                    crate::yarm_log!(
                        "AARCH64_FUTEX_WAIT_DISPATCH_CURRENT_SET_OK cpu={} tid={}",
                        cpu.0,
                        inc
                    );
                    crate::yarm_log!("AARCH64_FUTEX_WAIT_DISPATCH_RUNNING_OK tid={}", inc);
                    // Arch restore: TTBR0/ASID switch + EL0 frame restore under a brief
                    // `with_cpu` re-acquire (global guard already dropped above).
                    // U3 (canonical 203C): the EXACT-TOKEN resume transaction — activate the
                    // token's exact ASID, restore its exact EL0 context/TLS and consume its
                    // exact parked completion through the rank-2 seams. No broad `with_cpu`
                    // re-acquire and no broad-lock fallback. The shared core emits no
                    // `AARCH64_DIRECT_DISPATCH_*` telemetry; only this class's markers below.
                    //
                    // A marked incoming task cannot be reported resumed without a real frame, so
                    // a missing frame takes the same refusal path as an exact-identity refusal.
                    let resumed = match frame.as_deref_mut() {
                        Some(f) => super::aarch64::trap::direct_dispatch_resume_incoming_core(
                            shared, token, f,
                        )
                        .ok(),
                        None => None,
                    };
                    crate::kernel::boot::futex_wait_dispatch_clear(cpu_idx);
                    let Some(asid) = resumed else {
                        // Exact authority only: a `ContinuedCurrent` mark yields none, so no
                        // rollback is fabricated. `rolled_back` is reported truthfully.
                        let rolled_back = token
                            .into_dequeued_authority()
                            .is_some_and(|a| shared.direct_dispatch_rollback_split(a));
                        super::aarch64::trap::enter_post_lock_dispatch_fatal(cpu, inc, rolled_back);
                    };
                    crate::yarm_log!(
                        "AARCH64_FUTEX_WAIT_DISPATCH_TTBR0_OK tid={} asid={}",
                        inc,
                        asid
                    );
                    crate::yarm_log!("AARCH64_FUTEX_WAIT_DISPATCH_FRAME_OK tid={}", inc);
                    crate::yarm_log!("AARCH64_FUTEX_WAIT_DISPATCH_DONE result=ok");
                    crate::kernel::boot::maybe_log_futex_wait_retired();
                } else {
                    // Stage 195F IDLE OUTCOME: no runnable incoming task. This is a SUCCESSFUL
                    // idle (not a failure): the outgoing caller stays Blocked(Futex), `current`
                    // stays None, the deferral is cleared, and the BSP enters the REAL idle loop
                    // AFTER the broad lock is released. No frame is restored, no incoming is
                    // fabricated, and `idle_no_eret_loop()` is NEVER entered while holding
                    // `with_cpu` (the broad guard was dropped before this drain).
                    crate::yarm_log!("AARCH64_FUTEX_WAIT_DISPATCH_NO_INCOMING cpu={}", cpu.0);
                    crate::yarm_log!("AARCH64_FUTEX_WAIT_POST_LOCK_IDLE_BEGIN cpu={}", cpu.0);
                    crate::kernel::boot::futex_wait_dispatch_clear(cpu_idx);
                    // Lock-dropped proof: re-acquiring `with_cpu` here is only possible because
                    // the broad guard was released above (a held guard would deadlock). Inside,
                    // confirm `current` is None/idle. We restore NO frame.
                    // Lock-dropped proof: re-acquiring `with_cpu` here is only possible because
                    // the broad guard was released above (a held guard would deadlock). Inside,
                    // confirm `current` is None/idle. We restore NO frame.
                    //
                    // BLOCKS U3: this drain was briefly retired onto `current_tid_split_read(cpu)`
                    // and then RESTORED. Its only live acceptance gate is the Stage 195F
                    // no-incoming idle oracle, whose precondition ("all servers up and blocked on
                    // recv") is unreachable behind the pre-existing SpawnV5/initramfs stall — so
                    // the substitution could not be live-proven. The accepted pre-U3 body is
                    // retained verbatim rather than shipped unverified. The two FutexWait/Yield
                    // switch-success restores ARE live-proven and stay retired.
                    let current_none = shared
                        .with_cpu(cpu, |kernel| matches!(kernel.current_tid(), None | Some(0)))
                        .unwrap_or(true);
                    crate::yarm_log!(
                        "AARCH64_FUTEX_WAIT_POST_LOCK_IDLE_LOCK_DROPPED_OK cpu={}",
                        cpu.0
                    );
                    crate::yarm_log!("AARCH64_FUTEX_WAIT_DISPATCH_DONE result=idle");
                    // The idle outcome is still a genuine off-global-lock FutexWait retirement.
                    crate::kernel::boot::maybe_log_futex_wait_retired();
                    // Narrowly-gated idle-oracle attestation (default-off workload knob).
                    if crate::kernel::boot::aarch64_futex_wait_idle_oracle_enabled() {
                        crate::yarm_log!(
                            "AARCH64_FUTEX_WAIT_IDLE_ORACLE_DONE result=ok lock_dropped=1 current_none={}",
                            current_none as u32
                        );
                    }
                    // Enter the real BSP idle loop OUTSIDE `with_cpu` (emits
                    // AARCH64_FUTEX_WAIT_POST_LOCK_IDLE_ENTERED, then a `wfi` loop). Never returns.
                    super::aarch64::trap::enter_post_lock_idle(cpu);
                }
            } else {
                // Split FutexWake flipped the outgoing task to Runnable before the drain ran —
                // do NOT stale-dispatch it away; decline and clear (the trap returns to the
                // now-re-runnable task). No duplicate enqueue, no lost waiter.
                crate::yarm_log!(
                    "AARCH64_FUTEX_WAIT_DISPATCH_DEFERRED reason=state_changed cpu={}",
                    cpu.0
                );
                crate::kernel::boot::futex_wait_dispatch_clear(cpu_idx);
            }
        }
    }

    // Stage 195G (AARCH64 YIELD QUEUE-ADVANCING DISPATCH): the AArch64 port of the 192B drain —
    // the preempt sibling of the FutexWait drain above. The in-lock `yield_current` set the
    // caller Runnable, RE-ENQUEUED it exactly once, and cleared `current` (the caller returned
    // through the AArch64 Yield handler bypass). We re-verify `current` is still cleared, run the
    // authoritative queue-advancing dispatch through ONLY the rank-1 scheduler seam (which
    // dequeues the FIFO head — another task, or the re-enqueued caller itself when alone), mark
    // the incoming task Running (rank-2), then a brief `with_cpu` re-acquire performs ONLY the
    // AArch64 arch restore: incoming TTBR0_EL1/ASID switch (via `switch_address_space`, carrying
    // the DSB/ISB/TLBI ordering) + EL0 SPSR/ELR/GPR frame restore. NO x86_64 CR3 logic. There is
    // ALWAYS an incoming for a published Yield deferral — no idle outcome.
    #[cfg(target_arch = "aarch64")]
    {
        let yield_was_deferred = cpu_idx < crate::kernel::scheduler::MAX_CPUS
            && crate::kernel::boot::yield_dispatch_is_deferred(cpu_idx);
        if yield_was_deferred {
            let outgoing = crate::kernel::boot::yield_dispatch_outgoing(cpu_idx);
            // Re-verify `current` is still cleared (guards against an in-lock fallback having
            // superseded the deferral). Single-CPU + IRQ-off ⇒ the caller stays Runnable and
            // queued exactly once between the in-lock re-enqueue and here.
            let reverify_ok = shared.yield_reverify_ready(cpu);
            if reverify_ok {
                if let Some(t) = outgoing {
                    crate::yarm_log!("AARCH64_YIELD_DISPATCH_REVERIFY_OK tid={}", t);
                }
                // Queue-advancing dequeue + current assignment (rank-1 scheduler seam).
                let dispatch = shared.yield_dispatch_step_mut(cpu);
                // Stage 199D-WA3A-R2-SEAL (item E): all five outcomes are matched explicitly,
                // each with its own evidence. `RefusedTorn` is fatal and never returns.
                let marked = match shared.d6_genuine_mark_running_via_task_seam(dispatch) {
                    Mark::Marked(token) => Some(token),
                    Mark::Idle => {
                        crate::yarm_log!(
                            "AARCH64_YIELD_DISPATCH_DECLINED cpu={} reason=idle",
                            cpu.0
                        );
                        None
                    }
                    Mark::RefusedRolledBack => {
                        crate::yarm_log!(
                            "AARCH64_YIELD_DISPATCH_DECLINED cpu={} reason=refused_dequeue_undone",
                            cpu.0
                        );
                        None
                    }
                    Mark::RefusedNoSchedulerChange => {
                        crate::yarm_log!(
                            "AARCH64_YIELD_DISPATCH_DECLINED cpu={} reason=refused_scheduler_untouched",
                            cpu.0
                        );
                        None
                    }
                    Mark::RefusedTorn => dispatch_torn_fatal(
                        cpu,
                        dispatch.tid().map(|t| t.0).unwrap_or(u64::MAX),
                        "aarch64_yield_dispatch",
                    ),
                };
                if let Some(token) = marked {
                    let inc = token.tid();
                    crate::yarm_log!(
                        "AARCH64_YIELD_DISPATCH_DEQUEUE_OK cpu={} tid={}",
                        cpu.0,
                        inc
                    );
                    crate::yarm_log!(
                        "AARCH64_YIELD_DISPATCH_CURRENT_SET_OK cpu={} tid={}",
                        cpu.0,
                        inc
                    );
                    crate::yarm_log!("AARCH64_YIELD_DISPATCH_RUNNING_OK tid={}", inc);
                    // U3 (canonical 203C): the EXACT-TOKEN resume transaction — activate the
                    // token's exact ASID, restore its exact EL0 context/TLS and consume its
                    // exact parked completion through the rank-2 seams. No broad `with_cpu`
                    // re-acquire and no broad-lock fallback. The shared core emits no
                    // `AARCH64_DIRECT_DISPATCH_*` telemetry; only this class's markers below.
                    //
                    // A marked incoming task cannot be reported resumed without a real frame, so
                    // a missing frame takes the same refusal path as an exact-identity refusal.
                    let resumed = match frame.as_deref_mut() {
                        Some(f) => super::aarch64::trap::direct_dispatch_resume_incoming_core(
                            shared, token, f,
                        )
                        .ok(),
                        None => None,
                    };
                    crate::kernel::boot::yield_dispatch_clear(cpu_idx);
                    let Some(asid) = resumed else {
                        // Exact authority only: a `ContinuedCurrent` mark yields none, so no
                        // rollback is fabricated. `rolled_back` is reported truthfully.
                        let rolled_back = token
                            .into_dequeued_authority()
                            .is_some_and(|a| shared.direct_dispatch_rollback_split(a));
                        super::aarch64::trap::enter_post_lock_dispatch_fatal(cpu, inc, rolled_back);
                    };
                    crate::yarm_log!("AARCH64_YIELD_DISPATCH_TTBR0_OK tid={} asid={}", inc, asid);
                    crate::yarm_log!("AARCH64_YIELD_DISPATCH_FRAME_OK tid={}", inc);
                    crate::yarm_log!("AARCH64_YIELD_DISPATCH_DONE result=ok");
                    crate::kernel::boot::maybe_log_yield_retired();
                } else {
                    // A published Yield deferral MUST have an incoming (the re-enqueued caller is
                    // always a candidate). No incoming is a genuine failure — do NOT claim a
                    // transition; clear the deferral. (This path must never fire.)
                    crate::kernel::boot::yield_dispatch_clear(cpu_idx);
                    crate::yarm_log!(
                        "AARCH64_YIELD_DISPATCH_FAIL reason=no_incoming cpu={}",
                        cpu.0
                    );
                }
            } else {
                // An in-lock fallback already dispatched — do NOT double-dispatch.
                crate::yarm_log!(
                    "AARCH64_YIELD_DISPATCH_DEFERRED reason=state_changed cpu={}",
                    cpu.0
                );
                crate::kernel::boot::yield_dispatch_clear(cpu_idx);
            }
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
            let dispatch = shared.yield_dispatch_step_mut(cpu);
            // Stage 199D-WA3A-R2-SEAL (item E): all five outcomes are matched explicitly, each
            // with its own evidence. `RefusedTorn` is fatal and never returns.
            let marked = match shared.d6_genuine_mark_running_via_task_seam(dispatch) {
                Mark::Marked(token) => Some(token),
                Mark::Idle => {
                    crate::yarm_log!("YIELD_DISPATCH_DECLINED cpu={} reason=idle", cpu.0);
                    None
                }
                Mark::RefusedRolledBack => {
                    crate::yarm_log!(
                        "YIELD_DISPATCH_DECLINED cpu={} reason=refused_dequeue_undone",
                        cpu.0
                    );
                    None
                }
                Mark::RefusedNoSchedulerChange => {
                    crate::yarm_log!(
                        "YIELD_DISPATCH_DECLINED cpu={} reason=refused_scheduler_untouched",
                        cpu.0
                    );
                    None
                }
                Mark::RefusedTorn => dispatch_torn_fatal(
                    cpu,
                    dispatch.tid().map(|t| t.0).unwrap_or(u64::MAX),
                    "yield_queue_advancing_dispatch",
                ),
            };
            if let Some(token) = marked {
                let inc = token.tid();
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
                    let dispatch = shared.d6_genuine_local_dispatch_step_mut(cpu);
                    // Deferred Phase B. Queue-neutral, so the transition is normally
                    // `Running → Running` (or `Runnable → Runnable` for idle continuing to be
                    // idle). Stage 199D-WA3A-R2-SEAL (item E): all five outcomes are matched
                    // explicitly; `RefusedTorn` is fatal and never returns.
                    match shared.d6_genuine_mark_running_via_task_seam(dispatch) {
                        Mark::Marked(token) => {
                            let n = crate::kernel::boot::D6_GENUINE_MUT_DISPATCH_COUNT
                                .fetch_add(1, core::sync::atomic::Ordering::Relaxed)
                                + 1;
                            crate::yarm_log!(
                                "D6_GENUINE_MUT_DISPATCH_DONE cpu={} incoming={}",
                                cpu.0,
                                token.tid()
                            );
                            crate::yarm_log!("D6_GENUINE_MUT_DISPATCH_COUNT value={}", n);
                        }
                        Mark::Idle => {
                            crate::yarm_log!(
                                "D6_GENUINE_MUT_DISPATCH_DECLINED cpu={} reason=idle",
                                cpu.0
                            );
                        }
                        Mark::RefusedRolledBack => {
                            crate::yarm_log!(
                                "D6_GENUINE_MUT_DISPATCH_DECLINED cpu={} reason=refused_dequeue_undone",
                                cpu.0
                            );
                        }
                        Mark::RefusedNoSchedulerChange => {
                            crate::yarm_log!(
                                "D6_GENUINE_MUT_DISPATCH_DECLINED cpu={} reason=refused_scheduler_untouched",
                                cpu.0
                            );
                        }
                        Mark::RefusedTorn => dispatch_torn_fatal(
                            cpu,
                            dispatch.tid().map(|t| t.0).unwrap_or(u64::MAX),
                            "d6_genuine_local_dispatch",
                        ),
                    }
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

    // Stage 200C2B/200C2C1 (IpcReplyTimeout OFF-LOCK RETIREMENT): with the broad
    // `SpinLock<KernelState>` from `with_cpu` already dropped above, collect DUE
    // token-bearing reply-receive deadlines through the NARROW collector (rank-2 task
    // split seam) and drain the per-CPU deferred work through the OFF-LOCK completion
    // transaction (per-domain split-mut seams). Neither holds the broad lock — this is
    // the production timer/deadline entry for the retired reply-timeout class, SHARED by
    // the x86_64 and AArch64 cells (this seam runs for every arch that flows through
    // `handle_trap_entry_shared`, so the AArch64 timer IRQ reaches it after `with_cpu`
    // returns). Ordinary receive-timeout deadlines stay on the in-lock scan (they are
    // skipped by the collector's token-bearing filter). Default-off: a strict no-op
    // unless a per-arch oracle feature is built AND its selector is active.
    // NB: RISC-V does NOT flow through this shared entry — it wires the identical collector/drain
    // into its own trap wrapper's Phase 3. The explicit `not(riscv64)` gate keeps that a
    // single-driver invariant even if a future path routes RISC-V here, so the one-shot
    // attestation can never be driven from two wrappers.
    #[cfg(all(
        feature = "ipc-reply-timeout-oracle-core",
        not(target_arch = "riscv64")
    ))]
    if crate::kernel::boot::x86_ipc_reply_timeout_oracle_enabled() {
        let now = shared.reply_timeout_now_split_read();
        shared.collect_due_reply_timeout_work(now, cpu);
        shared.drain_reply_timeout_post_work(cpu, now);
    }

    // Stage 200D-2A: the SERVER-DEATH post-lock drain. Unlike the reply-timeout collector
    // above this is NOT feature-gated — server death is production behaviour on every
    // build. It runs here, after the broad guard has dropped, so the PeerDeath terminal
    // claim, the caller's result publication and the single scheduler enqueue all happen
    // outside `SpinLock<KernelState>`. An empty queue makes this a cheap no-op.
    #[cfg(not(target_arch = "riscv64"))]
    let _ = shared.drain_server_death_post_work(cpu);

    // Stage 200D-0B3: the x86_64 post-lock-drain attestation. Emitted only after EVERY shared
    // post-lock drain above has actually completed — dispatch, the D2/D6 seams, FutexWait,
    // Yield, the switch-plan stash, the reply-timeout collector and the server-death drain.
    // Stage 200D-0B1 emitted a "drain done" marker from inside `with_cpu`, before any of them
    // had run; naming an operation "done" before performing it is precisely what this stage
    // forbids. The stage CAS is what enforces it: this cannot fire unless the release
    // attestation above already fired for the same trap.
    #[cfg(target_arch = "x86_64")]
    if let Some((exit_tid, exit_asid)) = crate::kernel::boot::advance_exit_attestation(
        cpu_idx,
        crate::kernel::boot::EXIT_ATTEST_LOCK_RELEASED,
        crate::kernel::boot::EXIT_ATTEST_DRAINED,
    ) {
        crate::yarm_log!(
            "EXIT_TASK_POST_LOCK_DRAIN_DONE arch=x86_64 tid={} asid={} cpu={} broad_lock=0 drains=all result=ok",
            exit_tid,
            exit_asid.0,
            cpu.0
        );
    }

    // ── Stage 200D-0C1: the AArch64 `CurrentTaskExited` consumer ────────────────────────────
    //
    // THE single AArch64 production call to `take_post_lock_trap_disposition`. Its position is
    // the whole contract, and every clause of it is a property of THIS line, not of a comment:
    //
    //   after broad-lock release  — `shared.with_cpu(...)` above returned, dropping the
    //                               `SpinLock<KernelState>` guard. The brief `with_cpu`
    //                               re-acquires below are only possible BECAUSE it was
    //                               dropped; a still-held guard would deadlock here, so
    //                               reaching the restore at all is the proof.
    //   after post-lock drains    — this is the LAST statement of the post-lock section:
    //                               dispatch, AArch64 FutexWait (195E), AArch64 Yield (195G),
    //                               the switch-plan stash (117), the reply-timeout collector
    //                               (200C2C1) and the server-death drain (200D-2A) have all run.
    //   before outgoing restore   — the in-lock `restore_arch_thread_state` was SKIPPED by the
    //                               Stage 200D-0C1 handler bypass, so no user thread state has
    //                               been restored yet; this block selects and performs it.
    //   before frame commit       — the vector only calls `write_trapframe_back_to_vector_frame`
    //                               after `handle_trap_entry_shared` returns.
    //
    // This is deliberately NOT the x86_64 shape. x86_64 consumes inside the arch handler
    // (Stage 200D-0B2); AArch64 cannot, because its idle divergence never returns from inside
    // `with_cpu`. See the Stage 200D-0C1 audit note in `arch/aarch64/trap.rs`.
    //
    // The consumer performs NO teardown, NO enqueue, NO terminal claim, NO PeerDeath claim and
    // NO user-memory access. It validates the exact `{tid, asid}` incarnation, attests the
    // outcome, and either performs the replacement's arch restore or enters the established
    // idle primitive. It fails closed through the existing fatal trap path.
    #[cfg(target_arch = "aarch64")]
    if let crate::kernel::boot::PostLockTrapDisposition::CurrentTaskExited { tid, asid } =
        crate::kernel::boot::take_post_lock_trap_disposition(cpu_idx)
    {
        crate::yarm_log!(
            "EXIT_TASK_BROAD_LOCK_RELEASED arch=aarch64 cpu={} broad_lock=0 holder=with_cpu result=ok",
            cpu.0
        );
        crate::yarm_log!(
            "EXIT_TASK_POST_LOCK_DRAIN_DONE arch=aarch64 cpu={} broad_lock=0 drains=all result=ok",
            cpu.0
        );
        crate::yarm_log!(
            "EXIT_TASK_DISPOSITION_CONSUMED arch=aarch64 tid={} asid={} cpu={} broad_lock=0 result=ok",
            tid,
            asid.0,
            cpu.0
        );
        // Re-acquiring the broad guard here is itself the lock-dropped proof (Stage 195F
        // pattern). Inside, read ONLY: is the exiting incarnation still current, is its ASID
        // still the published one, is it terminal, and is it absent from runnable state.
        let (current, identity_ok, terminal, in_runqueue) = shared
            .with_cpu(cpu, |kernel| {
                let current = kernel.current_tid();
                // Identity is the FULL incarnation. A numeric TID match alone would let a
                // restarted task satisfy a stale disposition, so the ASID recorded at
                // publication must still be bound to that TID — or the TCB must be gone.
                let identity_ok = match kernel.task_asid(tid) {
                    Some(bound) => bound == asid,
                    None => true,
                };
                // The lifecycle has no distinct `Exiting` state: `exit_task` commits straight
                // to `Exited(status)`, and a reaped TCB is `Dead` or absent. All three are
                // terminal; anything else means the disposition does not describe reality.
                let terminal = matches!(
                    kernel.task_status(tid),
                    Some(crate::kernel::task::TaskStatus::Exited(_))
                        | Some(crate::kernel::task::TaskStatus::Dead)
                        | None
                );
                // Absence proof: not merely "off this CPU's queue" — the exiting incarnation
                // must be present in NO runqueue on ANY CPU, so it can never be re-selected.
                (
                    current,
                    identity_ok,
                    terminal,
                    kernel.task_present_in_any_runqueue(tid),
                )
            })
            .map_err(|err| TrapHandleError::Syscall(err.into()))?;
        if current == Some(tid) {
            crate::yarm_log!(
                "EXIT_TASK_EXITING_STILL_CURRENT arch=aarch64 tid={} cpu={} result=fail",
                tid,
                cpu.0
            );
            return Err(TrapHandleError::Syscall(
                crate::kernel::syscall::SyscallError::Internal,
            ));
        }
        if !identity_ok || !terminal || in_runqueue {
            crate::yarm_log!(
                "EXIT_TASK_WRONG_IDENTITY arch=aarch64 tid={} asid={} identity_ok={} terminal={} in_runqueue={} result=fail",
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
            "EXIT_TASK_EXITING_NOT_CURRENT arch=aarch64 tid={} asid={} cpu={} result=ok",
            tid,
            asid.0,
            cpu.0
        );
        crate::yarm_log!(
            "EXIT_TASK_ABSENCE_VALIDATED arch=aarch64 tid={} asid={} current=0 runqueue=0 restore_owner=0 identity=tid_asid result=ok",
            tid,
            asid.0
        );
        // Trap-depth ownership (Stage 200D-0C1 §4 audit): AArch64 has NO software
        // trap-dispatch depth counter — x86_64's `TRAP_DISPATCH_DEPTH` has no AArch64
        // analogue. Nested-entry state is owned entirely by the hardware exception return,
        // so the correct number of consumer-side clears is ZERO. None is made here.
        crate::yarm_log!(
            "EXIT_TASK_TRAP_DEPTH_OWNER arch=aarch64 cpu={} owner=hardware_eret clears=0 result=ok",
            cpu.0
        );
        match current {
            Some(next) if next != 0 => {
                if next == tid {
                    crate::yarm_log!(
                        "EXIT_TASK_RESELECTED_EXITING_TASK arch=aarch64 tid={} cpu={} result=fail",
                        tid,
                        cpu.0
                    );
                    return Err(TrapHandleError::Syscall(
                        crate::kernel::syscall::SyscallError::Internal,
                    ));
                }
                crate::yarm_log!(
                    "EXIT_TASK_RESTORE_OWNER arch=aarch64 owner=replacement exiting_tid={} next_tid={} cpu={} result=ok",
                    tid,
                    next,
                    cpu.0
                );
                // Arch restore for the REPLACEMENT only: incoming TTBR0_EL1/ASID switch (via
                // the generic HAL hook, carrying the DSB/ISB/TLBI ordering) + EL0 SPSR/ELR/GPR
                // frame restore — the same brief `with_cpu` shape the 195E/195G drains use.
                // The exiting task's ELR/SP_EL0 are never a source here.
                let restore = shared
                    .with_cpu(cpu, |kernel| {
                        kernel.d2_recv_switch_incoming_asid(next);
                        post_switch_restore_arch_thread_state(kernel, cpu, frame.as_deref_mut())
                    })
                    .map_err(|err| TrapHandleError::Syscall(err.into()));
                restore??;
                crate::yarm_log!(
                    "EXIT_TASK_RESTORE_DONE arch=aarch64 owner=replacement next_tid={} cpu={} result=ok",
                    next,
                    cpu.0
                );
            }
            _ => {
                crate::yarm_log!(
                    "EXIT_TASK_RESTORE_OWNER arch=aarch64 owner=idle exiting_tid={} cpu={} result=ok",
                    tid,
                    cpu.0
                );
                // No replacement: restore NOTHING and enter the established idle primitive
                // with the broad guard dropped. Diverges — never returns to the vector, so no
                // exception-return frame is ever committed for the exiting task.
                super::aarch64::trap::enter_post_lock_idle_after_exit(cpu, tid);
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
    // Stage 195A / 197A: DebugLog (NR 15) and FutexWake (NR 10) are the live AArch64
    // pre-lock split-dispatch classes. Peek the raw syscall number from x8 WITHOUT
    // committing the import; import the decoded ABI ONLY for those NRs (or when the
    // oracle proof knob is on for its full validation surface). Every other syscall
    // keeps `nr=0` in the frame, so the split dispatcher declines it and it falls back
    // to the UNCHANGED global-lock path — this is what keeps DebugLog + FutexWake the
    // ONLY newly-eligible pre-lock classes. (Stage 197A removed the NR 27
    // InitramfsReadChunk split class along with the syscall itself.)
    let raw_nr = frame.user_gpr(crate::arch::aarch64::syscall_abi::REG_X8);
    if raw_nr == crate::kernel::syscall::SYSCALL_DEBUG_LOG_NR
        || raw_nr == crate::kernel::syscall::SYSCALL_FUTEX_WAKE_NR
        || crate::kernel::boot::ipc_recv_oracle_proof_enabled()
        // Stage 199A2C1: admit IpcCall (NR 6) + IpcReply (NR 7) ONLY when the direct proof gate is
        // armed, so their six-argument ABI is imported into the frame for the off-lock request/reply
        // gates. With the gate off, `nr` stays 0 and the split dispatcher declines them (unchanged
        // global-lock fallback) — this keeps a normal boot byte-identical.
        // Stage 199D: admit IpcCall (NR 6) + IpcReply (NR 7) through the CANONICAL
        // `ipccall_direct_admission_enabled()` — the same predicate the split dispatcher
        // itself uses — so AArch64 gains no architecture-specific admission rule. It is
        // still `production || proof`, and the AArch64 production default is OFF, so a
        // normal AArch64 boot keeps `nr = 0` here and falls back byte-identically.
        || ((raw_nr == crate::kernel::syscall::SYSCALL_IPC_CALL_NR
            || raw_nr == crate::kernel::syscall::SYSCALL_IPC_REPLY_NR)
            && crate::kernel::boot::ipccall_direct_admission_enabled())
    {
        super::aarch64::trap::split_import_syscall_abi(frame);
    }
}
#[cfg(not(target_arch = "aarch64"))]
fn pre_split_import_syscall_abi(_frame: &mut TrapFrame) {}

#[cfg(target_arch = "aarch64")]
fn split_return_identity(
    shared: &crate::runtime::SharedKernel,
    cpu: CpuId,
) -> crate::runtime::SplitReturnIdentity {
    // Scheduler (rank 1) + task (rank 2) reads only — the same authoritative current-task
    // read the split helpers use, qualified with the incarnation's ASID.
    let tid = shared.current_tid_authoritative(cpu).unwrap_or(0);
    crate::runtime::SplitReturnIdentity {
        tid,
        asid: crate::kernel::vm::Asid(shared.task_asid_for_tid_split_read(tid) as u16),
    }
}
#[cfg(not(target_arch = "aarch64"))]
fn split_return_identity(
    _shared: &crate::runtime::SharedKernel,
    _cpu: CpuId,
) -> crate::runtime::SplitReturnIdentity {
    crate::runtime::SplitReturnIdentity {
        tid: 0,
        asid: crate::kernel::vm::Asid(0),
    }
}

#[cfg(target_arch = "aarch64")]
fn finalize_split_handled_syscall(
    shared: &crate::runtime::SharedKernel,
    cpu: CpuId,
    entering: crate::runtime::SplitReturnIdentity,
    frame: &mut TrapFrame,
) {
    // Stage 195A / 197A: finalize is reached ONLY when the split dispatcher HANDLED the
    // syscall. In production the newly-eligible AArch64 pre-lock classes are DebugLog
    // (nr=15) and FutexWake (nr=10), plus the oracle-validated classes.
    //
    // Stage 199D: this NO LONGER takes the broad `KernelState` lock. The return work is
    // split into frame-only steps outside every lock and two bounded rank-2 task-domain
    // transactions (exact-incarnation TLS take, exact-incarnation context commit) — see
    // `split_finalize_handled_syscall`. NR6/NR7 are admitted through the canonical
    // `ipccall_direct_admission_enabled()` predicate, matching the import above.
    if frame.syscall_num() == crate::kernel::syscall::SYSCALL_DEBUG_LOG_NR
        || frame.syscall_num() == crate::kernel::syscall::SYSCALL_FUTEX_WAKE_NR
        || crate::kernel::boot::ipc_recv_oracle_proof_enabled()
        || ((frame.syscall_num() == crate::kernel::syscall::SYSCALL_IPC_CALL_NR
            || frame.syscall_num() == crate::kernel::syscall::SYSCALL_IPC_REPLY_NR)
            && crate::kernel::boot::ipccall_direct_admission_enabled())
    {
        super::aarch64::trap::split_finalize_handled_syscall(shared, cpu, entering, frame);
    }
}
#[cfg(not(target_arch = "aarch64"))]
fn finalize_split_handled_syscall(
    _shared: &crate::runtime::SharedKernel,
    _cpu: CpuId,
    _entering: crate::runtime::SplitReturnIdentity,
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
