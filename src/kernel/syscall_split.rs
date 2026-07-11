// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Stage 28: trap/syscall split-dispatch bridge (whitelist-only scaffold).
//! Stage 29: live-wired for `ControlPlaneSetCnodeSlots` (NR 8) via
//! [`try_split_dispatch_into_frame`].
//! Stage 32B: live-wired for `IpcRecv` (NR 2), kernel-task queued-plain case only.
//! Stage 114: live-wired for `VmBrk` (NR 14), page-crossing-shrink case only, via
//! [`try_split_vm_brk_shrink_into_frame`].
//!
//! This module hosts the minimal, **whitelist-only** mechanism that classifies
//! a decoded `Syscall` as eligible for *split-dispatch* — i.e. servicing it via
//! per-domain split-mut/split-read helpers on [`SharedKernel`] WITHOUT taking the
//! global `SpinLock<KernelState>` and WITHOUT calling `with` / `with_cpu`.
//!
//! ## Default-deny contract
//!
//! [`try_split_dispatch`] returns `Some(result)` ONLY for syscalls on the
//! explicit whitelist. Every other syscall — including all IPC, Spawn/fork/exec,
//! VM, and futex paths — falls through the `_ => None` arm and MUST be handled by
//! the unchanged global-lock dispatch path (`SharedKernel::with_cpu` →
//! `KernelState::handle_trap` → `syscall::dispatch`). This guarantees that adding
//! the bridge can never silently change the behavior of any non-whitelisted
//! syscall: the fallback is the existing, fully-tested global-lock path.
//!
//! ## Stage 29 — live-wired result-writeback contract
//!
//! The whitelisted candidate (`ControlPlaneSetCnodeSlots`) returns a *non-trivial
//! trapframe payload*: the production handler writes
//! `frame.set_ok(slot_capacity, target_pid, 0)` — two meaningful return registers,
//! not a single status code. [`try_split_dispatch`] (Stage 28) returns only the
//! logical `Result<(), KernelError>`.
//!
//! Stage 29 adds [`try_split_dispatch_into_frame`], the minimal pre-global-lock
//! *result-writeback contract*. `TrapFrame::set_ok` / `set_err` are pure register
//! writes (no global-lock dependency, architecture-neutral — see
//! `kernel/trapframe.rs`), so the seam calls them directly:
//!   * It decodes `(target_pid, slots)` from the frame exactly as the global-lock
//!     handler does (`arg(SYSCALL_ARG_CAP)`, `arg(SYSCALL_ARG_PTR)`).
//!   * It reads the requester TID via `SharedKernel::current_tid_split_read(cpu)`
//!     (scheduler lock only) — value-equivalent to the global-lock
//!     `with_cpu(cpu, |k| k.current_tid())` the old `current_tid()` used.
//!   * On success it writes `set_ok(slots, pid, 0)` — byte-for-byte the encoding
//!     the global-lock handler produced — and returns `Some(Ok(()))`.
//!   * On a domain error it returns `Some(Err(TrapHandleError::Syscall(..)))` so
//!     the arch stub propagates it on exactly the path the old `Err(SyscallError)`
//!     return took (the control-plane syscall's errors are fatal/propagated, not
//!     user-recoverable — the old handler never wrote `set_err` for them either).
//!   * It returns `None` for every non-whitelisted syscall (and when the requester
//!     TID is unavailable), so the caller falls back to the UNCHANGED global-lock
//!     path.
//!
//! The split path never blocks/yields/schedules and never switches tasks, so
//! `entering_tid == exiting_tid` (i.e. `task_switched == false`) stays observable
//! to the arch `write_trap_returns_to_saved_regs` branch exactly as before. The
//! `entering_tid` / `exiting_tid` snapshots and the trap boundary are left
//! untouched. See `doc/KERNEL_LOCKING.md` §47.

use crate::kernel::boot::{KernelError, TrapHandleError};
use crate::kernel::scheduler::CpuId;
use crate::kernel::syscall::{Syscall, SyscallError};
use crate::kernel::trapframe::TrapFrame;
use crate::runtime::SharedKernel;

/// Syscalls eligible for split-dispatch (no global lock).
///
/// **WHITELIST ONLY.** A variant exists here only after the corresponding
/// `SharedKernel` split helper is proven safe (single ascending lock-domain
/// order, no blocking/yield/schedule, no user-memory copy in the bridge itself,
/// result encodable as the existing syscall return type).
// Stage 29: live-wired for `ControlPlaneCnodeSlots` via
// `try_split_dispatch_into_frame`. The default-deny `_ => None` fallback keeps
// every other syscall on the unchanged global-lock dispatch path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SplitEligibleSyscall {
    /// `Syscall::ControlPlaneSetCnodeSlots` (NR 8). Serviced by
    /// `SharedKernel::control_plane_set_process_cnode_slots_split_mut`
    /// (task read rank 2 → boot-config read → capability mutate rank 4).
    ControlPlaneCnodeSlots {
        requester_tid: u64,
        target_pid: u64,
        slots: usize,
    },
    /// `Syscall::IpcRecv` (NR 2), kernel-task receiver of a queued plain message.
    ///
    /// Stage 32B: split eligibility for IpcRecv cannot be fully decided from the
    /// syscall number + raw args alone — whether the receiver is a kernel task,
    /// whether the endpoint has a queued plain message, and whether a sender-wake
    /// or recv-v2 path applies are all resolved INSIDE
    /// `try_split_ipc_recv_queued_plain_into_frame`. This variant therefore marks
    /// IpcRecv as "attempt the split"; the helper itself returns `None` for every
    /// case it cannot service (user-ASID receiver, empty queue, sender-wake,
    /// cap-transfer, recv-v2), and that `None` propagates straight back to the
    /// global-lock fallback. The variant carries no decoded args for that reason.
    IpcRecvKernelTask,
    // Add others ONLY when the per-domain helper is proven safe.
    //
    // Stage 114: `Syscall::VmBrk` (NR 14) is intentionally NOT added here.
    // Like `IpcRecv`, its split eligibility cannot be decided from the
    // syscall number + raw args alone (group-leader status, brk bounds,
    // page-crossing, and online-CPU count all require domain reads), but
    // unlike `IpcRecv` there is no need for an enum variant: it is
    // special-cased directly in `try_split_dispatch_into_frame` (mirroring
    // the `Syscall::IpcRecv` special case below) and routed straight to
    // `try_split_vm_brk_shrink_into_frame`, never through
    // `classify_split_eligible` / `try_split_dispatch`.
}

/// Classify a decoded syscall + raw args into a split-eligible descriptor.
///
/// Returns `None` for every non-whitelisted syscall (default-deny). For the
/// whitelisted control-plane syscall it also validates the same argument
/// preconditions the global-lock handler enforces (`target_pid != 0`,
/// `slots != 0`); on a precondition miss it returns `None` so the caller falls
/// back to the global-lock path, which will produce the canonical
/// `InvalidArgs` error and the correct trapframe encoding.
pub(crate) fn classify_split_eligible(
    syscall: Syscall,
    requester_tid: u64,
    args: [u64; 6],
) -> Option<SplitEligibleSyscall> {
    match syscall {
        Syscall::ControlPlaneSetCnodeSlots => {
            // args[0] = target_pid (SYSCALL_ARG_CAP), args[1] = slots (SYSCALL_ARG_PTR).
            let target_pid = args[0];
            let slots = args[1] as usize;
            if target_pid == 0 || slots == 0 {
                // Defer the InvalidArgs encoding to the global-lock path.
                return None;
            }
            Some(SplitEligibleSyscall::ControlPlaneCnodeSlots {
                requester_tid,
                target_pid,
                slots,
            })
        }
        // Stage 32B: IpcRecv (NR 2) is split-eligible at classification time, but it
        // is serviced through the frame-level seam
        // (`try_split_dispatch_into_frame` → `try_split_ipc_recv_queued_plain_into_frame`),
        // not through `try_split_dispatch` (which has no `cpu`/`frame`). The variant
        // documents eligibility; `try_split_dispatch` returns `None` for it so the
        // arg-only caller defers to the frame-level recv path / global-lock fallback.
        Syscall::IpcRecv => Some(SplitEligibleSyscall::IpcRecvKernelTask),
        // Default-deny: every other syscall falls back to the global-lock path.
        _ => None,
    }
}

/// Try to dispatch a syscall through the split (no-global-lock) path.
///
/// Returns `Some(result)` if the syscall is on the whitelist and was serviced via
/// per-domain split helpers; returns `None` to signal the caller to fall back to
/// the unchanged global-lock dispatch path. This function itself never blocks,
/// yields, schedules, or copies user memory.
pub(crate) fn try_split_dispatch(
    shared: &SharedKernel,
    syscall: Syscall,
    requester_tid: u64,
    args: [u64; 6],
) -> Option<Result<(), KernelError>> {
    let eligible = classify_split_eligible(syscall, requester_tid, args)?;
    match eligible {
        SplitEligibleSyscall::ControlPlaneCnodeSlots {
            requester_tid,
            target_pid,
            slots,
        } => Some(shared.control_plane_set_process_cnode_slots_split_mut(
            requester_tid,
            target_pid,
            slots,
        )),
        // IpcRecv is serviced by the frame-level seam, not this arg-only path.
        // Returning `None` defers to `try_split_dispatch_into_frame`'s dedicated
        // recv routing (and ultimately the global-lock fallback).
        SplitEligibleSyscall::IpcRecvKernelTask => None,
    }
}

