// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use crate::arch::trap::{FaultAccess, FaultInfo, TrapEvent};
use crate::kernel::boot::{FaultBookkeepingMode, KernelState, TrapHandleError};
use crate::kernel::scheduler::{CpuId, MAX_CPUS};
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
    context: Riscv64TrapContext,
    mut frame: Option<&mut TrapFrame>,
    fault_bookkeeping_mode: FaultBookkeepingMode,
) -> Result<(), TrapHandleError> {
    let _ = kernel.set_current_cpu(cpu);
    // Stage 196A: `GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE[cpu]` is now OWNED by the RISC-V shared
    // trap wrapper (`handle_riscv_trap_entry_shared`): it sets the flag TRUE before this
    // broad-lock (`with_cpu`) phase and clears it AFTER, then runs `drain_dispatch_post_work`.
    // Because a real post-`with_cpu` drainer now exists, the blocked-waiter delivery producers
    // (`produce_blocked_waiter_{plain,ordinary_cap,reply_cap}_delivery`) may legitimately take
    // the DEFERRED snapshot path — the drainer completes the wake after the guard drops, exactly
    // as on x86_64/AArch64. The prior force-false (which forced the LEGACY inline wake because
    // RISC-V had no drainer) is therefore RETIRED here; the wrapper owns the flag lifecycle.
    // NOTE: the standalone `handle_trap_entry` entry (tests) leaves the flag at its default
    // (false), so unit tests still take the inline wake path — no drainer runs there.
    let _ = kernel.process_cross_cpu_work_for_cpu(cpu);
    kernel.handle_trap_event_with_fault_bookkeeping_mode(
        decode_trap_context(context),
        frame.as_deref_mut(),
        fault_bookkeeping_mode,
    )?;
    // Stage 196D (QUEUE-SWITCH FOUNDATION BYPASS): if the in-lock Yield handler recorded a
    // one-shot foundation switch deferral (it published + re-enqueued the outgoing task and
    // cleared `current`), the canonical in-lock restore below has NO current task to restore
    // and would either error (→ spurious idle/halt) or restore stale state. Skip ONLY the
    // in-lock restore + ret-lane export and return cleanly from the bounded `with_cpu` phase;
    // the wrapper's post-lock switch drain performs the authoritative dispatch + SATP/sfence +
    // frame restore for the INCOMING task. This bypass requires an ACTUAL pending deferral
    // (no generic "skip restore" flag) and is inert for every normal syscall.
    let cpu_idx = cpu.0 as usize;
    if crate::kernel::boot::riscv_queue_switch_foundation_is_deferred(cpu_idx) {
        crate::yarm_log!(
            "RISCV_QUEUE_SWITCH_FOUNDATION_HANDLER_RETURN_OK cpu={}",
            cpu.0
        );
        return Ok(());
    }
    // Stage 163L: restore FIRST so apply_user_context (called inside
    // resume_current_thread_with_frame) does not zero a0 (user_gprs[10])
    // from the pre-syscall TCB snapshot before we can export ret0 below.
    restore_arch_thread_state(kernel, cpu, frame.as_deref_mut())?;
    // RISC-V ecall does not advance SEPC automatically, but the boot bridge
    // (yarm_riscv64_trap_bridge) pre-advances tframe.saved_pc by +4 before
    // calling handle_trap_entry so that sync_current_thread_from_frame captures
    // sepc+4 into the TCB.  Stage 163L's restore reloads that sepc+4; adding
    // another +4 here would double-advance to sepc+8 (Stage 163M regression fix).
    // Export ret0→a0 and ret1→a1 (or error→a0) so userspace sees the correct
    // syscall return value — apply_user_context zeroed a0 from the pre-syscall
    // TCB snapshot.
    if context.scause == EXC_USER_ECALL {
        if let Some(f) = frame.as_deref_mut() {
            if crate::kernel::boot::ipc_recv_proof_sender_wake_active() {
                let tid = kernel.current_tid().unwrap_or(0);
                crate::yarm_log!(
                    "RISCV_FORK_PARENT_RET_BEFORE_RETURN tid={} ret0={} a0={} err={}",
                    tid,
                    f.ret0(),
                    f.user_gpr(10),
                    f.error
                );
            }
            if let Some(err) = f.error_code() {
                f.set_user_gpr(10, err);
            } else {
                f.set_user_gpr(10, f.ret0());
                f.set_user_gpr(11, f.ret1());
            }
            if crate::kernel::boot::ipc_recv_proof_sender_wake_active() {
                let tid = kernel.current_tid().unwrap_or(0);
                let nr = f.syscall_num();
                crate::yarm_log!(
                    "NONX86_SYSCALL_RETURN_LANE_SET arch=riscv64 tid={} nr={} ret0={} err={}",
                    tid,
                    nr,
                    f.ret0(),
                    f.error
                );
                crate::yarm_log!(
                    "RISCV_TRAP_RETURN_FRAME tid={} a0={} a1={} a2={} err={}",
                    tid,
                    f.user_gpr(10),
                    f.user_gpr(11),
                    f.user_gpr(12),
                    f.error
                );
            }
        }
    }
    Ok(())
}

