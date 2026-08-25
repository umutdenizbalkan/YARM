// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use crate::arch::trap::{FaultAccess, FaultInfo, TrapEvent};
use crate::kernel::boot::{FaultBookkeepingMode, KernelState, TrapHandleError};
use crate::kernel::scheduler::CpuId;
#[cfg(test)]
use crate::kernel::scheduler::MAX_CPUS;
use crate::kernel::trapframe::TrapFrame;
use crate::kernel::vm::VirtAddr;

const ESR_EC_SVC64: u32 = 0x15;
const ESR_EC_IABT_LOW: u32 = 0x20;
const ESR_EC_IABT_CUR: u32 = 0x21;
const ESR_EC_DABT_LOW: u32 = 0x24;
const ESR_EC_DABT_CUR: u32 = 0x25;
const ESR_EC_MASK: u32 = 0x3F;
const ARCH_TIMER_PPI_IRQ: u16 = 30;

const AARCH64_TRAP_TRACE: bool = false;

#[inline(always)]
fn aarch64_trap_trace(args: core::fmt::Arguments) {
    if AARCH64_TRAP_TRACE {
        crate::yarm_log!("{}", args);
    }
}

macro_rules! trap_trace { ($($arg:tt)*) => { aarch64_trap_trace(format_args!($($arg)*)) }; }

#[inline(always)]
fn idle_no_eret_loop() -> ! {
    crate::yarm_log!("SCHED_ENTER_IDLE_HLT");
    loop {
        unsafe { core::arch::asm!("wfi") };
    }
}

/// Stage 195F: enter the real BSP idle loop from the post-lock FutexWait drain, AFTER the broad
/// `KernelState` lock has been released. This is the SAME proven BSP idle primitive the normal
/// path uses (`idle_no_eret_loop`, a `wfi` loop) — NOT a second idle policy. `DAIF` is left as
/// the trap left it (IRQs are NOT permanently masked): `wfi` wakes on an unmasked pending
/// interrupt, the interrupt enters the normal AArch64 trap path (which re-acquires `with_cpu`
/// freely because it is released here), and either dispatches a now-runnable task or returns to
/// the `wfi`. `current` is None (no user task), so no stale userspace ELR/SPSR/frame is ever
/// returned. Never returns.
pub(crate) fn enter_post_lock_idle(cpu: CpuId) -> ! {
    crate::yarm_log!("AARCH64_FUTEX_WAIT_POST_LOCK_IDLE_ENTERED cpu={}", cpu.0);
    idle_no_eret_loop();
}

/// Stage 200D-0C1: the accepted-`ExitCurrentTask` idle outcome, entered from the post-lock
/// disposition consumer when the exiting task left no replacement.
///
/// This is NOT a second idle policy: it delegates to the identical `idle_no_eret_loop()`
/// primitive the normal path and the Stage 195F FutexWait idle outcome both use. Only the
/// attestation differs, so a live log can tell WHICH retirement class idled. Like 195F it runs
/// with the broad `KernelState` guard already dropped, so a wake IRQ re-enters the trap path
/// and dispatches normally. `current` is None, so no stale EL0 ELR/SPSR is ever returned.
/// Never returns.
pub(crate) fn enter_post_lock_idle_after_exit(cpu: CpuId, exiting_tid: u64) -> ! {
    crate::yarm_log!(
        "EXIT_TASK_IDLE_ENTERED arch=aarch64 cpu={} exiting_tid={} primitive=idle_no_eret_loop result=ok",
        cpu.0,
        exiting_tid
    );
    idle_no_eret_loop();
}

/// Stage 199D (AARCH64 BLOCKER 3) — the idle outcome of the post-lock direct dispatch drain.
///
/// Not a third idle policy: it delegates to the identical `idle_no_eret_loop()` primitive the
/// normal path, the 195F FutexWait idle outcome and the 200D-0C1 exit idle outcome all use.
/// Only the attestation differs, so a live log can tell which drain idled. The outgoing caller
/// stays parked, `current` stays clear, NO frame is restored, and the broad guard is already
/// dropped so a wake IRQ re-enters the trap path and dispatches normally. Never returns.
pub(crate) fn enter_post_lock_idle_after_direct_dispatch(cpu: CpuId, outgoing_tid: u64) -> ! {
    crate::yarm_log!(
        "AARCH64_DIRECT_DISPATCH_IDLE_ENTERED cpu={} outgoing_tid={} primitive=idle_no_eret_loop result=ok",
        cpu.0,
        outgoing_tid
    );
    idle_no_eret_loop();
}

/// Stage 199D (AARCH64 BLOCKER 3) — the EXPLICIT FATAL path of the post-lock dispatch drain.
///
/// Reached only when the drain had already mutated scheduler state (dequeued a task, set
/// `current`, marked it `Running`) and a later step then failed — the dequeue and `current`
/// disagreeing, or the selected task having no saved context to resume. Both are kernel-
/// invariant violations rather than races: the dequeue is what sets `current`, and a runnable
/// task without a saved context cannot be resumed by any path.
///
/// The caller has already rolled the scheduler mutation back exactly, so the state is
/// consistent when we stop; `rolled_back` records whether that succeeded. We deliberately do
/// NOT return: the alternative would be to `eret` through a frame belonging to a task the
/// scheduler has parked, which is precisely the corruption this path exists to prevent. Halting
/// with a named marker keeps the failure diagnosable in a live log.
pub(crate) fn enter_post_lock_dispatch_fatal(cpu: CpuId, incoming: u64, rolled_back: bool) -> ! {
    crate::yarm_log!(
        "AARCH64_DIRECT_DISPATCH_FATAL cpu={} incoming={} rolled_back={} reason=partial_dispatch_unrecoverable",
        cpu.0,
        incoming,
        rolled_back as u32
    );
    // `current` is clear and the task is back on the run queue (when the rollback succeeded), so
    // this is the same wake-capable halt the idle outcome uses — not a spin that masks the fault.
    idle_no_eret_loop();
}

/// Stage 199D (AARCH64 BLOCKER 3) — steps 4 and 5 of the post-lock dispatch: activate the
/// incoming task's address space and restore its complete EL0 frame and TLS state.
///
/// This is the broad-lock-free twin of what `restore_arch_thread_state_post_switch` does under
/// `with_cpu` for the FutexWait/Yield drains. It reproduces that path's observable effects
/// exactly, in the same order, but reaches every piece of state through bounded rank-2 task
/// seams instead of the broad `KernelState` lock:
///
/// 1. **ASID/TTBR0 activation** — through the SAME arch primitive
///    (`hal_adapters::switch_address_space`, carrying the established AArch64 DSB/ISB/TLBI
///    ordering), recorded in the single authoritative activation cell.
/// 2. **Complete saved EL0 context** — `apply_user_context` restores ELR, SP and all user
///    GPRs, resolved by EXACT incoming tid rather than by re-reading `current`.
/// 3. **x18 TLS** — the pending TLS-restore request is taken in the same rank-2 acquisition
///    as the context read, so the two cannot straddle a lock boundary.
/// 4. **Blocked-syscall completion** — consumed BEFORE the argument mirror and encoded into
///    the resume lanes, exactly as the in-lock path does, so a remotely completed caller
///    (e.g. a timed-out receive) resumes with its canonical result rather than stale receive
///    arguments. ELR is deliberately untouched: it was advanced once at block time.
/// 5. **Argument mirror** — `user_gprs[x0..x5] <- args[0..5]`, the `!syscall_return` branch of
///    `restore_arch_thread_state`. This drain resumes a task that is NOT returning from the
///    syscall being handled, so the mirror applies exactly as it does after a switch.
///
/// Returns `false` when the incoming task has no saved context to restore, in which case the
/// frame is left untouched and the caller fails closed.
/// U3 (canonical 203C) — which step of the exact-token resume refused.
///
/// The neutral core reports this so each caller can name the step in ITS OWN class telemetry;
/// the core itself emits no class-specific marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResumeRefusal {
    /// The exact ASID could not be activated (identity replaced, or no page-table root).
    Ttbr0,
    /// The exact saved EL0 context / TLS take refused.
    Context,
}