/// # Validation status
/// - LIVE_TRAP_SMOKE_X86_64 — entry point for the NR 8 live split-dispatch path;
///   called from `handle_trap_entry_shared` before the global lock; x86_64 smoke
///   validated (Stage 29 / 29A, marker `YARM_LOCK_SPLIT_DISPATCH nr=8 result=ok`).
///
/// Stage 29 live-wire seam: try to service a syscall through the split
/// (no-global-lock) path AND write its result into the trap frame.
///
/// This is the pre-global-lock *result-writeback contract*. It is called from
/// `handle_trap_entry_shared` BEFORE the global `with_cpu` lock is taken.
///
/// Returns:
/// * `Some(Ok(()))`  — the syscall was a whitelisted split-eligible one, was
///   serviced via the per-domain split helpers, and the success payload was
///   written into `frame` via `set_ok(..)`. The caller must SKIP the global-lock
///   dispatch entirely (the result is already in the frame).
/// * `Some(Err(e))`  — the syscall was whitelisted but the domain mutation failed.
///   `e` is the same `TrapHandleError::Syscall(..)` the global-lock path would have
///   returned for this error; the caller propagates it on the existing error path.
/// * `None`          — the syscall is NOT split-eligible (default-deny) OR the
///   requester TID was unavailable. The caller MUST fall back to the unchanged
///   global-lock dispatch path.
///
/// The split path never blocks, yields, schedules, switches tasks, or copies user
/// memory. Because no task switch occurs, `entering_tid == exiting_tid` and
/// `task_switched == false` remain observable to the arch return-register
/// writeback branch exactly as on the global-lock path.
pub(crate) fn try_split_dispatch_into_frame(
    shared: &SharedKernel,
    cpu: CpuId,
    frame: &mut TrapFrame,
) -> Option<Result<(), TrapHandleError>> {
    use crate::kernel::syscall::{SYSCALL_ARG_CAP, SYSCALL_ARG_PTR};

    // Stage 160B diagnostics (proof-knob–gated so normal/fast boots stay clean):
    // pin exactly where a syscall enters or skips the pre-global-lock split
    // dispatch. These read the frame's *decoded* syscall ABI (`syscall_num()` /
    // `arg()`), which is the same source the eligibility checks below use — so a
    // mismatch versus the real trapped syscall (e.g. an AArch64 frame whose
    // syscall_num/args have not yet been imported from the user GPRs) shows up
    // directly as `nr=0`.
    let probe = crate::kernel::boot::ipc_recv_oracle_proof_enabled();
    let raw_nr = frame.syscall_num();
    if probe {
        crate::yarm_log!("YARM_SPLIT_DISPATCH_ENTER nr={}", raw_nr);
    }

    // Default-deny by syscall number first (cheap, no lock).
    let Ok(syscall) = Syscall::decode(raw_nr) else {
        if probe {
            crate::yarm_log!(
                "YARM_SPLIT_DISPATCH_FALLBACK reason=nr_undecodable nr={}",
                raw_nr
            );
        }
        return None;
    };
    if classify_split_eligible_nr_only(syscall).is_none() {
        if probe {
            crate::yarm_log!(
                "YARM_SPLIT_DISPATCH_FALLBACK reason=nr_not_eligible nr={}",
                raw_nr
            );
        }
        return None;
    }

    // Stage 32B: IpcRecv (NR 2) is routed to the dedicated queued-plain recv
    // helper, which decides split eligibility INTERNALLY (kernel-task receiver,
    // queued plain message, no sender-wake / recv-v2). Crucially, every case the
    // helper cannot service returns `None`, and that `None` propagates UNCHANGED
    // back to the global-lock fallback below — the split path never converts a
    // would-be-fallback into a `Some(Err(..))` (it only returns `Some(Err)` for a
    // cap-resolution error the old path would have raised identically).
    if matches!(syscall, Syscall::IpcRecv) {
        if probe {
            crate::yarm_log!("YARM_SPLIT_DISPATCH_RECV_CONSIDER nr={}", raw_nr);
            crate::yarm_log!("YARM_SPLIT_DISPATCH_RECV_CALL");
        }
        return try_split_ipc_recv_queued_plain_into_frame(shared, cpu, frame);
    }

    // Stage 114: `VmBrk` (NR 14) is routed to the dedicated brk-shrink helper
    // for the same reason `IpcRecv` is above — eligibility (group leader,
    // page-crossing shrink, single CPU online) can only be decided inside the
    // helper. Every case it cannot service returns `None`, which propagates
    // UNCHANGED back to the global-lock fallback below.
    if matches!(syscall, Syscall::VmBrk) {
        return try_split_vm_brk_shrink_into_frame(shared, cpu, frame);
    }

    // Stage 191A (GLOBAL-LOCK-RETIRE, first class): DebugLog (NR 15) — a pure read
    // serviced off the global lock. The helper returns `None` for any case it cannot
    // service (hosted-dev, unavailable requester), which propagates UNCHANGED back to
    // the global-lock fallback below.
    if matches!(syscall, Syscall::DebugLog) {
        return try_split_debug_log_into_frame(shared, cpu, frame);
    }

    // Stage 191B (GLOBAL-LOCK-RETIRE, second class): FutexWake (NR 11) — waiter/
    // run-queue mutation only, no caller task-switch. The helper returns `None` for any
    // case it cannot service (invalid addr, hosted-dev, unavailable requester), which
    // propagates UNCHANGED to the global-lock fallback (producing the exact error).
    if matches!(syscall, Syscall::FutexWake) {
        return try_split_futex_wake_into_frame(shared, cpu, frame);
    }

    // Stage 191C (GLOBAL-LOCK-RETIRE, third class): InitramfsReadChunk (NR 27) — a
    // read-only user-copy syscall. It reads immutable initramfs/CPIO data and copies it
    // into the caller's (or PM's) user buffer; it never mutates task/scheduler/IPC/cap/VM
    // structural state and never allocates. The helper services the SUCCESS path off the
    // global lock; ANY error case (access gate, bad args, not-found, unwritable dest)
    // returns `None` → unchanged global-lock fallback, which produces the CANONICAL error
    // + diagnostic logs exactly as before (no silent success masking).
    if matches!(syscall, Syscall::InitramfsReadChunk) {
        return try_split_initramfs_read_chunk_into_frame(shared, cpu, frame);
    }

    // The requester TID is what the global-lock handler reads via
    // `current_tid(kernel)` (i.e. `kernel.current_tid()`).
    //
    // Stage 29A: this MUST use the authoritative `current_tid_authoritative(cpu)`
    // read, NOT `current_tid_split_read(cpu)`. At the live x86_64 pre-global-lock
    // trap point the split-read of the scheduler's per-CPU current slot is stale
    // (it can observe a prior task such as tid 0 instead of the running requester),
    // which made the requester-class permission check resolve the wrong task and
    // return `MissingRight`. The authoritative read binds `current_cpu` first and
    // returns the same task the global-lock handler sees. It is a read-only
    // current-task snapshot (no dispatch/yield/switch); the domain mutation below
    // still runs lock-free via the split-mut helper. If unavailable, fall back so
    // the global-lock path produces the canonical `Internal` error.
    let requester_tid = shared.current_tid_authoritative(cpu)?;

    // Decode args identically to `handle_control_plane_set_cnode_slots`.
    let mut args = [0u64; 6];
    for (i, slot) in args.iter_mut().enumerate() {
        *slot = frame.arg(i) as u64;
    }

    let result = try_split_dispatch(shared, syscall, requester_tid, args)?;
    match result {
        Ok(()) => {
            // Mirror the global-lock handler's exact success encoding:
            //   frame.set_ok(slot_capacity, target_pid as usize, 0)
            let target_pid = frame.arg(SYSCALL_ARG_CAP);
            let slots = frame.arg(SYSCALL_ARG_PTR);
            frame.set_ok(slots, target_pid, 0);
            Some(Ok(()))
        }
        Err(err) => Some(Err(TrapHandleError::Syscall(SyscallError::from(err)))),
    }
}

// ── Stage 191A GLOBAL-LOCK-RETIRE markers (first class) ──────────────────────
/// Emitted once, the first time a class is serviced off the global lock this boot.
pub const MARK_RETIRE_CLASS_BEGIN: &str = "GLOBAL_LOCK_RETIRE_CLASS_BEGIN";
/// Emitted once, after the first off-global-lock service of a class succeeds.
pub const MARK_RETIRE_CLASS_DONE: &str = "GLOBAL_LOCK_RETIRE_CLASS_DONE";
/// A class was inspected for retirement but kept global-lock-only; carries a reason.
pub const MARK_RETIRE_CLASS_DEFERRED: &str = "GLOBAL_LOCK_RETIRE_CLASS_DEFERRED";

/// One-shot latch so the DebugLog retirement markers are emitted exactly once.
#[cfg(not(feature = "hosted-dev"))]
static DEBUG_LOG_RETIRE_LOGGED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Stage 191A: service `DebugLog` (NR 15) through the split (no-global-lock) path.
///
/// DebugLog is the FIRST retired global-lock class. It is a pure READ: it resolves
/// the requester task, copies the user message bytes, logs `USER_LOG`, and writes
/// `set_ok(0,0,0)`. It never blocks/yields/schedules, never switches tasks, and never
/// mutates `KernelState` (`task_switched == false` stays observable). The copy runs
/// off the global lock via `SharedKernel::copy_from_user_asid_split_read` (VM
/// user-spaces lock + direct map). Behaviorally identical to the global-lock
/// `handle_debug_log` (same null/empty short-circuit, same copy-fail silent path,
/// same `USER_LOG` line, same `set_ok(0,0,0)`). Returns `None` only when the requester
/// TID is unavailable, so that case falls back to the unchanged global-lock path.
#[cfg(not(feature = "hosted-dev"))]
fn try_split_debug_log_into_frame(
    shared: &SharedKernel,
    cpu: CpuId,
    frame: &mut TrapFrame,
) -> Option<Result<(), TrapHandleError>> {
    use crate::kernel::ipc::Message;
    // DebugLog ABI: arg0 = user ptr, arg1 = len (no cap slot).
    let user_ptr = frame.arg(0);
    let raw_len = frame.arg(1) as u64;
    let len = (raw_len as usize).min(Message::MAX_PAYLOAD);

    // Authoritative requester TID (binds current_cpu; same task the global handler
    // sees). Unavailable → fall back to the global-lock path.
    let tid = shared.current_tid_authoritative(cpu)?;

    if user_ptr == 0 || len == 0 {
        // Same short-circuit as the global handler: OK, no log.
        frame.set_ok(0, 0, 0);
        maybe_log_debug_log_retired();
        return Some(Ok(()));
    }

    let asid = shared.task_asid_for_tid_split_read(tid);
    match shared.copy_from_user_asid_split_read(asid, user_ptr, len) {
        Some(payload) => {
            let msg = core::str::from_utf8(&payload[..len]).unwrap_or("<utf8_err>");
            crate::yarm_log!("USER_LOG tid={} msg={}", tid, msg);
        }
        // Copy failed (no mapping / not user-readable) — same as the global handler's
        // `DEBUG_LOG_COPY_FAIL` path: OK, no log.
        None => {}
    }
    frame.set_ok(0, 0, 0);
    maybe_log_debug_log_retired();
    Some(Ok(()))
}

