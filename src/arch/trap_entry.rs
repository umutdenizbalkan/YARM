// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use crate::arch::trap::TrapEvent;
use crate::kernel::boot::{FaultBookkeepingMode, KernelState, TrapHandleError};
use crate::kernel::dispatch_post_work::DispatchPostWorkDisposition;
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
///
/// U9 (canonical 203C): the stash drain now reaches the x86_64 and AArch64 arms through
/// [`post_switch_restore_arch_thread_state_split`] instead, so only the RISC-V arm still has a
/// caller. All three arms stay compiled rather than being cfg'd down to one: this is the
/// cross-arch FOUNDATION the drain is defined against, and the two split twins are proven against
/// these bodies, outcome for outcome. Losing an arm would leave the twin with nothing to be
/// equivalent to.
#[cfg(target_arch = "x86_64")]
#[allow(dead_code)]
pub(crate) fn post_switch_restore_arch_thread_state(
    kernel: &mut KernelState,
    cpu: CpuId,
    frame: Option<&mut TrapFrame>,
) -> Result<(), TrapHandleError> {
    super::x86_64::trap::restore_arch_thread_state(kernel, cpu, frame)
}

#[cfg(target_arch = "aarch64")]
#[allow(dead_code)]
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

/// U9 (canonical 203C) — the broad-lock-free twin of
/// [`post_switch_restore_arch_thread_state`], for the Stage 117 switch-plan stash drain.
///
/// # Why this transaction is safe to take apart
///
/// The drain it serves is entered only when `maybe_switch_kernel_context` stashed a plan, and that
/// gate (`exec_state.rs`) requires `online_cpu_count() <= 1`. Interrupts are additionally disabled
/// for the whole drain — hardware disabled them at trap entry and `SpinLock<KernelState>` neither
/// saves nor restores IRQ state. So between the rank-1 and rank-2 phases below there is no other
/// CPU and no interrupt that could observe or mutate the state in flight; releasing rank 1 before
/// taking rank 2 is a lock-ordering improvement, not a new window.
///
/// # Shape
///
/// 1. **rank 1** — [`crate::runtime::SharedKernel::post_switch_restore_admit_split`] reproduces
///    both halves of `with_cpu`: the online-CPU admission and the `current_cpu` bind the retired
///    body's `KernelState::current_tid()` depended on, then reads that TID under the same guard.
/// 2. **rank 1 is fully released** before any task-domain work.
/// 3. **rank 2**, ONE acquisition — the whole restore payload, TAKEN coherently.
/// 4. **rank 2 is fully released** before the frame, MSR, page-table and CR3 work.
/// 5. The arch application runs with NO lock held, through the SAME single frame writer the
///    in-lock restore uses.
///
/// The admission `Err` is mapped exactly as the retired callsite mapped it
/// (`TrapHandleError::Syscall(err.into())`), so a refused CPU still refuses the drain and the
/// restore never runs — as before, nothing was mutated when it does.
#[cfg(target_arch = "x86_64")]
pub(crate) fn post_switch_restore_arch_thread_state_split(
    shared: &crate::runtime::SharedKernel,
    cpu: CpuId,
    frame: Option<&mut TrapFrame>,
) -> Result<(), TrapHandleError> {
    // (1) rank 1. This happens unconditionally, exactly as `with_cpu` bound and validated the CPU
    // before the restore hook ran — including when the hook then returned early.
    let tid = shared
        .post_switch_restore_admit_split(cpu)
        .map_err(|err| TrapHandleError::Syscall(err.into()))?;
    // The retired hook's first statement. An absent frame is NOT an error.
    let Some(frame) = frame else {
        return Ok(());
    };
    // `apply_current_thread_to_frame` raised `TaskMissing` here, which `restore_arch_thread_state`
    // swallowed into `Ok(())` as "no user task scheduled yet (normal during early boot)".
    let Some(tid) = tid else {
        return Ok(());
    };
    // (3) rank 2, one acquisition. `None` is the same early-boot `TaskMissing` swallow: the TCB is
    // gone, so there is no context to apply and nothing was taken.
    let Some(snapshot) = shared.post_switch_restore_snapshot_split(tid) else {
        return Ok(());
    };
    // (5) No lock held: frame, FS base, per-CPU TLS record, and the pre-IRET CR3 invariant with its
    // rare repair path — `x86_apply_owner_revalidation_restore` is the established off-lock tail of
    // `restore_arch_thread_state`, reused rather than re-implemented.
    super::x86_64::trap::x86_apply_owner_revalidation_restore(shared, cpu, tid, snapshot, frame);
    Ok(())
}

/// U9 (canonical 203C) — the AArch64 broad-lock-free twin of
/// [`post_switch_restore_arch_thread_state`]. See the x86_64 twin above for the safety argument
/// and the phase shape; only the payload differs.
///
/// `syscall_return` is `false`, the value `post_switch_restore_arch_thread_state` always passed —
/// the incoming task is resuming from a context switch, not returning from a direct syscall.
#[cfg(target_arch = "aarch64")]
pub(crate) fn post_switch_restore_arch_thread_state_split(
    shared: &crate::runtime::SharedKernel,
    cpu: CpuId,
    frame: Option<&mut TrapFrame>,
) -> Result<(), TrapHandleError> {
    use crate::runtime::PostSwitchRestoreOutcome;

    // (1) rank 1, unconditional — see the x86_64 twin.
    let tid = shared
        .post_switch_restore_admit_split(cpu)
        .map_err(|err| TrapHandleError::Syscall(err.into()))?;
    // The retired body checked the frame BEFORE `current_tid()`, so an absent frame emitted neither
    // idle marker. That order is preserved.
    let Some(frame) = frame else {
        return Ok(());
    };
    let Some(tid) = tid else {
        crate::yarm_log!("SCHED_NO_RUNNABLE_USER_TASK");
        crate::yarm_log!("SCHED_ENTER_IDLE");
        return Ok(());
    };
    // (3) rank 2, one acquisition.
    match shared.post_switch_restore_facts_split(tid) {
        PostSwitchRestoreOutcome::Idle => {
            crate::yarm_log!("SCHED_ENTER_IDLE");
            Ok(())
        }
        PostSwitchRestoreOutcome::Missing => Err(TrapHandleError::Syscall(
            crate::kernel::syscall::SyscallError::from(
                crate::kernel::boot::KernelError::TaskMissing,
            ),
        )),
        // (5) No lock held: the SAME single frame writer the in-lock restore uses.
        PostSwitchRestoreOutcome::Facts(facts) => {
            super::aarch64::trap::apply_restored_thread_state(frame, cpu, &facts, false);
            Ok(())
        }
    }
}