// ── Stage 196A: RISC-V shared trap-entry wrapper + post-lock drain foundation ──
//
// One-shot latch for the structural wrapper markers (BEGIN / GLOBAL_LOCK_* /
// POST_LOCK_DRAIN_* / DONE). RISC-V traps fire thousands of times per boot
// (every syscall round-trip + every deferred timer/IRQ audit), so the markers
// are emitted exactly once (first trap) to prove the shared-path structure
// without flooding the boot log. The active-flag lifecycle itself runs on
// EVERY trap (see `handle_riscv_trap_entry_shared`); only the log lines are
// latched.
static RISCV_SHARED_TRAP_MARKERS_LOGGED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

// Stage 196B: one-shot latch for the DebugLog (NR 15) split-dispatch markers
// (RISCV_SPLIT_ABI_IMPORT_OK / YARM_LOCK_SPLIT_DISPATCH / RISCV_SPLIT_FINALIZE_OK).
// The split dispatch itself runs on EVERY DebugLog; only the log lines are latched
// so the thousands of boot-time DebugLog calls do not flood the log.
static RISCV_DEBUGLOG_SPLIT_MARKERS_LOGGED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

// Stage 196C: same one-shot latch for the FutexWake (NR 10) split-dispatch markers.
static RISCV_FUTEXWAKE_SPLIT_MARKERS_LOGGED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

// Stage 196A post-lock-drain FOUNDATION oracle state (default-off; armed by
// `yarm.riscv64_post_lock_foundation_oracle=1`).
//   - DONE_FLAG: one-shot guard so the oracle publishes/consumes exactly once.
//   - TOKEN: the per-CPU post-work token published during the broad-lock phase
//     (holds the requesting tid, +1 biased so 0 always means "empty").
static RISCV_POST_LOCK_FOUNDATION_ORACLE_DONE_FLAG: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);
static RISCV_POST_LOCK_FOUNDATION_ORACLE_TOKEN: [core::sync::atomic::AtomicU64; MAX_CPUS] =
    [const { core::sync::atomic::AtomicU64::new(0) }; MAX_CPUS];