/// Emit the DebugLog retirement markers exactly once (first off-global-lock service).
#[cfg(not(feature = "hosted-dev"))]
fn maybe_log_debug_log_retired() {
    if DEBUG_LOG_RETIRE_LOGGED
        .compare_exchange(
            false,
            true,
            core::sync::atomic::Ordering::AcqRel,
            core::sync::atomic::Ordering::Acquire,
        )
        .is_ok()
    {
        // Stage 195A: AArch64 emits an arch-tagged retirement marker (DebugLog is the
        // first live AArch64 split-dispatch class). Stage 196B: RISC-V likewise emits
        // an arch=riscv64 tag (DebugLog is its first — and only — split-dispatch class).
        // x86_64 keeps the exact untagged marker text — byte-identical to Stage 191A.
        #[cfg(target_arch = "aarch64")]
        {
            crate::yarm_log!("{} arch=aarch64 class=DebugLog", MARK_RETIRE_CLASS_BEGIN);
            crate::yarm_log!(
                "{} arch=aarch64 class=DebugLog result=ok",
                MARK_RETIRE_CLASS_DONE
            );
        }
        #[cfg(target_arch = "riscv64")]
        {
            crate::yarm_log!("{} arch=riscv64 class=DebugLog", MARK_RETIRE_CLASS_BEGIN);
            crate::yarm_log!(
                "{} arch=riscv64 class=DebugLog result=ok",
                MARK_RETIRE_CLASS_DONE
            );
        }
        #[cfg(not(any(target_arch = "aarch64", target_arch = "riscv64")))]
        {
            crate::yarm_log!("{} class=DebugLog", MARK_RETIRE_CLASS_BEGIN);
            crate::yarm_log!("{} class=DebugLog result=ok", MARK_RETIRE_CLASS_DONE);
        }
    }
}

/// Hosted-dev: DebugLog stays on the unchanged global-lock path (the split copy uses
/// the direct map, which only exists on real targets).
#[cfg(feature = "hosted-dev")]
fn try_split_debug_log_into_frame(
    _shared: &SharedKernel,
    _cpu: CpuId,
    _frame: &mut TrapFrame,
) -> Option<Result<(), TrapHandleError>> {
    None
}

/// One-shot latch so the FutexWake retirement markers are emitted exactly once.
#[cfg(not(feature = "hosted-dev"))]
static FUTEX_WAKE_RETIRE_LOGGED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Stage 191B: service `FutexWake` (NR 11) through the split (no-global-lock) path.
///
/// FutexWake is the SECOND retired global-lock class. The CALLER never task-switches;
/// the syscall only mutates waiter/run-queue state. This helper validates the futex
/// word EXACTLY like the global `validate_current_user_futex_word` (addr != 0, addr+3
/// below `KERNEL_SPACE_BASE`, 4 bytes user-readable), then wakes off the global lock
/// via `SharedKernel::futex_wake_split_mut` (task split-mut wake scan + scheduler
/// split-mut enqueue). It preserves the legacy return value (number of waiters woken)
/// and encodes it with `set_ok(woke, 0, 0)`. Any case it cannot service (invalid addr,
/// non-`u32` max_wake, unavailable requester) returns `None` → unchanged global-lock
/// fallback, which produces the CANONICAL error (WrongObject / UserMemoryFault /
/// InvalidArgs) exactly as before — no silent success masking.
#[cfg(not(feature = "hosted-dev"))]
fn try_split_futex_wake_into_frame(
    shared: &SharedKernel,
    cpu: CpuId,
    frame: &mut TrapFrame,
) -> Option<Result<(), TrapHandleError>> {
    use crate::kernel::syscall::{SYSCALL_ARG_CAP, SYSCALL_ARG_PTR};
    // FutexWake ABI: arg(CAP) = futex addr, arg(PTR) = max_wake.
    let addr = frame.arg(SYSCALL_ARG_CAP);
    // Non-`u32` max_wake → the global handler returns InvalidArgs; fall back.
    let max_wake = u32::try_from(frame.arg(SYSCALL_ARG_PTR) as u64).ok()?;

    let tid = shared.current_tid_authoritative(cpu)?;

    // Validate the futex word exactly like `validate_current_user_futex_word`. On ANY
    // validation miss, fall back so the global-lock path produces the canonical error.
    if addr == 0 {
        return None; // legacy: WrongObject
    }
    let end = addr.checked_add(core::mem::size_of::<u32>() - 1)?;
    if end as u64 >= crate::kernel::vm::KERNEL_SPACE_BASE {
        return None; // legacy: UserMemoryFault
    }
    let asid = shared.task_asid_for_tid_split_read(tid);
    if shared
        .copy_from_user_asid_split_read(asid, addr, core::mem::size_of::<u32>())
        .is_none()
    {
        return None; // legacy: UserMemoryFault
    }

    // Validation passed — wake off the global lock.
    // Stage 195C: AArch64 emits arch-tagged split markers (FutexWake is the third live
    // AArch64 split-dispatch class). Stage 196C: RISC-V likewise emits an arch=riscv64 tag
    // (with the woke count, mirroring aarch64). x86_64 keeps the exact untagged Stage 191B text.
    #[cfg(target_arch = "aarch64")]
    crate::yarm_log!("FUTEX_WAKE_SPLIT_BEGIN arch=aarch64");
    #[cfg(target_arch = "riscv64")]
    crate::yarm_log!("FUTEX_WAKE_SPLIT_BEGIN arch=riscv64");
    #[cfg(not(any(target_arch = "aarch64", target_arch = "riscv64")))]
    crate::yarm_log!("FUTEX_WAKE_SPLIT_BEGIN");
    let woke = shared.futex_wake_split_mut(cpu, addr, max_wake);
    crate::yarm_log!("FUTEX_WAKE_SPLIT_WAKE_OK count={}", woke);
    frame.set_ok(woke as usize, 0, 0);
    #[cfg(target_arch = "aarch64")]
    crate::yarm_log!("FUTEX_WAKE_SPLIT_DONE arch=aarch64 result=ok woke={}", woke);
    #[cfg(target_arch = "riscv64")]
    crate::yarm_log!("FUTEX_WAKE_SPLIT_DONE arch=riscv64 result=ok woke={}", woke);
    #[cfg(not(any(target_arch = "aarch64", target_arch = "riscv64")))]
    crate::yarm_log!("FUTEX_WAKE_SPLIT_DONE result=ok");
    maybe_log_futex_wake_retired();
    Some(Ok(()))
}

/// Emit the FutexWake retirement markers exactly once (first off-global-lock service).
#[cfg(not(feature = "hosted-dev"))]
fn maybe_log_futex_wake_retired() {
    if FUTEX_WAKE_RETIRE_LOGGED
        .compare_exchange(
            false,
            true,
            core::sync::atomic::Ordering::AcqRel,
            core::sync::atomic::Ordering::Acquire,
        )
        .is_ok()
    {
        // Stage 195C: AArch64 emits an arch-tagged retirement marker. Stage 196C: RISC-V
        // likewise emits arch=riscv64 (FutexWake is its second split-dispatch class). x86_64
        // keeps the exact untagged Stage 191B text (byte-identical, in the preserve list).
        #[cfg(target_arch = "aarch64")]
        {
            crate::yarm_log!("{} arch=aarch64 class=FutexWake", MARK_RETIRE_CLASS_BEGIN);
            crate::yarm_log!(
                "{} arch=aarch64 class=FutexWake result=ok",
                MARK_RETIRE_CLASS_DONE
            );
        }
        #[cfg(target_arch = "riscv64")]
        {
            crate::yarm_log!("{} arch=riscv64 class=FutexWake", MARK_RETIRE_CLASS_BEGIN);
            crate::yarm_log!(
                "{} arch=riscv64 class=FutexWake result=ok",
                MARK_RETIRE_CLASS_DONE
            );
        }
        #[cfg(not(any(target_arch = "aarch64", target_arch = "riscv64")))]
        {
            crate::yarm_log!("{} class=FutexWake", MARK_RETIRE_CLASS_BEGIN);
            crate::yarm_log!("{} class=FutexWake result=ok", MARK_RETIRE_CLASS_DONE);
        }
    }
}

/// Hosted-dev: FutexWake stays on the unchanged global-lock path (the futex-word
/// validation uses the direct map, which only exists on real targets). The wake logic
/// itself (`futex_wake_split_mut`) is arch-neutral and unit-tested directly.
#[cfg(feature = "hosted-dev")]
fn try_split_futex_wake_into_frame(
    _shared: &SharedKernel,
    _cpu: CpuId,
    _frame: &mut TrapFrame,
) -> Option<Result<(), TrapHandleError>> {
    None
}

