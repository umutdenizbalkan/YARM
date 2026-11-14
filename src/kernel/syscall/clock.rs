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
//! * **NR 20 `ClockNanosleep(clock_id, flags, req_ptr, rem_ptr)`** (checkpoint C, SLEEP1) — blocks the
//!   caller until `CLOCK_MONOTONIC` reaches the deadline the [`Timespec`] at `req_ptr` describes:
//!   relative by default, absolute with [`TIMER_ABSTIME`]; `CLOCK_MONOTONIC` or an initialized
//!   `CLOCK_REALTIME` (see `timekeeping::sleep_deadline_at`). Returns `0` when the deadline has
//!   been reached, never before. Errors, in check order: `InvalidArgs` (unknown clock, undefined
//!   flag, null or misaligned request pointer, misaligned remaining pointer), `PageFault` (the
//!   request is not readable), `InvalidArgs` (an unnormalized or negative request), `Unavailable`
//!   (realtime not initialized, or no monotonic source).
//!   `rem_ptr` (`0`, or an 8-aligned `Timespec` pointer) is the POSIX remaining-time output,
//!   which is written only when a sleep is INTERRUPTED. This kernel has no signals, so nothing
//!   interrupts a sleep — it ends only at its deadline or with its task — and `rem_ptr` is never
//!   written; it is accepted (and its alignment checked) so a future signal contract can fill it
//!   without an ABI change. There is no `EINTR`.
//!
//! NR 18 / 19 are non-switching and born split: the split route (`syscall_split`) and the broad
//! dispatcher answer with the SAME decode, the same order of checks and the same errors. NR 20 is
//! a SWITCHING class: it parks through the FutexWait park transaction on a kernel-private
//! `FutexKey::Sleep` key (`boot::futex`), on both routes, with the same front checks here.

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

/// NR 20 flag: the request is an ABSOLUTE time on `clock_id` (POSIX `TIMER_ABSTIME`).
pub const TIMER_ABSTIME: usize = 1;

/// The decoded `ClockNanosleep` arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NanosleepRequest {
    pub(crate) clock: ClockId,
    pub(crate) absolute: bool,
    pub(crate) ptr: VirtAddr,
    /// The remaining-time output (`0` = none). Never written: see the module documentation.
    pub(crate) rem: usize,
}

/// THE NR 20 decode, shared by both routes, in check order: clock, flags, request pointer,
/// remaining pointer.
pub(crate) fn decode_clock_nanosleep(frame: &TrapFrame) -> Result<NanosleepRequest, SyscallError> {
    let clock = ClockId::from_raw(frame.arg(0)).ok_or(SyscallError::InvalidArgs)?;
    let flags = frame.arg(1);
    if flags & !TIMER_ABSTIME != 0 {
        return Err(SyscallError::InvalidArgs);
    }
    let ptr = check_timespec_ptr(frame.arg(2))?;
    let rem = frame.arg(3);
    if rem != 0 {
        check_timespec_ptr(rem)?;
    }
    Ok(NanosleepRequest {
        clock,
        absolute: flags & TIMER_ABSTIME != 0,
        ptr,
        rem,
    })
}

/// THE NR 20 deadline, shared by both routes: the request's bytes (already copied in) to the
/// absolute `CLOCK_MONOTONIC` deadline in nanoseconds, from one clock read now.
pub(crate) fn nanosleep_deadline(req: NanosleepRequest, raw: &[u8]) -> Result<u64, SyscallError> {
    let mut b = [0u8; TIMESPEC_SIZE];
    b.copy_from_slice(raw.get(..TIMESPEC_SIZE).ok_or(SyscallError::InvalidArgs)?);
    let ts = Timespec::from_bytes(&b);
    if !ts.is_normalized() || ts.sec < 0 {
        return Err(SyscallError::InvalidArgs);
    }
    let mono = tk::monotonic_ns().map_err(SyscallError::from)?;
    tk::sleep_deadline_at(req.clock, req.absolute, ts, mono).map_err(SyscallError::from)
}

/// Broad dispatcher: NR 20. Parks through the broad FutexWait transaction on the caller's
/// `FutexKey::Sleep` key. A deadline already reached returns `0` without blocking; a sleep that
/// blocked carries the fail-closed provisional (`Internal`) in its frame until its deadline claim
/// parks the real result (`BlockedSyscallClass::Sleep`, `0`) for the resume boundary.
pub(crate) fn handle_clock_nanosleep(
    kernel: &mut KernelState,
    frame: &mut TrapFrame,
) -> Result<(), SyscallError> {
    let req = decode_clock_nanosleep(frame)?;
    let tid = kernel.current_tid().ok_or(SyscallError::from(
        crate::kernel::boot::KernelError::TaskMissing,
    ))?;
    let asid = kernel.task_asid(tid).ok_or(SyscallError::from(
        crate::kernel::boot::KernelError::UserMemoryFault,
    ))?;
    let raw = kernel
        .copy_from_user(asid, req.ptr, TIMESPEC_SIZE)
        .map_err(SyscallError::from)?;
    let deadline = nanosleep_deadline(req, &raw)?;
    match kernel
        .futex_wait_current_timed(
            0,
            0,
            crate::kernel::boot::futex::FutexKind::Sleep,
            Some(deadline),
        )
        .map_err(SyscallError::from)?
    {
        crate::kernel::boot::FutexWaitBroadOutcome::Blocked { .. } => {
            frame.set_err(SyscallError::Internal.code())
        }
        // Nothing to wait for: the deadline had been reached at registration (a sleep has no
        // word, so `NotBlocked` cannot arise; it would mean the same).
        crate::kernel::boot::FutexWaitBroadOutcome::TimedOut
        | crate::kernel::boot::FutexWaitBroadOutcome::NotBlocked => frame.set_ok(0, 0, 0),
    }
    Ok(())
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
