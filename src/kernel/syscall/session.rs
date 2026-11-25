// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! VFS-CORRECT1C — NR 21 `IpcCallerSession` (ABI v14).
//!
//! A server needs to know which PROCESS sent a request, not which thread: threads of one process
//! share its open files, and a TID is reused after its thread exits. The kernel already knows the
//! caller of every NR 6 call — the reply capability's record names it, and while that record is
//! `Available` the caller is blocked in exactly that call. This syscall reads it.
//!
//! - **op 0** (`IPC_CALLER_SESSION_OP_CALLER`): `arg1` = a reply capability in the current task's
//!   CNode. Returns `ret0` = the caller's thread-group id, `ret1` = its process incarnation
//!   (unique for the life of the kernel), `ret2` = the caller's TID. Any task holding a reply
//!   capability may ask about its own caller.
//! - **op 1** (`IPC_CALLER_SESSION_OP_ALIVE`): `arg1` = thread-group id, `arg2` = process
//!   incarnation. Returns `ret0` = the number of that process's live threads (0: gone for good)
//!   and `ret1` = 1. `SystemServer` callers only (`MissingRight` otherwise).
//!
//! On AArch64 and RISC-V an error is the code in `ret0` with `ret1 = ret2 = 0`; both ops return a
//! nonzero `ret1` on success (an incarnation is never 0), so `ret1 == 0` is the error test there.
//!
//! Policy — what a session owns, when it is reclaimed — stays with the server.

use super::{IPC_CALLER_SESSION_OP_ALIVE, IPC_CALLER_SESSION_OP_CALLER, SyscallError, current_tid};
use crate::kernel::boot::KernelState;
use crate::kernel::capabilities::CapId;
use crate::kernel::task::TaskClass;
use crate::kernel::trapframe::TrapFrame;

pub(crate) fn handle_ipc_caller_session(
    kernel: &mut KernelState,
    frame: &mut TrapFrame,
) -> Result<(), SyscallError> {
    match frame.arg(0) {
        IPC_CALLER_SESSION_OP_CALLER => {
            let reply_cap = CapId(frame.arg(1) as u64);
            let (tgid, incarnation, caller) = kernel
                .reply_cap_caller_session(reply_cap)
                .map_err(SyscallError::from)?;
            frame.set_ok(tgid as usize, incarnation as usize, caller as usize);
            Ok(())
        }
        IPC_CALLER_SESSION_OP_ALIVE => {
            let tid = current_tid(kernel)?;
            if kernel.task_class(tid) != Some(TaskClass::SystemServer) {
                return Err(SyscallError::MissingRight);
            }
            let live = kernel.session_live_threads(frame.arg(1) as u64, frame.arg(2) as u64);
            frame.set_ok(live as usize, 1, 0);
            Ok(())
        }
        _ => Err(SyscallError::InvalidArgs),
    }
}

/// The split route (every production syscall is served pre-lock): the same answers, from split
/// reads — the reply capability in the caller's CNode (capability rank 4), its record (IPC rank 3)
/// and the caller's TCB (task rank 2), each taken and released in turn. A record found invokable
/// names a caller blocked in its NR 6; the TCB read then requires that exact `{tid, asid}`.
pub(crate) fn ipc_caller_session_split(
    shared: &crate::runtime::SharedKernel,
    tid: u64,
    frame: &mut TrapFrame,
) -> Result<(), SyscallError> {
    use crate::kernel::boot::KernelError;
    match frame.arg(0) {
        IPC_CALLER_SESSION_OP_CALLER => {
            let (index, generation) = shared
                .resolve_reply_cap_split_read(tid, CapId(frame.arg(1) as u64))
                .map_err(SyscallError::from)?;
            if !shared.reply_record_externally_invokable_split_read(index, generation) {
                return Err(SyscallError::from(KernelError::InvalidCapability));
            }
            let caller = shared
                .reply_record_caller_split_read(index, generation)
                .ok_or(SyscallError::from(KernelError::InvalidCapability))?;
            let (tgid, incarnation) = shared
                .caller_session_split_read(caller)
                .ok_or(SyscallError::from(KernelError::InvalidCapability))?;
            frame.set_ok(tgid as usize, incarnation as usize, caller.tid.0 as usize);
            Ok(())
        }
        IPC_CALLER_SESSION_OP_ALIVE => {
            if shared.task_class_split_read(tid) != Some(TaskClass::SystemServer) {
                return Err(SyscallError::MissingRight);
            }
            let live =
                shared.session_live_threads_split_read(frame.arg(1) as u64, frame.arg(2) as u64);
            frame.set_ok(live as usize, 1, 0);
            Ok(())
        }
        _ => Err(SyscallError::InvalidArgs),
    }
}
