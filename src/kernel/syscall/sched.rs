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

/// FUTEX-FOUND1 / FUTEX-ABS-BITSET1 (ABI v13) — the decoded futex key arguments (NR 9 and NR 10
/// share the layout).
///
/// | slot | NR 9 `FutexWait` | NR 10 `FutexWake` |
/// |---|---|---|
/// | 0 | word address (4-byte aligned) | word address (4-byte aligned) |
/// | 1 | `expected` (`u32`) | `count` (`u32`) |
/// | 2 | flags: `SHARED` (bit 0), `TIMED` (bit 1), `BITSET` (bit 2); every other bit 0 | flags: `SHARED` (bit 0), `BITSET` (bit 2); every other bit 0 |
/// | 3 | with `TIMED` only: the ABSOLUTE `CLOCK_MONOTONIC` deadline, ns (`u64`) | not read |
/// | 4 | with `BITSET` only: the wait mask (`u32`, nonzero) | with `BITSET` only: the wake mask (`u32`, nonzero) |
///
/// The kernel compares `expected` with the word IT reads (there is no caller-supplied `observed`).
/// A slot is read only when its flag gives it meaning; without `BITSET` the mask is
/// `FUTEX_BITSET_MATCH_ANY`, so an unmasked call is the call it always was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FutexArgs {
    pub(crate) addr: usize,
    pub(crate) value: u32,
    pub(crate) kind: crate::kernel::boot::futex::FutexKind,
    /// FUTEX-ABS-BITSET1 — the call's mask (`FUTEX_BITSET_MATCH_ANY` without `BITSET`).
    pub(crate) mask: u32,
}

/// THE decode of the slots both calls share, in ONE precedence, every refusal `InvalidArgs` and
/// answered before the address range, the clock or the caller is consulted:
///
/// 1. an undefined flag bit (for the call's own `defined` set);
/// 2. slot 1 wider than `u32`;
/// 3. a misaligned word address;
/// 4. with `BITSET`: a mask wider than `u32`, or zero.
fn decode_futex_common(frame: &TrapFrame, defined: usize) -> Result<FutexArgs, SyscallError> {
    use crate::kernel::boot::futex::{
        FUTEX_ARG_MASK, FUTEX_BITSET_MATCH_ANY, FUTEX_FLAG_BITSET, FUTEX_FLAG_SHARED, FutexKind,
        futex_word_aligned,
    };
    let flags = frame.arg(SYSCALL_ARG_LEN);
    if flags & !defined != 0 {
        return Err(SyscallError::InvalidArgs);
    }
    let addr = frame.arg(SYSCALL_ARG_CAP);
    let value = u32::try_from(frame.arg(SYSCALL_ARG_PTR)).map_err(|_| SyscallError::InvalidArgs)?;
    if !futex_word_aligned(addr) {
        return Err(SyscallError::InvalidArgs);
    }
    let mask = if flags & FUTEX_FLAG_BITSET != 0 {
        match u32::try_from(frame.arg(FUTEX_ARG_MASK)) {
            Ok(m) if m != 0 => m,
            _ => return Err(SyscallError::InvalidArgs),
        }
    } else {
        FUTEX_BITSET_MATCH_ANY
    };
    let kind = if flags & FUTEX_FLAG_SHARED != 0 {
        FutexKind::Shared
    } else {
        FutexKind::Private
    };
    Ok(FutexArgs {
        addr,
        value,
        kind,
        mask,
    })
}

/// THE NR 10 decode, shared by the broad handler and the split route: [`decode_futex_common`]
/// with NR 10's flags (`SHARED`, `BITSET`); `TIMED` is an undefined bit here.
pub(crate) fn decode_futex_args(frame: &TrapFrame) -> Result<FutexArgs, SyscallError> {
    decode_futex_common(frame, crate::kernel::boot::futex::FUTEX_WAKE_FLAGS_DEFINED)
}

/// FUTEX-TIME1 / FUTEX-ABS-BITSET1 — the decoded NR 9 arguments: the key arguments (with the
/// mask), and the absolute deadline when `FUTEX_FLAG_TIMED` is set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FutexWaitArgs {
    pub(crate) key: FutexArgs,
    /// `Some` for a timed wait: the caller's ABSOLUTE `CLOCK_MONOTONIC` deadline, taken as given
    /// (every `u64` is a valid instant; `u64::MAX` is one no clock reaches). `None` without
    /// `FUTEX_FLAG_TIMED`, and `args[3]` unread.
    pub(crate) deadline: Option<crate::kernel::boot::futex::FutexDeadline>,
}

/// THE NR 9 decode, shared by the broad handler and the split route: [`decode_futex_common`] with
/// NR 9's flags (`SHARED`, `TIMED`, `BITSET`), then `args[3]` only if `TIMED` is set. Nothing about
/// the deadline is refused here: whether it has been reached, and whether its clock can be read,
/// are decided inside the compare-and-register hold, after the word's comparison.
pub(crate) fn decode_futex_wait_args(frame: &TrapFrame) -> Result<FutexWaitArgs, SyscallError> {
    use crate::kernel::boot::futex::{
        FUTEX_ARG_DEADLINE, FUTEX_FLAG_TIMED, FUTEX_WAIT_FLAGS_DEFINED, FutexDeadline,
    };
    let key = decode_futex_common(frame, FUTEX_WAIT_FLAGS_DEFINED)?;
    let deadline = (frame.arg(SYSCALL_ARG_LEN) & FUTEX_FLAG_TIMED != 0)
        .then(|| FutexDeadline::MonotonicNs(frame.arg(FUTEX_ARG_DEADLINE) as u64));
    Ok(FutexWaitArgs { key, deadline })
}

/// FUTEX-ABS-BITSET1 — the NR 9 admission timing of a decoded wait.
pub(crate) fn futex_wait_timing(
    deadline: Option<crate::kernel::boot::futex::FutexDeadline>,
) -> crate::kernel::boot::futex::FutexWaitTiming {
    use crate::kernel::boot::futex::FutexWaitTiming;
    deadline.map_or(FutexWaitTiming::Untimed, FutexWaitTiming::Deadline)
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
    /// FUTEX-ABS-BITSET1 — the word held `expected`, the wait is timed, and its clock could not be
    /// read inside the compare-and-register. **Nothing was written**, the caller is still current,
    /// and the answer is `Err(Unavailable)`.
    ClockUnavailable,
    /// FUTEX-ABS-BITSET1 — a zero mask reached the owner. **Nothing was written**; `InvalidArgs`.
    InvalidMask,
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
    match kernel
        .futex_wait_current_timed(
            args.key.addr,
            args.key.value,
            args.key.kind,
            futex_wait_timing(args.deadline),
            args.key.mask,
        )
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
        crate::kernel::boot::FutexWaitBroadOutcome::ClockUnavailable => {
            return Err(SyscallError::Unavailable);
        }
        crate::kernel::boot::FutexWaitBroadOutcome::InvalidMask => {
            return Err(SyscallError::InvalidArgs);
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
        .futex_wake_masked(args.addr, args.value, args.kind, args.mask)
        .map_err(SyscallError::from)?;
    frame.set_ok(woke as usize, 0, 0);
    Ok(())
}
