// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Scheduler-domain syscall handlers.
//!
//! D4 step 3: mechanically split from the parent `syscall.rs` module with zero
//! behavior change. `syscall.rs` keeps minimal delegation shims so dispatch
//! routing remains explicit while futex/yield semantics stay owned by the
//! existing `KernelState` scheduler/futex methods.

use super::{SYSCALL_ARG_CAP, SYSCALL_ARG_LEN, SYSCALL_ARG_PTR, SyscallError};
use crate::kernel::boot::KernelState;
use crate::kernel::trapframe::TrapFrame;

/// THE futex-word range policy — one owner, `boot::futex::futex_word_range_check`.
///
/// Both acquisition adapters (the broad owner and the split route) call this; only the
/// reachability of the address space differs between them, never the rule. A null word is
/// `WrongObject`; an address whose last byte overflows `usize` or lands at or above
/// `KERNEL_SPACE_BASE` is `UserMemoryFault`.
#[cfg_attr(feature = "hosted-dev", allow(dead_code))]
pub(crate) fn futex_word_range_check(addr: usize) -> Result<(), crate::kernel::boot::KernelError> {
    crate::kernel::boot::futex::futex_word_range_check(addr)
}

/// FUTEX-FOUND1 — the decoded futex syscall arguments (NR 9 and NR 10 share the layout).
///
/// | slot | NR 9 `FutexWait` | NR 10 `FutexWake` |
/// |---|---|---|
/// | 0 | word address (4-byte aligned) | word address (4-byte aligned) |
/// | 1 | `expected` (`u32`) | `count` (`u32`) |
/// | 2 | flags: `FUTEX_FLAG_SHARED` (bit 0); every other bit reserved, must be 0 | same |
///
/// The kernel compares `expected` with the word IT reads (there is no caller-supplied `observed`).
/// Later operations — an absolute deadline, a requeue target, a wake mask — are each selected by a
/// flag bit that is reserved today, and that bit is what gives meaning to slots 3 and up; so a
/// reserved bit set now is `InvalidArgs`, and slots 3.. are not consulted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FutexArgs {
    pub(crate) addr: usize,
    pub(crate) value: u32,
    pub(crate) kind: crate::kernel::boot::futex::FutexKind,
}

/// THE decode, shared by the broad handlers and the split routes: the value as `u32`, the flags,
/// then the word's alignment. Its errors are all `InvalidArgs` and are answered before the address
/// range or the caller is consulted.
pub(crate) fn decode_futex_args(frame: &TrapFrame) -> Result<FutexArgs, SyscallError> {
    let addr = frame.arg(SYSCALL_ARG_CAP);
    let value = u32::try_from(frame.arg(SYSCALL_ARG_PTR)).map_err(|_| SyscallError::InvalidArgs)?;
    let kind = crate::kernel::boot::futex::FutexKind::from_flags(frame.arg(SYSCALL_ARG_LEN))
        .ok_or(SyscallError::InvalidArgs)?;
    if !crate::kernel::boot::futex::futex_word_aligned(addr) {
        return Err(SyscallError::InvalidArgs);
    }
    Ok(FutexArgs { addr, value, kind })
}

/// FUTEX-TIME1 — the decoded NR 9 arguments: the key arguments, and the relative timeout when
/// `FUTEX_FLAG_TIMED` is set.
///
/// | slot | NR 9 `FutexWait` |
/// |---|---|
/// | 0 | word address (4-byte aligned) |
/// | 1 | `expected` (`u32`) |
/// | 2 | flags: `FUTEX_FLAG_SHARED` (bit 0), `FUTEX_FLAG_TIMED` (bit 1); every other bit 0 |
/// | 3 | with `FUTEX_FLAG_TIMED` only: the timeout, a RELATIVE duration in nanoseconds (`u64`) |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FutexWaitArgs {
    pub(crate) key: FutexArgs,
    /// `Some(ns)` for a timed wait; `None` without `FUTEX_FLAG_TIMED`, and `args[3]` unread.
    pub(crate) timeout_ns: Option<u64>,
}