/// U3 (canonical 203C) — the NEUTRAL exact-token incoming-resume transaction.
///
/// This is the body that used to live inline in [`direct_dispatch_resume_incoming`], lifted so
/// the AArch64 FutexWait and Yield post-lock switch drains can share it instead of re-acquiring
/// the broad lock. It is deliberately free of `AARCH64_DIRECT_DISPATCH_*` telemetry: those
/// markers belong to the IPC-direct class, not to every resume, so the wrapper below re-emits
/// them and FutexWait/Yield emit only their own class markers.
///
/// Every step names the incoming task by the mark TOKEN's exact incarnation — the `{tid, asid}`
/// pair this transaction actually marked — never by bare numeric TID and never by re-reading
/// `current`. If the TCB was replaced by a different incarnation that reused the TID, the step
/// refuses: the replacement's address space is never activated and its context is never copied.
///
/// `AARCH64_BLOCKED_SYSCALL_COMPLETION_CONSUMED` stays here: it is resume-class behavior (the
/// exact parked completion is consumed by whichever path resumes the task), not IPC-direct
/// telemetry.
///
/// Returns the activated ASID on success, or which step refused.
/// U6 §8 — encode a consumed blocked-SEND completion into a resumed AArch64 frame.
///
/// Two lanes, because unlike the reply-timeout class a send completion can succeed:
///
/// * `result == 0` — the receiver consumed the message. The syscall succeeded, so the success
///   lanes are what a non-blocking `ipc_send` return leaves: `x0 = 0` and no transferred-cap
///   return.
/// * `result != 0` — a canonical error (today only `TimedOut`). The code lands in the x0 lane
///   and x1..x5 are zeroed, following the established AArch64 error convention
///   (`export_syscall_result_to_user_gprs`'s error path), so stale send ARGUMENTS can never be
///   mistaken for a result.
///
/// `ELR_EL1` is deliberately untouched in both cases — it was advanced exactly once at block
/// time. The caller mirrors `arg0..arg5` into `x0..x5` immediately after.
fn encode_blocked_send_completion(frame: &mut TrapFrame, result: u64) {
    frame.set_arg(0, result as usize);
    for lane in 1..=5 {
        frame.set_arg(lane, 0);
    }
}

pub(crate) fn direct_dispatch_resume_incoming_core(
    shared: &crate::runtime::SharedKernel,
    token: crate::runtime::DispatchMarkToken,
    frame: &mut TrapFrame,
) -> Result<u16, ResumeRefusal> {
    let incoming = token.tid();
    let asid = shared
        .direct_dispatch_activate_asid_split(token)
        .ok_or(ResumeRefusal::Ttbr0)?;
    let (context, tls) = shared
        .direct_dispatch_restore_context_split(token)
        .ok_or(ResumeRefusal::Context)?;
    frame.apply_user_context(context);
    frame.set_user_gpr(
        crate::arch::aarch64::syscall_abi::REG_X18_TLS,
        tls.unwrap_or(0),
    );
    #[cfg(test)]
    {
        let idx = token.cpu().0 as usize;
        if idx < MAX_CPUS {
            LAST_RESTORED_TLS_BASE[idx].store(tls.unwrap_or(0), Ordering::Relaxed);
        }
    }
    #[cfg(feature = "ipc-reply-timeout-oracle-core")]
    if let Some(done) = shared.direct_dispatch_take_completion_split(token) {
        frame.set_arg(0, done.result as usize);
        for lane in 1..=5 {
            frame.set_arg(lane, 0);
        }
        crate::yarm_log!(
            "AARCH64_BLOCKED_SYSCALL_COMPLETION_CONSUMED tid={} class={:?} result={} blocked_generation={} elr=0x{:016x} result=ok",
            incoming,
            done.syscall_class,
            done.result,
            done.blocked_generation,
            frame.saved_pc() as u64
        );
        crate::kernel::boot::maybe_emit_reply_timeout_class_retired();
    }
    // U6 §8 — the BLOCKED-SEND completion boundary (production-live, no feature gate).
    //
    // A blocked sender resumes here, and its `ELR_EL1` was advanced exactly once at block time
    // (see `crate::arch::trap::aarch64_syscall_elr_policy`), so the `SVC` is never re-executed
    // and the handler is never re-entered. The parked completion is therefore the ONLY thing
    // that can supply the result — without it the sender would return the `WouldBlock` its
    // saved frame still carries for a message the receiver already took.
    if let Some(done) = shared.direct_dispatch_take_send_completion_split(token) {
        encode_blocked_send_completion(frame, done.result);
        crate::yarm_log!(
            "AARCH64_BLOCKED_SEND_COMPLETION_CONSUMED tid={} class={} result={} blocked_generation={} elr=0x{:016x} result=ok",
            incoming,
            done.syscall_class.slug(),
            done.result,
            done.blocked_generation,
            frame.saved_pc() as u64
        );
    }
    use crate::arch::aarch64::syscall_abi::{REG_X0, REG_X1, REG_X2, REG_X3, REG_X4, REG_X5};
    frame.set_user_gpr(REG_X0, frame.arg(0));
    frame.set_user_gpr(REG_X1, frame.arg(1));
    frame.set_user_gpr(REG_X2, frame.arg(2));
    frame.set_user_gpr(REG_X3, frame.arg(3));
    frame.set_user_gpr(REG_X4, frame.arg(4));
    frame.set_user_gpr(REG_X5, frame.arg(5));
    let _ = incoming;
    Ok(asid)
}