/// U9-D3 §7 — the D6 cleanup overlay at the Stage 117 switch-plan stash drain, now with NO broad
/// lock. This replaced `post_switch_restore_broad_tail`, the last broad acquisition on this path.
///
/// The overlay is reached only under `yarm.d6_switch_proof=1` or `yarm.d6_switch_a=1`, and it is
/// NOT obsolete diagnostics. `d6_ensure_post_cleanup_task_stacks_mapped` is a functional repair —
/// it shares every live task's kernel-stack pages into the active root and into every task root,
/// without which a post-cleanup trap faults on a supervisor stack write (Stage 165D/165F/165G) —
/// and `D6_SWITCH_A_DONE` is emitted only here, for the D6-SWITCH-A *production* unlocked-switch
/// milestone, whose knob runs this path with the controlled-proof knob OFF.
/// `scripts/qemu-x86_64-core-smoke.sh` hard-asserts both. Every one of those effects is preserved
/// here, in the same order; only the lock shape changed.
///
/// **Why it can be split now.** U3 and U9/203C both recorded the same blocker: the cleanup
/// allocates backing frames (`alloc_user_data_frame`, rank 6) and maps them across address spaces,
/// so a split form needs `with_memory_split_mut` — fenced by `AI_AGENT_RULES` §14.4 "without the
/// lock-free `await_tlb_shootdown_ack` design and multi-CPU smoke". U9-D3 delivered exactly that:
/// vector 0xF1 is the sole target-side invalidation and generation-matched ACK producer, proven
/// live from CPL0 and CPL3. The fence is lifted, and this site is no longer excluded.
///
/// It owes no shootdown of its own: it only ADDS supervisor mappings for frames that were
/// unmapped in the target root, and a mapping that fails hands its frame straight back
/// (`free_unmapped_user_data_frame_split`) before any page table could refer to it.
///
/// The **RISC-V restore** that used to share this tail is gone with it, and nothing is weakened:
/// the drain is statically unreachable on RISC-V. `maybe_switch_kernel_context`
/// (`exec_state.rs`) gates the stash on `!cfg!(target_arch = "riscv64") && …`, so
/// `DISPATCH_SWITCH_PLAN_STASH` is never populated there and `has_plan()` — this whole block's
/// entry condition — is never true. `post_switch_restore_arch_thread_state`'s RISC-V arm remains
/// defined as the cross-arch FOUNDATION, exactly as the x86_64 and AArch64 arms do.
#[cfg_attr(not(target_arch = "x86_64"), allow(unused_variables))]
fn post_switch_d6_cleanup_split(
    shared: &crate::runtime::SharedKernel,
    cpu: CpuId,
    d6_switch_a_mode: bool,
) {
    // rank 1 → rank 2 → lock-free CR3 restore, all inside; no lock spans any of it.
    #[cfg(target_arch = "x86_64")]
    KernelState::d6_emit_proof_cleanup_arch_markers_split(shared, cpu);
    // Stage 133: verify ASID 1 maps the fault page before emitting DONE. This reads CR3 and walks
    // page tables directly — it never needed kernel state at all.
    #[cfg(target_arch = "x86_64")]
    KernelState::d6_check_asid1_stack_page_mapped_split();
    // Stage 165D/165F/165G, unchanged in every effect: share every live task's kernel stack pages
    // into the active root and all task roots so no post-cleanup trap faults on a supervisor stack
    // write. Rank 6 is now taken for the frame ALLOCATION alone and released before each mapping.
    #[cfg(all(target_arch = "x86_64", not(test), not(feature = "hosted-dev")))]
    if let Err(err) = KernelState::d6_ensure_post_cleanup_task_stacks_mapped_split(shared, cpu) {
        crate::yarm_log!("D6_POST_CLEANUP_STACK_MAP_FAILED err={:?}", err);
    }
    crate::yarm_log!("D6_CONTROLLED_SWITCH_PROOF_CLEANUP_DONE");
    if d6_switch_a_mode {
        crate::yarm_log!("D6_SWITCH_A_DONE");
    }
}

