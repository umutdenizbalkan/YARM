// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use crate::arch::trap::{FaultAccess, FaultInfo, TrapEvent};
use crate::kernel::boot::{FaultBookkeepingMode, KernelState, TrapHandleError};
use crate::kernel::scheduler::{CpuId, MAX_CPUS};
use crate::kernel::trapframe::TrapFrame;
use crate::kernel::vm::VirtAddr;
// Stage 199D-WA3A-R2-SEAL (item E): every dispatch-mark consumer in this file matches all five
// outcomes explicitly; `RefusedTorn` reaches `dispatch_torn_fatal` and never returns.
use crate::runtime::{DispatchMarkOutcome as Mark, dispatch_torn_fatal};
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

/// Stage 197B: why the RISC-V trap wrapper decided to enter the kernel idle terminal. Idle is a
/// FIRST-CLASS SUCCESS outcome (not an error and not an `Err(Internal)` sentinel), tagged with an
/// explicit provenance so a genuine internal failure can never be mistaken for intentional idle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiscvIdleReason {
    /// The default-on FutexWait queue-advancing drain found no runnable incoming task while the
    /// outgoing caller stays `Blocked(Futex)` — the retired-class post-lock idle outcome.
    FutexWaitNoIncoming,
    /// Stage 198A1/198B: a canonical BLOCKING IPC syscall (IpcRecv / IpcCall / IpcSend) blocked the
    /// caller and dispatched away leaving no runnable task. The provenance is AUTHORITATIVE —
    /// published by the arch-neutral blocking seam (`BLOCKED_SYSCALL_IDLE_PROVENANCE`, with the
    /// exact blocking class recorded separately) and consumed here — so idle is a POSITIVE outcome
    /// of a real blocking operation, never inferred from `current == None` + zero-runnable state
    /// alone. (Generalized from the recv-only `BlockedRecvNoRunnable`, since the producer covers all
    /// three blocking IPC syscalls, not recv alone.)
    BlockedIpcNoRunnable,
    /// Stage 200D-0D1: an accepted `ExitCurrentTask` removed the last runnable task. Distinct
    /// from `BlockedIpcNoRunnable` because nothing is blocked — the task is gone — so the
    /// blocking-seam provenance token that branch requires is legitimately absent.
    ExitCurrentTaskNoRunnable,
}

/// Stage 197B: the explicit, typed result of the RISC-V shared trap-entry wrapper. It replaces the
/// former `Err(Internal)`-shaped idle sentinel: intentional idle is `EnterKernelIdle` (a success),
/// while a genuine internal failure stays on the `Err` channel of
/// `Result<RiscvTrapEntryOutcome, TrapHandleError>` and can never be read as idle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiscvTrapEntryOutcome {
    /// Same-task syscall return (DebugLog / FutexWake split, a normal handled syscall, or a
    /// FutexWait/Yield that did not switch): the trap `sret`s back to the caller.
    ReturnToCurrent,
    /// A post-lock switch drain (FutexWait switch / Yield switch) armed an incoming task's frame +
    /// SATP; the trap `sret`s into the incoming task.
    ReturnToIncoming,
    /// Intentional kernel idle — enter the WFI idle terminal with a typed provenance.
    EnterKernelIdle { reason: RiscvIdleReason },
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
    // Stage 200C2C2 — BLOCKED-SYSCALL COMPLETION boundary (RISC-V).
    //
    // Like AArch64, a blocked recv on this port is NEVER re-entered: the boot bridge
    // pre-advances `sepc` by +4 before `handle_trap_entry`, so the TCB captures `sepc+4` and a
    // resumed caller `sret`s to the instruction after its `ecall`. A remotely completed caller
    // therefore cannot receive its result by "returning" from the handler — it consumes the
    // exact parked completion HERE.
    //
    // Placement is deliberate: this runs AFTER `resume_current_thread_with_frame` (whose
    // `apply_user_context` reloads the saved `user_gprs` — the very write that would otherwise
    // clobber a result lane, cf. the Stage 163L a0-zeroing note below), so the canonical result
    // is written LAST and cannot be overwritten by the restored pre-block snapshot. `sepc`,
    // `sstatus`, `satp`/ASID, `sp` and `tp`/TLS are all left exactly as restored — the ecall
    // return address was advanced once, at block time.
    #[cfg(feature = "ipc-reply-timeout-oracle-core")]
    if let Some(current_tid) = kernel.current_tid() {
        if let Some(done) = kernel.take_blocked_syscall_completion(current_tid) {
            // RISC-V syscall-error convention. The result is written to BOTH lanes so it survives
            // either resume route:
            //   * post-switch resume — nothing runs after this, so a0 (`user_gprs[10]`) is what
            //     userspace sees; a1 is cleared so a stale success lane cannot be read as payload;
            //   * same-task return — the `EXC_USER_ECALL` export below re-derives a0 from the
            //     frame's ERROR lane (`if let Some(err) = f.error_code()`), which would otherwise
            //     overwrite the GPR write. Setting `set_err` makes that export carry the canonical
            //     code instead of clobbering it.
            frame.set_err(done.result as usize);
            frame.set_user_gpr(10, done.result as usize);
            frame.set_user_gpr(11, 0);
            // Delivery-authoritative: emitted only AFTER the canonical result is encoded into the
            // outgoing frame's established error lanes, and reporting those FINAL lane values.
            crate::yarm_log!(
                "RISCV_BLOCKED_SYSCALL_COMPLETION_DELIVERED tid={} class={:?} result=TimedOut code={} blocked_generation={} sepc=0x{:016x} final_a0={} final_a1={} result=ok",
                current_tid,
                done.syscall_class,
                done.result,
                done.blocked_generation,
                frame.saved_pc() as u64,
                frame.user_gpr(10),
                frame.user_gpr(11)
            );
            // Retirement is authorized ONLY here — after the exact completion was consumed and
            // the canonical result encoded into a valid `sret` frame.
            crate::kernel::boot::maybe_emit_reply_timeout_class_retired();
        }
    }
    let idx = cpu.0 as usize;
    if idx < MAX_CPUS {
        LAST_RESTORED_TLS_BASE[idx].store(tls.unwrap_or(0), Ordering::Relaxed);
    }
    Ok(())
}

