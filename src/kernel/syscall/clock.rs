// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! SCHED-TIME-FOUNDATION1 B (TIMEKEEP1) — the clock syscalls (ABI v12).
//!
//! * **NR 18 `ClockGetTime(clock_id, ts_ptr)`** — writes one coherent [`Timespec`] (16 bytes,
//!   8-aligned) for `CLOCK_REALTIME` (0) or `CLOCK_MONOTONIC` (1) to `ts_ptr` in the caller's
//!   address space; returns `0`. Errors: `InvalidArgs` (unknown clock, null or misaligned
//!   pointer), `PageFault` (any of the 16 bytes not a writable user mapping; nothing written),
//!   `Unavailable` (realtime not initialized, or no monotonic source).
//! * **NR 19 `ClockSetRealtime(ts_ptr)`** — the authorized control-plane initialization of
//!   `CLOCK_REALTIME` from the [`Timespec`] at `ts_ptr` (UTC since the Unix epoch). Policy: a
//!   `SystemServer` caller only, else `MissingRight` with nothing changed. `InvalidArgs` for an
//!   unnormalized or pre-epoch value. Returns `0`.
//!
//! Both are non-switching and born split: the split route (`syscall_split`) and the broad
//! dispatcher answer with the SAME decode, the same order of checks and the same errors.

use super::SyscallError;
use crate::kernel::boot::KernelState;
use crate::kernel::timekeeping::{
    self as tk, ClockError, ClockId, TIMESPEC_ALIGN, TIMESPEC_SIZE, Timespec,
};
use crate::kernel::trapframe::TrapFrame;
use crate::kernel::vm::VirtAddr;

impl From<ClockError> for SyscallError {
    fn from(e: ClockError) -> Self {
        match e {
            ClockError::InvalidClock => SyscallError::InvalidArgs,
            ClockError::Unavailable => SyscallError::Unavailable,
            ClockError::Overflow => SyscallError::InvalidArgs,
            ClockError::Busy => SyscallError::WouldBlock,
        }
    }
}

/// A user timespec pointer: nonzero, 8-aligned, the 16 bytes inside the address range (no wrap).
pub(crate) fn check_timespec_ptr(ptr: usize) -> Result<VirtAddr, SyscallError> {
    if ptr == 0 || !ptr.is_multiple_of(TIMESPEC_ALIGN) || ptr.checked_add(TIMESPEC_SIZE).is_none() {
        return Err(SyscallError::InvalidArgs);
    }
    Ok(VirtAddr(ptr as u64))
}

/// The decoded `ClockGetTime` arguments, in the canonical check order: clock, then pointer.
pub(crate) fn decode_clock_gettime(frame: &TrapFrame) -> Result<(ClockId, VirtAddr), SyscallError> {
    let clock = ClockId::from_raw(frame.arg(0)).ok_or(SyscallError::InvalidArgs)?;
    let ptr = check_timespec_ptr(frame.arg(1))?;
    Ok((clock, ptr))
}

/// The value `ClockGetTime` writes (one read of the clock).
pub(crate) fn clock_gettime_value(clock: ClockId) -> Result<Timespec, SyscallError> {
    tk::clock_gettime(clock).map_err(SyscallError::from)
}

/// Validate a realtime value read from the caller.
pub(crate) fn check_realtime_value(ts: Timespec) -> Result<Timespec, SyscallError> {
    if !ts.is_normalized() || ts.sec < 0 {
        return Err(SyscallError::InvalidArgs);
    }
    Ok(ts)
}

pub(crate) fn apply_clock_ok(frame: &mut TrapFrame) {
    frame.set_ok(0, 0, 0);
}

/// Broad dispatcher: NR 18.
pub(crate) fn handle_clock_gettime(
    kernel: &mut KernelState,
    frame: &mut TrapFrame,
) -> Result<(), SyscallError> {
    let (clock, ptr) = decode_clock_gettime(frame)?;
    let tid = kernel.current_tid().ok_or(SyscallError::from(
        crate::kernel::boot::KernelError::TaskMissing,
    ))?;
    let asid = kernel.task_asid(tid).ok_or(SyscallError::from(
        crate::kernel::boot::KernelError::UserMemoryFault,
    ))?;
    let ts = clock_gettime_value(clock)?;
    kernel
        .copy_to_user_all_or_nothing(asid, ptr, &ts.to_bytes())
        .map_err(SyscallError::from)?;
    apply_clock_ok(frame);
    Ok(())
}