/// One-shot latch so the InitramfsReadChunk retirement markers are emitted exactly once.
#[cfg(not(feature = "hosted-dev"))]
static INITRAMFS_READ_CHUNK_RETIRE_LOGGED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Stage 191C: service `InitramfsReadChunk` (NR 27) through the split (no-global-lock)
/// path.
///
/// InitramfsReadChunk is the THIRD retired global-lock class and the FIRST read-only
/// USER-COPY class. It is effectively a read: it copies immutable initramfs/CPIO file
/// bytes into the caller's own ASID (or PM's, for the Phase 2B bridge). It never
/// blocks/yields/schedules, never switches tasks, never allocates, and mutates NO
/// task/scheduler/IPC/cap/VM structural state — the only write is to the destination
/// user buffer, through the same validated user-copy authority the legacy handler uses.
///
/// This helper mirrors the global `handle_initramfs_read_chunk` and services only the
/// SUCCESS outcomes off the global lock (a completed copy, and the EOF short-circuit
/// `set_ok(0,0,0)`). EVERY error outcome returns `None`, so the unchanged global-lock
/// handler produces the CANONICAL error and its exact diagnostic log
/// (`INITRAMFS_READ_CHUNK_DENIED` / `INITRAMFS_READ_CHUNK_NOT_FOUND`, `MissingRight` /
/// `InvalidArgs` / `Internal` / `UserMemoryFault` / `PageFault`) — never a silent
/// success. Because the user-copy seam is TWO-PASS (validate-all then write), a `None`
/// on an unwritable destination means ZERO user-memory bytes were written on the split
/// path, so the fallback re-run is equivalent to the legacy path alone.
#[cfg(not(feature = "hosted-dev"))]
fn try_split_initramfs_read_chunk_into_frame(
    shared: &SharedKernel,
    cpu: CpuId,
    frame: &mut TrapFrame,
) -> Option<Result<(), TrapHandleError>> {
    use crate::kernel::syscall::validate_user_region;
    use crate::kernel::task::TaskClass;
    use yarm_srv_common::cpio::CpioArchive;

    // Must match `PM_BOOTSTRAP_TID` in `kernel::syscall` (the Phase 2B bridge target).
    const PM_BOOTSTRAP_TID: u64 = 3;

    // Authoritative requester TID (binds current_cpu; same task the global handler
    // sees). Unavailable → fall back to the global-lock path (canonical `Internal`).
    let caller_tid = shared.current_tid_authoritative(cpu)?;

    // ── Access gate: SystemServer only. Any other class → fall back so the global
    //    handler emits `INITRAMFS_READ_CHUNK_DENIED` + `MissingRight`. ────────────────
    if shared.task_class_split_read(caller_tid) != Some(TaskClass::SystemServer) {
        return None;
    }

    // Decode args identically to `handle_initramfs_read_chunk`.
    let name_ptr = frame.arg(0);
    let name_len = frame.arg(1);
    let offset = frame.arg(2) as u64;
    let dst_ptr = frame.arg(3);
    let max_len = core::cmp::min(frame.arg(4), 4096);
    let target_tid_arg = frame.arg(5) as u64;

    // Arg validation — mirror the global handler. On any miss fall back (no mutation),
    // so the global path produces the canonical `InvalidArgs` / `MissingRight`.
    if name_len == 0 || name_len > 128 {
        return None; // legacy: InvalidArgs
    }
    if dst_ptr == 0 {
        return None; // legacy: InvalidArgs
    }
    if target_tid_arg != 0 && target_tid_arg != PM_BOOTSTRAP_TID {
        return None; // legacy: DENIED log + MissingRight
    }

    // Read the file name from the caller's ASID (read-only user copy). A copy miss /
    // non-UTF-8 name falls back to the canonical `InvalidArgs`.
    let caller_asid = shared.task_asid_for_tid_split_read(caller_tid);
    let name_buf = shared.copy_from_user_asid_split_read(caller_asid, name_ptr, name_len)?;
    let raw_name = core::str::from_utf8(&name_buf[..name_len]).ok()?;
    // Accept "sbin/x", "/sbin/x", "/initramfs/sbin/x" — identical normalization.
    let name = raw_name.trim_start_matches('/');
    let name = name.strip_prefix("initramfs/").unwrap_or(name);
    let name = name.trim_start_matches('/');

    // Immutable initramfs blob (static accessor; no lock) + pure CPIO parse.
    let initrd = crate::kernel::boot::Bootstrap::boot_initrd_bytes()?; // None → InvalidArgs
    let entry = CpioArchive::new(initrd).find(name).ok()?; // Err → InvalidArgs
    let data = match entry {
        Some(e) => e.file_data(),
        // File not found: fall back so the global handler logs
        // `INITRAMFS_READ_CHUNK_NOT_FOUND` and returns the canonical `Internal`.
        None => return None,
    };

    let offset_usize = offset as usize;
    if offset_usize >= data.len() {
        // EOF (file exists, offset past end) — same as the global handler: OK, no copy.
        frame.set_ok(0, 0, 0);
        maybe_log_initramfs_read_chunk_retired();
        return Some(Ok(()));
    }
    let available = data.len() - offset_usize;
    let to_copy = core::cmp::min(available, max_len);
    let src = &data[offset_usize..offset_usize + to_copy];

    // Resolve the destination ASID: caller's own (target 0) or PM's (Phase 2B bridge).
    let dst_asid_raw = if target_tid_arg == 0 {
        caller_asid
    } else {
        shared.task_asid_for_tid_split_read(target_tid_arg)
    };
    if dst_asid_raw == 0 {
        return None; // task ASID unavailable → canonical UserMemoryFault / PageFault
    }

    // For the caller's own ASID, mirror the legacy `validate_user_region(dst_ptr,
    // to_copy)` bounds check (the PM bridge does not perform it). On failure fall back
    // (no write) → canonical `InvalidArgs`.
    if target_tid_arg == 0 && validate_user_region(dst_ptr as u64, to_copy as u64).is_err() {
        return None;
    }

    // Two-pass user-copy: validates every destination page BEFORE writing any byte, so a
    // fault leaves zero bytes written and we can fall back with no mutation. On success
    // every byte is written, byte-identical to the legacy bulk copy.
    if shared
        .copy_slice_to_user_asid_split_write(dst_asid_raw, dst_ptr, src)
        .is_err()
    {
        // Unwritable destination — fall back so the global handler produces the exact
        // error (`UserMemoryFault` → `PageFault` for the PM bridge; `SyscallError::from`
        // for the caller's ASID). No user-memory byte was written on the split path.
        return None;
    }

    crate::yarm_log!(
        "INITRAMFS_READ_CHUNK_SPLIT_BEGIN name_len={} to_copy={} target_tid={}",
        name_len,
        to_copy,
        target_tid_arg
    );
    frame.set_ok(0, to_copy, 0);
    crate::yarm_log!(
        "INITRAMFS_READ_CHUNK_SPLIT_DONE to_copy={} result=ok",
        to_copy
    );
    maybe_log_initramfs_read_chunk_retired();
    Some(Ok(()))
}

/// Emit the InitramfsReadChunk retirement markers exactly once (first off-global-lock
/// service).
#[cfg(not(feature = "hosted-dev"))]
fn maybe_log_initramfs_read_chunk_retired() {
    if INITRAMFS_READ_CHUNK_RETIRE_LOGGED
        .compare_exchange(
            false,
            true,
            core::sync::atomic::Ordering::AcqRel,
            core::sync::atomic::Ordering::Acquire,
        )
        .is_ok()
    {
        // Stage 195B: AArch64 emits an arch-tagged retirement marker (InitramfsReadChunk
        // is the second live AArch64 split-dispatch class). x86_64/riscv64 keep the exact
        // untagged marker text — byte-identical to Stage 191C.
        #[cfg(target_arch = "aarch64")]
        {
            crate::yarm_log!(
                "{} arch=aarch64 class=InitramfsReadChunk",
                MARK_RETIRE_CLASS_BEGIN
            );
            crate::yarm_log!(
                "{} arch=aarch64 class=InitramfsReadChunk result=ok",
                MARK_RETIRE_CLASS_DONE
            );
        }
        #[cfg(not(target_arch = "aarch64"))]
        {
            crate::yarm_log!("{} class=InitramfsReadChunk", MARK_RETIRE_CLASS_BEGIN);
            crate::yarm_log!(
                "{} class=InitramfsReadChunk result=ok",
                MARK_RETIRE_CLASS_DONE
            );
        }
    }
}

/// Hosted-dev: InitramfsReadChunk stays on the unchanged global-lock path (the split
/// user-copy uses the direct map, which only exists on real targets).
#[cfg(feature = "hosted-dev")]
fn try_split_initramfs_read_chunk_into_frame(
    _shared: &SharedKernel,
    _cpu: CpuId,
    _frame: &mut TrapFrame,
) -> Option<Result<(), TrapHandleError>> {
    None
}

// ── Stage 191D FUTEXWAIT BLOCK-PUBLISH SEAM markers + deferral ─────────────────────────
//
// FutexWait (NR 1) is DEFERRED: it is NOT added to `classify_split_eligible_nr_only` and
// stays FULLY global-lock-only. Unlike DebugLog/FutexWake/InitramfsReadChunk, a matched
// FutexWait BLOCKS the caller and must dispatch a DIFFERENT runnable task — the
// queue-ADVANCING "switch_required" case that `dispatch_next_task` performs. The kernel's
// own out-of-lock dispatch relocation (D6-GENUINE, `exec_state.rs::dispatch_next_task`)
// explicitly restricts itself to the queue-NEUTRAL case and falls back to the in-lock
// (global-lock) path with `reason=switch_required` for exactly this scenario, so the
// futex-wait block+dispatch cannot be serviced off the global lock without the disclaimed
// multi-stage dispatch rewrite. Stage 191D therefore LANDS + proves the block-publish seam
// (`SharedKernel::futex_wait_would_block_split_read` = Phase A value-check,
// `SharedKernel::futex_wait_publish_block_split_mut` = Phase B block-publish) as
// HELPER-ONLY, ready for that future stage, but does NOT wire FutexWait live.
//
/// FutexWait split marker vocabulary (emitted only if/when FutexWait is wired live; the
/// block-publish seam emits `FUTEX_WAIT_SPLIT_BLOCK_PUBLISH_OK` today from its Phase B).
pub const MARK_FUTEX_WAIT_SPLIT_BEGIN: &str = "FUTEX_WAIT_SPLIT_BEGIN";
pub const MARK_FUTEX_WAIT_SPLIT_VALUE_CHECK_OK: &str = "FUTEX_WAIT_SPLIT_VALUE_CHECK_OK";
pub const MARK_FUTEX_WAIT_SPLIT_BLOCK_PUBLISH_OK: &str = "FUTEX_WAIT_SPLIT_BLOCK_PUBLISH_OK";
pub const MARK_FUTEX_WAIT_SPLIT_DONE_BLOCKED: &str = "FUTEX_WAIT_SPLIT_DONE result=blocked";
/// The one concrete blocker that keeps FutexWait's LIVE retirement deferred: the matched
/// wait's queue-advancing dispatch is the global-lock `switch_required` case.
pub const MARK_FUTEX_WAIT_DEFERRED_REASON: &str = "GLOBAL_LOCK_RETIRE_CLASS_DEFERRED class=FutexWait reason=block_dispatch_switch_required_needs_global_lock";