/// U3 (canonical 203C) — the shared EXACT-TOKEN resume transaction for the three RISC-V
/// post-lock switch drains (queue-switch foundation, FutexWait switch-success, Yield
/// switch-success). It is the RISC-V analogue of AArch64's `direct_dispatch_resume_incoming`
/// and replaces the broad `with_cpu` re-acquire each of those drains used to perform.
///
/// Every step names the incoming task by the mark TOKEN's exact incarnation — the `{tid, asid}`
/// pair this transaction actually marked — never by the bare numeric TID and never by re-reading
/// `current`. If the TCB was replaced by a different incarnation that reused the TID, each step
/// refuses and this returns `None`, so the caller rolls back with its exact dequeued authority
/// and diverges: the replacement's address space is never activated and its context is never
/// copied into the frame.
///
/// Ordering matches the in-lock `restore_arch_thread_state` path this replaces:
///   1. real ASID activation (map kernel-shared → page-table root → `satp`, whose write issues
///      the `sfence.vma`), performed inside `direct_dispatch_activate_asid_split`;
///   2. the exact saved `UserRegisterContext` applied to the frame — RISC-V performs NO
///      AArch64-style x0..x5 argument mirror, matching `apply_current_thread_to_frame`;
///   3. under `ipc-reply-timeout-oracle-core`, the exact parked completion consumed LAST so the
///      canonical result cannot be clobbered by the restored pre-block snapshot, encoding the
///      same RISC-V lanes (`set_err`, a0, a1) and emitting the same delivery marker;
///   4. the TLS-restore take recorded in `LAST_RESTORED_TLS_BASE`, as before. `tp`/TLS itself is
///      left exactly as restored by the saved context — there is no separate TLS lane write.
///
/// Returns the activated ASID so each caller can emit its own class-specific SATP marker, or
/// `None` on any exact-identity refusal — in which case the frame carries no partial success and
/// the caller must NOT report FRAME_OK / SRET_ARMED.
fn direct_dispatch_resume_incoming(
    shared: &crate::runtime::SharedKernel,
    token: crate::runtime::DispatchMarkToken,
    frame: &mut TrapFrame,
) -> Option<u16> {
    let incoming = token.tid();
    let cpu = token.cpu();
    let asid = shared.direct_dispatch_activate_asid_split(token)?;
    let (context, tls) = shared.direct_dispatch_restore_context_split(token)?;
    frame.apply_user_context(context);
    #[cfg(feature = "ipc-reply-timeout-oracle-core")]
    if let Some(done) = shared.direct_dispatch_take_completion_split(token) {
        frame.set_err(done.result as usize);
        frame.set_user_gpr(10, done.result as usize);
        frame.set_user_gpr(11, 0);
        crate::yarm_log!(
            "RISCV_BLOCKED_SYSCALL_COMPLETION_DELIVERED tid={} class={:?} result=TimedOut code={} blocked_generation={} sepc=0x{:016x} final_a0={} final_a1={} result=ok",
            incoming,
            done.syscall_class,
            done.result,
            done.blocked_generation,
            frame.saved_pc() as u64,
            frame.user_gpr(10),
            frame.user_gpr(11)
        );
        crate::kernel::boot::maybe_emit_reply_timeout_class_retired();
    }
    let _ = incoming;
    let idx = cpu.0 as usize;
    if idx < MAX_CPUS {
        LAST_RESTORED_TLS_BASE[idx].store(tls.unwrap_or(0), Ordering::Relaxed);
    }
    Some(asid)
}