/// U9-QA — the Stage 117 switch-plan stash DRAIN, extracted verbatim so it has exactly one
/// implementation and two callers.
///
/// It was inline in `handle_trap_entry_shared`, reachable only after the broad `with_cpu`
/// returned. U9-QA gives it a second caller: a PRE-LOCK route that published its own terminal
/// transition and stashed a plan through `SharedKernel::queue_advance_commit_split` must drive
/// the same apply, and must not enter the terminal broad dispatcher to do it.
///
/// Nothing about the apply changed. It still takes the plan, performs the arch switch with no
/// lock held, runs the U9/203C off-lock incoming restore on x86_64 and AArch64, and runs the
/// U9-D3 §7 split D6 cleanup when a proof/D6-SWITCH-A run has just completed.
fn drain_switch_plan_stash(
    shared: &crate::runtime::SharedKernel,
    cpu: CpuId,
    mut frame: Option<&mut TrapFrame>,
) -> Result<(), TrapHandleError> {
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
            // U9 (canonical 203C): restore the incoming task's arch thread state (populate its
            // trap frame with its user-mode register context) with NO broad lock re-acquired.
            //
            // Reached on x86_64 and AArch64 — the only architectures whose stash gate can
            // populate `DISPATCH_SWITCH_PLAN_STASH` at all, since that gate (`exec_state.rs`)
            // opens with `!cfg!(target_arch = "riscv64") && …`. This drain is therefore
            // statically unreachable on RISC-V, which keeps its verbatim in-lock restore inside
            // the tail below rather than an unproven split.
            #[cfg(not(target_arch = "riscv64"))]
            let restore_result =
                post_switch_restore_arch_thread_state_split(shared, cpu, frame.as_deref_mut());
            #[cfg(target_arch = "riscv64")]
            let restore_result: Result<(), TrapHandleError> = Ok(());

            // U9-D3 §7: the D6 cleanup overlay is the ONLY remaining consumer here, and it no
            // longer touches the broad lock — this site holds NO broad acquisition at all. An
            // ordinary production switch skips it entirely (`is_proof_done` is false).
            //
            // The `|| cfg!(target_arch = "riscv64")` arm went with the tail. It existed to run
            // RISC-V's in-lock restore, and that arm was already statically unreachable: the stash
            // gate in `maybe_switch_kernel_context` opens with `!cfg!(target_arch = "riscv64")`,
            // so `has_plan()` — this block's entry condition — is never true on RISC-V. Removing
            // an unreachable branch weakens nothing; the FOUNDATION restore stays defined.
            //
            // Order is preserved: the restore ran first and its result is propagated last, so the
            // overlay still observes exactly the state it did before, and a restore error still
            // surfaces only after the cleanup has run.
            if is_proof_done {
                post_switch_d6_cleanup_split(shared, cpu, d6_switch_a_mode);
            }
            restore_result?;
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
    Ok(())
}

/// U9-QA §2 — the ONE owner of this CPU's trap-path-active window.
///
/// `GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE` means "a drainer WILL consume whatever this trap
/// publishes". Stage 117 set it around the broad `with_cpu` only, because only in-lock code
/// could publish. U9-QA gives the PRE-LOCK split route the same ability, so the window must
/// open before that route runs — which is also what lets `queue_advance_admit_split` answer
/// truthfully there instead of refusing `NoTrapDrainer`.
///
/// Widening the window widens the obligation to close it, and `handle_trap_entry_shared` now
/// leaves through several paths: a completed non-switching split class, an ordinary syscall
/// error, a kernel-side error, the broad dispatcher's own `?`, and the fall-through to the
/// drains. Rather than duplicate the clear at each, this type owns it: `settle` clears exactly
/// once and `Drop` calls `settle`, so every RETURNING path is covered without a written clear.
///
/// U9-QA §2 (RISC-V): the RISC-V wrapper `handle_riscv_trap_entry_shared` owns the same window
/// for the same reason and uses this same type, so there is ONE flag lifecycle in the tree rather
/// than one per bridge.
///
/// `Drop` cannot cover a DIVERGING path — the drains' idle and fatal landings never return, so
/// they never unwind. That is why `settle` is also called explicitly at the single point after
/// the broad dispatcher and before the drains: by the time any divergence is reachable, the
/// window is already closed, and the later `Drop` is a no-op.
pub(crate) struct TrapPathWindow {
    cpu_idx: usize,
    settled: core::cell::Cell<bool>,
}

impl TrapPathWindow {
    pub(crate) fn establish(cpu: CpuId) -> Self {
        let cpu_idx = cpu.0 as usize;
        if cpu_idx < crate::kernel::scheduler::MAX_CPUS {
            crate::kernel::boot::GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE[cpu_idx]
                .store(true, core::sync::atomic::Ordering::Relaxed);
        }
        Self {
            cpu_idx,
            settled: core::cell::Cell::new(false),
        }
    }

    /// Close the window. Idempotent by construction, so an explicit call before a divergence
    /// and the `Drop` that follows a return together still clear it exactly once.
    pub(crate) fn settle(&self) {
        if self.settled.replace(true) {
            return;
        }
        if self.cpu_idx < crate::kernel::scheduler::MAX_CPUS {
            crate::kernel::boot::GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE[self.cpu_idx]
                .store(false, core::sync::atomic::Ordering::Relaxed);
        }
    }
}

impl Drop for TrapPathWindow {
    fn drop(&mut self) {
        self.settle();
    }
}

