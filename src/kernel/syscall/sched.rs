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