/// Stage 196A: the RISC-V shared trap-entry wrapper — the contract-equivalent
/// of the x86_64/AArch64 `handle_trap_entry_shared`, but purpose-built for the
/// RISC-V trap bridge and enabling **zero** retirement classes.
///
/// Phases (mirroring `arch/trap_entry.rs::handle_trap_entry_shared`):
///   1. **Pre-lock phase** — the split dispatcher services exactly ONE class,
///      DebugLog (NR 15, Stage 196B), off the global lock and returns early
///      (skipping the broad-lock phase entirely). EVERY other RISC-V syscall is
///      declined here (its nr never reaches `try_split_dispatch_into_frame`) and
///      falls through to the unchanged broad-lock handler exactly once — so no
///      other retirement class is enabled.
///   2. **Broad-lock phase** — `GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE[cpu]` is set
///      TRUE (the RISC-V path now OWNS the flag, replacing the retired
///      force-false), then the UNCHANGED canonical trap handler runs inside a
///      single bounded `shared.with_cpu` callback. No raw `&mut KernelState`
///      escapes this callback; there is no nested broad lock.
///   3. **Post-lock phase** — the outer `SpinLock<KernelState>` guard is dropped,
///      the flag is cleared, and `drain_dispatch_post_work` runs any blocked-
///      waiter delivery the broad-lock phase stashed (the real post-`with_cpu`
///      drainer that lets the deferred-snapshot producers wake receivers off the
///      broad borrow). The default-off foundation oracle then proves genuine
///      post-lock-drain ordering with a real lock-dropped re-acquire.
///
/// The bridge performs the RISC-V-specific frame write-back + SATP activation +
/// `sret` AFTER this returns; this wrapper does not touch the trap frame's
/// register lanes beyond what the canonical handler already does.
pub fn handle_riscv_trap_entry_shared(
    shared: &crate::runtime::SharedKernel,
    cpu: CpuId,
    context: Riscv64TrapContext,
    frame: &mut TrapFrame,
) -> Result<(), TrapHandleError> {
    use core::sync::atomic::Ordering;
    let cpu_idx = cpu.0 as usize;
    let is_syscall = matches!(decode_trap_context(context), TrapEvent::Syscall);

    // ── Phase 1: pre-lock split dispatch — DebugLog (NR 15) ONLY ──
    // Stage 196B/196C: RISC-V enables exactly TWO split-dispatch retirement classes,
    // DebugLog (NR 15) and FutexWake (NR 10). The RISC-V trap bridge has ALREADY
    // imported the syscall ABI into the portable frame (a7→nr, a0..a5→args), so the
    // split ABI is present; we gate the split dispatcher to those two NRs explicitly
    // here so that the shared `try_split_dispatch_into_frame` (which also knows
    // IpcRecv / VmBrk / InitramfsReadChunk / ControlPlaneSetCnodeSlots) can NEVER
    // service any other class on RISC-V. Both classes are serviced off the global
    // lock and return EARLY (skipping the broad-lock phase + the active flag
    // entirely): DebugLog is a pure read, and FutexWake only mutates waiter/run-queue
    // state without switching the CALLER (it stays `current`). Neither needs a
    // post-lock drain, so the RISC-V bridge's existing same-task ecall write-back
    // (sepc+4 once, sstatus preserved, a0 result lane from `set_ok`) finalizes them.
    // Every other syscall falls through to the unchanged broad-lock handler once.
    let nr = frame.syscall_num();
    let split_eligible = is_syscall
        && (nr == crate::kernel::syscall::SYSCALL_DEBUG_LOG_NR
            || nr == crate::kernel::syscall::SYSCALL_FUTEX_WAKE_NR);
    if split_eligible {
        // Per-class one-shot latch so BOTH classes' markers appear once (without
        // flooding: DebugLog fires thousands of times, FutexWake a handful).
        let log_split = if nr == crate::kernel::syscall::SYSCALL_DEBUG_LOG_NR {
            !RISCV_DEBUGLOG_SPLIT_MARKERS_LOGGED.swap(true, Ordering::Relaxed)
        } else {
            !RISCV_FUTEXWAKE_SPLIT_MARKERS_LOGGED.swap(true, Ordering::Relaxed)
        };
        if log_split {
            crate::yarm_log!("RISCV_SPLIT_ABI_IMPORT_OK nr={}", nr);
        }
        if let Some(result) =
            crate::kernel::syscall_split::try_split_dispatch_into_frame(shared, cpu, frame)
        {
            match result {
                Ok(()) => {
                    // The class helper already wrote the success lanes via `set_ok`
                    // and emitted its arch-tagged GLOBAL_LOCK_RETIRE_CLASS_{BEGIN,DONE}
                    // (and, for FutexWake, FUTEX_WAKE_SPLIT_{BEGIN,DONE}) markers. Skip
                    // the broad-lock phase: the active flag is NOT set, so no drain is
                    // owed and nothing is left true across the sret.
                    if log_split {
                        crate::yarm_log!(
                            "YARM_LOCK_SPLIT_DISPATCH arch=riscv64 nr={} cpu={} result=ok",
                            nr,
                            cpu.0
                        );
                        crate::yarm_log!("RISCV_SPLIT_FINALIZE_OK nr={} result=ok", nr);
                    }
                    return Ok(());
                }
                Err(TrapHandleError::Syscall(e)) => {
                    // A normal syscall error produced on the split path is encoded
                    // into the frame and returned to userspace (parity with the
                    // global-lock path); the split path stashed no switch plan.
                    frame.set_err(e.code());
                    if log_split {
                        crate::yarm_log!(
                            "YARM_LOCK_SPLIT_DISPATCH arch=riscv64 nr={} cpu={} result=handled_err code={}",
                            nr,
                            cpu.0,
                            e.code()
                        );
                        crate::yarm_log!("RISCV_SPLIT_FINALIZE_OK nr={} result=handled_err", nr);
                    }
                    return Ok(());
                }
                // A genuine kernel-side failure (e.g. MissingTrapFrame) — propagate.
                Err(other) => return Err(other),
            }
        }
        // The helper declined (None: unavailable requester, or a FutexWake
        // validation miss that the global-lock path must encode canonically) —
        // fall through to the unchanged broad-lock handler exactly once.
    }

    // One-shot latch for the structural markers. Consumed HERE (after the DebugLog
    // early-return), so a split-DebugLog trap — which never reaches the broad-lock
    // phase — does NOT swallow the latch; the markers fire on the first trap that
    // actually runs the broad-lock phase (a timer/IRQ or non-DebugLog syscall).
    let log_structural = !RISCV_SHARED_TRAP_MARKERS_LOGGED.swap(true, Ordering::Relaxed);
    if log_structural {
        crate::yarm_log!("RISCV_SHARED_TRAP_ENTRY_BEGIN cpu={}", cpu.0);
    }

    // ── Phase 2: own the active flag, then run the canonical handler in-lock ──
    // Set the flag BEFORE the broad-lock phase so the blocked-waiter producers
    // see a real drainer will run (deferred-snapshot path), and clear it AFTER.
    if cpu_idx < MAX_CPUS {
        crate::kernel::boot::GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE[cpu_idx]
            .store(true, Ordering::Relaxed);
    }
    if log_structural {
        crate::yarm_log!("RISCV_GLOBAL_LOCK_DROP_ACTIVE_SET cpu={}", cpu.0);
    }

    // Foundation-oracle arming decision (default-off, one-shot, syscalls only).
    let oracle_arm = is_syscall
        && crate::kernel::boot::riscv_post_lock_foundation_oracle_enabled()
        && !RISCV_POST_LOCK_FOUNDATION_ORACLE_DONE_FLAG.load(Ordering::Acquire);

    let inner_result = shared
        .with_cpu(cpu, |kernel| {
            // Foundation oracle PUBLISH — during the broad-lock phase, stash a
            // one-shot post-work token (the requester tid, +1 biased). This is a
            // pure atomic write: it mutates NO scheduler / capability / user / task
            // state and copies no user data. It only records "a post-lock drain is
            // owed for this tid".
            if oracle_arm && cpu_idx < MAX_CPUS {
                let tid = kernel.current_tid().unwrap_or(0);
                RISCV_POST_LOCK_FOUNDATION_ORACLE_TOKEN[cpu_idx]
                    .store(tid.wrapping_add(1), Ordering::Release);
                crate::yarm_log!(
                    "RISCV_POST_LOCK_FOUNDATION_ORACLE_PUBLISH_OK cpu={} tid={}",
                    cpu.0,
                    tid
                );
            }
            // Reborrow so `frame` stays available for the Stage 196D post-lock switch drain
            // (which restores the INCOMING task's frame after the broad guard drops).
            handle_trap_entry_with_fault_bookkeeping_mode(
                kernel,
                cpu,
                context,
                Some(&mut *frame),
                FaultBookkeepingMode::RecordInHandleTrapEvent,
            )
        })
        .map_err(|err| TrapHandleError::Syscall(err.into()));

    if log_structural {
        crate::yarm_log!("RISCV_GLOBAL_LOCK_PHASE_DONE cpu={}", cpu.0);
    }
    // Clear the flag now that the broad borrow has dropped; the drain below
    // completes any stashed blocked-waiter delivery.
    if cpu_idx < MAX_CPUS {
        crate::kernel::boot::GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE[cpu_idx]
            .store(false, Ordering::Relaxed);
    }
    if log_structural {
        crate::yarm_log!("RISCV_GLOBAL_LOCK_DROP_ACTIVE_CLEAR cpu={}", cpu.0);
    }

    let inner_result = inner_result?;

    // ── Phase 3: post-lock drain (broad guard released) ──
    if log_structural {
        crate::yarm_log!("RISCV_POST_LOCK_DRAIN_BEGIN cpu={}", cpu.0);
    }
    // The real post-`with_cpu` dispatch-return drainer: executes any blocked-
    // waiter delivery the broad-lock phase stashed. Inert on traps that stash
    // nothing (the common case). This is the mechanism that makes the RISC-V
    // deferred-snapshot wake path complete AFTER the broad borrow drops.
    shared.drain_dispatch_post_work(cpu)?;

    // Foundation-oracle DRAIN — consume the token published in-lock, proving the
    // outer guard is genuinely dropped by RE-ACQUIRING `with_cpu` here (a held
    // guard would deadlock). Reads only `current_tid`; performs no mutation.
    if oracle_arm && cpu_idx < MAX_CPUS {
        let token = RISCV_POST_LOCK_FOUNDATION_ORACLE_TOKEN[cpu_idx].swap(0, Ordering::AcqRel);
        if token != 0 {
            let published_tid = token.wrapping_sub(1);
            // Lock-dropped proof: this re-acquire is only possible because the
            // broad guard above was released — a still-held guard deadlocks here.
            let current_after = shared
                .with_cpu(cpu, |kernel| kernel.current_tid())
                .ok()
                .flatten();
            crate::yarm_log!(
                "RISCV_POST_LOCK_FOUNDATION_ORACLE_LOCK_DROPPED_OK cpu={}",
                cpu.0
            );
            crate::yarm_log!(
                "RISCV_POST_LOCK_FOUNDATION_ORACLE_DRAIN_OK cpu={} tid={}",
                cpu.0,
                published_tid
            );
            // Same-task return: the oracle syscall neither blocks nor switches, so
            // the trap will `sret` back to the publishing task (current == token).
            if current_after == Some(published_tid) {
                crate::yarm_log!(
                    "RISCV_POST_LOCK_FOUNDATION_ORACLE_USER_RETURN_OK tid={}",
                    published_tid
                );
                crate::yarm_log!("RISCV_POST_LOCK_FOUNDATION_ORACLE_DONE result=ok");
            } else {
                crate::yarm_log!(
                    "RISCV_POST_LOCK_FOUNDATION_ORACLE_DONE result=task_switched current={:?}",
                    current_after
                );
            }
            RISCV_POST_LOCK_FOUNDATION_ORACLE_DONE_FLAG.store(true, Ordering::Release);
        }
    }

    // ── Stage 196D: queue-advancing context-switch FOUNDATION drain ──
    // If the in-lock Yield handler recorded a one-shot foundation switch deferral (published +
    // re-enqueued the outgoing task, cleared `current`), perform the authoritative post-lock
    // switch to the INCOMING task now that the broad guard is released: dequeue B (rank-1
    // scheduler seam), set B current, mark B Running (rank-2 task seam), then a brief `with_cpu`
    // re-acquire does the REAL RISC-V arch restore — construct + write B's SATP (with the
    // `sfence.vma` inside `write_satp`) and restore B's saved frame (sepc/sstatus/GPRs) into the
    // trap frame. The bridge then `sret`s into B. NO x86 CR3 / AArch64 TTBR0 logic is used.
    if cpu_idx < MAX_CPUS && crate::kernel::boot::riscv_queue_switch_foundation_is_deferred(cpu_idx)
    {
        crate::yarm_log!("RISCV_QUEUE_SWITCH_FOUNDATION_DRAIN_BEGIN cpu={}", cpu.0);
        let outgoing = crate::kernel::boot::riscv_queue_switch_foundation_outgoing(cpu_idx);
        // Lock-dropped proof: `yield_reverify_ready` re-acquires the scheduler seam through the
        // SharedKernel (only possible because the broad `with_cpu` guard was released above — a
        // still-held guard would deadlock). It also confirms `current` is still cleared.
        let reverify_ok = shared.yield_reverify_ready(cpu);
        crate::yarm_log!(
            "RISCV_QUEUE_SWITCH_FOUNDATION_LOCK_DROPPED_OK cpu={}",
            cpu.0
        );
        if reverify_ok {
            // Queue-advancing dequeue of the FIFO head (the incoming task B).
            let incoming = shared.yield_dispatch_step_mut(cpu);
            if let Some(inc) = incoming {
                crate::yarm_log!(
                    "RISCV_QUEUE_SWITCH_FOUNDATION_DEQUEUE_OK cpu={} incoming={}",
                    cpu.0,
                    inc
                );
                crate::yarm_log!(
                    "RISCV_QUEUE_SWITCH_FOUNDATION_CURRENT_SET_OK cpu={} incoming={}",
                    cpu.0,
                    inc
                );
                shared.d6_genuine_mark_running_via_task_seam(incoming);
                crate::yarm_log!("RISCV_QUEUE_SWITCH_FOUNDATION_RUNNING_OK incoming={}", inc);
                // Brief `with_cpu` re-acquire: real SATP write + sfence.vma + frame restore.
                let restore = shared
                    .with_cpu(cpu, |kernel| {
                        // Incoming ASID / page-table root → construct + write SATP. `write_satp`
                        // executes `csrw satp` THEN `sfence.vma x0, x0` (a global flush — see
                        // below), so both the address-space activation and the required
                        // ordering fence are real hardware operations, not markers.
                        if let Some(asid) = kernel.task_asid(inc) {
                            let _ =
                                crate::arch::riscv64::page_table::map_kernel_shared_into_asid(asid);
                            if let Some(satp) = crate::arch::riscv64::page_table::cr3_for_asid(asid)
                            {
                                crate::arch::riscv64::page_table::write_satp(satp);
                                crate::yarm_log!(
                                    "RISCV_QUEUE_SWITCH_FOUNDATION_SATP_OK incoming={} asid={}",
                                    inc,
                                    asid.0
                                );
                                crate::yarm_log!(
                                    "RISCV_QUEUE_SWITCH_FOUNDATION_SFENCE_OK incoming={}",
                                    inc
                                );
                            }
                        }
                        // Restore B's saved user frame (sepc/sstatus/GPRs) into the trap frame;
                        // the bridge propagates it to the hardware frame and `sret`s into B.
                        restore_arch_thread_state(kernel, cpu, Some(&mut *frame))
                    })
                    .map_err(|err| TrapHandleError::Syscall(err.into()));
                crate::kernel::boot::riscv_queue_switch_foundation_clear(cpu_idx);
                restore??;
                crate::yarm_log!("RISCV_QUEUE_SWITCH_FOUNDATION_FRAME_OK incoming={}", inc);
                crate::yarm_log!("RISCV_QUEUE_SWITCH_FOUNDATION_SRET_ARMED incoming={}", inc);
                crate::yarm_log!("RISCV_QUEUE_SWITCH_FOUNDATION_DRAIN_DONE result=ok");
            } else {
                // No incoming task: this is a genuine FAILURE (the oracle guarantees B exists).
                // Do NOT fabricate an idle task or a success marker.
                crate::kernel::boot::riscv_queue_switch_foundation_clear(cpu_idx);
                crate::yarm_log!(
                    "RISCV_QUEUE_SWITCH_FOUNDATION_FAIL reason=no_incoming cpu={} outgoing={:?}",
                    cpu.0,
                    outgoing
                );
            }
        } else {
            // An in-lock fallback superseded the deferral (current no longer cleared) — decline.
            crate::kernel::boot::riscv_queue_switch_foundation_clear(cpu_idx);
            crate::yarm_log!(
                "RISCV_QUEUE_SWITCH_FOUNDATION_FAIL reason=state_changed cpu={}",
                cpu.0
            );
        }
    }

    if log_structural {
        crate::yarm_log!("RISCV_POST_LOCK_DRAIN_DONE cpu={} result=ok", cpu.0);
        crate::yarm_log!("RISCV_SHARED_TRAP_ENTRY_DONE cpu={}", cpu.0);
    }

    // `inner_result` is the canonical handler's `Result<(), TrapHandleError>`
    // (the outer `with_cpu` KernelError was already propagated by the `?` above).
    inner_result
}