/// FUTEX-TIME1 — THE NR 9 decode, shared by the broad handler and the split route: the timed flag
/// is taken out first, then exactly [`decode_futex_args`] (so every key refusal is unchanged and
/// NR 10, which keeps using that decode, still refuses the timed bit), then `args[3]` only if the
/// flag was set. Every refusal is `InvalidArgs`.
pub(crate) fn decode_futex_wait_args(frame: &TrapFrame) -> Result<FutexWaitArgs, SyscallError> {
    use crate::kernel::boot::futex::{FUTEX_FLAG_TIMED, FUTEX_WAIT_FLAGS_DEFINED};
    let flags = frame.arg(SYSCALL_ARG_LEN);
    if flags & !FUTEX_WAIT_FLAGS_DEFINED != 0 {
        return Err(SyscallError::InvalidArgs);
    }
    let mut key_frame = frame.clone();
    key_frame.set_arg(SYSCALL_ARG_LEN, flags & !FUTEX_FLAG_TIMED);
    let key = decode_futex_args(&key_frame)?;
    let timeout_ns = (flags & FUTEX_FLAG_TIMED != 0).then(|| frame.arg(3) as u64);
    Ok(FutexWaitArgs { key, timeout_ns })
}

/// FUTEX-TIME1 — a relative timeout in nanoseconds as a number of scheduler ticks.
///
/// `0` is `0` ticks: the deadline is the admission instant, so a matching word times out at once
/// and a differing one still answers `0`. Any other duration is `ceil(ns / period) + 1` ticks: the
/// clock counts SERVICED timer periods, each re-armed from a fresh sample after the previous one,
/// so `n` increments span at least `n - 1` full periods — the `+ 1` absorbs the partial period the
/// wait is admitted in, and the wait is guaranteed at least `ns` before it can time out. There is
/// no upper bound (masked sections delay the tick; dispatch adds latency). Total: every `u64` is a
/// valid duration and none overflows here (`u64::MAX / period + 2` fits).
pub(crate) const fn futex_timeout_ticks(timeout_ns: u64, period_ns: u64) -> u64 {
    if timeout_ns == 0 {
        0
    } else {
        timeout_ns.div_ceil(period_ns) + 1
    }
}

/// FUTEX-TIME1 — THE absolute deadline of a timed wait admitted at scheduler tick `now`, or `None`
/// if `now + ticks` would overflow `u64` (refused `InvalidArgs`; unreachable — the tick would have
/// to run for millennia first). The clock is a non-wrapping monotone `u64` for any physical
/// uptime, so the collector's plain `now >= deadline` needs no wrapping arithmetic.
pub(crate) fn futex_wait_deadline(now: u64, timeout_ns: u64) -> Option<u64> {
    let period = crate::arch::platform_constants::SCHED_TICK_PERIOD_NS;
    let ticks = futex_timeout_ticks(timeout_ns, period);
    let deadline = now.checked_add(ticks);
    #[cfg(feature = "futex-time-witness")]
    crate::kernel::boot::futex::time_witness::deadline(timeout_ns, period, ticks, now, deadline);
    deadline
}

/// THE check of compare-and-park: does the word the KERNEL read still hold `expected`?
///
/// The caller's own read is not consulted — it can be stale by the time the kernel parks. The
/// park transaction applies this to its decision read and again to the read it makes after the
/// waiter is registered, both from the same mapping the key was resolved from.
/// FUTEX-REQUEUE1 (NR 17) — the decoded compare-and-requeue arguments.
///
/// | slot | meaning |
/// |---|---|
/// | 0 | source futex word address (4-byte aligned) |
/// | 1 | target futex word address (4-byte aligned) |
/// | 2 | `expected` source word (`u32`) |
/// | 3 | most waiters to wake (`u32`) |
/// | 4 | most further waiters to requeue (`u32`) |
/// | 5 | flags: bit 0 source shared, bit 1 target shared; every other bit reserved, must be 0 |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FutexRequeueArgs {
    pub(crate) source: usize,
    pub(crate) target: usize,
    pub(crate) expected: u32,
    pub(crate) wake: u32,
    pub(crate) requeue: u32,
    pub(crate) source_kind: crate::kernel::boot::futex::FutexKind,
    pub(crate) target_kind: crate::kernel::boot::futex::FutexKind,
}

/// NR 17 flag: the source word is a shared futex.
pub(crate) const FUTEX_REQUEUE_SOURCE_SHARED: usize = 1 << 0;
/// NR 17 flag: the target word is a shared futex.
pub(crate) const FUTEX_REQUEUE_TARGET_SHARED: usize = 1 << 1;
const FUTEX_REQUEUE_FLAGS_DEFINED: usize =
    FUTEX_REQUEUE_SOURCE_SHARED | FUTEX_REQUEUE_TARGET_SHARED;
/// NR 17 success status word (`ret2`): always carries `DONE`, so on AArch64 and RISC-V — where an
/// error is reported in `x0`/`a0` with `x2`/`a2` zero — a success can never be read as an error.
pub(crate) const FUTEX_REQUEUE_STATUS_DONE: usize = 1 << 8;
/// NR 17 success status bit: the wake portion stopped at a waiter it could not place (its home
/// queue full, or its CPU unable to take work); that waiter and every later one it would have
/// woken are still parked on the SOURCE key. The requeue portion is unaffected.
pub(crate) const FUTEX_REQUEUE_STATUS_CAPACITY_LIMITED: usize = 1 << 0;