pub(crate) fn direct_dispatch_resume_incoming(
    shared: &crate::runtime::SharedKernel,
    token: crate::runtime::DispatchMarkToken,
    frame: &mut TrapFrame,
) -> bool {
    // U3: a THIN wrapper over the neutral core. Its marker contract is unchanged — the same
    // `AARCH64_DIRECT_DISPATCH_IDENTITY_REFUSED tid=.. step=ttbr0|context` refusal distinction
    // and the same `AARCH64_DIRECT_DISPATCH_TTBR0_OK` on success — so its existing direct caller
    // sees exactly what it saw before.
    let incoming = token.tid();
    match direct_dispatch_resume_incoming_core(shared, token, frame) {
        Ok(asid) => {
            crate::yarm_log!(
                "AARCH64_DIRECT_DISPATCH_TTBR0_OK tid={} asid={}",
                incoming,
                asid
            );
            true
        }
        Err(ResumeRefusal::Ttbr0) => {
            crate::yarm_log!(
                "AARCH64_DIRECT_DISPATCH_IDENTITY_REFUSED tid={} step=ttbr0",
                incoming
            );
            false
        }
        Err(ResumeRefusal::Context) => {
            crate::yarm_log!(
                "AARCH64_DIRECT_DISPATCH_IDENTITY_REFUSED tid={} step=context",
                incoming
            );
            false
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Aarch64TrapContext {
    pub esr_el1: u32,
    pub far_el1: u64,
    pub irq_line: Option<u16>,
    pub is_timer_irq: bool,
}

#[cfg(test)]
use core::sync::atomic::{AtomicUsize, Ordering};

#[cfg(test)]
static LAST_RESTORED_TLS_BASE: [AtomicUsize; MAX_CPUS] = [const { AtomicUsize::new(0) }; MAX_CPUS];

#[cfg(test)]
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
    syscall_return: bool,
) -> Result<(), TrapHandleError> {
    let Some(frame) = frame else {
        return Ok(());
    };
    let Some(current_tid) = kernel.current_tid() else {
        crate::yarm_log!("SCHED_NO_RUNNABLE_USER_TASK");
        crate::yarm_log!("SCHED_ENTER_IDLE");
        return Ok(());
    };
    if current_tid == 0 || kernel.task_asid(current_tid).is_none() {
        crate::yarm_log!("SCHED_ENTER_IDLE");
        return Ok(());
    }
    // U3 (canonical 203C): the GATHER half. Every task-owned fact this restore needs is read or
    // consumed here, then handed to the single frame writer below. The takes are unchanged in
    // decision and in order — `IpcSend` before `IpcRecv`, and only when this is not a direct
    // syscall return — and none of them reads the frame, so performing them all before the first
    // frame write is behaviour-preserving. Splitting the gather from the write is what lets the
    // post-lock exit boundary reach the SAME writer with facts taken off the broad lock.
    let context = kernel
        .thread_user_context(current_tid)
        .ok_or(crate::kernel::boot::KernelError::TaskMissing)
        .map_err(crate::kernel::syscall::SyscallError::from)
        .map_err(TrapHandleError::Syscall)?;
    let tls = kernel
        .take_tls_restore_request(current_tid)
        .map_err(crate::kernel::syscall::SyscallError::from)
        .map_err(TrapHandleError::Syscall)?;
    let send_completion = (!syscall_return)
        .then(|| {
            kernel.take_blocked_syscall_completion_of_class(
                current_tid,
                crate::kernel::task::BlockedSyscallClass::IpcSend,
            )
        })
        .flatten();
    #[cfg(feature = "ipc-reply-timeout-oracle-core")]
    let recv_completion = (!syscall_return)
        .then(|| kernel.take_blocked_syscall_completion(current_tid))
        .flatten();
    apply_restored_thread_state(
        frame,
        cpu,
        &crate::kernel::task::ThreadRestoreFacts {
            tid: current_tid,
            context,
            tls,
            send_completion,
            #[cfg(feature = "ipc-reply-timeout-oracle-core")]
            recv_completion,
        },
        syscall_return,
    );
    Ok(())
}

/// U3 (canonical 203C) — THE frame-write half of the AArch64 thread-state restore.
///
/// One writer, THREE producers. `restore_arch_thread_state` reaches it with facts gathered under
/// the broad lock; `post_exit_restore_replacement` reaches it with facts taken by
/// `SharedKernel::post_exit_replacement_restore_split` with every domain lock released; and U9's
/// `post_switch_restore_arch_thread_state_split` reaches it with facts taken by
/// `SharedKernel::post_switch_restore_facts_split`, likewise off every lock. None of them carries
/// a second copy of this sequence, so the completion-before-argument-mirror ordering, the
/// error-lane convention and the TLS lane can only ever be changed in one place.
///
/// Touches nothing but `frame`: no kernel state is read, taken or written here. `facts` is already
/// the only remaining copy of whatever was consumed, so every completion it carries is encoded
/// exactly once, by construction.
pub(crate) fn apply_restored_thread_state(
    frame: &mut TrapFrame,
    cpu: CpuId,
    facts: &crate::kernel::task::ThreadRestoreFacts,
    syscall_return: bool,
) {
    let current_tid = facts.tid;
    let tls = facts.tls;
    frame.apply_user_context(facts.context);
    frame.set_user_gpr(
        crate::arch::aarch64::syscall_abi::REG_X18_TLS,
        tls.unwrap_or(0),
    );
    // For a freshly created task the saved user_gprs are [0; 32] while
    // arg0..arg5 hold the startup ABI values, so we mirror args into user_gprs.
    // For a resumed task capture_user_context already wrote user_gprs[i] into
    // arg_i, so the assignment is idempotent.
    //
    // Skip the mirror on a direct syscall return (!task_switched && Syscall):
    // export_syscall_result_to_user_gprs runs immediately after and sets
    // user_gprs[x0..x2] from the syscall return values.  Mirroring here would
    // overwrite those correct values with the original syscall input args.
    if !syscall_return {
        // Stage 200C2C1B — BLOCKED-SYSCALL COMPLETION boundary. A blocked recv on this port is
        // never re-entered (its `ELR_EL1` already points past the `SVC`), so a remotely completed
        // caller resumes straight to userspace from here. Encode the EXACT parked completion the
        // gather consumed — BEFORE the argument mirror below — into the resume lanes. Exactly-once
        // consumption is a property of the gather (there is one slot and it was emptied there);
        // exactly-once ENCODING is a property of this block, which sees the only remaining copy.
        //
        // Encoding follows the established AArch64 error convention (mirrors
        // `export_syscall_result_to_user_gprs`'s error path): the error code lands in the x0 lane
        // and x1..x5 are zeroed, so the stale receive ARGUMENTS can never be mistaken for a result.
        // ELR is deliberately untouched — it was advanced exactly once at block time.
        // U6 §8 — the BLOCKED-SEND completion boundary on the IN-LOCK resume path, the sibling
        // of the post-lock one in `direct_dispatch_resume_incoming_core`. Ungated: the send
        // class is production-live on every build, and it is class-scoped so it can never
        // consume the reply-timeout consumer's `IpcRecv` entry below.
        if let Some(done) = facts.send_completion {
            encode_blocked_send_completion(frame, done.result);
            crate::yarm_log!(
                "AARCH64_BLOCKED_SEND_COMPLETION_CONSUMED tid={} class={} result={} blocked_generation={} elr=0x{:016x} result=ok",
                current_tid,
                done.syscall_class.slug(),
                done.result,
                done.blocked_generation,
                frame.saved_pc() as u64
            );
        }
        #[cfg(feature = "ipc-reply-timeout-oracle-core")]
        if let Some(done) = facts.recv_completion {
            frame.set_arg(0, done.result as usize);
            for lane in 1..=5 {
                frame.set_arg(lane, 0);
            }
            crate::yarm_log!(
                "AARCH64_BLOCKED_SYSCALL_COMPLETION_CONSUMED tid={} class={:?} result={} blocked_generation={} elr=0x{:016x} result=ok",
                current_tid,
                done.syscall_class,
                done.result,
                done.blocked_generation,
                frame.saved_pc() as u64
            );
            // The retirement marker is authorized ONLY here — after the resumed caller's exact
            // completion was consumed and its canonical result encoded (never at production time).
            crate::kernel::boot::maybe_emit_reply_timeout_class_retired();
        }
        frame.set_user_gpr(crate::arch::aarch64::syscall_abi::REG_X0, frame.arg(0));
        frame.set_user_gpr(crate::arch::aarch64::syscall_abi::REG_X1, frame.arg(1));
        frame.set_user_gpr(crate::arch::aarch64::syscall_abi::REG_X2, frame.arg(2));
        frame.set_user_gpr(crate::arch::aarch64::syscall_abi::REG_X3, frame.arg(3));
        frame.set_user_gpr(crate::arch::aarch64::syscall_abi::REG_X4, frame.arg(4));
        frame.set_user_gpr(crate::arch::aarch64::syscall_abi::REG_X5, frame.arg(5));
    }
    trap_trace!(
        "AARCH64_FIRST_ENTRY_ARGS tid={} x0=0x{:x} x1=0x{:x} x2=0x{:x} x3=0x{:x} x4=0x{:x} x5=0x{:x}",
        current_tid,
        frame.arg(0),
        frame.arg(1),
        frame.arg(2),
        frame.arg(3),
        frame.arg(4),
        frame.arg(5)
    );
    trap_trace!(
        "AARCH64_CONTEXT_RESTORE_FULL tid={} elr=0x{:016x} sp=0x{:016x} x0=0x{:016x} x1=0x{:016x} x29=0x{:016x} x30=0x{:016x} ctx_ptr=0x{:x}",
        current_tid,
        frame.saved_pc() as u64,
        frame.saved_sp() as u64,
        frame.user_gpr(0) as u64,
        frame.user_gpr(1) as u64,
        frame.user_gpr(29) as u64,
        frame.user_gpr(30) as u64,
        frame as *const _ as usize
    );
    #[cfg(test)]
    {
        let idx = cpu.0 as usize;
        if idx < MAX_CPUS {
            LAST_RESTORED_TLS_BASE[idx].store(tls.unwrap_or(0), Ordering::Relaxed);
        }
    }
    #[cfg(not(test))]
    let _ = (cpu, tls);
}

/// U3 (canonical 203C) — the post-lock AArch64 `CurrentTaskExited` REPLACEMENT restore.
///
/// The off-lock half of the acquisition this stage retired. Everything here runs with EVERY
/// domain lock released: `SharedKernel::post_exit_replacement_restore_split` already took the
/// replacement's ASID and its restore facts as one coherent rank-1 → rank-2 observation, so this
/// performs only hardware and trap-frame work.
///
/// Outcome for outcome with the retired `with_cpu` body:
///
/// * the ASID activation is the SAME arch primitive the in-lock `d2_recv_switch_incoming_asid`
///   reached — `hal_adapters::switch_address_space`, carrying the established AArch64
///   DSB/ISB/TLBI ordering — followed by the same authoritative per-CPU activation record
///   `Hal::switch_address_space` writes, so every `active_asid_on` consumer observes it
///   identically. `asid == None` activates nothing, exactly as an absent `task_asid` did;
/// * `enter_idle` reproduces the restore's `SCHED_ENTER_IDLE` marker and its success return;
/// * the frame work is the SAME single writer the in-lock restore uses, with `syscall_return`
///   false — the value `post_switch_restore_arch_thread_state` always passed.
///
/// Only the replacement is ever a source: the exiting task's identity never reaches this function.
#[cfg(target_arch = "aarch64")]
pub(crate) fn post_exit_restore_replacement(
    cpu: CpuId,
    frame: Option<&mut TrapFrame>,
    restore: &crate::runtime::ExitReplacementRestore,
) {
    if let Some(asid) = restore.asid {
        crate::arch::hal_adapters::switch_address_space(asid);
        crate::arch::hal::note_address_space_activated(cpu, asid);
    }
    if restore.enter_idle {
        crate::yarm_log!("SCHED_ENTER_IDLE");
    }
    if let (Some(frame), Some(facts)) = (frame, restore.facts.as_ref()) {
        apply_restored_thread_state(frame, cpu, facts, false);
    }
}

/// Stage 117: post-switch arch thread state restore, called after
/// `switch_frames` in the incoming task's context. `syscall_return` is always
/// `false` here — the incoming task is resuming from a context switch, not a
/// direct syscall return.
///
/// U9 (canonical 203C): the stash drain now reaches AArch64 through
/// `trap_entry::post_switch_restore_arch_thread_state_split`, so this in-lock body no longer has a
/// production caller. It is kept, not deleted: it is the FOUNDATION the split twin is proven
/// equivalent to, branch for branch, and its `syscall_return = false` choice is the fact that twin
/// inherits.
#[allow(dead_code)]
pub(crate) fn restore_arch_thread_state_post_switch(
    kernel: &mut KernelState,
    cpu: CpuId,
    frame: Option<&mut TrapFrame>,
) -> Result<(), TrapHandleError> {
    restore_arch_thread_state(kernel, cpu, frame, false)
}

fn import_syscall_abi_from_user_gprs(frame: &mut TrapFrame) {
    frame.set_syscall_num(frame.user_gpr(crate::arch::aarch64::syscall_abi::REG_X8));
    frame.set_arg(0, frame.user_gpr(crate::arch::aarch64::syscall_abi::REG_X0));
    frame.set_arg(1, frame.user_gpr(crate::arch::aarch64::syscall_abi::REG_X1));
    frame.set_arg(2, frame.user_gpr(crate::arch::aarch64::syscall_abi::REG_X2));
    frame.set_arg(3, frame.user_gpr(crate::arch::aarch64::syscall_abi::REG_X3));
    frame.set_arg(4, frame.user_gpr(crate::arch::aarch64::syscall_abi::REG_X4));
    frame.set_arg(5, frame.user_gpr(crate::arch::aarch64::syscall_abi::REG_X5));
}

fn export_syscall_result_to_user_gprs(frame: &mut TrapFrame) {
    if let Some(error) = frame.error_code() {
        frame.set_user_gpr(crate::arch::aarch64::syscall_abi::REG_X0, error);
        frame.set_user_gpr(crate::arch::aarch64::syscall_abi::REG_X1, 0);
        frame.set_user_gpr(crate::arch::aarch64::syscall_abi::REG_X2, 0);
        frame.set_user_gpr(crate::arch::aarch64::syscall_abi::REG_X3, 0);
        frame.set_user_gpr(crate::arch::aarch64::syscall_abi::REG_X4, 0);
        frame.set_user_gpr(crate::arch::aarch64::syscall_abi::REG_X5, 0);
    } else {
        frame.set_user_gpr(crate::arch::aarch64::syscall_abi::REG_X0, frame.ret0());
        frame.set_user_gpr(crate::arch::aarch64::syscall_abi::REG_X1, frame.ret1());
        frame.set_user_gpr(crate::arch::aarch64::syscall_abi::REG_X2, frame.ret2());
        frame.set_user_gpr(crate::arch::aarch64::syscall_abi::REG_X3, frame.arg(3));
        frame.set_user_gpr(crate::arch::aarch64::syscall_abi::REG_X4, frame.arg(4));
        frame.set_user_gpr(crate::arch::aarch64::syscall_abi::REG_X5, frame.arg(5));
    }
}

/// Stage 160C: AArch64 trap-ABI bracketing for the pre-global-lock split
/// dispatch — IMPORT half. Decodes the syscall ABI (x8 → `syscall_num`,
/// x0–x5 → `args[]`) from the user GPRs into the `TrapFrame` *before* the split
/// dispatcher inspects it. Without this, the AArch64 vector handler hands the
/// shared trap entry a frame whose decoded `syscall_num`/`args` are still zero
/// (they are normally populated by `import_syscall_abi_from_user_gprs` only
/// inside the global handler, which runs *after* the split dispatch), so every
/// split-eligible syscall was rejected at the NR gate (`nr=0`) and fell back to
/// the global `legacy_full_path` (Stage 160B diagnosis).
///
/// Reuses the exact same import helper the global path uses, so the split path
/// sees byte-identical decoded ABI.
pub(crate) fn split_import_syscall_abi(frame: &mut TrapFrame) {
    import_syscall_abi_from_user_gprs(frame);
    // Stage 195A: production import-OK attestation (DebugLog live acceptance).
    crate::yarm_log!("AARCH64_SPLIT_ABI_IMPORT_OK nr={}", frame.syscall_num());
    crate::yarm_log!("AARCH64_SPLIT_ABI_IMPORT_DONE nr={}", frame.syscall_num());
}

/// Stage 160C: AArch64 trap-ABI bracketing — EXPORT + SVC-advance half. Runs
/// only when the split dispatcher actually HANDLED the syscall (returned
/// `Some`), to return its result to userspace exactly like the global path does
/// for a non-task-switched syscall.
///
/// The split path returns `Some` ONLY for a *completed* syscall — a successful
/// delivery or a definitive error (e.g. the recv-v2 queued-split rollback's
/// `InvalidArgs`). `WouldBlock` (the only retry case) returns `None` and stays on
/// the global path, which keeps its own block-and-retry PC policy. A completed
/// syscall therefore ALWAYS advances past the `SVC`, using the SAME resume PC the
/// proven global `IpcRecv`-success path uses (`last_vector_raw_elr() + 4`). The
/// export + `set_thread_user_context` + `restore_arch_thread_state` sequence and
/// ordering mirror the global non-task-switched syscall-return path
/// (`handle_trap_entry_with_fault_bookkeeping_mode`). The split path never
/// switches tasks, so `task_switched == false` always holds here.
pub(crate) fn split_finalize_handled_syscall(
    shared: &crate::runtime::SharedKernel,
    _cpu: CpuId,
    id: crate::runtime::SplitReturnIdentity,
    frame: &mut TrapFrame,
) {
    // ── Frame-only, outside every lock ───────────────────────────────────────────────
    //
    // Stage 195B fix: the resume PC is `last_vector_raw_elr()` with NO `+4`. On AArch64 the
    // synchronous-exception ELR_EL1 for an `SVC` already points at the instruction FOLLOWING
    // the `SVC`, exactly as the proven global non-IpcRecv return path uses it
    // (`syscall_resume_pc = raw_vector_return_pc`, no `+4`). The earlier `+4` over-advanced by
    // one instruction, skipping the caller's return-register load (`mov rN, x0`); DebugLog
    // tolerated the skip (it ignores its return), but a multi-return-lane split class returned
    // a stale register. The fix is retained: it guards every split-dispatched class whose
    // caller consumes a return value (e.g. FutexWake's count).
    let resume_pc = crate::arch::aarch64::boot::last_vector_raw_elr() as usize;
    frame.set_saved_pc(resume_pc);
    crate::yarm_log!(
        "AARCH64_SPLIT_CONTEXT_SAVE_DONE x0=0x{:x}",
        frame.user_gpr(crate::arch::aarch64::syscall_abi::REG_X0)
    );
    crate::yarm_log!(
        "AARCH64_SPLIT_SVC_ADVANCE_DONE pc=0x{:016x}",
        resume_pc as u64
    );
    crate::yarm_log!(
        "AARCH64_SPLIT_ABI_EXPORT_BEGIN err={} x0_before=0x{:x}",
        frame.error_code().unwrap_or(0),
        frame.user_gpr(crate::arch::aarch64::syscall_abi::REG_X0)
    );

    // ── Bounded rank-2 task-domain transaction #1: exact incarnation + TLS ───────────
    //
    // Stage 199D: this replaces `shared.with_cpu(...)` — the BROAD `KernelState` lock — which
    // every HANDLED AArch64 split syscall used to reacquire on its way out, so an eligible
    // NR6/NR7 transaction could never complete without retaking it.
    //
    // The legacy sequence was: save context → `restore_arch_thread_state` (which read that
    // same context straight back out of the TCB and applied it to the frame, then took TLS) →
    // export → re-sync args → save context again.
    //
    // **The pre-export save and the read-back are provably redundant for a NON-SWITCHING split
    // return, and are removed.** `TrapFrame::apply_user_context(capture_user_context())` is an
    // exact identity — the two move the same nine fields (pc, sp, user_gprs, args[0..5]) and
    // nothing else — and `set_thread_user_context` / `thread_user_context` store and return
    // `tcb.user_context` verbatim. The pre-export save's ONLY consumer was that read-back, and
    // the post-export save overwrites it before anything else can observe it. So the round
    // trip could only ever restore what it had just written. What is NOT redundant, and is
    // kept, is the TLS-restore take and the stale-incarnation bail.
    //
    // `None` = stale incarnation: skip the restore exactly as the legacy path did when
    // `current_tid()` was absent, `0`, or had no ASID — the export below still runs.
    if let Some(tls) = shared.split_return_take_tls_split(id) {
        frame.set_user_gpr(
            crate::arch::aarch64::syscall_abi::REG_X18_TLS,
            tls.unwrap_or(0),
        );
        #[cfg(test)]
        {
            let idx = _cpu.0 as usize;
            if idx < MAX_CPUS {
                LAST_RESTORED_TLS_BASE[idx].store(tls.unwrap_or(0), Ordering::Relaxed);
            }
        }
    } else {
        crate::yarm_log!("SCHED_ENTER_IDLE");
    }

    // ── Frame-only, outside every lock ───────────────────────────────────────────────
    //
    // The export sees the error encoded by `set_err` and writes it to x0; the diagnostics
    // prove x0_after carries the error code (e.g. InvalidArgs = 2) on the rollback path.
    export_syscall_result_to_user_gprs(frame);
    // Stage 195B fix: re-sync args[0..2] to the just-exported x0..x2 — byte-identical to the
    // global non-task-switched return path. Without this the resumed task reads the stale x0
    // (the original syscall arg0) instead of the exported return value.
    frame.set_arg(0, frame.user_gpr(crate::arch::aarch64::syscall_abi::REG_X0));
    frame.set_arg(1, frame.user_gpr(crate::arch::aarch64::syscall_abi::REG_X1));
    frame.set_arg(2, frame.user_gpr(crate::arch::aarch64::syscall_abi::REG_X2));

    // ── Bounded rank-2 task-domain transaction #2: the final context commit ──────────
    let mut ctx = frame.capture_user_context();
    ctx.instruction_ptr = crate::kernel::vm::VirtAddr(resume_pc as u64);
    let _ = shared.split_return_commit_context_split(id, ctx);

    crate::yarm_log!(
        "AARCH64_SPLIT_ABI_EXPORT_DONE err={} x0_after=0x{:x}",
        frame.error_code().unwrap_or(0),
        frame.user_gpr(crate::arch::aarch64::syscall_abi::REG_X0)
    );
    // Stage 195A: finalize-OK attestation. `result=ok` for a success return;
    // `result=error code=<code>` when the split path encoded a canonical error
    // (e.g. an explicitly-exercised UserMemoryFault), so error parity is visible.
    match frame.error_code() {
        Some(code) => crate::yarm_log!(
            "AARCH64_SPLIT_FINALIZE_OK nr={} result=error code={}",
            frame.syscall_num(),
            code
        ),
        None => crate::yarm_log!(
            "AARCH64_SPLIT_FINALIZE_OK nr={} result=ok",
            frame.syscall_num()
        ),
    }
}

pub fn decode_trap_context(context: Aarch64TrapContext) -> TrapEvent {
    if context.is_timer_irq {
        return TrapEvent::TimerInterrupt;
    }
    if context.irq_line == Some(ARCH_TIMER_PPI_IRQ) {
        return TrapEvent::TimerInterrupt;
    }
    if let Some(irq) = context.irq_line {
        return TrapEvent::ExternalInterrupt(irq);
    }

    match (context.esr_el1 >> 26) & ESR_EC_MASK {
        ESR_EC_SVC64 => TrapEvent::Syscall,
        ESR_EC_IABT_LOW | ESR_EC_IABT_CUR => TrapEvent::PageFault(FaultInfo {
            addr: VirtAddr(context.far_el1),
            access: FaultAccess::Execute,
        }),
        ESR_EC_DABT_LOW | ESR_EC_DABT_CUR => {
            let is_write = ((context.esr_el1 >> 6) & 1) != 0;
            TrapEvent::PageFault(FaultInfo {
                addr: VirtAddr(context.far_el1),
                access: if is_write {
                    FaultAccess::Write
                } else {
                    FaultAccess::Read
                },
            })
        }
        _ => TrapEvent::Unknown {
            arch_code: context.esr_el1 as u64,
        },
    }
}

pub fn handle_trap_entry(
    kernel: &mut KernelState,
    cpu: CpuId,
    context: Aarch64TrapContext,
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
    context: Aarch64TrapContext,
    mut frame: Option<&mut TrapFrame>,
    fault_bookkeeping_mode: FaultBookkeepingMode,
) -> Result<(), TrapHandleError> {
    let event = decode_trap_context(context);
    let entering_tid = kernel.current_tid();
    let raw_vector_return_pc = crate::arch::aarch64::boot::last_vector_raw_elr() as usize;

    crate::yarm_log!(
        "AARCH64_TRAP_ORIGINAL_TID tid={}",
        entering_tid.unwrap_or(0)
    );

    if matches!(event, TrapEvent::Syscall) {
        if let Some(trapframe) = frame.as_deref_mut() {
            import_syscall_abi_from_user_gprs(trapframe);
        }
    }
    let _ = kernel.set_current_cpu(cpu);
    let _ = kernel.process_cross_cpu_work_for_cpu(cpu);
    if let Err(err) = kernel.handle_trap_event_with_fault_bookkeeping_mode(
        event,
        frame.as_deref_mut(),
        fault_bookkeeping_mode,
    ) {
        crate::yarm_log!("AARCH64_TRAP_DISPATCH_RESULT err={:?}", err);
        crate::yarm_log!("AARCH64_TRAP_FAIL_REASON handle_trap_event");
        return Err(err);
    }
    trap_trace!("AARCH64_TRAP_DISPATCH_RESULT ok");

    if matches!(event, TrapEvent::Syscall) {
        trap_trace!(
            "AARCH64_SYSCALL_RAW_RETURN_PC value=0x{:016x}",
            raw_vector_return_pc as u64
        );
    }

    let exiting_tid = kernel.current_tid();
    // A context switch occurred if the current task changed during the syscall handler.
    let task_switched = matches!(event, TrapEvent::Syscall) && entering_tid != exiting_tid;
    let syscall_resume_pc = if matches!(event, TrapEvent::Syscall) {
        let tid = entering_tid.unwrap_or(0);
        let syscall_nr = frame.as_ref().map(|f| f.syscall_num()).unwrap_or(0);
        let (final_pc, reason) =
            crate::arch::trap::aarch64_syscall_elr_policy(raw_vector_return_pc, syscall_nr);
        trap_trace!(
            "AARCH64_ELR_POLICY tid={} nr={} raw=0x{:016x} final=0x{:016x} reason={}",
            tid,
            syscall_nr,
            raw_vector_return_pc as u64,
            final_pc as u64,
            reason
        );
        final_pc
    } else {
        raw_vector_return_pc
    };

    if !task_switched && matches!(event, TrapEvent::Syscall) {
        if let Some(trapframe) = frame.as_deref_mut() {
            let saved_pc_final = syscall_resume_pc;
            trapframe.set_saved_pc(saved_pc_final);
            if let Some(tid) = kernel.current_tid() {
                let mut ctx = trapframe.capture_user_context();
                ctx.instruction_ptr = crate::kernel::vm::VirtAddr(saved_pc_final as u64);
                let _ = kernel.set_thread_user_context(tid, ctx);
            }
        }
    }

    // Stage 195E (AARCH64 FUTEXWAIT HANDLER BYPASS): a committed FutexWait deferral clears
    // `current` on purpose (the queue-advancing dispatch is relocated OUT of the broad lock to
    // the trap-entry drain). Without this bypass the `current == None` case would enter
    // `idle_no_eret_loop()` INSIDE `with_cpu` and never return, so the post-lock drain could
    // never run. The bypass is strictly FutexWait-deferral-specific — any other
    // `current == None|Some(0)` keeps the exact idle behavior. The outgoing (blocked) caller's
    // context is still saved by the `task_switched` block below (entering != None == exiting).
    let futex_wait_bypass = {
        let idx = cpu.0 as usize;
        idx < crate::kernel::scheduler::MAX_CPUS
            && crate::kernel::boot::futex_wait_dispatch_is_deferred(idx)
    };
    // Stage 195G: the parallel Yield-deferral-specific bypass. A committed Yield deferral
    // re-enqueued the caller (Runnable) and cleared `current`, so `current == None` here too;
    // the post-lock Yield drain performs the authoritative dispatch (always an incoming — the
    // caller itself when alone; NO idle outcome). Strictly Yield-deferral-specific.
    let yield_bypass = {
        let idx = cpu.0 as usize;
        idx < crate::kernel::scheduler::MAX_CPUS
            && crate::kernel::boot::yield_dispatch_is_deferred(idx)
    };
    // Stage 200D-0C1 (AARCH64 EXITCURRENTTASK HANDLER BYPASS): the third — and structurally
    // different — member of this family. An accepted NR16 published a
    // `PostLockTrapDisposition::CurrentTaskExited` for this CPU while the broad lock was held.
    //
    // Both effects of this bypass are REQUIRED for the consumer to exist at all:
    //
    //   * the idle divergence below calls `idle_no_eret_loop()` INSIDE `with_cpu` and never
    //     returns, so an exit that leaves no replacement would strand the disposition forever
    //     (unconsumed, with the broad lock held) — the post-lock section could never run;
    //   * `restore_arch_thread_state` below runs INSIDE `with_cpu`, so leaving it enabled would
    //     commit the outgoing frame BEFORE the post-lock consumer had validated anything.
    //
    // Skipping both relocates the outgoing selection to the post-lock consumer in
    // `arch/trap_entry.rs`, which runs after the broad guard is dropped AND after every shared
    // post-lock drain. Strictly disposition-specific: only an accepted NR16 ever publishes one,
    // so every other trap on every other path is bit-identical to before.
    let exit_disposition_bypass = {
        let idx = cpu.0 as usize;
        idx < crate::kernel::scheduler::MAX_CPUS
            && crate::kernel::boot::post_lock_trap_disposition_pending(idx)
    };
    if exit_disposition_bypass {
        crate::yarm_log!(
            "EXIT_TASK_INLOCK_BYPASS_ARMED arch=aarch64 cpu={} exiting_tid={} inlock_restore=0 inlock_idle=0 result=ok",
            cpu.0,
            entering_tid.unwrap_or(0)
        );
    }
    // U4 (AARCH64 D2 HANDLER BYPASSES): the fourth and fifth members of the same family, and
    // required for the same reason. A committed blocking IpcRecv / IpcSend deferral marks the
    // caller `Blocked(Endpoint{Receive,Send})` and clears `current` on purpose, relocating the
    // queue-advancing dispatch to the post-lock trap-entry drain. Without these bypasses the
    // `current == None` case below enters `idle_no_eret_loop()` INSIDE `with_cpu` and never
    // returns, so the D2 drain could never run at all — the CPU would halt holding a published
    // deferral while a runnable task waited. Each is strictly ITS OWN class's deferral: any
    // other `current == None|Some(0)` keeps the exact idle behavior, and every other trap on
    // every other path is bit-identical to before.
    let d2_recv_bypass = {
        let idx = cpu.0 as usize;
        idx < crate::kernel::scheduler::MAX_CPUS
            && crate::kernel::boot::d2_recv_dispatch_is_deferred(idx)
    };
    let d2_send_bypass = {
        let idx = cpu.0 as usize;
        idx < crate::kernel::scheduler::MAX_CPUS
            && crate::kernel::boot::d2_send_dispatch_is_deferred(idx)
    };
    let post_lock_bypass = futex_wait_bypass
        || yield_bypass
        || exit_disposition_bypass
        || d2_recv_bypass
        || d2_send_bypass;
    if matches!(exiting_tid, None | Some(0)) {
        if exit_disposition_bypass {
            // No in-lock idle: the post-lock consumer names the idle outcome and enters the
            // SAME `idle_no_eret_loop()` primitive after the broad guard has dropped.
            crate::yarm_log!(
                "EXIT_TASK_INLOCK_IDLE_DEFERRED arch=aarch64 cpu={} outgoing_tid={} result=ok",
                cpu.0,
                entering_tid.unwrap_or(0)
            );
        } else if futex_wait_bypass {
            crate::yarm_log!(
                "AARCH64_FUTEX_WAIT_HANDLER_BYPASS_BEGIN cpu={} outgoing_tid={}",
                cpu.0,
                entering_tid.unwrap_or(0)
            );
        } else if yield_bypass {
            crate::yarm_log!(
                "AARCH64_YIELD_HANDLER_BYPASS_BEGIN cpu={} outgoing_tid={}",
                cpu.0,
                entering_tid.unwrap_or(0)
            );
        } else if d2_recv_bypass {
            crate::yarm_log!(
                "AARCH64_D2_RECV_HANDLER_BYPASS_BEGIN cpu={} outgoing_tid={}",
                cpu.0,
                entering_tid.unwrap_or(0)
            );
        } else if d2_send_bypass {
            crate::yarm_log!(
                "AARCH64_D2_SEND_HANDLER_BYPASS_BEGIN cpu={} outgoing_tid={}",
                cpu.0,
                entering_tid.unwrap_or(0)
            );
        } else {
            trap_trace!("AARCH64_IDLE_NO_ERET cpu={}", cpu.0);
            idle_no_eret_loop();
        }
    }

    // Stage 200D-0C1: an accepted NR16 makes `task_switched` true (entering = the exiting task,
    // exiting = its replacement or None), but the outgoing task is DEAD. Saving a resume context
    // into an `Exited` TCB — and stamping the exiting task's ELR into the shared trap frame —
    // would be exactly the "old frame" state the consumer must prove is never produced. The
    // exiting incarnation is never resumed, so there is nothing to save.
    if task_switched && !exit_disposition_bypass {
        // Save the original task's post-syscall resume PC to its TCB.
        // sync_current_thread_from_frame already ran (before yield), but we also
        // fix the frame's saved_pc here and re-save so the original task resumes at
        // the correct ELR (SVC return address) when next dispatched.
        if let Some(trapframe) = frame.as_deref_mut() {
            trapframe.set_saved_pc(syscall_resume_pc);
            if let Some(orig_tid) = entering_tid {
                crate::yarm_log!(
                    "AARCH64_CONTEXT_SAVE_FULL tid={} elr=0x{:016x} sp=0x{:016x} x0=0x{:016x} x1=0x{:016x} x29=0x{:016x} x30=0x{:016x} ctx_ptr=0x{:x}",
                    orig_tid,
                    trapframe.saved_pc() as u64,
                    trapframe.saved_sp() as u64,
                    trapframe.user_gpr(0) as u64,
                    trapframe.user_gpr(1) as u64,
                    trapframe.user_gpr(29) as u64,
                    trapframe.user_gpr(30) as u64,
                    trapframe as *const _ as usize
                );
                let ctx = trapframe.capture_user_context();
                let _ = kernel.set_thread_user_context(orig_tid, ctx);
                crate::yarm_log!(
                    "AARCH64_SYSCALL_BLOCK_SAVE tid={} saved_elr=0x{:016x}",
                    orig_tid,
                    syscall_resume_pc as u64
                );
            }
        }
        trap_trace!(
            "AARCH64_SYSCALL_RETURN_SAVE tid={} elr=0x{:016x}",
            entering_tid.unwrap_or(0),
            syscall_resume_pc as u64
        );
        trap_trace!("AARCH64_DISPATCH_NEXT_TID tid={}", exiting_tid.unwrap_or(0));
    }

    // Stage 117: skip restore_arch_thread_state when a global-lock-drop plan
    // is stashed for this CPU. The restore will be called post-switch in
    // `handle_trap_entry_shared` after `switch_frames` runs outside the lock.
    let cpu_idx = cpu.0 as usize;
    let switch_pending = cpu_idx < crate::kernel::scheduler::MAX_CPUS
        && unsafe { crate::kernel::boot::DISPATCH_SWITCH_PLAN_STASH[cpu_idx].has_plan() };
    let syscall_return = !task_switched && matches!(event, TrapEvent::Syscall);
    // Stage 195E: on a FutexWait-deferral bypass, skip the in-lock restore entirely — `current`
    // is None (the restore would only no-op into SCHED_ENTER_IDLE), and the authoritative EL0
    // frame restore for the INCOMING task runs in the post-lock drain's `with_cpu` re-acquire.
    if !switch_pending && !post_lock_bypass {
        if let Err(err) =
            restore_arch_thread_state(kernel, cpu, frame.as_deref_mut(), syscall_return)
        {
            crate::yarm_log!("AARCH64_TRAP_DISPATCH_RESULT err={:?}", err);
            crate::yarm_log!("AARCH64_TRAP_FAIL_REASON restore_arch_thread_state");
            return Err(err);
        }
    }
    if futex_wait_bypass {
        crate::yarm_log!("AARCH64_FUTEX_WAIT_HANDLER_BYPASS_DONE cpu={}", cpu.0);
    } else if yield_bypass {
        crate::yarm_log!("AARCH64_YIELD_HANDLER_BYPASS_DONE cpu={}", cpu.0);
    } else if d2_recv_bypass {
        crate::yarm_log!("AARCH64_D2_RECV_HANDLER_BYPASS_DONE cpu={}", cpu.0);
    } else if d2_send_bypass {
        crate::yarm_log!("AARCH64_D2_SEND_HANDLER_BYPASS_DONE cpu={}", cpu.0);
    }

    if !task_switched && matches!(event, TrapEvent::Syscall) {
        if let Some(trapframe) = frame.as_deref_mut() {
            if crate::kernel::boot::ipc_recv_proof_sender_wake_active() {
                let tid = kernel.current_tid().unwrap_or(0);
                crate::yarm_log!(
                    "AARCH64_FORK_PARENT_RET_BEFORE_RETURN tid={} ret0={} x0={} err={}",
                    tid,
                    trapframe.ret0(),
                    trapframe.user_gpr(crate::arch::aarch64::syscall_abi::REG_X0),
                    trapframe.error
                );
            }
            export_syscall_result_to_user_gprs(trapframe);
            // Stage 163L: sync args[0..2] to match the just-exported user_gprs
            // so that a post-switch resume via restore_arch_thread_state(syscall_return=false)
            // gets the correct values.  That path runs the arg-mirror which sets
            // user_gprs[x0..x2] = args[0..2]; without this update, args[0..2]
            // still hold the original input arguments (e.g. fork arg0=0), so the
            // mirror destroys the exported ret0=child_tid.  Re-save the TCB so
            // the updated context persists across any switch-plan stash.
            trapframe.set_arg(
                0,
                trapframe.user_gpr(crate::arch::aarch64::syscall_abi::REG_X0),
            );
            trapframe.set_arg(
                1,
                trapframe.user_gpr(crate::arch::aarch64::syscall_abi::REG_X1),
            );
            trapframe.set_arg(
                2,
                trapframe.user_gpr(crate::arch::aarch64::syscall_abi::REG_X2),
            );
            if let Some(tid) = kernel.current_tid() {
                let ctx = trapframe.capture_user_context();
                let _ = kernel.set_thread_user_context(tid, ctx);
            }
            if crate::kernel::boot::ipc_recv_proof_sender_wake_active() {
                let tid = kernel.current_tid().unwrap_or(0);
                let nr = trapframe.syscall_num();
                crate::yarm_log!(
                    "NONX86_SYSCALL_RETURN_LANE_SET arch=aarch64 tid={} nr={} ret0={} err={}",
                    tid,
                    nr,
                    trapframe.ret0(),
                    trapframe.error
                );
                crate::yarm_log!(
                    "AARCH64_TRAP_RETURN_FRAME tid={} x0={} x1={} x2={} err={}",
                    tid,
                    trapframe.user_gpr(crate::arch::aarch64::syscall_abi::REG_X0),
                    trapframe.user_gpr(crate::arch::aarch64::syscall_abi::REG_X1),
                    trapframe.user_gpr(crate::arch::aarch64::syscall_abi::REG_X2),
                    trapframe.error
                );
            }
            trap_trace!(
                "AARCH64_POST_RESTORE_EXPORT tid={} x0={} x1={} x2={}",
                kernel.current_tid().unwrap_or(0),
                trapframe.user_gpr(crate::arch::aarch64::syscall_abi::REG_X0),
                trapframe.user_gpr(crate::arch::aarch64::syscall_abi::REG_X1),
                trapframe.user_gpr(crate::arch::aarch64::syscall_abi::REG_X2)
            );
        }
    }

    if task_switched {
        // Returning to a different thread: registers are sourced from saved user context.
        if let Some(trapframe) = frame.as_deref_mut() {
            trap_trace!(
                "AARCH64_RETURN_CONTEXT_SOURCE tid={} source=saved_context",
                exiting_tid.unwrap_or(0)
            );
            trap_trace!(
                "AARCH64_RETURNING_SAVED_CONTEXT tid={} elr=0x{:016x}",
                exiting_tid.unwrap_or(0),
                trapframe.saved_pc() as u64
            );
        }
    } else if matches!(event, TrapEvent::Syscall) {
        // Same task continues: set the return ELR to the instruction after the SVC.
        if let Some(trapframe) = frame.as_deref_mut() {
            if kernel.current_tid() == Some(0) {
                trap_trace!("AARCH64_IDLE_NO_ERET cpu={}", cpu.0);
                idle_no_eret_loop();
            }
            if trapframe.syscall_num() == crate::kernel::syscall::Syscall::IpcRecv as usize
                && let Some(tid) = kernel.current_tid()
            {
                crate::yarm_log!(
                    "IPC_RECV_WAKE_RETURN_REGS tid={} x0={} x1={} x2={} x3={} x4={}",
                    tid,
                    trapframe.ret0(),
                    trapframe.ret1(),
                    trapframe.ret2(),
                    trapframe.arg(3),
                    trapframe.arg(4)
                );
            }
            trap_trace!(
                "AARCH64_RETURN_CONTEXT_SOURCE tid={} source=trapframe",
                kernel.current_tid().unwrap_or(0)
            );
        }
    }

    if let Some(trapframe) = frame.as_deref_mut() {
        if !task_switched && matches!(event, TrapEvent::Syscall) {
            let saved_pc_final = syscall_resume_pc;
            trapframe.set_saved_pc(saved_pc_final);
        }

        let actual_elr = trapframe.saved_pc();
        trap_trace!("AARCH64_MSR_ELR_ACTUAL value=0x{:016x}", actual_elr as u64);

        if kernel.current_tid().unwrap_or(0) != 0 && actual_elr < 0x400000 {
            crate::yarm_log!(
                "AARCH64_BAD_USER_ELR tid={} elr=0x{:016x}",
                kernel.current_tid().unwrap_or(0),
                actual_elr as u64
            );
            panic!("AARCH64_BAD_USER_ELR");
        }

        trap_trace!(
            "AARCH64_ERET_ACTUAL tid={} elr=0x{:016x} x0={} x1={} x2={} x3={}",
            kernel.current_tid().unwrap_or(0),
            actual_elr as u64,
            trapframe.user_gpr(crate::arch::aarch64::syscall_abi::REG_X0),
            trapframe.user_gpr(crate::arch::aarch64::syscall_abi::REG_X1),
            trapframe.user_gpr(crate::arch::aarch64::syscall_abi::REG_X2),
            trapframe.user_gpr(crate::arch::aarch64::syscall_abi::REG_X3),
        );
        trap_trace!(
            "AARCH64_FINAL_USER_GPRS tid={} x0={} x1={} x2={}",
            kernel.current_tid().unwrap_or(0),
            trapframe.user_gpr(crate::arch::aarch64::syscall_abi::REG_X0),
            trapframe.user_gpr(crate::arch::aarch64::syscall_abi::REG_X1),
            trapframe.user_gpr(crate::arch::aarch64::syscall_abi::REG_X2),
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arch::trap::Trap;

    #[test]
    fn decode_svc64_syscall() {
        let ev = decode_trap_context(Aarch64TrapContext {
            esr_el1: ESR_EC_SVC64 << 26,
            far_el1: 0,
            irq_line: None,
            is_timer_irq: false,
        });
        assert_eq!(ev.trap(), Trap::Syscall);
    }

    #[test]
    fn decode_timer_irq() {
        let ev = decode_trap_context(Aarch64TrapContext {
            esr_el1: 0,
            far_el1: 0,
            irq_line: None,
            is_timer_irq: true,
        });
        assert_eq!(ev.trap(), Trap::TimerInterrupt);
    }

    #[test]
    fn decode_arch_timer_ppi_irq_as_timer() {
        let ev = decode_trap_context(Aarch64TrapContext {
            esr_el1: 0,
            far_el1: 0,
            irq_line: Some(30),
            is_timer_irq: false,
        });
        assert_eq!(ev.trap(), Trap::TimerInterrupt);
    }

    #[test]
    fn decode_external_irq() {
        let ev = decode_trap_context(Aarch64TrapContext {
            esr_el1: 0,
            far_el1: 0,
            irq_line: Some(44),
            is_timer_irq: false,
        });
        assert_eq!(ev.trap(), Trap::ExternalInterrupt);
        assert_eq!(ev.irq(), Some(44));
    }

    #[test]
    fn syscall_abi_imports_x_register_arguments() {
        let mut frame = TrapFrame::new(0, [0; 6]);
        frame.set_user_gpr(crate::arch::aarch64::syscall_abi::REG_X8, 42);
        frame.set_user_gpr(crate::arch::aarch64::syscall_abi::REG_X0, 10);
        frame.set_user_gpr(crate::arch::aarch64::syscall_abi::REG_X1, 11);
        frame.set_user_gpr(crate::arch::aarch64::syscall_abi::REG_X2, 12);
        frame.set_user_gpr(crate::arch::aarch64::syscall_abi::REG_X3, 13);
        frame.set_user_gpr(crate::arch::aarch64::syscall_abi::REG_X4, 14);
        frame.set_user_gpr(crate::arch::aarch64::syscall_abi::REG_X5, 15);

        import_syscall_abi_from_user_gprs(&mut frame);

        assert_eq!(frame.syscall_num(), 42);
        assert_eq!(frame.arg(0), 10);
        assert_eq!(frame.arg(1), 11);
        assert_eq!(frame.arg(2), 12);
        assert_eq!(frame.arg(3), 13);
        assert_eq!(frame.arg(4), 14);
        assert_eq!(frame.arg(5), 15);
    }

    #[test]
    fn syscall_abi_exports_return_registers() {
        let mut frame = TrapFrame::new(0, [0; 6]);
        frame.set_arg(3, 10);
        frame.set_arg(4, 11);
        frame.set_arg(5, 12);
        frame.set_ok(7, 8, 9);
        export_syscall_result_to_user_gprs(&mut frame);
        assert_eq!(frame.user_gpr(crate::arch::aarch64::syscall_abi::REG_X0), 7);
        assert_eq!(frame.user_gpr(crate::arch::aarch64::syscall_abi::REG_X1), 8);
        assert_eq!(frame.user_gpr(crate::arch::aarch64::syscall_abi::REG_X2), 9);
        assert_eq!(
            frame.user_gpr(crate::arch::aarch64::syscall_abi::REG_X3),
            10
        );
        assert_eq!(
            frame.user_gpr(crate::arch::aarch64::syscall_abi::REG_X4),
            11
        );
        assert_eq!(
            frame.user_gpr(crate::arch::aarch64::syscall_abi::REG_X5),
            12
        );

        frame.set_err(5);
        export_syscall_result_to_user_gprs(&mut frame);
        assert_eq!(frame.user_gpr(crate::arch::aarch64::syscall_abi::REG_X0), 5);
        assert_eq!(frame.user_gpr(crate::arch::aarch64::syscall_abi::REG_X1), 0);
        assert_eq!(frame.user_gpr(crate::arch::aarch64::syscall_abi::REG_X2), 0);
        assert_eq!(frame.user_gpr(crate::arch::aarch64::syscall_abi::REG_X3), 0);
        assert_eq!(frame.user_gpr(crate::arch::aarch64::syscall_abi::REG_X4), 0);
        assert_eq!(frame.user_gpr(crate::arch::aarch64::syscall_abi::REG_X5), 0);
    }

    #[test]
    fn decode_data_abort_write_fault() {
        let ev = decode_trap_context(Aarch64TrapContext {
            esr_el1: (ESR_EC_DABT_LOW << 26) | (1 << 6),
            far_el1: 0xABCD_4000,
            irq_line: None,
            is_timer_irq: false,
        });
        assert_eq!(ev.trap(), Trap::PageFault);
        assert_eq!(
            ev.fault(),
            Some(FaultInfo {
                addr: VirtAddr(0xABCD_4000),
                access: FaultAccess::Write,
            })
        );
    }

    #[test]
    fn decode_data_abort_current_el_is_page_fault() {
        let ev = decode_trap_context(Aarch64TrapContext {
            esr_el1: ESR_EC_DABT_CUR << 26,
            far_el1: 0x6000,
            irq_line: None,
            is_timer_irq: false,
        });
        assert_eq!(
            ev,
            TrapEvent::PageFault(FaultInfo {
                addr: VirtAddr(0x6000),
                access: FaultAccess::Read,
            })
        );
    }

    #[test]
    fn decode_unknown_exception_class_is_unknown_event() {
        let ev = decode_trap_context(Aarch64TrapContext {
            esr_el1: 0x3F << 26,
            far_el1: 0,
            irq_line: None,
            is_timer_irq: false,
        });
        assert_eq!(ev.trap(), Trap::Unknown);
    }

    #[test]
    fn trap_entry_sets_cpu_and_handles_irq() {
        use crate::kernel::boot::Bootstrap;

        let mut state = crate::std::boxed::Box::new(Bootstrap::init().expect("init"));
        state.bring_up_cpu(CpuId(2)).expect("cpu2");

        handle_trap_entry(
            &mut state,
            CpuId(2),
            Aarch64TrapContext {
                esr_el1: 0,
                far_el1: 0,
                irq_line: Some(11),
                is_timer_irq: false,
            },
            None,
        )
        .expect("irq");

        assert_eq!(state.current_cpu(), CpuId(2));
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
            CpuId(2),
            Aarch64TrapContext {
                esr_el1: 0,
                far_el1: 0,
                irq_line: None,
                is_timer_irq: true,
            },
            Some(&mut frame),
        )
        .expect("trap");
        assert_eq!(last_restored_tls_base(CpuId(2)), Some(0xCAFE_0000));
        assert_eq!(
            frame.user_gpr(crate::arch::aarch64::syscall_abi::REG_X18_TLS),
            0xCAFE_0000
        );
    }

    #[test]
    fn tls_restore_slots_are_isolated_per_cpu() {
        use crate::kernel::boot::{Bootstrap, UserImageSpec};
        use crate::kernel::task::TaskClass;

        let mut state = crate::std::boxed::Box::new(Bootstrap::init().expect("init"));
        state.bring_up_cpu(CpuId(1)).expect("cpu1");
        state.bring_up_cpu(CpuId(2)).expect("cpu2");
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
            Aarch64TrapContext {
                esr_el1: 0,
                far_el1: 0,
                irq_line: None,
                is_timer_irq: true,
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
            Aarch64TrapContext {
                esr_el1: 0,
                far_el1: 0,
                irq_line: None,
                is_timer_irq: true,
            },
            Some(&mut frame_b),
        )
        .expect("trap b");

        assert_eq!(last_restored_tls_base(CpuId(1)), Some(0xAAA0_0000));
        assert_eq!(last_restored_tls_base(CpuId(0)), Some(0xBBB0_0000));
    }

    // ── Stage 81A: AArch64-specific parity coverage ───────────────────────────

    #[test]
    fn stage81a_aarch64_export_syscall_error_encodes_into_x0_not_propagates() {
        // Verifies the AArch64-specific return path: export_syscall_result_to_user_gprs
        // puts error codes in x0 (REG_X0) and zeroes x1..x5.
        // After the Stage 81A parity fix, user syscall errors reach this path
        // (encoded in frame.error_code) rather than causing a TrapHandleError halt.
        let mut frame = TrapFrame::new(0, [0; 6]);
        frame.set_err(crate::kernel::syscall::SyscallError::InvalidArgs.code());
        export_syscall_result_to_user_gprs(&mut frame);
        assert_eq!(
            frame.user_gpr(crate::arch::aarch64::syscall_abi::REG_X0),
            crate::kernel::syscall::SyscallError::InvalidArgs.code(),
            "InvalidArgs code must appear in x0 after export"
        );
        assert_eq!(
            frame.user_gpr(crate::arch::aarch64::syscall_abi::REG_X1),
            0,
            "x1 must be zeroed for error returns"
        );
    }

    #[test]
    fn stage81a_aarch64_halt_source_requires_err_from_shared_kernel_dispatch() {
        // Source inspection: the halt is guarded by is_ok() on the shared
        // dispatch path. After the parity fix, dispatch_trap_entry_with_shared_kernel
        // returns Ok for normal syscall errors (they are encoded in the frame).
        let boot_src = include_str!("boot.rs");
        assert!(
            boot_src.contains("YARM_AARCH64_TRAP_HANDLE halting"),
            "halt path must remain documented in aarch64/boot.rs"
        );
        assert!(
            boot_src.contains(".is_ok()"),
            "frame writeback must be guarded on is_ok() result"
        );
        let fault_src = include_str!("../../kernel/boot/fault_state.rs");
        assert!(
            !fault_src
                .contains("dispatch_syscall(self, trapframe).map_err(TrapHandleError::Syscall)?"),
            "parity fix must be present: dispatch errors must not propagate to AArch64 halt path"
        );
    }
}
