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

/// U9-FUTEX-WAIT-FINAL §2 — THE futex-word range policy, in one place.
///
/// `KernelState::validate_current_user_futex_word` and the off-lock split read used to carry
/// byte-identical copies of these two checks, and the copy is what let the split route erase them:
/// it answered `Option<bool>`, so `addr == 0` and a kernel-range address came back as the same
/// `None` and both were handed to the broad dispatcher to re-raise. There is one policy now, with
/// two ACQUISITION adapters around it — the broad validator and the split reader — because only
/// the reachability of the address space differs between them, never the rule.
///
/// The two answers are the canonical ones, unchanged: a null word is `WrongObject`, and an
/// address whose last byte overflows `usize` or lands at or above `KERNEL_SPACE_BASE` is
/// `UserMemoryFault`. Nothing about the caller's `expected`/`observed` comparison is decided here
/// — that is the ABI's, and it happens after the address is known readable.
pub(crate) fn futex_word_range_check(addr: usize) -> Result<(), crate::kernel::boot::KernelError> {
    use crate::kernel::boot::KernelError;
    if addr == 0 {
        return Err(KernelError::WrongObject);
    }
    let end = addr.checked_add(core::mem::size_of::<u32>() - 1);
    if end.is_none_or(|end| end as u64 >= crate::kernel::vm::KERNEL_SPACE_BASE) {
        return Err(KernelError::UserMemoryFault);
    }
    Ok(())
}

/// U9-FUTEX-WAIT-FINAL §2 — what a validated `FutexWait` decided, as a fact rather than a `bool`.
///
/// `futex_wait_current` answers `Result<bool, KernelError>` and its `bool` means "did this call
/// park the caller". Naming the two outcomes keeps the split route from having to re-derive which
/// one it is holding, and keeps the ABI honest at the one place that decides it: YARM's NR 9 takes
/// the comparison from the CALLER — `expected` and `observed` are both arguments — so the kernel
/// validates that the futex word is readable and then compares the two values it was given. It
/// reads no word to make this decision, and there is no bitset, timeout, requeue or PI here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FutexWaitDecision {
    /// `expected == observed`: the caller's view still holds, so it parks.
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
    /// PARKED. The exact incarnation is `Blocked(Futex(addr))` and current on no CPU, and the
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
    /// The rank-1 compare-and-clear found a DIFFERENT task current. The rank-2 registration is
    /// undone exactly, so nothing is left published.
    VictimChanged,
    /// The TCB is not the entering incarnation, or is gone, or is not `Running`. **Nothing was
    /// written** — this is the canonical `TaskMissing` population.
    IncarnationMoved,
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
    let addr = frame.arg(SYSCALL_ARG_CAP);
    let expected =
        u32::try_from(frame.arg(SYSCALL_ARG_PTR)).map_err(|_| SyscallError::InvalidArgs)?;
    let observed =
        u32::try_from(frame.arg(SYSCALL_ARG_LEN)).map_err(|_| SyscallError::InvalidArgs)?;
    let blocked = kernel
        .futex_wait_current(addr, expected, observed)
        .map_err(SyscallError::from)?;
    frame.set_ok(usize::from(blocked), 0, 0);
    Ok(())
}

pub(super) fn handle_futex_wake(
    kernel: &mut KernelState,
    frame: &mut TrapFrame,
) -> Result<(), SyscallError> {
    let addr = frame.arg(SYSCALL_ARG_CAP);
    let max_wake =
        u32::try_from(frame.arg(SYSCALL_ARG_PTR)).map_err(|_| SyscallError::InvalidArgs)?;
    let woke = kernel
        .futex_wake(addr, max_wake)
        .map_err(SyscallError::from)?;
    frame.set_ok(woke as usize, 0, 0);
    Ok(())
}