/// THE NR 17 decode, shared by the broad handler and the split route: the three counts as `u32`,
/// the flags, then both addresses' alignment. Every refusal is `InvalidArgs`.
pub(crate) fn decode_futex_requeue_args(
    frame: &TrapFrame,
) -> Result<FutexRequeueArgs, SyscallError> {
    use crate::kernel::boot::futex::{FutexKind, futex_word_aligned};
    let u = |i: usize| u32::try_from(frame.arg(i)).map_err(|_| SyscallError::InvalidArgs);
    let (expected, wake, requeue) = (u(2)?, u(3)?, u(4)?);
    let flags = frame.arg(5);
    if flags & !FUTEX_REQUEUE_FLAGS_DEFINED != 0 {
        return Err(SyscallError::InvalidArgs);
    }
    let kind = |bit: usize| {
        if flags & bit != 0 {
            FutexKind::Shared
        } else {
            FutexKind::Private
        }
    };
    let (source, target) = (frame.arg(0), frame.arg(1));
    if !futex_word_aligned(source) || !futex_word_aligned(target) {
        return Err(SyscallError::InvalidArgs);
    }
    Ok(FutexRequeueArgs {
        source,
        target,
        expected,
        wake,
        requeue,
        source_kind: kind(FUTEX_REQUEUE_SOURCE_SHARED),
        target_kind: kind(FUTEX_REQUEUE_TARGET_SHARED),
    })
}

/// THE NR 17 result mapping, shared by both routes. A refusal that wrote nothing is an error —
/// `WouldBlock` for a source word that no longer holds `expected`, `InvalidArgs` for a target that
/// is the source key, the key error otherwise — so a mismatch can never look like a successful
/// zero-work call. A success writes the woken count, the requeued count and the status word.
pub(crate) fn apply_futex_requeue_result(
    frame: &mut TrapFrame,
    result: Result<
        crate::kernel::boot::futex::FutexRequeueOutcome,
        crate::kernel::boot::futex::FutexRequeueRefusal,
    >,
) -> Result<(), SyscallError> {
    use crate::kernel::boot::futex::FutexRequeueRefusal as R;
    match result {
        Ok(outcome) => {
            let status = FUTEX_REQUEUE_STATUS_DONE
                | if outcome.capacity_limited {
                    FUTEX_REQUEUE_STATUS_CAPACITY_LIMITED
                } else {
                    0
                };
            frame.set_ok(outcome.woken as usize, outcome.requeued as usize, status);
            Ok(())
        }
        Err(R::ValueChanged) => Err(SyscallError::WouldBlock),
        Err(R::SameKey) => Err(SyscallError::InvalidArgs),
        Err(R::Key(err)) => Err(SyscallError::from(err)),
    }
}

pub(crate) fn futex_should_park(expected: u32, word: u32) -> bool {
    word == expected
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FutexWaitDecision {
    /// The word the kernel read equals `expected`: park.
    Park,
    /// The futex word already moved out from under the caller. Nothing is published and the
    /// syscall answers `0` — the canonical `set_ok(usize::from(false), 0, 0)`.
    Proceed,
}

/// U9-FUTEX-WAIT-FINAL §3 — what the exact `FutexWait` park transaction achieved.
///
/// The predecessor answered `bool`, and `false` meant only "the caller had no TCB". Everything
/// else it could produce — including a wake that landed mid-publication — was indistinguishable
/// from success, because the clear ran unconditionally afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FutexParkOutcome {
    /// PARKED. The exact incarnation is `Blocked(Futex(token))` with its keyed record published,
    /// and current on no CPU, and the
    /// priority it was removed at is carried so a compensation can put it back exactly. The drain
    /// owes the queue advance.
    Parked {
        priority: crate::kernel::scheduler::TaskPriority,
    },
    /// A WAKER WON during publication. Between the rank-2 registration and the rank-1
    /// compare-and-clear, an `NR 10` wake found this waiter, made it `Runnable` and attempted its
    /// enqueue. The caller is not parked; the settlement carries what the placement recovery
    /// achieved, because the clear had already run when this was discovered.
    ///
    /// The EXACT incarnation rides with it — including the priority the clear actually removed,
    /// which is the only record of the placement and which no caller can reconstruct — so a
    /// settlement handed to the bridge names the task this trap entered from and not a
    /// reconstruction of it.
    WokenDuringPublication {
        entering: crate::kernel::recv_waiter_split::RecvEnteringIncarnation,
        recovered: crate::kernel::recv_waiter_split::RecvUnwindOutcome,
    },
    /// BL3b — the word no longer equals `expected` when re-read AFTER the waiter was registered.
    /// A waker that stored the new value before scanning for waiters missed this registration,
    /// so parking would lose its wake. The registration is undone exactly, the caller is still
    /// this CPU's current, and the wait returns without blocking.
    ValueChanged,
    /// BL3b — the word could not be re-read after registration (its page went away). The
    /// registration is undone exactly and the read's error is the answer.
    WordUnreadable(crate::kernel::boot::KernelError),
    /// The rank-1 compare-and-clear found a DIFFERENT task current. The rank-2 registration is
    /// undone exactly, so nothing is left published.
    VictimChanged,
    /// The TCB is not the entering incarnation, or is gone, or is not `Running`. **Nothing was
    /// written** — this is the canonical `TaskMissing` population.
    IncarnationMoved,
    /// FUTEX-FOUND1 — every waiter slot holds a live record. **Nothing was written** and the
    /// caller is still current. Unreachable in production (one slot per possible task).
    CapacityExhausted,
    /// FUTEX-TIME1 — the word held `expected` and the deadline had already been reached at the
    /// compare-and-register: the timeout won before any registration. **Nothing was written**, the
    /// caller is still current, and the answer is `Err(TimedOut)`.
    TimedOut,
}