/// U3 — the truthful terminal for a post-lock switch drain whose EXACT-token resume refused.
/// This is NOT a torn dispatch (`dispatch_torn_fatal` would record a false reason): the mark
/// succeeded and the dequeue is real, but the incoming task's identity, address space or saved
/// context no longer matches the incarnation this transaction marked. The exact dequeued
/// authority — and only that; a `ContinuedCurrent` mark never yields one — is rolled back, then
/// we diverge. Returning through the outgoing task's frame, `ReturnToCurrent` or idle is
/// forbidden: the address space may already have been switched.
fn dispatch_resume_refused_fatal(
    shared: &crate::runtime::SharedKernel,
    token: crate::runtime::DispatchMarkToken,
    cpu: CpuId,
    incoming: u64,
    site: &str,
) -> ! {
    if let Some(authority) = token.into_dequeued_authority() {
        let _ = shared.direct_dispatch_rollback_split(authority);
    }
    panic!(
        "{site}: exact-token resume refused for incoming tid={incoming} on cpu={} — \
         dispatch rolled back; refusing to return through another task's frame",
        cpu.0
    );
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
    // Stage 196E (FUTEXWAIT RETIREMENT BYPASS): the same return-path bypass for a real FutexWait
    // deferral. The in-lock `futex_wait_current` published `Blocked(Futex)` + cleared `current`
    // and declined the in-lock dispatch, so the canonical restore below has NO current task and
    // would restore stale state. Skip it and return cleanly; the wrapper's post-lock FutexWait
    // drain performs the authoritative dispatch + real SATP/sfence.vma + frame restore for the
    // INCOMING task. This bypass is NARROW: it requires an ACTUAL pending FutexWait deferral (no
    // generic "skip restore" flag), is independent of the 196D foundation bypass above, and is
    // inert for normal syscalls and for legacy FutexWait (oracle off/ineligible ⇒ no deferral).
    if crate::kernel::boot::futex_wait_dispatch_is_deferred(cpu_idx) {
        let outgoing = crate::kernel::boot::futex_wait_dispatch_outgoing(cpu_idx).unwrap_or(0);
        crate::yarm_log!(
            "RISCV_FUTEX_WAIT_HANDLER_BYPASS_BEGIN cpu={} outgoing={}",
            cpu.0,
            outgoing
        );
        crate::yarm_log!("RISCV_FUTEX_WAIT_HANDLER_BYPASS_DONE cpu={}", cpu.0);
        return Ok(());
    }
    // Stage 196G (YIELD RETIREMENT BYPASS): the same return-path bypass for a real Yield deferral.
    // The in-lock `yield_current` set the caller Runnable, RE-ENQUEUED it exactly once, and cleared
    // `current`, so the canonical restore below has NO current task and would restore stale state.
    // Skip it and return cleanly; the wrapper's post-lock Yield drain performs the authoritative
    // queue-advancing dispatch + real SATP/sfence.vma + frame restore for the INCOMING task
    // (another task, or the re-enqueued caller itself when alone — always an incoming, never idle).
    // NARROW: requires an ACTUAL pending Yield deferral (no generic skip flag), independent of the
    // FutexWait + 196D foundation bypasses above, and inert for normal syscalls + legacy Yield.
    if crate::kernel::boot::yield_dispatch_is_deferred(cpu_idx) {
        let outgoing = crate::kernel::boot::yield_dispatch_outgoing(cpu_idx).unwrap_or(0);
        crate::yarm_log!(
            "RISCV_YIELD_HANDLER_BYPASS_BEGIN cpu={} outgoing={}",
            cpu.0,
            outgoing
        );
        crate::yarm_log!("RISCV_YIELD_HANDLER_BYPASS_DONE cpu={}", cpu.0);
        return Ok(());
    }
    // ── Stage 200D-0D1 (EXITCURRENTTASK IN-LOCK BYPASS) ────────────────────────────────
    //
    // The fourth member of the 196D/196E/196G bypass family, and deliberately the NARROWEST.
    // The source audit showed RISC-V does NOT need what AArch64 needed: there is no in-lock
    // idle divergence here (idle is a typed `EnterKernelIdle` produced in Phase 3), and the
    // canonical restore below reads the CURRENT task — which `exit_task` has already switched
    // to the replacement — so it restores the RIGHT task's context, not the exiting one's.
    //
    // What must be suppressed is the block AFTER the restore: for `EXC_USER_ECALL` it writes
    // `ret0`/`ret1` into `user_gpr(10)`/`(11)`. Those are the accepted NR16's own return values,
    // and NR16 must never publish a result. Letting it run would write a result on behalf of a
    // task that no longer exists into the frame the replacement is about to be resumed from.
    // (The bridge's `task_switched` write-back happens to overwrite a0..a5 from `arg[]`
    // afterwards, so this is latent rather than observed — which is exactly why it is fixed
    // here rather than left to depend on a downstream overwrite.)
    //
    // The no-replacement case additionally skips the restore: with `current == None` the
    // canonical restore would fail and propagate `Err`, turning an ordinary idle outcome into a
    // fatal trap. Phase 3 produces the typed idle instead.
    //
    // Requires an ACTUAL pending disposition (no generic skip flag) and is inert for every
    // other syscall on every other path.
    if crate::kernel::boot::post_lock_trap_disposition_pending(cpu_idx) {
        let replacement = kernel.current_tid().filter(|t| *t != 0);
        crate::yarm_log!(
            "EXIT_TASK_INLOCK_BYPASS_ARMED arch=riscv64 cpu={} replacement={} inlock_result_export=0 broad_lock=1 result=ok",
            cpu.0,
            replacement.unwrap_or(0)
        );
        if replacement.is_none() {
            // Idle outcome: restore NOTHING. Phase 3 names the outcome and returns typed idle.
            return Ok(());
        }
        // Replacement outcome: apply the REPLACEMENT's saved context (the canonical restore
        // sources `current`, which is already the replacement), then return WITHOUT the ecall
        // result export.
        restore_arch_thread_state(kernel, cpu, frame.as_deref_mut())?;
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

// Stage 197B: one-shot latch for the default-off NEGATIVE oracle (forced genuine internal error).
static RISCV_TYPED_OUTCOME_INTERNAL_ERROR_ORACLE_FIRED: core::sync::atomic::AtomicBool =
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
) -> Result<RiscvTrapEntryOutcome, TrapHandleError> {
    use core::sync::atomic::Ordering;
    let cpu_idx = cpu.0 as usize;
    let is_syscall = matches!(decode_trap_context(context), TrapEvent::Syscall);

    // ── Phase 1: pre-lock split dispatch — DebugLog (NR 15) ONLY ──
    // Stage 196B/196C: RISC-V enables exactly TWO split-dispatch retirement classes,
    // DebugLog (NR 15) and FutexWake (NR 10). The RISC-V trap bridge has ALREADY
    // imported the syscall ABI into the portable frame (a7→nr, a0..a5→args), so the
    // split ABI is present; we gate the split dispatcher to those two NRs explicitly
    // here so that the shared `try_split_dispatch_into_frame` (which also knows
    // IpcRecv / VmBrk / ControlPlaneSetCnodeSlots) can NEVER
    // service any other class on RISC-V. Both classes are serviced off the global
    // lock and return EARLY (skipping the broad-lock phase + the active flag
    // entirely): DebugLog is a pure read, and FutexWake only mutates waiter/run-queue
    // state without switching the CALLER (it stays `current`). Neither needs a
    // post-lock drain, so the RISC-V bridge's existing same-task ecall write-back
    // (sepc+4 once, sstatus preserved, a0 result lane from `set_ok`) finalizes them.
    // Every other syscall falls through to the unchanged broad-lock handler once.
    let nr = frame.syscall_num();
    // Stage 199A2C2 / Stage 199D (RISC-V readiness blocker 1): admit IpcCall (NR 6) + IpcReply
    // (NR 7) into the shared split dispatcher through the CANONICAL admission predicate, so the
    // off-lock request/reply gates run on RISC-V. The RISC-V bridge has already imported a7→nr +
    // a0..a5→args into the portable frame, so all six arguments are present. With admission
    // closed, NR6/NR7 are NOT split-eligible and fall through UNCHANGED to the broad-lock handler
    // (a normal boot is byte-identical). A handled NR6/NR7 finalizes via the SAME same-task ecall
    // write-back as DebugLog/FutexWake (sepc+4 once, sstatus preserved, a0 result lane from
    // `set_ok`) → `ReturnToCurrent`.
    //
    // This asks `ipccall_direct_admission_enabled()`, NOT `ipccall_direct_proof_enabled()`. The
    // two are equal on RISC-V *today* — admission is `production || proof` and production is
    // `cfg!(target_arch = "x86_64")`, so on RISC-V it is `false || proof` — which is exactly why
    // the swap is behaviour-preserving. What it buys is the future: while this asked the proof
    // gate directly, flipping the RISC-V production predicate would have been a SILENT NO-OP,
    // because `nr` would never reach `try_split_dispatch_into_frame`. Every one of the three
    // questions — ABI import (unconditional on this bridge), whitelist admission (here) and
    // direct-handler reachability (`syscall_split::try_split_dispatch_into_frame`) — now flows
    // through the one canonical helper.
    let is_ipc_direct = (nr == crate::kernel::syscall::SYSCALL_IPC_CALL_NR
        || nr == crate::kernel::syscall::SYSCALL_IPC_REPLY_NR)
        && crate::kernel::boot::ipccall_direct_admission_enabled();
    let split_eligible = is_syscall
        && (nr == crate::kernel::syscall::SYSCALL_DEBUG_LOG_NR
            || nr == crate::kernel::syscall::SYSCALL_FUTEX_WAKE_NR
            || is_ipc_direct);
    if split_eligible {
        // Per-class one-shot latch so BOTH DebugLog + FutexWake markers appear once (without
        // flooding). NR6/NR7 emit their arch-tagged retirement markers from the drain (kernel), not
        // here, so they never touch the DebugLog/FutexWake latches.
        let log_split = if nr == crate::kernel::syscall::SYSCALL_DEBUG_LOG_NR {
            !RISCV_DEBUGLOG_SPLIT_MARKERS_LOGGED.swap(true, Ordering::Relaxed)
        } else if nr == crate::kernel::syscall::SYSCALL_FUTEX_WAKE_NR {
            !RISCV_FUTEXWAKE_SPLIT_MARKERS_LOGGED.swap(true, Ordering::Relaxed)
        } else {
            false
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
                    return Ok(RiscvTrapEntryOutcome::ReturnToCurrent);
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
                    return Ok(RiscvTrapEntryOutcome::ReturnToCurrent);
                }
                // A genuine kernel-side failure (e.g. MissingTrapFrame) — propagate.
                Err(other) => return Err(other),
            }
        }
        // The helper declined (None: unavailable requester, or a FutexWake
        // validation miss that the global-lock path must encode canonically) —
        // fall through to the unchanged broad-lock handler exactly once.
    }

    // ── Stage 197B NEGATIVE oracle (default-off) ──
    // Force a GENUINE internal trap-handling error on the FIRST syscall from a LIVE current task.
    // The current task is provably live here (a user syscall trap; `current != 0`), so this is
    // NOT an idle condition. The bridge must take the fatal `RISCV_TRAP_HANDLE_FAILED` path — it
    // must NEVER read this `Err` as a FutexWait typed-idle success. This proves the error/idle
    // separation directly.
    if is_syscall && crate::kernel::boot::riscv_typed_outcome_internal_error_oracle_enabled() {
        let cur = shared.current_tid_authoritative(cpu).unwrap_or(0);
        if cur != 0
            && !RISCV_TYPED_OUTCOME_INTERNAL_ERROR_ORACLE_FIRED.swap(true, Ordering::Relaxed)
        {
            crate::yarm_log!(
                "RISCV_TYPED_OUTCOME_INTERNAL_ERROR_ORACLE_BEGIN cpu={} current={}",
                cpu.0,
                cur
            );
            return Err(TrapHandleError::Syscall(
                crate::kernel::syscall::SyscallError::Internal,
            ));
        }
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

    // Stage 197B / 198A1: a GENUINE internal failure ALWAYS stays on the `Err` channel — it can
    // never be read as idle. Stage 198A1 removes the former state-inferred terminal-idle
    // reclassification on this Err path (it inferred intentional idle from `current == None` + zero
    // runnable, which is now forbidden): intentional idle is produced ONLY from explicit typed
    // provenance (FutexWaitNoIncoming on the FutexWait drain, BlockedIpcNoRunnable on the Ok tail).
    // An Err here takes the fatal `RISCV_TRAP_HANDLE_FAILED` bridge path regardless of `current`.
    inner_result?;

    // `switched` becomes true iff a post-lock switch drain (FutexWait switch / Yield switch) armed
    // an incoming task; it selects `ReturnToIncoming` vs `ReturnToCurrent` at the single tail
    // return below. It NEVER selects idle — idle is produced only by the explicit typed branches.
    let mut switched = false;

    // ── Phase 3: post-lock drain (broad guard released) ──
    if log_structural {
        crate::yarm_log!("RISCV_POST_LOCK_DRAIN_BEGIN cpu={}", cpu.0);
    }
    // The real post-`with_cpu` dispatch-return drainer: executes any blocked-
    // waiter delivery the broad-lock phase stashed. Inert on traps that stash
    // nothing (the common case). This is the mechanism that makes the RISC-V
    // deferred-snapshot wake path complete AFTER the broad borrow drops.
    shared.drain_dispatch_post_work(cpu)?;

    // Stage 200C2C2 (IpcReplyTimeout OFF-LOCK RETIREMENT, RISC-V cell): with the broad
    // `SpinLock<KernelState>` from Phase 2's `with_cpu` genuinely released, collect DUE
    // token-bearing reply-receive deadlines through the NARROW collector (rank-2 task split seam)
    // and drain the per-CPU deferred work through the OFF-LOCK completion transaction (per-domain
    // split-mut seams). This is the arch-neutral machinery accepted for x86_64 and AArch64 — only
    // the call site and the monotonic clock differ. `now` comes from the RISC-V `time` CSR (the
    // scheduler tick does not advance reliably under a user workload on this port), the SAME clock
    // domain the deadline is armed in. Ordinary receive-timeout deadlines stay on the in-lock scan
    // (the collector's token-bearing filter skips them). Default-off: a strict no-op unless the
    // RISC-V oracle feature is built AND its selector is active.
    #[cfg(feature = "ipc-reply-timeout-oracle-core")]
    if crate::kernel::boot::x86_ipc_reply_timeout_oracle_enabled() {
        let now = shared.reply_timeout_now_split_read();
        shared.collect_due_reply_timeout_work(now, cpu);
        shared.drain_reply_timeout_post_work(cpu, now);
    }

    // Stage 200D-2A: the SERVER-DEATH post-lock drain, ungated (production behaviour on
    // every build). RISC-V drives its own post-lock area, so the drain is wired here for
    // the same reason the collector above is — one driver per port.
    let _ = shared.drain_server_death_post_work(cpu);

    // Foundation-oracle DRAIN — consume the token published in-lock and re-read the
    // current task. Reads only the scheduler's per-CPU current slot; performs no mutation.
    //
    // U3 (canonical 203C): this drain no longer re-acquires the broad lock. Its source
    // position is what proves the outer guard is gone — Phase 2's `with_cpu(cpu, …)`
    // closure has already returned above, so there is no broad `&mut KernelState` alive
    // here, and `cpu` is explicit and authoritative at this boundary. The current task is
    // now read through the authoritative rank-1 scheduler seam instead.
    //
    // The read is the same read: `with_cpu(cpu, |k| k.current_tid())` set `current_cpu =
    // cpu` and then resolved `scheduler.current_tid_on(current_cpu)` under the scheduler
    // lock; `current_tid_split_read(cpu)` resolves `scheduler.current_tid_on(cpu)` under
    // that same rank-1 lock, without the broad guard and without re-setting `current_cpu`
    // (Phase 2 already set it to this `cpu`). Any preceding post-lock dispatch, timeout or
    // server-death work above is visible because the split read happens at the same program
    // boundary and serializes on the scheduler domain. `None` stays `None` — an offline or
    // unknown CPU yields `None` exactly as the old `.ok().flatten()` did, and there is
    // deliberately NO broad-lock fallback if the read declines.
    if oracle_arm && cpu_idx < MAX_CPUS {
        let token = RISCV_POST_LOCK_FOUNDATION_ORACLE_TOKEN[cpu_idx].swap(0, Ordering::AcqRel);
        if token != 0 {
            let published_tid = token.wrapping_sub(1);
            let current_after = shared.current_tid_split_read(cpu);
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
            let dispatch = shared.yield_dispatch_step_mut(cpu);
            if let Some(inc) = dispatch.tid().map(|t| t.0) {
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
                // Stage 199D-WA3A-R2-SEAL (item E): exact `Runnable → Running` (or the
                // queue-neutral `Running → Running`), with all five outcomes matched
                // explicitly. A refusal has already undone exactly what the selection did, so
                // this drain returns to the unchanged current — except `RefusedTorn`, which is
                // fatal and must never return to userspace or continue scheduling.
                let token = match shared.d6_genuine_mark_running_via_task_seam(dispatch) {
                    Mark::Marked(token) => token,
                    Mark::Idle => {
                        crate::yarm_log!(
                            "RISCV_DISPATCH_DECLINED cpu={} incoming={} reason=idle",
                            cpu.0,
                            inc
                        );
                        return Ok(RiscvTrapEntryOutcome::ReturnToCurrent);
                    }
                    Mark::RefusedRolledBack => {
                        crate::yarm_log!(
                            "RISCV_DISPATCH_DECLINED cpu={} incoming={} reason=refused_dequeue_undone",
                            cpu.0,
                            inc
                        );
                        return Ok(RiscvTrapEntryOutcome::ReturnToCurrent);
                    }
                    Mark::RefusedNoSchedulerChange => {
                        crate::yarm_log!(
                            "RISCV_DISPATCH_DECLINED cpu={} incoming={} reason=refused_scheduler_untouched",
                            cpu.0,
                            inc
                        );
                        return Ok(RiscvTrapEntryOutcome::ReturnToCurrent);
                    }
                    Mark::RefusedTorn => {
                        dispatch_torn_fatal(cpu, inc, "riscv_queue_switch_foundation_dispatch")
                    }
                };
                crate::yarm_log!("RISCV_QUEUE_SWITCH_FOUNDATION_RUNNING_OK incoming={}", inc);
                // U3 (canonical 203C): the EXACT-TOKEN resume transaction — real
                // `map_kernel_shared_into_asid` + page-table root + `write_satp` (which issues
                // the `sfence.vma`) inside the rank-2 activation seam, then the exact saved
                // context, the TLS take and any parked completion. No broad `with_cpu`
                // re-acquire and no broad-lock fallback: an exact-identity refusal rolls the
                // dequeue back and diverges rather than resuming another task's frame.
                let Some(asid) = direct_dispatch_resume_incoming(shared, token, &mut *frame) else {
                    crate::kernel::boot::riscv_queue_switch_foundation_clear(cpu_idx);
                    dispatch_resume_refused_fatal(
                        shared,
                        token,
                        cpu,
                        inc,
                        "riscv_queue_switch_foundation_dispatch",
                    );
                };
                crate::yarm_log!(
                    "RISCV_QUEUE_SWITCH_FOUNDATION_SATP_OK incoming={} asid={}",
                    inc,
                    asid
                );
                crate::yarm_log!("RISCV_QUEUE_SWITCH_FOUNDATION_SFENCE_OK incoming={}", inc);
                crate::kernel::boot::riscv_queue_switch_foundation_clear(cpu_idx);
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

    // ── Stage 196E: queue-advancing FUTEXWAIT RETIREMENT drain ──
    // If the in-lock `futex_wait_current` recorded a one-shot FutexWait dispatch deferral
    // (published `Blocked(Futex)` + cleared `current`, declined the in-lock dispatch), perform
    // the authoritative post-lock switch to the INCOMING task now that the broad guard is
    // released: re-verify the outgoing waiter is STILL `Blocked(Futex)` (rank-2 task seam),
    // dequeue B (rank-1 scheduler seam), set B current, mark B Running (rank-2), then a brief
    // `with_cpu` re-acquire does the REAL RISC-V arch restore — construct + write B's SATP (with
    // the `sfence.vma` inside `write_satp`) and restore B's saved frame. This REUSES the 196D
    // switch machinery (write_satp / cr3_for_asid / restore_arch_thread_state); it does NOT
    // duplicate the SATP or frame-restore implementations. The bridge then `sret`s into B. This
    // is the FIRST genuine off-global-lock RISC-V syscall retirement that context-switches the
    // blocking caller. NO x86 CR3 / AArch64 TTBR0 logic is used.
    if cpu_idx < MAX_CPUS && crate::kernel::boot::futex_wait_dispatch_is_deferred(cpu_idx) {
        crate::yarm_log!("RISCV_FUTEX_WAIT_DISPATCH_DRAIN_BEGIN cpu={}", cpu.0);
        crate::yarm_log!("GLOBAL_LOCK_RETIRE_CLASS_BEGIN arch=riscv64 class=FutexWait");
        let outgoing = crate::kernel::boot::futex_wait_dispatch_outgoing(cpu_idx);
        // Lock-dropped proof: `futex_wait_reverify_blocked` re-acquires the rank-2 task seam
        // through the SharedKernel (only possible because the broad `with_cpu` guard was released
        // above — a still-held guard would deadlock) AND confirms the waiter is still
        // `Blocked(Futex)` (guards against a FutexWake race before dispatch).
        let reverify_ok = outgoing
            .map(|t| shared.futex_wait_reverify_blocked(t))
            .unwrap_or(false);
        crate::yarm_log!("RISCV_FUTEX_WAIT_DISPATCH_LOCK_DROPPED_OK cpu={}", cpu.0);
        if reverify_ok {
            if let Some(out) = outgoing {
                crate::yarm_log!("RISCV_FUTEX_WAIT_DISPATCH_REVERIFY_OK tid={}", out);
            }
            // Queue-advancing dequeue of the FIFO head (the incoming task B).
            let dispatch = shared.futex_wait_dispatch_step_mut(cpu);
            if let Some(inc) = dispatch.tid().map(|t| t.0) {
                crate::yarm_log!(
                    "RISCV_FUTEX_WAIT_DISPATCH_DEQUEUE_OK cpu={} incoming={}",
                    cpu.0,
                    inc
                );
                crate::yarm_log!(
                    "RISCV_FUTEX_WAIT_DISPATCH_CURRENT_SET_OK cpu={} incoming={}",
                    cpu.0,
                    inc
                );
                // Stage 199D-WA3A-R2-SEAL (item E): exact `Runnable → Running` (or the
                // queue-neutral `Running → Running`), with all five outcomes matched
                // explicitly. A refusal has already undone exactly what the selection did, so
                // this drain returns to the unchanged current — except `RefusedTorn`, which is
                // fatal and must never return to userspace or continue scheduling.
                let token = match shared.d6_genuine_mark_running_via_task_seam(dispatch) {
                    Mark::Marked(token) => token,
                    Mark::Idle => {
                        crate::yarm_log!(
                            "RISCV_DISPATCH_DECLINED cpu={} incoming={} reason=idle",
                            cpu.0,
                            inc
                        );
                        return Ok(RiscvTrapEntryOutcome::ReturnToCurrent);
                    }
                    Mark::RefusedRolledBack => {
                        crate::yarm_log!(
                            "RISCV_DISPATCH_DECLINED cpu={} incoming={} reason=refused_dequeue_undone",
                            cpu.0,
                            inc
                        );
                        return Ok(RiscvTrapEntryOutcome::ReturnToCurrent);
                    }
                    Mark::RefusedNoSchedulerChange => {
                        crate::yarm_log!(
                            "RISCV_DISPATCH_DECLINED cpu={} incoming={} reason=refused_scheduler_untouched",
                            cpu.0,
                            inc
                        );
                        return Ok(RiscvTrapEntryOutcome::ReturnToCurrent);
                    }
                    Mark::RefusedTorn => dispatch_torn_fatal(cpu, inc, "riscv_futex_wait_dispatch"),
                };
                crate::yarm_log!("RISCV_FUTEX_WAIT_DISPATCH_RUNNING_OK incoming={}", inc);
                // U3 (canonical 203C): the EXACT-TOKEN resume transaction — real
                // `map_kernel_shared_into_asid` + page-table root + `write_satp` (which issues
                // the `sfence.vma`) inside the rank-2 activation seam, then the exact saved
                // context, the TLS take and any parked completion. No broad `with_cpu`
                // re-acquire and no broad-lock fallback: an exact-identity refusal rolls the
                // dequeue back and diverges rather than resuming another task's frame.
                let Some(asid) = direct_dispatch_resume_incoming(shared, token, &mut *frame) else {
                    crate::kernel::boot::futex_wait_dispatch_clear(cpu_idx);
                    dispatch_resume_refused_fatal(
                        shared,
                        token,
                        cpu,
                        inc,
                        "riscv_futex_wait_dispatch",
                    );
                };
                crate::yarm_log!(
                    "RISCV_FUTEX_WAIT_DISPATCH_SATP_OK incoming={} asid={}",
                    inc,
                    asid
                );
                crate::yarm_log!("RISCV_FUTEX_WAIT_DISPATCH_SFENCE_OK incoming={}", inc);
                crate::kernel::boot::futex_wait_dispatch_clear(cpu_idx);
                crate::yarm_log!("RISCV_FUTEX_WAIT_DISPATCH_FRAME_OK incoming={}", inc);
                crate::yarm_log!("RISCV_FUTEX_WAIT_DISPATCH_SRET_ARMED incoming={}", inc);
                crate::yarm_log!("RISCV_FUTEX_WAIT_DISPATCH_DONE result=ok");
                crate::yarm_log!(
                    "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=riscv64 class=FutexWait result=ok"
                );
                // Stage 197B: an incoming task was dispatched + its frame/SATP armed → the tail
                // return is the typed ReturnToIncoming (NOT idle, NOT an error).
                switched = true;
            } else {
                // Stage 196F POST-LOCK IDLE OUTCOME: no runnable incoming task. This is a
                // SUCCESSFUL idle (not a failure): the outgoing caller stays `Blocked(Futex)`
                // (reverify_ok proved it above), `current` stays None, the deferral is cleared,
                // and the BSP enters the REAL RISC-V idle loop — via the bridge's EXISTING proven
                // idle policy — AFTER the broad lock is released. No frame is restored, no incoming
                // is fabricated, and no `sret` is attempted.
                crate::yarm_log!("RISCV_FUTEX_WAIT_DISPATCH_NO_INCOMING cpu={}", cpu.0);
                crate::yarm_log!("RISCV_FUTEX_WAIT_POST_LOCK_IDLE_BEGIN cpu={}", cpu.0);
                // U3 (canonical 203C): confirm `current` is None/idle through the authoritative
                // rank-1 scheduler seam. This is NOT a broad re-acquisition any more.
                //
                // What proves the broad guard is gone is the source position: Phase 2's
                // `with_cpu(cpu, …)` closure has already returned above, so no broad
                // `&mut KernelState` is alive here, and `cpu` is explicit and authoritative.
                //
                // Exact behavioral parity with the deleted `with_cpu` read. That read set
                // `current_cpu = cpu` (already this `cpu`, set by Phase 2) and evaluated
                // `matches!(kernel.current_tid(), None | Some(0))`, where `current_tid()`
                // resolves `scheduler.current_tid_on(current_cpu)` under the scheduler lock;
                // `current_tid_split_read(cpu)` resolves `scheduler.current_tid_on(cpu)` under
                // that same rank-1 lock. So: `None` → true, `Some(0)` → true, `Some(nonzero)`
                // → false, unchanged. The old `.unwrap_or(true)` covered a `with_cpu`
                // validation failure (invalid/offline CPU); the split read yields `None` in
                // exactly that case, which the `None` arm already maps to true. No broad-lock
                // fallback is added if the read declines.
                let current_none = matches!(shared.current_tid_split_read(cpu), None | Some(0));
                crate::yarm_log!(
                    "RISCV_FUTEX_WAIT_POST_LOCK_IDLE_LOCK_DROPPED_OK cpu={}",
                    cpu.0
                );
                crate::kernel::boot::futex_wait_dispatch_clear(cpu_idx);
                crate::yarm_log!("RISCV_FUTEX_WAIT_DISPATCH_DONE result=idle");
                crate::yarm_log!(
                    "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=riscv64 class=FutexWait result=ok"
                );
                // Narrowly-gated idle-oracle attestation (default-off workload knob).
                // `outgoing_blocked=1` because reverify_ok proved the caller is still
                // `Blocked(Futex)`; `current_none` from the rank-1 seam read above.
                // `lock_dropped=1` is retained for smoke/contract compatibility, and since U3 it
                // attests that execution REACHED this post-broad-lock boundary — the outer
                // `with_cpu` closure returned — NOT that a fresh broad re-acquisition succeeded.
                // The retirement evidence is the census deletion plus this oracle actually
                // running; the marker field alone is not evidence.
                if crate::kernel::boot::riscv_futex_wait_idle_oracle_enabled() {
                    crate::yarm_log!(
                        "RISCV_FUTEX_WAIT_IDLE_ORACLE_DONE result=ok lock_dropped=1 current_none={} outgoing_blocked=1",
                        current_none as u32
                    );
                }
                crate::yarm_log!("RISCV_FUTEX_WAIT_POST_LOCK_IDLE_ENTERED cpu={}", cpu.0);
                // Stage 197B: return the EXPLICIT typed idle outcome (a SUCCESS), NOT an
                // `Err(Internal)` sentinel. The bridge matches `EnterKernelIdle`, asserts the
                // `current == None|Some(0)` invariant, emits RISCV_TYPED_IDLE_OUTCOME +
                // RISCV_KERNEL_IDLE_WAITING_FOR_IO, runs the timer/PLIC idle-safe-point init, and
                // enters `riscv_trap_halt` (wfi) — the SAME idle terminal, now typed. No stale
                // frame is restored and no `sret` is armed; the active flag was already cleared.
                return Ok(RiscvTrapEntryOutcome::EnterKernelIdle {
                    reason: RiscvIdleReason::FutexWaitNoIncoming,
                });
            }
        } else {
            // A split FutexWake flipped the outgoing waiter to Runnable before the drain ran —
            // do NOT stale-dispatch it away, do NOT double-enqueue, do NOT lose it, do NOT emit
            // retirement success. Clear the deferral and decline (unreachable in the controlled
            // single-dispatcher oracle: nothing runs between the in-lock publish and this drain).
            crate::kernel::boot::futex_wait_dispatch_clear(cpu_idx);
            crate::yarm_log!(
                "RISCV_FUTEX_WAIT_DISPATCH_DEFERRED reason=state_changed cpu={}",
                cpu.0
            );
        }
    }

    // ── Stage 196G: queue-advancing YIELD RETIREMENT drain (DEFAULT-ON) ──
    // If the in-lock `yield_current` recorded a Yield deferral (set the caller Runnable,
    // re-enqueued it exactly once, cleared `current`, declined the in-lock dispatch), perform the
    // authoritative post-lock switch now that the broad guard is released: reverify `current` is
    // still cleared (lock-dropped proof), dequeue the FIFO head (another task, or the re-enqueued
    // caller itself when alone), set it current, mark it Running, then a brief `with_cpu` re-acquire
    // does the REAL RISC-V arch restore — construct + write the incoming SATP (`sfence.vma` inside
    // `write_satp`) and restore the incoming saved frame. The bridge then `sret`s into it. Reuses
    // the 196D–196F switch machinery; NO x86 CR3 / AArch64 TTBR0 logic. A published Yield ALWAYS
    // has an incoming (the re-enqueued caller) — there is NO idle outcome, and no-incoming is a real
    // invariant FAILURE (never idle, never `Err(Internal)`, never a fabricated task).
    if cpu_idx < MAX_CPUS && crate::kernel::boot::yield_dispatch_is_deferred(cpu_idx) {
        crate::yarm_log!("RISCV_YIELD_DISPATCH_DRAIN_BEGIN cpu={}", cpu.0);
        crate::yarm_log!("GLOBAL_LOCK_RETIRE_CLASS_BEGIN arch=riscv64 class=Yield");
        let outgoing = crate::kernel::boot::yield_dispatch_outgoing(cpu_idx);
        // Lock-dropped proof: `yield_reverify_ready` re-acquires the scheduler seam through the
        // SharedKernel (only possible because the broad `with_cpu` guard was released above — a
        // still-held guard would deadlock) AND confirms `current` is still cleared (the caller is
        // Runnable + queued exactly once).
        let reverify_ok = shared.yield_reverify_ready(cpu);
        crate::yarm_log!("RISCV_YIELD_DISPATCH_LOCK_DROPPED_OK cpu={}", cpu.0);
        if reverify_ok {
            if let Some(out) = outgoing {
                crate::yarm_log!("RISCV_YIELD_DISPATCH_REVERIFY_OK outgoing={}", out);
            }
            // Queue-advancing dequeue of the FIFO head.
            let dispatch = shared.yield_dispatch_step_mut(cpu);
            if let Some(inc) = dispatch.tid().map(|t| t.0) {
                crate::yarm_log!(
                    "RISCV_YIELD_DISPATCH_DEQUEUE_OK cpu={} incoming={}",
                    cpu.0,
                    inc
                );
                crate::yarm_log!(
                    "RISCV_YIELD_DISPATCH_CURRENT_SET_OK cpu={} incoming={}",
                    cpu.0,
                    inc
                );
                // Stage 199D-WA3A-R2-SEAL (item E): exact `Runnable → Running` (or the
                // queue-neutral `Running → Running`), with all five outcomes matched
                // explicitly. A refusal has already undone exactly what the selection did, so
                // this drain returns to the unchanged current — except `RefusedTorn`, which is
                // fatal and must never return to userspace or continue scheduling.
                let token = match shared.d6_genuine_mark_running_via_task_seam(dispatch) {
                    Mark::Marked(token) => token,
                    Mark::Idle => {
                        crate::yarm_log!(
                            "RISCV_DISPATCH_DECLINED cpu={} incoming={} reason=idle",
                            cpu.0,
                            inc
                        );
                        return Ok(RiscvTrapEntryOutcome::ReturnToCurrent);
                    }
                    Mark::RefusedRolledBack => {
                        crate::yarm_log!(
                            "RISCV_DISPATCH_DECLINED cpu={} incoming={} reason=refused_dequeue_undone",
                            cpu.0,
                            inc
                        );
                        return Ok(RiscvTrapEntryOutcome::ReturnToCurrent);
                    }
                    Mark::RefusedNoSchedulerChange => {
                        crate::yarm_log!(
                            "RISCV_DISPATCH_DECLINED cpu={} incoming={} reason=refused_scheduler_untouched",
                            cpu.0,
                            inc
                        );
                        return Ok(RiscvTrapEntryOutcome::ReturnToCurrent);
                    }
                    Mark::RefusedTorn => dispatch_torn_fatal(cpu, inc, "riscv_yield_dispatch"),
                };
                crate::yarm_log!("RISCV_YIELD_DISPATCH_RUNNING_OK incoming={}", inc);
                // U3 (canonical 203C): the EXACT-TOKEN resume transaction — real
                // `map_kernel_shared_into_asid` + page-table root + `write_satp` (which issues
                // the `sfence.vma`) inside the rank-2 activation seam, then the exact saved
                // context, the TLS take and any parked completion. No broad `with_cpu`
                // re-acquire and no broad-lock fallback: an exact-identity refusal rolls the
                // dequeue back and diverges rather than resuming another task's frame.
                let Some(asid) = direct_dispatch_resume_incoming(shared, token, &mut *frame) else {
                    crate::kernel::boot::yield_dispatch_clear(cpu_idx);
                    dispatch_resume_refused_fatal(shared, token, cpu, inc, "riscv_yield_dispatch");
                };
                crate::yarm_log!(
                    "RISCV_YIELD_DISPATCH_SATP_OK incoming={} asid={}",
                    inc,
                    asid
                );
                crate::yarm_log!("RISCV_YIELD_DISPATCH_SFENCE_OK incoming={}", inc);
                crate::kernel::boot::yield_dispatch_clear(cpu_idx);
                crate::yarm_log!("RISCV_YIELD_DISPATCH_FRAME_OK incoming={}", inc);
                crate::yarm_log!("RISCV_YIELD_DISPATCH_SRET_ARMED incoming={}", inc);
                crate::yarm_log!("RISCV_YIELD_DISPATCH_DONE result=ok");
                crate::yarm_log!(
                    "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=riscv64 class=Yield result=ok"
                );
                // Stage 197B: Yield always dispatched an incoming (the re-enqueued caller or
                // another task) → typed ReturnToIncoming. Yield NEVER produces an idle outcome.
                switched = true;
            } else {
                // A published Yield deferral MUST have an incoming (the re-enqueued caller is
                // always a candidate). No incoming is a genuine invariant FAILURE — NOT idle, NOT a
                // success, NO `Err(Internal)` sentinel, NO fabricated task. Clear the deferral and
                // emit the failure marker (unreachable in practice; the smoke rejects it).
                crate::kernel::boot::yield_dispatch_clear(cpu_idx);
                crate::yarm_log!(
                    "RISCV_YIELD_DISPATCH_FAIL reason=no_incoming cpu={} outgoing={:?}",
                    cpu.0,
                    outgoing
                );
            }
        } else {
            // An in-lock fallback superseded the deferral (current no longer cleared) — do NOT
            // double-dispatch. Clear and decline (unreachable single-CPU + IRQ-off).
            crate::kernel::boot::yield_dispatch_clear(cpu_idx);
            crate::yarm_log!(
                "RISCV_YIELD_DISPATCH_DEFERRED reason=state_changed cpu={}",
                cpu.0
            );
        }
    }

    if log_structural {
        crate::yarm_log!("RISCV_POST_LOCK_DRAIN_DONE cpu={} result=ok", cpu.0);
        crate::yarm_log!("RISCV_SHARED_TRAP_ENTRY_DONE cpu={}", cpu.0);
    }

    // ── Stage 200D-0D1: the RISC-V `CurrentTaskExited` consumer ─────────────────────────
    //
    // THE single RISC-V production call to `take_post_lock_trap_disposition`. Its position is
    // the contract, and every clause is a property of THIS line rather than of a comment:
    //
    //   after broad-lock release — Phase 2's `with_cpu` returned far above; the brief
    //                              re-acquire below is only possible BECAUSE the guard dropped
    //                              (a still-held guard would deadlock), so reaching the
    //                              validation at all is the proof.
    //   after every drain        — `drain_dispatch_post_work`, the reply-timeout collector +
    //                              drain, `drain_server_death_post_work`, the foundation
    //                              oracle, and the 196D/196E/196G switch drains have all run.
    //   before frame application — the bridge applies `tframe` to the hardware frame only
    //                              after this wrapper returns.
    //   before `sret`            — likewise; `sret` is the bridge's, several statements later.
    //
    // It must also precede the Stage 198A1 terminal-idle block below: that block treats
    // `current == None` WITHOUT blocking-seam provenance as a defect and takes the fatal `Err`
    // path. An accepted exit legitimately produces `current == None` with no provenance (nothing
    // blocked — the task is gone), so the exit outcome is decided here, before that check can
    // misread it.
    //
    // The consumer performs NO teardown, NO enqueue, NO PeerDeath or Timeout claim, NO result
    // publication, NO `publish_riscv_user_return`, NO userspace copy and NO second scheduler
    // path. It validates the exact `{tid, asid, cpu}` incarnation, attests the outcome, and
    // selects between the replacement the in-lock bypass already restored and the established
    // typed idle terminal.
    if let crate::kernel::boot::PostLockTrapDisposition::CurrentTaskExited { tid, asid } =
        crate::kernel::boot::take_post_lock_trap_disposition(cpu_idx)
    {
        crate::yarm_log!(
            "EXIT_TASK_BROAD_LOCK_RELEASED arch=riscv64 tid={} asid={} cpu={} broad_lock=0 holder=with_cpu result=ok",
            tid,
            asid.0,
            cpu.0
        );
        crate::yarm_log!(
            "EXIT_TASK_POST_LOCK_DRAIN_DONE arch=riscv64 cpu={} broad_lock=0 drains=all result=ok",
            cpu.0
        );
        crate::yarm_log!(
            "EXIT_TASK_DISPOSITION_CONSUMED arch=riscv64 tid={} asid={} cpu={} broad_lock=0 result=ok",
            tid,
            asid.0,
            cpu.0
        );
        // Lock-dropped proof + identity read in ONE coherent split transaction.
        //
        // U3 (canonical 203C): this was a brief broad `with_cpu` re-acquire. It is now
        // `post_lock_exit_validation_split`, which takes the SAME four read-only facts under
        // the rank-1 scheduler lock with the rank-2 task lock NESTED inside it — canonical
        // ascending rank order, one snapshot that cannot tear between the scheduler read and
        // the task read. Every clause the consumer decides on below is unchanged: full
        // `{tid, asid}` incarnation validation, an absent TCB meaning identity-safe AND
        // terminal, absence from EVERY CPU's runqueue rather than this CPU's alone, and the
        // identical `KernelError` for an invalid/offline CPU.
        //
        // The lock-dropped proof is undiminished: acquiring EITHER domain lock here is only
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
                "EXIT_TASK_EXITING_STILL_CURRENT arch=riscv64 tid={} cpu={} result=fail",
                tid,
                cpu.0
            );
            return Err(TrapHandleError::Syscall(
                crate::kernel::syscall::SyscallError::Internal,
            ));
        }
        if !identity_ok || !terminal || in_runqueue {
            crate::yarm_log!(
                "EXIT_TASK_WRONG_IDENTITY arch=riscv64 tid={} asid={} identity_ok={} terminal={} in_runqueue={} result=fail",
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
            "EXIT_TASK_EXITING_NOT_CURRENT arch=riscv64 tid={} asid={} cpu={} broad_lock=0 result=ok",
            tid,
            asid.0,
            cpu.0
        );
        crate::yarm_log!(
            "EXIT_TASK_ABSENCE_VALIDATED arch=riscv64 tid={} asid={} current=0 runqueue=0 restore_owner=0 frame_source=0 identity=tid_asid broad_lock=0 result=ok",
            tid,
            asid.0
        );
        // sret / trap-depth ownership (Stage 200D-0D1 audit): RISC-V has NO software
        // trap-dispatch depth counter on this path — x86_64's `TRAP_DISPATCH_DEPTH` has no
        // RISC-V analogue, and the bridge owns the single `sret`. The correct number of
        // consumer-side clears is therefore ZERO, and none is made here.
        crate::yarm_log!(
            "EXIT_TASK_SRET_OWNER arch=riscv64 cpu={} owner=trap_bridge software_depth_clears=0 broad_lock=0 result=ok",
            cpu.0
        );
        match current {
            Some(next) if next != 0 => {
                if next == tid {
                    crate::yarm_log!(
                        "EXIT_TASK_RESELECTED_EXITING_TASK arch=riscv64 tid={} cpu={} result=fail",
                        tid,
                        cpu.0
                    );
                    return Err(TrapHandleError::Syscall(
                        crate::kernel::syscall::SyscallError::Internal,
                    ));
                }
                crate::yarm_log!(
                    "EXIT_TASK_RESTORE_OWNER arch=riscv64 owner=replacement exiting_tid={} next_tid={} cpu={} broad_lock=0 result=ok",
                    tid,
                    next,
                    cpu.0
                );
                // The replacement's saved sepc/sstatus/GPRs were applied to `tframe` by the
                // in-lock bypass (which sourced `current`, already the replacement). Nothing is
                // re-applied here: a second restore would be a duplicate frame authority.
                crate::yarm_log!(
                    "EXIT_TASK_RESTORE_DONE arch=riscv64 owner=replacement next_tid={} cpu={} frame_source=replacement_tcb result=ok",
                    next,
                    cpu.0
                );
                // Typed outcome: the resumed task differs from the trap's entering task, so the
                // bridge's `task_switched` write-back sources the replacement's saved context.
                switched = true;
            }
            _ => {
                crate::yarm_log!(
                    "EXIT_TASK_RESTORE_OWNER arch=riscv64 owner=idle exiting_tid={} cpu={} broad_lock=0 result=ok",
                    tid,
                    cpu.0
                );
                // No replacement: no frame was restored by the bypass and none is armed here.
                // Return the ESTABLISHED typed idle terminal — the same `riscv_trap_halt` (wfi)
                // path FutexWait-no-incoming and the terminal all-blocked case already use, with
                // its own provenance so a live log can tell which class idled. No second idle
                // loop is introduced, and no `sret` is attempted.
                return Ok(RiscvTrapEntryOutcome::EnterKernelIdle {
                    reason: RiscvIdleReason::ExitCurrentTaskNoRunnable,
                });
            }
        }
    }

    // Stage 198A1: blocking-syscall TERMINAL-IDLE on the Ok path, from AUTHORITATIVE PROVENANCE —
    // NOT from scheduler state alone. A canonical blocking syscall (IpcRecv / IpcCall / IpcSend)
    // that blocks the last runnable task succeeds (`Ok`) while clearing `current`; the arch-neutral
    // blocking seam published `BLOCKED_SYSCALL_IDLE_PROVENANCE` for the tid it blocked. We CONSUME
    // that token here (always, so it never leaks to the next trap) and combine it with the terminal
    // scheduler state:
    //   * provenance present + terminal (current None|Some(0) AND zero runnable) → typed
    //     `EnterKernelIdle { BlockedIpcNoRunnable }`. Without this the bridge would `sret` a stale
    //     frame as tid 0 and hot-spin re-entering the blocked recv (an `IPC_RECV_ENTER tid=0` loop).
    //   * terminal state WITHOUT provenance → a BUG (spurious `current == None`): emit a defensive
    //     marker and take the canonical `Err` path (RISCV_TRAP_HANDLE_FAILED). NEVER silent idle.
    //   * not terminal → the provenance (if any) is simply consumed; fall through to the same-task /
    //     incoming tail return. A plain IpcSend SENDER keeps `current` non-zero → ReturnToCurrent.
    // FutexWait no-incoming produced its own typed idle earlier and never reaches here; Yield either
    // switches (`switched=true`) or keeps `current` and never idles.
    if !switched {
        let provenance = crate::kernel::boot::blocked_syscall_idle_provenance_take(cpu_idx);
        let terminal_idle = shared
            .with_cpu(cpu, |kernel| {
                matches!(kernel.current_tid(), None | Some(0))
                    && kernel.runnable_count_on_cpu(cpu) == 0
            })
            .unwrap_or(false);
        match (provenance, terminal_idle) {
            (Some((blocked_tid, class)), true) => {
                crate::yarm_log!(
                    "RISCV_BLOCKED_IPC_IDLE_PROVENANCE_OK tid={} class={}",
                    blocked_tid,
                    class.as_str()
                );
                return Ok(RiscvTrapEntryOutcome::EnterKernelIdle {
                    reason: RiscvIdleReason::BlockedIpcNoRunnable,
                });
            }
            (None, true) => {
                // DEFENSIVE: terminal-idle scheduler state with NO authoritative blocking-syscall
                // provenance must NEVER be read as intentional idle — it is a bug (a non-blocking
                // syscall left `current == None`). Take the canonical error path.
                crate::yarm_log!(
                    "RISCV_BLOCKED_IDLE_NO_PROVENANCE cpu={} current_none=1 runnable=0 result=defensive_err",
                    cpu.0
                );
                return Err(TrapHandleError::Syscall(
                    crate::kernel::syscall::SyscallError::Internal,
                ));
            }
            _ => {}
        }
    }

    // Stage 197B: the sole non-idle tail return. The broad-lock handler succeeded (`Ok` above);
    // a switch drain either armed an incoming task (`switched` → ReturnToIncoming) or the caller
    // returns same-task (ReturnToCurrent). This is NEVER an idle outcome — idle is produced only
    // by the explicit typed idle branches (FutexWait no-incoming / terminal all-blocked).
    Ok(if switched {
        RiscvTrapEntryOutcome::ReturnToIncoming
    } else {
        RiscvTrapEntryOutcome::ReturnToCurrent
    })
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
            .reserve_and_spawn_user_task_from_image_for_test(UserImageSpec {
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
