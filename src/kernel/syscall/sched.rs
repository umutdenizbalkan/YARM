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

/// THE check of compare-and-park: does the word the KERNEL read still hold `expected`?
///
/// The caller's own read is not consulted — it can be stale by the time the kernel parks. The
/// park transaction applies this to its decision read and again to the read it makes after the
/// waiter is registered, both from the same mapping the key was resolved from.
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
    let args = decode_futex_args(frame)?;
    let blocked = kernel
        .futex_wait_current(args.addr, args.value, args.kind)
        .map_err(SyscallError::from)?;
    frame.set_ok(usize::from(blocked), 0, 0);
    Ok(())
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