/// # Validation status
/// - LIVE_TRAP_SMOKE_X86_64 (Stage 32B) — wired into the live trap seam:
///   `try_split_dispatch_into_frame` routes IpcRecv (NR 2) here BEFORE the global
///   lock. Only the kernel-task queued-plain case is serviced; every other case
///   returns `None` and propagates to the unchanged global-lock fallback. See
///   `doc/KERNEL_LOCKING.md` §50.11.
///
/// Stage 31 split-recv seam: attempt to service an `IpcRecv` for a plain queued
/// message on a buffered endpoint, delivered to a kernel-task receiver, with no
/// recv-v2 metadata. Default-deny for every other case.
///
// Lock order: [no lock] → current_tid_authoritative (takes+releases global) →
//             ipc_state_lock (rank 3) → [release] → [no lock]
// Forbidden under ipc_state_lock: scheduler lock, capability lock, VM lock, user-copy
// task_switched: always false (no dispatch/yield/switch)
///
/// Returns:
/// * `Some(Ok(()))` — a plain message was dequeued; success lanes are written into
///   `frame` byte-for-byte as the kernel-task branch of the old recv path
///   (`set_ok(sender, raw_len, NO_TRANSFER_CAP)` + inline payload words).
/// * `Some(Err(e))` — the recv cap was invalid; `e` is the same error the old
///   global-lock recv path returned.
/// * `None` — NOT split-eligible (default-deny): empty queue, recv-v2, cap-transfer
///   or reply-cap message, user-ASID receiver (would require a forbidden user copy),
///   sender-waiter refill, blocking, timeout, or a non-IpcRecv syscall.
///
/// Stage 32B live-wire scope: the realistic live x86_64 receivers (PM/init/VFS) are
/// user-ASID tasks whose plain-recv writeback needs `copy_to_current_user`, which
/// is still forbidden on the split path — those are rejected here (`None`) and fall
/// back unchanged. Only a kernel-task receiver of a queued plain message is
/// serviced on the split path; the endpoint-cap resolution is performed via the
/// Stage 32 phase-separated split-read (`resolve_endpoint_recv_cap_split_read`).
pub(crate) fn try_split_ipc_recv_queued_plain_into_frame(
    shared: &SharedKernel,
    cpu: CpuId,
    frame: &mut TrapFrame,
) -> Option<Result<(), TrapHandleError>> {
    // Number-only default-deny gate: only IpcRecv is considered here.
    let syscall = Syscall::decode(frame.syscall_num()).ok()?;
    if !matches!(syscall, Syscall::IpcRecv) {
        return None;
    }
    shared.try_split_ipc_recv_queued_plain_into_frame(cpu, frame)
}

/// # Validation status
/// - M2_SEAM_LIVE_D3_BRK_SHRINK (Stage 114) — wired into the live trap seam:
///   `try_split_dispatch_into_frame` routes `VmBrk` (NR 14) here BEFORE the
///   global lock. Only the page-crossing shrink case (at most one CPU online,
///   group-leader caller) is serviced; every other case returns `None` and
///   propagates to the unchanged global-lock fallback (`handle_vm_brk`).
///
/// Thin number-only gate mirroring [`try_split_ipc_recv_queued_plain_into_frame`]:
/// re-decode the syscall number defensively, reject anything but `VmBrk`, then
/// delegate to `SharedKernel::try_split_vm_brk_shrink_into_frame`, which holds
/// the full eligibility logic and the single-CPU-online safety proof.
pub(crate) fn try_split_vm_brk_shrink_into_frame(
    shared: &SharedKernel,
    cpu: CpuId,
    frame: &mut TrapFrame,
) -> Option<Result<(), TrapHandleError>> {
    // Number-only default-deny gate: only VmBrk is considered here.
    let syscall = Syscall::decode(frame.syscall_num()).ok()?;
    if !matches!(syscall, Syscall::VmBrk) {
        return None;
    }
    shared.try_split_vm_brk_shrink_into_frame(cpu, frame)
}

