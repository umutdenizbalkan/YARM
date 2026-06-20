// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Stage 150: IPC frame argument codec helpers.
//!
//! Pure encoding/decoding functions for IPC syscall ABI arguments. These
//! operate only on `TrapFrame` and `Message` values — no kernel state
//! mutation, no lock acquisition, no IPC ordering dependency.
//!
//! Mechanically extracted from `syscall.rs` with zero behavior change.
//! `syscall.rs` re-imports all items so existing call sites (split-recv
//! seam, `dispatch`, `complete_blocked_recv_for_waiter`, and `ipc.rs`) are
//! unaffected.

use super::{
    OPCODE_INLINE, SYSCALL_ARG_INLINE_PAYLOAD1, SYSCALL_ARG_TRANSFER_CAP, SYSCALL_NO_TRANSFER_CAP,
    SyscallError,
};
use crate::kernel::boot::KernelState;
use crate::kernel::capabilities::CapId;
use crate::kernel::ipc::Message;
use crate::kernel::trapframe::TrapFrame;

pub(super) fn sender_tid_to_ret(tid: u64) -> Result<usize, SyscallError> {
    usize::try_from(tid).map_err(|_| SyscallError::Internal)
}

pub(super) fn transfer_cap_arg(
    _kernel: &KernelState,
    frame: &TrapFrame,
) -> Result<Option<CapId>, SyscallError> {
    let raw = frame.arg(SYSCALL_ARG_TRANSFER_CAP) as u64;
    if raw == SYSCALL_NO_TRANSFER_CAP {
        return Ok(None);
    }
    Ok(Some(CapId(raw)))
}

pub(super) fn decode_ipc_send_timeout_ticks(frame: &TrapFrame) -> u64 {
    frame.arg(SYSCALL_ARG_INLINE_PAYLOAD1) as u64
}

pub(super) fn encode_transfer_cap_ret(
    frame: &mut TrapFrame,
    cap: Option<u64>,
) -> Result<(), SyscallError> {
    let value = cap.unwrap_or(SYSCALL_NO_TRANSFER_CAP);
    frame.set_ret2(usize::try_from(value).map_err(|_| SyscallError::Internal)?);
    Ok(())
}

#[inline]
pub(super) fn should_strip_inline_opcode_prefix(msg: &Message) -> bool {
    msg.opcode == OPCODE_INLINE
        && ((msg.flags & Message::FLAG_REPLY_CAP) != 0
            || (msg.flags & Message::FLAG_CAP_TRANSFER) != 0)
}
