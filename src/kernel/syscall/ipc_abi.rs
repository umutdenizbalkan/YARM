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
    SYSCALL_ARG_INLINE_PAYLOAD1, SYSCALL_ARG_TRANSFER_CAP, SYSCALL_NO_TRANSFER_CAP, SyscallError,
};
use crate::kernel::boot::KernelState;
use crate::kernel::capabilities::CapId;
use crate::kernel::ipc::Message;
use crate::kernel::trapframe::TrapFrame;

pub(super) fn sender_tid_to_ret(tid: u64) -> Result<usize, SyscallError> {
    usize::try_from(tid).map_err(|_| SyscallError::Internal)
}

/// The **one canonical presence predicate** for the transfer-capability argument.
///
/// `SYSCALL_NO_TRANSFER_CAP` (`u64::MAX`) is the only encoding that means "no capability".
/// Every other value — **including a raw `0`** — names one; `transfer_cap_arg_zero_is_not_treated_as_none`
/// pins that. Any code that needs to know whether a frame carries a transfer cap must ask
/// here rather than re-testing the sentinel, so the direct-IPC eligibility contract and the
/// legacy decode can never disagree about what "cap-bearing" means.
pub(crate) fn transfer_cap_arg_present(frame: &TrapFrame) -> bool {
    frame.arg(SYSCALL_ARG_TRANSFER_CAP) as u64 != SYSCALL_NO_TRANSFER_CAP
}

/// The same decode as [`transfer_cap_arg`], without the unused `KernelState` the broad
/// signature carries, so the pre-lock reply route can ask it too. Both go through
/// [`transfer_cap_arg_present`], so there is still exactly ONE sentinel rule.
pub(crate) fn transfer_cap_arg_value(frame: &TrapFrame) -> Option<CapId> {
    transfer_cap_arg_present(frame).then(|| CapId(frame.arg(SYSCALL_ARG_TRANSFER_CAP) as u64))
}

/// THE cap-bearing reply message framing, shared by the legacy reply handler and the direct
/// reply lane so the two cannot frame the same reply differently.
///
/// `FLAG_CAP_TRANSFER_PLAIN` (bit 2), never `FLAG_CAP_TRANSFER` (bit 0): the latter makes the
/// receiver strip a two-byte inline opcode prefix that the ipc_send/ipc_call protocol prepends
/// and a reply never does, which would silently truncate the caller's payload.
pub(crate) fn frame_reply_message_with_cap(
    sender_tid: u64,
    payload: &[u8],
    transfer_handle: u64,
) -> Result<Message, SyscallError> {
    Message::with_header(
        sender_tid,
        super::OPCODE_INLINE,
        Message::FLAG_CAP_TRANSFER_PLAIN,
        Some(transfer_handle),
        payload,
    )
    .map_err(|_| SyscallError::InvalidArgs)
}

pub(super) fn transfer_cap_arg(
    _kernel: &KernelState,
    frame: &TrapFrame,
) -> Result<Option<CapId>, SyscallError> {
    if !transfer_cap_arg_present(frame) {
        return Ok(None);
    }
    Ok(Some(CapId(frame.arg(SYSCALL_ARG_TRANSFER_CAP) as u64)))
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

/// True when the sender framed an inline opcode prefix on this message.
///
/// Delegates to the single canonical rule in
/// [`super::ipc_recv_core::should_strip_inline_opcode_prefix_parts`] so the header
/// predicate and the payload projection can never drift apart.
///
/// Retained as the message-level spelling of the rule (its module placement and visibility
/// are pinned by the Stage 154 boundary guards) and exercised by the unit tests; every
/// delivery path now reaches the rule through the projection instead of calling it directly.
#[cfg_attr(not(test), allow(dead_code))]
#[inline]
pub(super) fn should_strip_inline_opcode_prefix(msg: &Message) -> bool {
    super::ipc_recv_core::should_strip_inline_opcode_prefix_parts(msg.opcode, msg.flags)
}