/// Number-only split eligibility classifier (no arg validation, no lock).
///
/// Used by [`try_split_dispatch_into_frame`] as the fast default-deny gate before
/// reading any scheduler/task state. Argument-precondition validation is still
/// performed by `classify_split_eligible`, so a syscall that passes this gate but
/// fails its preconditions (e.g. `target_pid == 0`) still falls back to the
/// global-lock path for the canonical error encoding.
fn classify_split_eligible_nr_only(syscall: Syscall) -> Option<Syscall> {
    match syscall {
        Syscall::ControlPlaneSetCnodeSlots => Some(syscall),
        // Stage 32B: IpcRecv (NR 2) passes the NR gate so the live seam attempts the
        // kernel-task queued-plain split via `try_split_ipc_recv_queued_plain_into_frame`.
        // Final eligibility (kernel-task receiver, queued plain, no sender-wake/recv-v2)
        // is decided inside that helper; ineligible cases return `None` → fallback.
        Syscall::IpcRecv => Some(syscall),
        // Stage 114: VmBrk (NR 14) passes the NR gate so the live seam attempts the
        // page-crossing-shrink split via `try_split_vm_brk_shrink_into_frame`. Final
        // eligibility (group leader, page-crossing shrink, single CPU online) is
        // decided inside that helper; ineligible cases return `None` → fallback.
        Syscall::VmBrk => Some(syscall),
        // Stage 191A (GLOBAL-LOCK-RETIRE, first class): DebugLog (NR 15) is a pure READ
        // syscall — it resolves the current task, copies user bytes, logs, and never
        // blocks/yields/switches tasks or mutates KernelState. It is serviced off the
        // global lock via `try_split_debug_log_into_frame`. Any case it cannot service
        // returns `None` → unchanged global-lock fallback.
        Syscall::DebugLog => Some(syscall),
        // Stage 191B (GLOBAL-LOCK-RETIRE, second class): FutexWake (NR 10) — the CALLER
        // never task-switches; it only mutates waiter/run-queue state (Blocked→Runnable
        // + enqueue). Serviced off the global lock via `try_split_futex_wake_into_frame`
        // (task split-mut wake scan + scheduler split-mut enqueue). NOT FutexWait (NR 9,
        // which blocks the caller — stays global-lock-only). Ineligible cases (invalid
        // addr) return `None` → unchanged global-lock fallback, which produces the exact
        // error. (NR 11 is SpawnThread, NOT FutexWake — do not confuse the two.)
        Syscall::FutexWake => Some(syscall),
        // Stage 191C (GLOBAL-LOCK-RETIRE, third class): InitramfsReadChunk (NR 27) is a
        // read-only user-copy syscall — it copies immutable initramfs/CPIO bytes into a
        // user buffer and mutates NO task/scheduler/IPC/cap/VM structural state. Its
        // SUCCESS path is serviced off the global lock via
        // `try_split_initramfs_read_chunk_into_frame`; every error case returns `None` →
        // unchanged global-lock fallback (canonical error + diagnostic logs). NOT
        // CreateInitramfsFileSliceMo (NR 28), which MINTS a capability (cap-state
        // mutation) and stays global-lock-only.
        Syscall::InitramfsReadChunk => Some(syscall),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::boot::Bootstrap;
    use crate::kernel::scheduler::CpuId;
    use crate::kernel::syscall::{
        SYSCALL_CONTROL_PLANE_SET_CNODE_SLOTS_NR, SYSCALL_COUNT, SYSCALL_IPC_RECV_NR,
        SYSCALL_IPC_SEND_NR, SYSCALL_SPAWN_PROCESS_NR, SYSCALL_VM_MAP_NR,
    };
    use crate::kernel::task::TaskClass;

    fn decode(nr: usize) -> Syscall {
        Syscall::decode(nr).expect("decode syscall nr")
    }

    /// Boot a SharedKernel with a SystemServer requester (900) and an App target
    /// (901), with the requester dispatched as the current task — the same setup
    /// the Stage 27 control-plane helper test uses.
    fn shared_with_control_plane_requester() -> (SharedKernel, u64, u64) {
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        kernel.with(|state| {
            state
                .register_task_with_class(900, TaskClass::SystemServer)
                .expect("system server");
            state
                .register_task_with_class(901, TaskClass::App)
                .expect("target app");
            state.enqueue_current_cpu(900).expect("enqueue");
            state.dispatch_next_task().expect("dispatch");
            if state.current_tid() != Some(900) {
                state.yield_current().expect("switch");
            }
        });
        let _ = CpuId(0);
        (kernel, 900, 901)
    }

    #[test]
    fn stage28_split_dispatch_whitelist_accepts_cnode_slots_syscall() {
        let (kernel, requester, target) = shared_with_control_plane_requester();
        let before = kernel
            .with(|state| {
                let cnode = state.process_cnode_for_pid(target).expect("cnode");
                state.cnode_slot_capacity(cnode)
            })
            .expect("before capacity");
        let requested = before.saturating_add(4);

        let syscall = decode(SYSCALL_CONTROL_PLANE_SET_CNODE_SLOTS_NR);
        let args = [target, requested as u64, 0, 0, 0, 0];

        // Must be classified eligible.
        assert_eq!(
            classify_split_eligible(syscall, requester, args),
            Some(SplitEligibleSyscall::ControlPlaneCnodeSlots {
                requester_tid: requester,
                target_pid: target,
                slots: requested,
            }),
            "control-plane cnode-slots must be split-eligible"
        );

        // Must dispatch through the split path and mutate the capability domain.
        let result = try_split_dispatch(&kernel, syscall, requester, args);
        assert_eq!(
            result,
            Some(Ok(())),
            "split dispatch must service the syscall"
        );

        let after = kernel.with(|state| {
            let cnode = state.process_cnode_for_pid(target).expect("cnode");
            state.cnode_slot_capacity(cnode)
        });
        assert_eq!(
            after,
            Some(requested),
            "split path must resize the target cnode"
        );
    }

    #[test]
    fn stage28_split_dispatch_whitelist_rejects_ipc_send() {
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        let syscall = decode(SYSCALL_IPC_SEND_NR);
        let args = [1, 2, 3, 4, 5, 6];
        assert_eq!(classify_split_eligible(syscall, 1, args), None);
        assert_eq!(
            try_split_dispatch(&kernel, syscall, 1, args),
            None,
            "IPC send must fall back to the global-lock path"
        );
    }

    #[test]
    fn stage28_split_dispatch_whitelist_rejects_ipc_recv() {
        // Stage 32B: IpcRecv now classifies as `IpcRecvKernelTask` (it is serviced by
        // the frame-level seam), but the ARG-ONLY `try_split_dispatch` path still
        // returns `None` — IpcRecv is never serviced through this entry point; it
        // defers to `try_split_dispatch_into_frame` / global-lock fallback.
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        let syscall = decode(SYSCALL_IPC_RECV_NR);
        let args = [1, 2, 3, 4, 5, 6];
        assert_eq!(
            classify_split_eligible(syscall, 1, args),
            Some(SplitEligibleSyscall::IpcRecvKernelTask)
        );
        assert_eq!(
            try_split_dispatch(&kernel, syscall, 1, args),
            None,
            "IPC recv must not be serviced by the arg-only split path"
        );
    }

    #[test]
    fn stage28_split_dispatch_whitelist_rejects_spawnv5() {
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        let syscall = decode(SYSCALL_SPAWN_PROCESS_NR);
        let args = [1, 2, 3, 4, 5, 6];
        assert_eq!(classify_split_eligible(syscall, 1, args), None);
        assert_eq!(
            try_split_dispatch(&kernel, syscall, 1, args),
            None,
            "SpawnV5 must fall back to the global-lock path"
        );
    }

    #[test]
    fn stage28_split_dispatch_whitelist_rejects_vm_map() {
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        let syscall = decode(SYSCALL_VM_MAP_NR);
        let args = [1, 2, 3, 4, 5, 6];
        assert_eq!(classify_split_eligible(syscall, 1, args), None);
        assert_eq!(
            try_split_dispatch(&kernel, syscall, 1, args),
            None,
            "VM map must fall back to the global-lock path"
        );
    }

    #[test]
    fn stage28_split_dispatch_fallback_preserved_for_unwhitelisted() {
        // Every non-whitelisted syscall number must classify as None — the
        // default-deny contract. We exhaustively walk every decodable syscall and
        // assert that only ControlPlaneSetCnodeSlots and IpcRecv (Stage 32B) are
        // ever eligible, and that the ARG-ONLY `try_split_dispatch` services none of
        // them with zero args (IpcRecv is always deferred to the frame-level seam).
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        let args = [0u64; 6]; // zero args → even cnode-slots fails preconditions → None
        for nr in 0..SYSCALL_COUNT {
            let Ok(syscall) = Syscall::decode(nr) else {
                continue; // gaps in the NR space are not valid syscalls
            };
            // With zero args, only IpcRecv (NR 2, no arg preconditions) classifies
            // eligible; cnode-slots fails its preconditions and everything else is
            // default-deny.
            if matches!(syscall, Syscall::IpcRecv) {
                assert_eq!(
                    classify_split_eligible(syscall, 1, args),
                    Some(SplitEligibleSyscall::IpcRecvKernelTask),
                    "IpcRecv must classify as split-eligible (frame-level serviced)"
                );
            } else {
                assert_eq!(
                    classify_split_eligible(syscall, 1, args),
                    None,
                    "syscall nr {} must default-deny with zero args",
                    nr
                );
            }
            assert_eq!(
                try_split_dispatch(&kernel, syscall, 1, args),
                None,
                "syscall nr {} must not be serviced by the arg-only split path with zero args",
                nr
            );
        }
        // And the control-plane syscall with valid args IS the sole eligible one.
        let cp = decode(SYSCALL_CONTROL_PLANE_SET_CNODE_SLOTS_NR);
        assert!(
            classify_split_eligible(cp, 1, [5, 8, 0, 0, 0, 0]).is_some(),
            "control-plane cnode-slots with valid args must be eligible"
        );
    }

    #[test]
    fn stage28_syscall_count_unchanged() {
        // ABI guard: the split-dispatch scaffold is pure additive infrastructure
        // and must not alter the syscall ABI.
        assert_eq!(SYSCALL_COUNT, 32, "Stage 28 must not change SYSCALL_COUNT");
    }

    #[test]
    fn stage28_stage27_split_mut_helper_still_works() {
        // Regression: the Stage 27 split-mut helper the bridge delegates to must
        // still behave identically when invoked directly.
        let (kernel, requester, target) = shared_with_control_plane_requester();
        let before = kernel
            .with(|state| {
                let cnode = state.process_cnode_for_pid(target).expect("cnode");
                state.cnode_slot_capacity(cnode)
            })
            .expect("before");
        let requested = before.saturating_add(8);
        kernel
            .control_plane_set_process_cnode_slots_split_mut(requester, target, requested)
            .expect("split-mut helper");
        let after = kernel.with(|state| {
            let cnode = state.process_cnode_for_pid(target).expect("cnode");
            state.cnode_slot_capacity(cnode)
        });
        assert_eq!(after, Some(requested), "Stage 27 helper must still resize");

        // Absent requester still yields the stable TaskMissing error.
        let err = kernel
            .control_plane_set_process_cnode_slots_split_mut(123_456, target, 8)
            .expect_err("absent requester must fail");
        assert_eq!(err, KernelError::TaskMissing);
    }

    // ----------------------------------------------------------------------
    // Stage 29 — live-wired result-writeback seam (try_split_dispatch_into_frame)
    // ----------------------------------------------------------------------

    use crate::kernel::trapframe::TrapFrame;

    const CPU0: CpuId = CpuId(0);

    /// Build the same NR-8 trap frame the live arch path constructs:
    /// arg(SYSCALL_ARG_CAP)=target_pid, arg(SYSCALL_ARG_PTR)=slots.
    fn cnode_slots_frame(target_pid: u64, slots: usize) -> TrapFrame {
        TrapFrame::new(
            SYSCALL_CONTROL_PLANE_SET_CNODE_SLOTS_NR,
            [target_pid as usize, slots, 0, 0, 0, 0],
        )
    }

    /// Boot a SharedKernel where an App requester (901) is the current task on
    /// CPU 0, plus a second App target (902). Used to exercise the MissingRight
    /// guard (a non-system-server App may only resize its own cnode).
    fn shared_with_app_requester() -> (SharedKernel, u64, u64) {
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        kernel.with(|state| {
            state
                .register_task_with_class(901, TaskClass::App)
                .expect("app requester");
            state
                .register_task_with_class(902, TaskClass::App)
                .expect("app target");
            state.enqueue_current_cpu(901).expect("enqueue");
            state.dispatch_next_task().expect("dispatch");
            if state.current_tid() != Some(901) {
                state.yield_current().expect("switch");
            }
        });
        (kernel, 901, 902)
    }

    #[test]
    fn stage29_split_cnode_slots_ok_return_lanes() {
        let (kernel, _requester, target) = shared_with_control_plane_requester();
        let before = kernel
            .with(|state| {
                let cnode = state.process_cnode_for_pid(target).expect("cnode");
                state.cnode_slot_capacity(cnode)
            })
            .expect("before");
        let requested = before.saturating_add(4);
        let mut frame = cnode_slots_frame(target, requested);

        let result = try_split_dispatch_into_frame(&kernel, CPU0, &mut frame);
        assert_eq!(result, Some(Ok(())), "split seam must service NR 8");
        // Exact lanes the old global-lock handler produced: set_ok(slots, pid, 0).
        assert_eq!(frame.ret0(), requested, "ret0 == slots");
        assert_eq!(frame.ret1(), target as usize, "ret1 == target pid");
        assert_eq!(frame.ret2(), 0, "ret2 == 0");
        assert_eq!(frame.error_code(), None, "no error on success");

        let after = kernel.with(|state| {
            let cnode = state.process_cnode_for_pid(target).expect("cnode");
            state.cnode_slot_capacity(cnode)
        });
        assert_eq!(after, Some(requested), "capability domain actually resized");
    }

    #[test]
    fn stage29_split_cnode_slots_missing_task_error() {
        // Requester TID with no registered task → TaskMissing. Exercised via the
        // helper the seam delegates to (the seam itself always reads a present
        // current TID; an absent requester must surface the same error).
        let (kernel, _requester, target) = shared_with_control_plane_requester();
        let syscall = decode(SYSCALL_CONTROL_PLANE_SET_CNODE_SLOTS_NR);
        let args = [target, 16, 0, 0, 0, 0];
        let result = try_split_dispatch(&kernel, syscall, 424_242, args);
        assert_eq!(result, Some(Err(KernelError::TaskMissing)));
    }

    #[test]
    fn stage29_split_cnode_slots_bad_requester_class_error() {
        // App requester (901) targeting a DIFFERENT pid (902) → MissingRight.
        let (kernel, _requester, target) = shared_with_app_requester();
        let mut frame = cnode_slots_frame(target, 16);
        let result = try_split_dispatch_into_frame(&kernel, CPU0, &mut frame);
        assert_eq!(
            result,
            Some(Err(TrapHandleError::Syscall(SyscallError::from(
                KernelError::MissingRight
            )))),
            "App requester resizing another pid's cnode must be MissingRight"
        );
        // On error the seam must NOT write a success payload.
        assert_eq!(frame.ret0(), 0);
        assert_eq!(frame.ret1(), 0);
    }

    #[test]
    fn stage29_split_cnode_slots_missing_cnode_error() {
        // System-server requester targeting a pid with no registered cnode and no
        // pre-reserved cnode space: the create path must fail rather than fabricate
        // a success. We use a target pid that was never registered.
        let (kernel, _requester, _target) = shared_with_control_plane_requester();
        let unregistered_pid = 7_777u64;
        // Whatever the domain decides (create or reject), the seam must propagate
        // the SAME Result the split-mut helper returns — never silently OK with a
        // bogus frame payload. Compare seam vs direct helper.
        let mut frame = cnode_slots_frame(unregistered_pid, 16);
        let seam = try_split_dispatch_into_frame(&kernel, CPU0, &mut frame);
        let direct =
            kernel.control_plane_set_process_cnode_slots_split_mut(900, unregistered_pid, 16);
        match (seam, direct) {
            (Some(Ok(())), Ok(())) => {
                // Create path succeeded: the frame must carry the canonical lanes.
                assert_eq!(frame.ret0(), 16);
                assert_eq!(frame.ret1(), unregistered_pid as usize);
            }
            (Some(Err(TrapHandleError::Syscall(s))), Err(k)) => {
                assert_eq!(
                    s,
                    SyscallError::from(k),
                    "seam error must equal helper error"
                );
                assert_eq!(
                    frame.error_code(),
                    None,
                    "seam never writes set_err for hard errors"
                );
            }
            (seam, direct) => panic!("seam/direct divergence: {seam:?} vs {direct:?}"),
        }
    }

    #[test]
    fn stage29_split_cnode_slots_duplicate_update_ok() {
        // Calling the seam twice with the same target must be idempotent-OK.
        let (kernel, _requester, target) = shared_with_control_plane_requester();
        let before = kernel
            .with(|state| {
                let cnode = state.process_cnode_for_pid(target).expect("cnode");
                state.cnode_slot_capacity(cnode)
            })
            .expect("before");
        let requested = before.saturating_add(6);
        let mut f1 = cnode_slots_frame(target, requested);
        assert_eq!(
            try_split_dispatch_into_frame(&kernel, CPU0, &mut f1),
            Some(Ok(()))
        );
        let mut f2 = cnode_slots_frame(target, requested);
        assert_eq!(
            try_split_dispatch_into_frame(&kernel, CPU0, &mut f2),
            Some(Ok(()))
        );
        assert_eq!(f2.ret0(), requested);
        assert_eq!(f2.ret1(), target as usize);
        let after = kernel.with(|state| {
            let cnode = state.process_cnode_for_pid(target).expect("cnode");
            state.cnode_slot_capacity(cnode)
        });
        assert_eq!(after, Some(requested));
    }

    #[test]
    fn stage29_split_cnode_slots_capacity_resize_ok() {
        // Distinct grow then a second grow: lanes track the latest request.
        let (kernel, _requester, target) = shared_with_control_plane_requester();
        let base = kernel
            .with(|state| {
                let cnode = state.process_cnode_for_pid(target).expect("cnode");
                state.cnode_slot_capacity(cnode)
            })
            .expect("base");
        let grow1 = base.saturating_add(2);
        let mut f1 = cnode_slots_frame(target, grow1);
        assert_eq!(
            try_split_dispatch_into_frame(&kernel, CPU0, &mut f1),
            Some(Ok(()))
        );
        assert_eq!(f1.ret0(), grow1);
        let grow2 = grow1.saturating_add(5);
        let mut f2 = cnode_slots_frame(target, grow2);
        assert_eq!(
            try_split_dispatch_into_frame(&kernel, CPU0, &mut f2),
            Some(Ok(()))
        );
        assert_eq!(f2.ret0(), grow2);
        let after = kernel.with(|state| {
            let cnode = state.process_cnode_for_pid(target).expect("cnode");
            state.cnode_slot_capacity(cnode)
        });
        assert_eq!(after, Some(grow2));
    }

    #[test]
    fn stage29_split_cnode_slots_error_code_preserved() {
        // The error code surfaced by the seam must equal the From<KernelError>
        // SyscallError code of the underlying domain error (MissingRight → 4).
        let (kernel, _requester, target) = shared_with_app_requester();
        let mut frame = cnode_slots_frame(target, 16);
        let Some(Err(TrapHandleError::Syscall(err))) =
            try_split_dispatch_into_frame(&kernel, CPU0, &mut frame)
        else {
            panic!("expected a Syscall error");
        };
        assert_eq!(err, SyscallError::from(KernelError::MissingRight));
        assert_eq!(err.code(), SyscallError::MissingRight.code());
    }

    #[test]
    fn stage29_split_cnode_slots_no_scheduler_side_effect() {
        // The split path must not switch tasks: current TID is unchanged across it.
        let (kernel, requester, target) = shared_with_control_plane_requester();
        let before_tid = kernel.current_tid_split_read(CPU0);
        assert_eq!(before_tid, Some(requester));
        let mut frame = cnode_slots_frame(target, 12);
        let _ = try_split_dispatch_into_frame(&kernel, CPU0, &mut frame);
        let after_tid = kernel.current_tid_split_read(CPU0);
        assert_eq!(
            after_tid,
            Some(requester),
            "no task switch (task_switched==false)"
        );
    }

    #[test]
    fn stage29_split_cnode_slots_no_ipc_side_effect() {
        // The split path must not enqueue IPC: the target task stays runnable and
        // its status is not changed to any blocked endpoint state.
        let (kernel, _requester, target) = shared_with_control_plane_requester();
        let mut frame = cnode_slots_frame(target, 14);
        let _ = try_split_dispatch_into_frame(&kernel, CPU0, &mut frame);
        let status = kernel.with(|state| state.task_status(target));
        assert!(
            !matches!(
                status,
                Some(crate::kernel::task::TaskStatus::Blocked(
                    crate::kernel::task::WaitReason::EndpointSend(_)
                        | crate::kernel::task::WaitReason::EndpointReceive(_)
                ))
            ),
            "split path must not block the target on any endpoint"
        );
    }

    // ---- Part 5: fallback safety ----

    #[test]
    fn stage29_only_nr8_is_split_eligible() {
        assert!(
            classify_split_eligible_nr_only(decode(SYSCALL_CONTROL_PLANE_SET_CNODE_SLOTS_NR))
                .is_some()
        );
    }

    #[test]
    fn stage29_ipc_send_not_eligible() {
        let (kernel, _r, _t) = shared_with_control_plane_requester();
        let mut frame = TrapFrame::new(SYSCALL_IPC_SEND_NR, [1, 2, 3, 4, 5, 6]);
        assert_eq!(
            try_split_dispatch_into_frame(&kernel, CPU0, &mut frame),
            None,
            "IPC send must fall back to the global-lock path"
        );
        assert!(classify_split_eligible_nr_only(decode(SYSCALL_IPC_SEND_NR)).is_none());
    }

    #[test]
    fn stage29_spawnv5_not_eligible() {
        let (kernel, _r, _t) = shared_with_control_plane_requester();
        let mut frame = TrapFrame::new(SYSCALL_SPAWN_PROCESS_NR, [1, 2, 3, 4, 5, 6]);
        assert_eq!(
            try_split_dispatch_into_frame(&kernel, CPU0, &mut frame),
            None
        );
        assert!(classify_split_eligible_nr_only(decode(SYSCALL_SPAWN_PROCESS_NR)).is_none());
    }

    #[test]
    fn stage29_vm_map_not_eligible() {
        let (kernel, _r, _t) = shared_with_control_plane_requester();
        let mut frame = TrapFrame::new(SYSCALL_VM_MAP_NR, [1, 2, 3, 4, 5, 6]);
        assert_eq!(
            try_split_dispatch_into_frame(&kernel, CPU0, &mut frame),
            None
        );
        assert!(classify_split_eligible_nr_only(decode(SYSCALL_VM_MAP_NR)).is_none());
    }

    #[test]
    fn stage29_futex_not_eligible() {
        let (kernel, _r, _t) = shared_with_control_plane_requester();
        // FutexWait (NR 9) is genuinely never split-eligible — it BLOCKS the caller,
        // so it stays global-lock-only. (Stage 191B split-retired FutexWake (NR 10),
        // which does NOT block the caller; that eligibility is pinned separately.)
        let mut frame = TrapFrame::new(
            crate::kernel::syscall::SYSCALL_FUTEX_WAIT_NR,
            [1, 2, 3, 4, 5, 6],
        );
        assert_eq!(
            try_split_dispatch_into_frame(&kernel, CPU0, &mut frame),
            None
        );
        assert!(
            classify_split_eligible_nr_only(decode(crate::kernel::syscall::SYSCALL_FUTEX_WAIT_NR))
                .is_none(),
            "FutexWait must stay global-lock-only (it blocks the caller)"
        );
        // Stage 191B: FutexWake IS now split-eligible.
        assert!(
            classify_split_eligible_nr_only(decode(crate::kernel::syscall::SYSCALL_FUTEX_WAKE_NR))
                .is_some(),
            "FutexWake must be split-eligible (Stage 191B)"
        );
    }

    /// Stage 195C guard: pin the exact FutexWake / FutexWait / SpawnThread NR identities so a
    /// future edit cannot silently reintroduce the "FutexWake is NR 11" confusion (NR 11 is
    /// SpawnThread; FutexWake is NR 10; FutexWait is NR 9). Only FutexWake (NR 10) is
    /// split-eligible; FutexWait (NR 9) and SpawnThread (NR 11) stay global-lock-only.
    #[test]
    fn stage195c_futex_wake_nr10_split_eligible_wait_and_spawn_thread_excluded() {
        use crate::kernel::syscall::{
            SYSCALL_FUTEX_WAIT_NR, SYSCALL_FUTEX_WAKE_NR, SYSCALL_SPAWN_THREAD_NR,
        };
        // The real syscall numbers — the Stage 195C task text's "NR11" for FutexWake is wrong.
        assert_eq!(SYSCALL_FUTEX_WAIT_NR, 9, "FutexWait is NR 9");
        assert_eq!(SYSCALL_FUTEX_WAKE_NR, 10, "FutexWake is NR 10 (NOT 11)");
        assert_eq!(
            SYSCALL_SPAWN_THREAD_NR, 11,
            "NR 11 is SpawnThread, NOT FutexWake"
        );
        assert!(
            matches!(decode(SYSCALL_FUTEX_WAKE_NR), Syscall::FutexWake),
            "NR 10 must decode to FutexWake"
        );
        // Only FutexWake (NR 10) passes the NR-only split gate.
        assert!(classify_split_eligible_nr_only(decode(SYSCALL_FUTEX_WAKE_NR)).is_some());
        assert!(classify_split_eligible_nr_only(decode(SYSCALL_FUTEX_WAIT_NR)).is_none());
        assert!(classify_split_eligible_nr_only(decode(SYSCALL_SPAWN_THREAD_NR)).is_none());
    }

    #[test]
    fn stage29_syscall_count_still_30() {
        assert_eq!(SYSCALL_COUNT, 32, "Stage 29 must not change SYSCALL_COUNT");
    }

    #[test]
    fn stage29_whitelist_exhaustive() {
        // Iterate the full NR space; only NR 8 (cnode-slots), NR 2 (IpcRecv,
        // Stage 32B), NR 14 (VmBrk, Stage 114), NR 15 (DebugLog, Stage 191A),
        // NR 10 (FutexWake, Stage 191B), and NR 27 (InitramfsReadChunk, Stage 191C)
        // may pass the NR-only split-eligibility gate. Every other syscall stays
        // global-lock-only.
        for nr in 0..SYSCALL_COUNT {
            let Ok(syscall) = Syscall::decode(nr) else {
                continue;
            };
            let eligible = classify_split_eligible_nr_only(syscall).is_some();
            if nr == SYSCALL_CONTROL_PLANE_SET_CNODE_SLOTS_NR
                || nr == SYSCALL_IPC_RECV_NR
                || nr == crate::kernel::syscall::SYSCALL_VM_BRK_NR
                || nr == crate::kernel::syscall::SYSCALL_DEBUG_LOG_NR
                || nr == crate::kernel::syscall::SYSCALL_FUTEX_WAKE_NR
                || nr == crate::kernel::syscall::SYSCALL_INITRAMFS_READ_CHUNK_NR
            {
                assert!(eligible, "NR {nr} must be split-eligible");
            } else {
                assert!(!eligible, "NR {nr} must NOT be split-eligible");
            }
        }
    }

    // ---- Stage 188H: pre-189 readiness guard ----

    #[test]
    fn stage188h_reap_faulted_task_excluded_from_split_dispatch() {
        // SUP-L7K-A ReapFaultedTask (NR 31) is a PM-only, global-lock-only
        // terminal-task reap. It must NEVER enter the split (no-global-lock)
        // dispatch path: both the arg-aware and NR-only classifiers default-deny
        // it, and `try_split_dispatch` must return `None` (defer to global lock).
        // This pins the invariant explicitly so a future stage cannot silently
        // whitelist it while wiring the AP/multi-dispatcher path in Stage 189.
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        let syscall = decode(crate::kernel::syscall::SYSCALL_REAP_FAULTED_TASK_NR);
        assert!(
            matches!(syscall, Syscall::ReapFaultedTask),
            "NR 31 must decode to ReapFaultedTask"
        );
        let args = [3u64, 0, 0, 0, 0, 0];
        assert_eq!(
            classify_split_eligible(syscall, 3, args),
            None,
            "ReapFaultedTask must NOT be arg-aware split-eligible"
        );
        assert!(
            classify_split_eligible_nr_only(syscall).is_none(),
            "ReapFaultedTask must NOT be NR-only split-eligible"
        );
        assert_eq!(
            try_split_dispatch(&kernel, syscall, 3, args),
            None,
            "ReapFaultedTask must fall back to the global-lock dispatch path"
        );
    }

    // ---- Part 6: result-writeback equivalence ----

    #[test]
    fn stage29_split_result_ok_encodes_same_as_old_path() {
        // The seam's success lanes must equal what the old global-lock handler
        // produced: set_ok(slot_capacity, target_pid, 0).
        let (kernel, _requester, target) = shared_with_control_plane_requester();
        let before = kernel
            .with(|state| {
                let cnode = state.process_cnode_for_pid(target).expect("cnode");
                state.cnode_slot_capacity(cnode)
            })
            .expect("before");
        let requested = before.saturating_add(3);
        let mut seam_frame = cnode_slots_frame(target, requested);
        assert_eq!(
            try_split_dispatch_into_frame(&kernel, CPU0, &mut seam_frame),
            Some(Ok(()))
        );

        // Reference encoding the old path used.
        let mut ref_frame = cnode_slots_frame(target, requested);
        ref_frame.set_ok(requested, target as usize, 0);

        assert_eq!(seam_frame.ret0(), ref_frame.ret0());
        assert_eq!(seam_frame.ret1(), ref_frame.ret1());
        assert_eq!(seam_frame.ret2(), ref_frame.ret2());
        assert_eq!(seam_frame.error_code(), ref_frame.error_code());
    }

    #[test]
    fn stage29_split_result_err_encodes_same_as_old_path() {
        // On a domain error the seam returns TrapHandleError::Syscall(e) — exactly
        // what the old handler's `Err(SyscallError)` became at the trap boundary —
        // and leaves the frame return lanes untouched (no set_ok), matching the old
        // path which never wrote set_ok on error.
        let (kernel, _requester, target) = shared_with_app_requester();
        let mut frame = cnode_slots_frame(target, 16);
        let result = try_split_dispatch_into_frame(&kernel, CPU0, &mut frame);
        assert_eq!(
            result,
            Some(Err(TrapHandleError::Syscall(SyscallError::from(
                KernelError::MissingRight
            ))))
        );
        assert_eq!(frame.ret0(), 0, "no success payload on error");
        assert_eq!(frame.ret1(), 0, "no success payload on error");
    }

    #[test]
    fn stage29_split_result_no_task_switch() {
        // entering_tid == exiting_tid across the seam ⇒ task_switched == false,
        // which the arch path requires to take the write_trap_returns branch.
        let (kernel, requester, target) = shared_with_control_plane_requester();
        let entering = kernel.current_tid_split_read(CPU0);
        let mut frame = cnode_slots_frame(target, 10);
        let _ = try_split_dispatch_into_frame(&kernel, CPU0, &mut frame);
        let exiting = kernel.current_tid_split_read(CPU0);
        assert_eq!(entering, exiting);
        assert_eq!(exiting, Some(requester));
    }

    #[test]
    fn stage29_split_dispatch_fallback_path_unchanged() {
        // A None return from the seam means the global-lock handler still runs.
        // Prove the global-lock dispatch produces the canonical result for the
        // same NR-8 frame the seam would have serviced — i.e. the fallback path is
        // intact and value-equivalent.
        let (kernel, _requester, target) = shared_with_control_plane_requester();
        // A NON-whitelisted syscall returns None from the seam.
        let mut send_frame = TrapFrame::new(SYSCALL_IPC_SEND_NR, [1, 2, 3, 4, 5, 6]);
        assert_eq!(
            try_split_dispatch_into_frame(&kernel, CPU0, &mut send_frame),
            None,
            "non-whitelisted syscall must fall back (None)"
        );
        // And the global-lock handler can still service NR 8 directly.
        let before = kernel
            .with(|state| {
                let cnode = state.process_cnode_for_pid(target).expect("cnode");
                state.cnode_slot_capacity(cnode)
            })
            .expect("before");
        let requested = before.saturating_add(7);
        let mut nr8 = cnode_slots_frame(target, requested);
        kernel
            .with(|state| crate::kernel::syscall::dispatch(state, &mut nr8))
            .expect("global-lock dispatch");
        assert_eq!(nr8.ret0(), requested);
        assert_eq!(nr8.ret1(), target as usize);
    }

    // ---- Stage 32B: IpcRecv classification ----

    #[test]
    fn stage32b_ipc_recv_classify_nr2_eligible() {
        // NR 2 (IpcRecv) now passes the NR-only split-eligibility gate.
        assert!(
            classify_split_eligible_nr_only(decode(SYSCALL_IPC_RECV_NR)).is_some(),
            "IpcRecv (NR 2) must be split-eligible at the NR gate"
        );
        // And the arg-level classifier maps it to the IpcRecvKernelTask variant.
        assert_eq!(
            classify_split_eligible(decode(SYSCALL_IPC_RECV_NR), 1, [0; 6]),
            Some(SplitEligibleSyscall::IpcRecvKernelTask)
        );
    }

    #[test]
    fn stage32b_ipc_recv_timeout_nr_not_in_whitelist() {
        // IpcRecvTimeout (NR 5) must NOT be split-eligible: it stays on the
        // global-lock path (scheduler/deadline interaction).
        assert!(
            classify_split_eligible_nr_only(decode(
                crate::kernel::syscall::SYSCALL_IPC_RECV_TIMEOUT_NR
            ))
            .is_none(),
            "IpcRecvTimeout must NOT be split-eligible"
        );
        assert_eq!(
            classify_split_eligible(
                decode(crate::kernel::syscall::SYSCALL_IPC_RECV_TIMEOUT_NR),
                1,
                [0; 6]
            ),
            None,
            "IpcRecvTimeout must fall back"
        );
    }

    #[test]
    fn stage32b_ipc_send_call_reply_not_split_eligible() {
        // The sender-side IPC syscalls stay default-deny.
        for nr in [
            SYSCALL_IPC_SEND_NR,
            crate::kernel::syscall::SYSCALL_IPC_CALL_NR,
            crate::kernel::syscall::SYSCALL_IPC_REPLY_NR,
        ] {
            assert!(
                classify_split_eligible_nr_only(decode(nr)).is_none(),
                "NR {nr} must NOT be split-eligible"
            );
        }
    }

    #[test]
    fn stage32b_arg_only_dispatch_defers_ipc_recv() {
        // The arg-only try_split_dispatch must NEVER service IpcRecv: it returns
        // None so the frame-level seam (and ultimately the global lock) handles it.
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        assert_eq!(
            try_split_dispatch(&kernel, decode(SYSCALL_IPC_RECV_NR), 1, [1, 0, 0, 0, 0, 0]),
            None,
            "arg-only dispatch must defer IpcRecv"
        );
    }

    #[test]
    fn stage32b_syscall_count_30() {
        assert_eq!(
            SYSCALL_COUNT, 32,
            "Stage 42+43 adds RecvSharedV3 (NR 30); stage32b invariant updated"
        );
    }
}
