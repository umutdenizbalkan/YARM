// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Stage 150/151: IPC frame argument codec helpers — pure ABI/frame codec only.
//!
//! ## Module boundary invariants (audited Stage 151)
//!
//! This module is **pure IPC ABI/frame codec only**. Specifically:
//!
//! - **No kernel-state mutation.** Functions take `&KernelState` at most for
//!   ABI argument reads; no `&mut KernelState` parameter appears here.
//! - **No lock acquisition.** No rank-tagged lock (IPC, scheduler, task-state,
//!   VM, or memory) is acquired here.
//! - **No cap-slot materialization.** Cap-table grant, take, revoke, and
//!   received-message cap materialization remain in `syscall.rs` / `ipc.rs`.
//! - **No VM or shared-memory mapping.** Shared-region mapping and all VM
//!   mapping helpers remain in `syscall.rs` / `ipc.rs` / `vm.rs`.
//! - **No reply-cap lifecycle handling.** Reply-cap mint, take, and rollback
//!   remain in `syscall.rs`.
//! - **`syscall.rs` remains dispatch owner.** The dispatch function and the
//!   `Syscall` enum are defined in `syscall.rs`; this module contains no
//!   dispatch logic.
//! - **`syscall/ipc.rs` remains stateful IPC implementation owner.** The
//!   blocking-send, blocking-recv, call, reply, and waiter-delivery state
//!   machines live in `ipc.rs` and `syscall.rs`, not here.
//!
//! Mechanically extracted from `syscall.rs` with zero behavior change.
//! `syscall.rs` re-imports all items so existing call sites in the
//! split-recv seam, the dispatch path, the waiter-delivery path, and
//! `ipc.rs` are unaffected.

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