pub(super) fn handle_yield(
    kernel: &mut KernelState,
    frame: &mut TrapFrame,
) -> Result<(), SyscallError> {
    kernel.yield_current().map_err(SyscallError::from)?;
    frame.set_ok(0, 0, 0);
    Ok(())
}

pub(super) fn handle_futex_wait(
    kernel: &mut KernelState,
    frame: &mut TrapFrame,
) -> Result<(), SyscallError> {
    // U9-FUTEX-WAIT-FINAL §4 — the LIVE half of the closure claim, measured where arrivals are:
    // `dispatch`'s `Syscall::FutexWait` arm is this function's only caller, and it is reached only
    // when the split route declined.
    crate::yarm_log!(
        "FUTEX_WAIT_BROAD_ENTRY nr=9 tid={} result=broad_entry",
        kernel.current_tid().unwrap_or(u64::MAX)
    );
    let args = decode_futex_wait_args(frame)?;
    let deadline = match args.timeout_ns {
        None => None,
        Some(ns) => Some(
            futex_wait_deadline(kernel.scheduler_tick_now(), ns)
                .ok_or(SyscallError::InvalidArgs)?,
        ),
    };
    match kernel
        .futex_wait_current_timed(args.key.addr, args.key.value, args.key.kind, deadline)
        .map_err(SyscallError::from)?
    {
        crate::kernel::boot::FutexWaitBroadOutcome::NotBlocked => frame.set_ok(0, 0, 0),
        // An untimed wait that blocked: its only outcome is a wake.
        crate::kernel::boot::FutexWaitBroadOutcome::Blocked { timed: false } => {
            frame.set_ok(1, 0, 0)
        }
        // A TIMED wait that blocked: the winner is not known yet. The frame carries a fail-closed
        // provisional (`Internal`), never `1`; the terminal claim parks the real result as its
        // `FutexWait` completion, which the resume boundary encodes over this.
        crate::kernel::boot::FutexWaitBroadOutcome::Blocked { timed: true } => {
            frame.set_err(SyscallError::Internal.code())
        }
        crate::kernel::boot::FutexWaitBroadOutcome::TimedOut => {
            return Err(SyscallError::TimedOut);
        }
    }
    Ok(())
}

pub(super) fn handle_futex_cmp_requeue(
    kernel: &mut KernelState,
    frame: &mut TrapFrame,
) -> Result<(), SyscallError> {
    let args = decode_futex_requeue_args(frame)?;
    let result = kernel.futex_cmp_requeue_current(
        (args.source, args.source_kind),
        (args.target, args.target_kind),
        args.expected,
        args.wake,
        args.requeue,
    );
    apply_futex_requeue_result(frame, result)
}

pub(super) fn handle_futex_wake(
    kernel: &mut KernelState,
    frame: &mut TrapFrame,
) -> Result<(), SyscallError> {
    let args = decode_futex_args(frame)?;
    let woke = kernel
        .futex_wake(args.addr, args.value, args.kind)
        .map_err(SyscallError::from)?;
    frame.set_ok(woke as usize, 0, 0);
    Ok(())
}
