// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! NR 15 `DebugLog` syscall handler.
//!
//! Stage 102: mechanically split from the parent `syscall.rs` module with zero
//! behavior change. The dispatch arm in `syscall.rs` (`Syscall::DebugLog =>
//! handle_debug_log`) is unchanged; this module only hosts the moved body.
//! See `doc/KERNEL_UNLOCKING.md` for the decomposition map.

use super::{AARCH64_SYSCALL_TRACE, SyscallError};
use crate::kernel::boot::KernelState;
use crate::kernel::ipc::Message;
use crate::kernel::trapframe::TrapFrame;

pub(super) fn handle_debug_log(kernel: &mut KernelState, frame: &mut TrapFrame) -> Result<(), SyscallError> {
    // ABI: arg0=ptr, arg1=len (no cap slot; do not use SYSCALL_ARG_PTR/LEN here).
    let a0 = frame.arg(0);
    let a1 = frame.arg(1);
    let a2 = frame.arg(2);
    let tid = kernel.current_tid().unwrap_or(0);
    syscall_trace!(
        "DEBUG_LOG_ARGS tid={} a0=0x{:x} a1=0x{:x} a2=0x{:x}",
        tid,
        a0,
        a1,
        a2
    );
    let user_ptr = a0;
    let raw_len = a1;
    let len = raw_len.min(Message::MAX_PAYLOAD);
    syscall_trace!(
        "DEBUG_LOG_ENTER tid={} ptr=0x{:x} len={}",
        tid,
        user_ptr,
        raw_len
    );
    if user_ptr == 0 || len == 0 {
        frame.set_ok(0, 0, 0);
        return Ok(());
    }
    let payload = match kernel.copy_from_current_user(user_ptr, len) {
        Ok(data) => data,
        Err(e) => {
            syscall_trace!("DEBUG_LOG_COPY_FAIL tid={} err={:?}", tid, e);
            frame.set_ok(0, 0, 0);
            return Ok(());
        }
    };
    syscall_trace!("DEBUG_LOG_COPY_OK tid={} len={}", tid, len);
    let msg_str = core::str::from_utf8(&payload[..len]).unwrap_or("<utf8_err>");
    crate::yarm_log!("USER_LOG tid={} msg={}", tid, msg_str);
    frame.set_ok(0, 0, 0);
    Ok(())
}