/// Stage 196A (Part 5): RISC-V post-switch architecture-restore FOUNDATION.
///
/// This is the RISC-V analogue of x86_64 `restore_arch_thread_state` /
/// AArch64 `restore_arch_thread_state_post_switch`: it restores the incoming
/// task's user register context into the trap frame. On RISC-V a future
/// queue-advancing drain (FutexWait / Yield / D2, all still deferred) would call
/// this AFTER the authoritative out-of-lock dispatch selects an incoming task,
/// under a brief `with_cpu` re-acquire, to complete the switch. It is NOT wired
/// on any live RISC-V path in this foundation stage (no retirement class is
/// enabled), so `arch/trap_entry.rs::post_switch_restore_arch_thread_state`
/// only delegates here for its documented contract; the SATP/`sfence.vma`
/// activation for such a switch is performed by the trap bridge today
/// (`map_kernel_shared_into_asid` + `write_satp` on the resumed task's asid,
/// carrying the required ordering). Replacing the prior silent `Ok(())` no-op
/// with this documented, exercisable API is the Part 5 deliverable: a future
/// switch drain uses the incoming task's SATP/ASID (bridge activation) together
/// with the sepc/sstatus/GPR restore performed here via
/// `resume_current_thread_with_frame`.
pub fn restore_arch_thread_state_post_switch(
    kernel: &mut KernelState,
    cpu: CpuId,
    frame: Option<&mut TrapFrame>,
) -> Result<(), TrapHandleError> {
    // The register-context restore is identical to the same-task path: reload the
    // incoming task's user GPRs / sepc (saved_pc) / sstatus-derived state and TLS
    // base into the trap frame. The SATP/ASID activation that MUST precede the
    // `sret` for a genuine cross-task switch is the caller's responsibility (the
    // bridge does it today); this function owns only the frame-side restore.
    restore_arch_thread_state(kernel, cpu, frame)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arch::trap::Trap;

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
                startup_args: UserImageSpec::DEFAULT_STARTUP_ARGS,
                ..Default::default()
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
                startup_args: UserImageSpec::DEFAULT_STARTUP_ARGS,
                ..Default::default()
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