/// Broad dispatcher: NR 19.
pub(crate) fn handle_clock_set_realtime(
    kernel: &mut KernelState,
    frame: &mut TrapFrame,
) -> Result<(), SyscallError> {
    let ptr = check_timespec_ptr(frame.arg(0))?;
    let tid = kernel.current_tid().ok_or(SyscallError::from(
        crate::kernel::boot::KernelError::TaskMissing,
    ))?;
    if kernel.task_class(tid) != Some(crate::kernel::task::TaskClass::SystemServer) {
        return Err(SyscallError::MissingRight);
    }
    let asid = kernel.task_asid(tid).ok_or(SyscallError::from(
        crate::kernel::boot::KernelError::UserMemoryFault,
    ))?;
    let raw = kernel
        .copy_from_user(asid, ptr, TIMESPEC_SIZE)
        .map_err(SyscallError::from)?;
    let mut b = [0u8; TIMESPEC_SIZE];
    b.copy_from_slice(&raw[..TIMESPEC_SIZE]);
    let ts = check_realtime_value(Timespec::from_bytes(&b))?;
    tk::set_realtime(ts).map_err(SyscallError::from)?;
    crate::yarm_log!(
        "TIMEKEEP_REALTIME_SET tid={} sec={} nsec={} broad_lock=1",
        tid,
        ts.sec,
        ts.nsec
    );
    apply_clock_ok(frame);
    Ok(())
}

/// Split route: NR 18, off the broad lock. Same decode, same errors; the clock read takes no
/// lock, the copy-out takes only the caller's VM / memory seams (rank 5 / 6).
pub(crate) fn clock_gettime_split(
    shared: &crate::runtime::SharedKernel,
    tid: u64,
    frame: &mut TrapFrame,
) -> Result<(), SyscallError> {
    let (clock, ptr) = decode_clock_gettime(frame)?;
    let asid = shared.task_asid_for_tid_split_read(tid);
    if asid == 0 {
        return Err(SyscallError::from(
            crate::kernel::boot::KernelError::UserMemoryFault,
        ));
    }
    let ts = clock_gettime_value(clock)?;
    shared
        .copy_to_user_split_all_or_nothing(
            crate::kernel::vm::Asid(asid as u16),
            ptr,
            &ts.to_bytes(),
        )
        .map_err(SyscallError::from)?;
    apply_clock_ok(frame);
    Ok(())
}

/// Split route: NR 19, off the broad lock.
pub(crate) fn clock_set_realtime_split(
    shared: &crate::runtime::SharedKernel,
    tid: u64,
    frame: &mut TrapFrame,
) -> Result<(), SyscallError> {
    let ptr = check_timespec_ptr(frame.arg(0))?;
    if shared.task_class_split_read(tid) != Some(crate::kernel::task::TaskClass::SystemServer) {
        return Err(SyscallError::MissingRight);
    }
    let asid = shared.task_asid_for_tid_split_read(tid);
    if asid == 0 {
        return Err(SyscallError::from(
            crate::kernel::boot::KernelError::UserMemoryFault,
        ));
    }
    let raw = shared
        .copy_from_user_split(crate::kernel::vm::Asid(asid as u16), ptr, TIMESPEC_SIZE)
        .map_err(SyscallError::from)?;
    let mut b = [0u8; TIMESPEC_SIZE];
    b.copy_from_slice(&raw[..TIMESPEC_SIZE]);
    let ts = check_realtime_value(Timespec::from_bytes(&b))?;
    tk::set_realtime(ts).map_err(SyscallError::from)?;
    crate::yarm_log!(
        "TIMEKEEP_REALTIME_SET tid={} sec={} nsec={} broad_lock=0",
        tid,
        ts.sec,
        ts.nsec
    );
    apply_clock_ok(frame);
    Ok(())
}
