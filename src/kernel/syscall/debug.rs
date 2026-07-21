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
use crate::kernel::trapframe::TrapFrame;

/// Stage 198B: maximum DebugLog message length (bytes). Wider than an IPC `Message::MAX_PAYLOAD`
/// (128) so the canonical ordinary-cap live attestations (~138 bytes) log untruncated. This bounds
/// ONLY the DebugLog copy seam; IPC message framing is unchanged. Kept in sync with the split
/// DebugLog handler (`syscall_split.rs`) and the userspace `MAX_LOG_LEN` (`yarm-user-rt`).
pub(crate) const DEBUG_LOG_MAX_BYTES: usize = 192;

pub(super) fn handle_debug_log(
    kernel: &mut KernelState,
    frame: &mut TrapFrame,
) -> Result<(), SyscallError> {
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
    // Stage 198B: DebugLog messages may be up to DEBUG_LOG_MAX_BYTES (192) — wider than an IPC
    // `Message::MAX_PAYLOAD` (128) — so the canonical ordinary-cap attestations (~138 bytes with
    // `arch=riscv64`/`aarch64`) log UNTRUNCATED. This is a copy-length cap on the DebugLog seam
    // ONLY (a dedicated stack buffer + the slice copy); it does NOT change IPC message framing or
    // the DebugLog split-dispatch / retirement mechanism.
    let len = raw_len.min(DEBUG_LOG_MAX_BYTES);
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
    let mut payload = [0u8; DEBUG_LOG_MAX_BYTES];
    if let Err(e) = kernel.copy_from_current_user_into_slice(user_ptr, len, &mut payload) {
        syscall_trace!("DEBUG_LOG_COPY_FAIL tid={} err={:?}", tid, e);
        frame.set_ok(0, 0, 0);
        return Ok(());
    }
    syscall_trace!("DEBUG_LOG_COPY_OK tid={} len={}", tid, len);
    let msg_str = core::str::from_utf8(&payload[..len]).unwrap_or("<utf8_err>");
    crate::yarm_log!("USER_LOG tid={} msg={}", tid, msg_str);
    // Stage 199A2D2C2B2: the cross-CPU request seal's terminal kernel marker is emitted ONLY after
    // the resumed CPU-1 server's userspace X86_AP_RECV_V2_CONTINUED marker is observed here (never
    // merely after enqueue/IPI) AND the kernel counters attest one complete delivery. Once, gated.
    crate::kernel::boot::maybe_emit_ipccall_direct_smp_request_ok(msg_str);
    // Stage 199A2D2C2C: the cross-CPU REPLY seal's terminal kernel marker is emitted ONLY after the
    // resumed CPU-0 client's userspace X86_BSP_REPLY_USER_VALIDATED marker is observed here AND the
    // kernel counters attest one complete reply delivery. Once, gated.
    crate::kernel::boot::maybe_emit_ipcreply_direct_smp_reply_ok(msg_str);
    frame.set_ok(0, 0, 0);
    Ok(())
}