pub fn handle_trap_entry_shared(
    shared: &crate::runtime::SharedKernel,
    cpu: CpuId,
    context: ArchTrapContext,
    mut frame: Option<&mut TrapFrame>,
) -> Result<(), TrapHandleError> {
    use crate::kernel::syscall_split::SplitDispatchDisposition;

    // Stage 117 / U9-QA §2: open the trap-path-active window HERE — before the pre-lock split
    // dispatch, not just around `with_cpu`. See `TrapPathWindow`; it is the one owner of this
    // flag and of clearing it on every path out of this function.
    let cpu_idx = cpu.0 as usize;
    let trap_path = TrapPathWindow::establish(cpu);

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
    // requester TID) returns `NotHandled` and falls through to the UNCHANGED
    // global-lock dispatch below. This is gated on the trap being a syscall so non-syscall
    // events (page faults, timer/external IRQs) never enter the seam.
    //
    // U9-QA §2: the seam now answers with three meanings rather than two. `NotHandled` and
    // `Complete` behave exactly as `None` and `Some` did. `QueueAdvanceCommitted` is the new
    // one: a terminal transition has been published, so this trap may neither enter the broad
    // dispatcher nor return through the outgoing frame — it falls through to the drains.
    let mut queue_advance_committed = false;
    // U9-TM §2: the pre-lock TIMER route. It runs before the syscall seam and is mutually
    // exclusive with it — a timer interrupt carries no syscall NR — and it refuses BEFORE any
    // claim, tick or mutation when a proof knob is armed or when this tick would preempt, so a
    // refused trap reaches the unchanged broad arm having changed nothing.
    // U9-FT4: the pre-lock AArch64 terminal PageFault route. It runs before the timer and
    // syscall seams and is mutually exclusive with both. It refuses BEFORE any publication for
    // every class, architecture or endpoint condition it does not admit, so a declined trap
    // reaches the unchanged broad arm having changed nothing. When it commits it holds a
    // RESERVED deferral, so the existing post-lock drain is guaranteed to apply an incoming
    // context — `QueueAdvanceCommitted` without one is what FT3 got wrong.
    {
        let pf = match decode_trap_context(context) {
            TrapEvent::PageFault(f) => Some(f),
            _ => None,
        };
        if pf.is_some() {
            match crate::kernel::syscall_split::try_split_terminal_page_fault_dispatch(
                shared,
                cpu,
                pf,
                frame.as_deref(),
            ) {
                SplitDispatchDisposition::NotHandled => {}
                SplitDispatchDisposition::QueueAdvanceCommitted => {
                    crate::yarm_log!(
                        "QUEUE_ADVANCE_BROAD_DISPATCH_SKIPPED cpu={} reason=terminal_fault_committed",
                        cpu.0
                    );
                    queue_advance_committed = true;
                }
                SplitDispatchDisposition::Complete(_) => {
                    // Fail-closed after publication: the report is out, so the broad emitter must
                    // NOT run again. No deferral is held on this path.
                    queue_advance_committed = true;
                }
                other => {
                    crate::yarm_log!(
                        "TERMINAL_FAULT_UNEXPECTED_DISPOSITION cpu={} value={:?}",
                        cpu.0,
                        other
                    );
                    debug_assert!(
                        false,
                        "the terminal PageFault route yields NotHandled, QueueAdvanceCommitted or Complete"
                    );
                }
            }
        }
    }
    let mut post_work_committed = false;
    {
        let is_timer = matches!(decode_trap_context(context), TrapEvent::TimerInterrupt);
        match crate::kernel::syscall_split::try_split_timer_dispatch(shared, cpu, is_timer) {
            SplitDispatchDisposition::NotHandled => {}
            SplitDispatchDisposition::PostWorkCommitted => {
                // The tick and the re-arm are done and no scheduler state changed. Skip the
                // broad arm — entering it would tick a SECOND time — and fall through so the
                // architecture tail still runs the production timeout pipeline.
                post_work_committed = true;
            }
            other => {
                // The timer route produces only those two. Anything else would mean a
                // non-preempting tick had claimed a terminal transition.
                crate::yarm_log!(
                    "TIMER_SPLIT_UNEXPECTED_DISPOSITION cpu={} value={:?}",
                    cpu.0,
                    other
                );
                debug_assert!(
                    false,
                    "the timer route yields NotHandled or PostWorkCommitted"
                );
            }
        }
    }
    if !post_work_committed && matches!(decode_trap_context(context), TrapEvent::Syscall) {
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
            let disposition =
                crate::kernel::syscall_split::try_split_dispatch_into_frame(shared, cpu, frame);
            // U9-QA §2: the COMMITTED disposition. The split route published a terminal
            // transition, so the caller is no longer current on this CPU and the live frame no
            // longer belongs to anything this trap may return through.
            //
            // Two things happen here and nowhere else. The architecture syscall-return ABI is
            // finalized against the OUTGOING incarnation — the caller must observe its result
            // and its advanced PC when it is later resumed, not when it next traps. Then the
            // outgoing user context is captured into that exact TCB, keyed on the identity the
            // deferral carries rather than on any ambient "current" lookup, because the drain
            // is about to overwrite the live frame with the INCOMING task's context.
            if matches!(disposition, SplitDispatchDisposition::QueueAdvanceCommitted) {
                finalize_split_handled_syscall(shared, cpu, entering, frame);
                let outgoing = crate::kernel::boot::futex_wait_dispatch_outgoing(cpu_idx);
                let captured = outgoing
                    .map(|t| shared.capture_outgoing_user_context_split(t, frame))
                    .unwrap_or(false);
                crate::yarm_log!(
                    "YARM_LOCK_SPLIT_DISPATCH {}nr={} cpu={} result=queue_advance_committed outgoing={} captured={}",
                    SPLIT_DISPATCH_ARCH_TAG,
                    frame.syscall_num(),
                    cpu.0,
                    outgoing.unwrap_or(u64::MAX),
                    u8::from(captured),
                );
                queue_advance_committed = true;
            }
            if let SplitDispatchDisposition::Complete(result) = disposition {
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

    // Stage 117 / U9-QA §2: the broad dispatcher is entered ONLY after a `NotHandled`
    // disposition — i.e. only when nothing has been mutated and the trap still needs handling.
    //
    // After `QueueAdvanceCommitted` it must be skipped, and not as an optimisation: the caller
    // is already `Blocked(Futex)` and current on no CPU, so `handle_trap` would re-execute
    // FutexWait against a task that has already blocked, and `dispatch_next_task` would advance
    // the queue a second time for one publication. Skipping is what makes "the queue-advance
    // drain runs exactly once" true.
    //
    // Note this is decided by the DISPOSITION, never by inspecting the switch-plan stash. A
    // stale or unrelated stash must not be able to alter dispatch control flow.
    //
    // Stage 117: pass `frame.as_deref_mut()` (reborrow) so that `frame` remains
    // available after `with_cpu` returns for the stash drain below.
    // The `Ok(Ok(()))` shape mirrors the broad call exactly: the outer `Result` is the lock
    // acquisition, the inner one the arch handler's own result. Skipping the acquisition yields
    // a successful acquisition of nothing and a successful handler, so both `?` sites below stay
    // untouched.
    let inner_result: Result<Result<(), TrapHandleError>, TrapHandleError> =
        if queue_advance_committed || post_work_committed {
            crate::yarm_log!(
                "QUEUE_ADVANCE_BROAD_DISPATCH_SKIPPED cpu={} reason={}",
                cpu.0,
                if queue_advance_committed {
                    "publication_committed"
                } else {
                    "timer_post_work_committed"
                }
            );
            Ok(Ok(()))
        } else {
            shared
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
                            TrapHandleError::Syscall(crate::kernel::syscall::SyscallError::from(
                                err,
                            ))
                        })?;
                    handle_trap_entry_with_fault_bookkeeping_mode(
                        kernel,
                        cpu,
                        context,
                        frame.as_deref_mut(),
                        fault_bookkeeping_mode,
                    )
                })
                .map_err(|err| TrapHandleError::Syscall(err.into()))
        };

    // U9-QA §2: the SINGLE settlement point for the trap-path-active window, covering both
    // paths that reach the drains. It is explicit rather than left to `Drop` because the drains
    // below contain DIVERGING landings (idle, fatal) that never unwind — the window has to be
    // closed before any of them is reachable. Every other exit from this function returns, so
    // `TrapPathWindow::drop` closes it there; `settle` is idempotent, so it happens exactly once
    // either way.
    trap_path.settle();

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
    // U6 §4: the drain now reports what it requires of THIS wrapper, and is given the caller's
    // frame so a refused blocking-send commit can encode its canonical error into it.
    let post_work_disposition = shared.drain_dispatch_post_work(cpu, frame.as_deref_mut())?;
    match post_work_disposition {
        // Every pre-U6 variant, and an empty stash, land here: behaviour unchanged.
        DispatchPostWorkDisposition::NoCallerAction => {}
        DispatchPostWorkDisposition::ImmediateReturn { error } => {
            // The commit was refused with nothing mutated, so the caller is still running and
            // still owns this frame. Its canonical error is already encoded; return now, before
            // the D2 drains, because there is no deferral to drain — the transaction armed none.
            crate::yarm_log!(
                "U6_BLOCKING_SEND_IMMEDIATE_RETURN cpu={} err={:?} result=ok",
                cpu.0,
                error
            );
            return Ok(());
        }
        DispatchPostWorkDisposition::SenderCommittedBlocked { tid } => {
            // The sender is parked and this CPU has no current task. Fall through WITHOUT
            // writing any syscall result: this frame belongs to a blocked task now, and its
            // result arrives from the completion its waker publishes. The D2-send drain below
            // performs the queue-advancing dispatch the transaction armed.
            crate::yarm_log!(
                "U6_BLOCKING_SEND_WRAPPER_BLOCKED arch={} cpu={} tid={} result=ok",
                if cfg!(target_arch = "x86_64") {
                    "x86_64"
                } else {
                    "aarch64"
                },
                cpu.0,
                tid
            );
        }
    }

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
    // U4: captured on every architecture this shared entry serves.
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    let d2_recv_was_deferred = crate::kernel::boot::d2_recv_dispatch_is_deferred(cpu_idx);
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    let d2_send_was_deferred = crate::kernel::boot::d2_send_dispatch_is_deferred(cpu_idx);
    // Stage 192A: capture the FutexWait queue-advancing dispatch deferral state (set by the
    // in-lock `futex_wait_current`); its drain below clears it.
    #[cfg(target_arch = "x86_64")]
    let futex_wait_was_deferred = crate::kernel::boot::futex_wait_dispatch_is_deferred(cpu_idx);
    // Stage 192B: capture the Yield queue-advancing dispatch deferral state (set by the
    // in-lock `yield_current`); its drain below clears it.
    #[cfg(target_arch = "x86_64")]
    let yield_was_deferred = crate::kernel::boot::yield_dispatch_is_deferred(cpu_idx);

    // ── U4: the architecture adapters for the shared D2 recv/send drains ────────────────
    //
    // Everything above the resume — the deferral take, the outgoing re-verification, the ONE
    // rank-1 dequeue, the rank-2 mark transition and all five WA3A outcomes — is shared
    // scheduler policy and stays in one place. Only the final resume, the post-mutation fatal
    // terminal and the idle settlement are architecture-specific, and each delegates to the
    // per-architecture transaction that already exists.

    /// Resume the marked incarnation. `true` iff the incoming task's frame is now armed.
    ///
    /// x86_64 uses the neutral exact-token resume core (ASID activation, context/TLS, FS base,
    /// pre-IRET CR3). AArch64 uses ITS neutral exact-token core — TTBR0 activation, saved EL0
    /// context, x18 TLS, parked completion and the x0..x5 argument mirror — deliberately the
    /// `_core` form, so this ordinary D2 path emits NO `AARCH64_DIRECT_DISPATCH_*` telemetry:
    /// those markers belong to the direct NR6/NR7 class, not to blocking recv/send.
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[allow(unused_variables)]
    fn d2_resume_marked_incoming(
        shared: &crate::runtime::SharedKernel,
        token: crate::runtime::DispatchMarkToken,
        frame: Option<&mut TrapFrame>,
    ) -> bool {
        #[cfg(target_arch = "x86_64")]
        {
            crate::arch::x86_64::trap::x86_post_lock_resume_marked_incoming(shared, token, frame)
                .is_ok()
        }
        #[cfg(target_arch = "aarch64")]
        {
            match frame {
                Some(f) => crate::arch::aarch64::trap::direct_dispatch_resume_incoming_core(
                    shared, token, f,
                )
                .is_ok(),
                // No frame to arm: the scheduler is already mutated, so this is a refusal, not
                // a decline. The caller rolls back and diverges.
                None => false,
            }
        }
    }

    /// The post-mutation resume-refusal terminal. Never returns: the scheduler believes the
    /// incoming task is running, so returning through any frame is forbidden.
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    fn d2_resume_refused_fatal(cpu: CpuId, incoming: u64, rolled_back: bool) -> ! {
        #[cfg(target_arch = "x86_64")]
        {
            crate::arch::x86_64::trap::enter_post_lock_dispatch_fatal(cpu, incoming, rolled_back)
        }
        #[cfg(target_arch = "aarch64")]
        {
            crate::arch::aarch64::trap::enter_post_lock_dispatch_fatal(cpu, incoming, rolled_back)
        }
    }

    /// Settle a D2 drain that selected no incoming task — a typed SUCCESS, never an error.
    ///
    /// On x86_64 this returns: the accepted epilogue already owns the idle decision for this
    /// CPU, and U4 does not move it. On AArch64 it must NOT return — the block commit cleared
    /// `current`, so the frame the vector epilogue would `eret` through belongs to a parked
    /// task; it enters the ESTABLISHED post-lock idle terminal instead, the same one the
    /// direct-dispatch drain uses for its no-incoming settlement.
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[allow(unused_variables)]
    fn d2_settle_idle(cpu: CpuId, outgoing: Option<u64>) {
        #[cfg(target_arch = "aarch64")]
        crate::arch::aarch64::trap::enter_post_lock_idle_after_direct_dispatch(
            cpu,
            outgoing.unwrap_or(u64::MAX),
        );
    }

    // Stage 169 (D2-GENUINE-SEND): drain the deferred blocking-SEND queue-
    // advancing dispatch OUTSIDE the global lock (mirrors the recv drain below).
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
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
                // U3 (203C): the broad `with_cpu` re-acquire that ran
                // `d2_recv_switch_incoming_asid(inc)` + the arch restore is retired. The
                // COMPLETE token is carried into one neutral exact-token transaction, so the
                // ASID, context, TLS and CR3 authority all come from the marked incarnation
                // rather than from a bare TID re-reading `current`.
                let resumed = d2_resume_marked_incoming(shared, token, frame.as_deref_mut());
                // The deferral is cleared exactly once, on both outcomes, before anything else
                // — exactly where the old code cleared it.
                crate::kernel::boot::d2_send_dispatch_clear(cpu_idx);
                if !resumed {
                    // Refused AFTER the scheduler was mutated: undo the dequeue with the
                    // token's own narrowed authority (a `ContinuedCurrent` mark yields none,
                    // and none is fabricated), then diverge. No success marker is emitted.
                    let rolled_back = token
                        .into_dequeued_authority()
                        .is_some_and(|a| shared.direct_dispatch_rollback_split(a));
                    d2_resume_refused_fatal(cpu, inc, rolled_back);
                }
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
                // Typed successful terminal, never an error. On AArch64 this does not return.
                d2_settle_idle(cpu, outgoing);
            }
        } else {
            crate::yarm_log!(
                "D2_SEND_GENUINE_FALLBACK reason=state_changed cpu={}",
                cpu.0
            );
            crate::kernel::boot::d2_send_dispatch_clear(cpu_idx);
        }
    }
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
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
                //
                // U3 (203C): this was a brief broad `with_cpu` re-acquire. It is now the SAME
                // neutral exact-token transaction the homologous blocking-send switch-success
                // uses — the two were byte-identical bodies, and they now share one
                // implementation rather than two copies that could drift.
                let resumed = d2_resume_marked_incoming(shared, token, frame.as_deref_mut());
                crate::kernel::boot::d2_recv_dispatch_clear(cpu_idx);
                if !resumed {
                    let rolled_back = token
                        .into_dequeued_authority()
                        .is_some_and(|a| shared.direct_dispatch_rollback_split(a));
                    d2_resume_refused_fatal(cpu, inc, rolled_back);
                }
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
                // Typed successful terminal, never an error. On AArch64 this does not return.
                d2_settle_idle(cpu, outgoing);
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
                // U3 (203C): the broad `with_cpu` re-acquire is retired onto the SAME neutral
                // exact-token transaction the D2 send/recv switch-success restores already use
                // — unchanged, not extended. The complete token carries the ASID, context, TLS
                // and CR3 authority, so nothing here decides on a bare TID.
                let resumed = crate::arch::x86_64::trap::x86_post_lock_resume_marked_incoming(
                    shared,
                    token,
                    frame.as_deref_mut(),
                );
                crate::kernel::boot::futex_wait_dispatch_clear(cpu_idx);
                if resumed.is_err() {
                    // Refused after the scheduler was mutated: undo the dequeue with the
                    // token's own narrowed authority (a `ContinuedCurrent` mark yields none,
                    // and none is fabricated), then diverge. No FRAME_OK, no counter bump and
                    // no success marker is emitted on this path.
                    let rolled_back = token
                        .into_dequeued_authority()
                        .is_some_and(|a| shared.direct_dispatch_rollback_split(a));
                    crate::arch::x86_64::trap::enter_post_lock_dispatch_fatal(
                        cpu,
                        inc,
                        rolled_back,
                    );
                }
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
                //
                // U9-QA §4: this is the x86_64 TERMINAL-IDLE settlement, and it settles by
                // RETURNING. The outgoing waiter is `Blocked(Futex)` and `current` is clear, so
                // the raw trap tail's `exiting_tid is None` landing takes this CPU into
                // `idle_halt_loop()` with the depth clear and attestation epilogue intact. No
                // frame is restored here and nothing diverges — see
                // `settle_post_lock_terminal_idle`.
                crate::kernel::boot::futex_wait_dispatch_clear(cpu_idx);
                crate::arch::x86_64::trap::settle_post_lock_terminal_idle(
                    cpu,
                    outgoing.unwrap_or(u64::MAX),
                    "futex_wait",
                );
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
            // U9-FT4: the drain admits TWO outgoing states and verifies each exactly — a
            // FutexWait caller is `Blocked(Futex)`, a terminally faulted task is `Faulted`. Both
            // mean the outgoing task is off the CPU and a queue advance is owed, so they share
            // THIS drain rather than growing a second one. Neither predicate is loosened.
            let reverify_ok = outgoing
                .map(|t| {
                    shared.futex_wait_reverify_blocked(t)
                        || shared.terminal_fault_reverify_faulted(t)
                })
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
                    // Lock-dropped proof: reaching a lock-taking read here is only possible
                    // because the broad guard was released above (a held guard would deadlock —
                    // the broad `SpinLock<KernelState>` contains the rank-1 scheduler domain, so
                    // the deadlock property is identical for the narrow acquisition). Confirm
                    // `current` is None/idle. We restore NO frame.
                    //
                    // U3 (canonical 203C): this was a brief broad `with_cpu` re-acquire. It is now
                    // the EXISTING authoritative rank-1 transaction, `current_tid_authoritative`,
                    // which is exactly what the retired closure resolved to — not a new seam:
                    //
                    //   * `with_cpu` ran `set_current_cpu(cpu)` first, which VALIDATES with
                    //     `validate_online_cpu` and then binds `sched.current_cpu = cpu`;
                    //     `current_tid_authoritative` applies the same predicate and performs the
                    //     same binding, and leaves `current_cpu` unchanged when it refuses.
                    //   * `kernel.current_tid()` then read `current_tid_on(self.current_cpu())`.
                    //     `current_tid_authoritative` reproduces that lookup verbatim, including
                    //     the freestanding-AArch64 branch where `current_cpu()` is derived from
                    //     `MPIDR_EL1` rather than from the field just bound.
                    //   * the broad body took the scheduler lock TWICE (once in `set_current_cpu`,
                    //     once in `current_tid`), coherent only by virtue of the broad guard; the
                    //     transaction does validate + bind + read under ONE rank-1 acquisition, so
                    //     it is strictly more coherent and never less.
                    //
                    // The predicate and the refusal policy are unchanged, outcome for outcome. The
                    // helper returns `Option<u64>`, so the legacy `matches!(…, None | Some(0))`
                    // splits across the same two lines it always did: `Some(0)` is the idle task,
                    // and BOTH "no current task" and "CPU refused" arrive as `None` and are mapped
                    // to `true` by the SAME `unwrap_or(true)` the retired callsite used. Every
                    // other TID is `false`, exactly as before.
                    //
                    // Deliberately NOT `current_tid_split_read`: that reader does not bind
                    // `current_cpu`, which is why the earlier substitution here was reverted.
                    // Deliberately NOT `terminal_idle_on_cpu_split`: it additionally requires
                    // `runnable_count_on(cpu) == 0`, which would STRENGTHEN this predicate.
                    let current_none = shared
                        .current_tid_authoritative(cpu)
                        .map(|current| current == 0)
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
                // U3 (203C): same retirement as the FutexWait switch-success above and the two
                // D2 restores before it — one shared exact-token transaction, reused unchanged.
                let resumed = crate::arch::x86_64::trap::x86_post_lock_resume_marked_incoming(
                    shared,
                    token,
                    frame.as_deref_mut(),
                );
                crate::kernel::boot::yield_dispatch_clear(cpu_idx);
                if resumed.is_err() {
                    let rolled_back = token
                        .into_dequeued_authority()
                        .is_some_and(|a| shared.direct_dispatch_rollback_split(a));
                    crate::arch::x86_64::trap::enter_post_lock_dispatch_fatal(
                        cpu,
                        inc,
                        rolled_back,
                    );
                }
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
    drain_switch_plan_stash(shared, cpu, frame.as_deref_mut())?;

    // U7 (canonical 199E) — THE PRODUCTION IPC-TIMEOUT ENTRY, x86_64 + AArch64 cell.
    //
    // With the broad `SpinLock<KernelState>` from `with_cpu` already dropped above, scan both
    // OFF-LOCK-retired timeout classes through the cursor-bounded arch-neutral collector (rank-2
    // task split seam) and settle each class's due work through its own off-lock transaction
    // (per-domain split-mut seams). Nothing here holds the broad lock.
    //
    // Stage 200C2B wired this same machinery behind the reply-timeout oracle FEATURE and its
    // runtime selector, so production still processed both classes inside
    // `process_ipc_timeout_deadlines` under the broad lock. U7 removes both gates: this is now
    // the ordinary production timer/deadline path on every build and every boot, and the two
    // classes are gone from the broad-lock scan. The oracle knobs still select oracle SCENARIOS;
    // they no longer decide where timeouts are processed.
    //
    // Ordinary receive-timeout deadlines are NOT a U7 class and stay on the in-lock scan (the
    // collector's classifier skips them).
    //
    // NB: RISC-V does NOT flow through this shared entry — it wires the identical seam into its
    // own trap wrapper's Phase 3. The explicit `not(riscv64)` gate keeps that a single-driver
    // invariant even if a future path routes RISC-V here, so the one-shot attestations can never
    // be driven from two wrappers.
    #[cfg(not(target_arch = "riscv64"))]
    shared.run_due_ipc_timeout_work(cpu);

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
        // Lock-dropped proof + identity read in ONE coherent split transaction.
        //
        // U3 (canonical 203C): this was a brief broad `with_cpu` re-acquire (the Stage 195F
        // pattern). It is now `post_lock_exit_validation_split` — the SAME transaction the
        // RISC-V exit consumer already uses, not a second helper — which takes the same four
        // read-only facts under the rank-1 scheduler lock with the rank-2 task lock NESTED
        // inside it. That is canonical ascending rank order and one snapshot that cannot tear
        // between the scheduler read and the task read, which the old body could: it read
        // `current_tid` and `task_present_in_any_runqueue` from rank 1 and `task_asid` /
        // `task_status` from rank 2, each re-entering its own domain lock underneath the broad
        // guard.
        //
        // Every clause this consumer decides on below is unchanged, fact for fact:
        //
        //   * `current` — the current TID on the exact trapping CPU. `with_cpu(cpu, …)` bound
        //     `current_cpu` to this CPU as a side effect of admission, so the old
        //     `kernel.current_tid()` resolved to `current_tid_on(cpu)`, which is what the
        //     transaction reads directly.
        //   * `identity_ok` — the FULL `{tid, asid}` incarnation. A numeric TID match alone
        //     would let a restarted task satisfy a stale disposition. An absent TCB, and a TCB
        //     carrying no ASID, are identity-safe — exactly what `task_asid` returning `None`
        //     meant here.
        //   * `terminal` — `Exited(_)`, `Dead`, or absent. The lifecycle has no distinct
        //     `Exiting` state: `exit_task` commits straight to `Exited(status)` and a reaped
        //     TCB is `Dead` or gone. Anything else means the disposition does not describe
        //     reality.
        //   * `in_runqueue` — absence from EVERY CPU's runqueue and current slot, not merely
        //     this CPU's queue. `task_present_in_any_runqueue` delegated straight to
        //     `Scheduler::task_present_anywhere`, which is the predicate the transaction calls.
        //
        // `validate_online_cpu` refusal is preserved: an invalid or offline CPU still yields
        // the identical `KernelError` through the identical `map_err`, so the failure path
        // below is untouched.
        //
        // The lock-dropped proof is undiminished. Acquiring EITHER domain lock here is only
        // possible because the Phase-2 broad guard was released far above — the broad
        // `SpinLock<KernelState>` contains both domains, so a still-held guard would deadlock
        // exactly as it would have against the old `with_cpu`.
        let crate::runtime::PostLockExitValidation {
            current,
            identity_ok,
            terminal,
            in_runqueue,
        } = shared
            .post_lock_exit_validation_split(cpu, tid, asid)
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
                // frame restore. The exiting task's ELR/SP_EL0 are never a source here.
                //
                // U3 (canonical 203C): this was the second brief broad `with_cpu` re-acquire of
                // this consumer — `d2_recv_switch_incoming_asid(next)` followed by
                // `post_switch_restore_arch_thread_state`, both under one guard that also
                // performed the TTBR0 write and every frame write while holding it. It is now
                // ONE authoritative exact-identity transaction plus a purely off-lock apply:
                //
                //   * `post_exit_replacement_restore_split` authenticates the SAME
                //     `validate_online_cpu` admission predicate `with_cpu` used, binds
                //     `current_cpu` the same way, resolves the replacement on THIS exact CPU, and
                //     takes the ASID, the saved context, the TLS request and the parked
                //     blocked-syscall completion as ONE coherent rank-1 → rank-2 observation
                //     (ascending canonical order, one acquisition of each). The retired body read
                //     those facts through four separate re-entrant domain acquisitions underneath
                //     the broad guard, which could tear between the ASID it activated and the
                //     context it restored.
                //   * `post_exit_restore_replacement` then does the TTBR0/frame/hardware work with
                //     EVERY domain lock released, through the SAME single frame writer the in-lock
                //     restore uses — so the completion-before-argument-mirror ordering, the
                //     error-lane convention and the TLS lane have exactly one owner.
                //
                // `frame.is_some()` is passed in because the retired body switched the address
                // space unconditionally and only then discovered it had no frame: with no frame,
                // nothing may be consumed. The refusal classes are unchanged — an invalid or
                // offline CPU still yields the identical `KernelError` through the identical
                // `map_err`, and a stale replacement identity fails closed onto the same
                // `TrapHandleError::Syscall` path the restore's `TaskMissing` already took.
                let restore = shared
                    .post_exit_replacement_restore_split(cpu, tid, next, frame.is_some())
                    .map_err(|err| TrapHandleError::Syscall(err.into()))?;
                super::aarch64::trap::post_exit_restore_replacement(
                    cpu,
                    frame.as_deref_mut(),
                    &restore,
                );
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
        // U9-QA §2: FutexWait (NR 9) — the one SWITCHING pre-lock class. Without its ABI in the
        // frame the split dispatcher sees `nr = 0` and declines, which is why AArch64 kept taking
        // the in-lock deferral while x86_64 and RISC-V had moved to the pre-lock route. This is
        // the AArch64 counterpart of the RISC-V `split_eligible` gate, and it admits NR 9 for the
        // same reason: this architecture has the apply convention FutexWait needs
        // (`direct_dispatch_resume_incoming_core` — TTBR0/ASID activation, exact EL0 context,
        // exact parked completion), and its Stage 195E drain has driven it live for that class
        // since it landed.
        || raw_nr == crate::kernel::syscall::SYSCALL_FUTEX_WAIT_NR
        // U9-RX4 — IpcRecv (NR 2), admitted for exactly the reason NR 9 was, once the blocker
        // U9-RX3 recorded here was actually fixed rather than worked around.
        //
        // U9-RX3 measured that admitting NR 2 made its blocking route fire correctly on this
        // architecture, and reverted anyway: the import is not selective, so it also reaches the
        // Stage-32B queued-plain split recv, whose boundary writeback dropped a materialized
        // reply cap and skipped the canonical receiver-visible projection. PM answered
        // `PM_RECV_DECODE_FAIL`, never replied, and its caller stayed blocked. That was a
        // PRE-EXISTING defect — the same failure was already visible on x86_64 — and importing
        // NR 2 would have imposed it on a second architecture.
        //
        // U9-RX4 repaired it in the one writeback owner, so the precondition this admission was
        // waiting on is met. What the class needs from the architecture it demonstrably has: the
        // shared D2-recv drain — `d2_recv_reverify_blocked`, `d2_recv_dispatch_step_mut`,
        // `d2_resume_marked_incoming` — has settled AArch64 blocking-recv resumes live since U4
        // widened it here.
        || raw_nr == crate::kernel::syscall::SYSCALL_IPC_RECV_NR
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
    //
    // U9-QA §2: FutexWait (nr=9) joins them. It is the one SWITCHING class, and it needs this
    // more than the others do: the blocked caller will not trap again to collect its result, so
    // its `set_ok(1,0,0)` and its advanced SVC must be committed into THIS incarnation now, or
    // it would re-execute the `svc` and re-block when the wake eventually resumes it.
    if frame.syscall_num() == crate::kernel::syscall::SYSCALL_DEBUG_LOG_NR
        || frame.syscall_num() == crate::kernel::syscall::SYSCALL_FUTEX_WAKE_NR
        || frame.syscall_num() == crate::kernel::syscall::SYSCALL_FUTEX_WAIT_NR
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
