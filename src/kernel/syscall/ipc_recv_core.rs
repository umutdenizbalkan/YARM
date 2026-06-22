// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Stage 154: D1/D5 IPC/cap receive-core cap-boundary landing zone.
//!
//! This module is the dedicated landing zone for the IPC/cap receive-delivery
//! cluster that Stage 153 proved is pinned in `syscall.rs` by load-bearing
//! ordering constraints. Stage 154 begins the migration with the **only**
//! genuinely pure fragment of that cluster: the recv-v2 metadata frame encoder.
//!
//! ## What lives here (Stage 154)
//!
//! - [`encode_recv_v2_meta`] — a **pure** function that serializes the 40-byte
//!   recv-v2 metadata frame. No kernel state, no lock acquisition, no
//!   capability-slot mutation, no reply-cap lifecycle, no user-memory copy, no
//!   VM mutation. It is a byte codec only.
//!
//! ## What does NOT live here yet (still pinned in `syscall.rs`)
//!
//! The stateful cap/materialization seams remain in `syscall.rs` and are NOT
//! moved by Stage 154, because re-homing them requires QEMU smoke proof of
//! byte-identical delivery (recv-v2 / reply-cap / split-recv markers), which is
//! a precondition recorded in doc/KERNEL_UNLOCKING.md §5.1.1 / §5.1.2:
//!
//! - `complete_blocked_recv_for_waiter` (blocked-waiter delivery),
//! - `try_split_recv_queued_plain_with_snapshot_locked` (live queued split),
//! - `materialize_received_message_cap` / `_routed` / `materialize_received_transfer_cap`
//!   (cap-slot mint/grant + reply-cap one-shot record/rollback),
//! - `try_endpoint_split_recv`, `clear_blocked_recv_state`,
//! - the `IPC_RECV_META_V2_ENCODED_LEN` constant (single definition in
//!   `syscall.rs`; this module only *references* it via `super::`).
//!
//! ## Ordering invariants this module must never break (Stage 153 proof)
//!
//! When the stateful seams eventually migrate here, the following orderings are
//! load-bearing and proven distinct between the two delivery paths. The pure
//! encoder below participates in both *only* as the meta-serialization step; it
//! must stay free of any side effect so it can be called at the exact point each
//! path requires without perturbing ordering.
//!
//! ### Lock ordering (doc/KERNEL_LOCKING.md §4)
//! scheduler (2) → task (3) → ipc (4) → capability (5) → vm (6). The IPC lock is
//! always released before the capability lock is taken for materialization, and
//! before any user-memory copy.
//!
//! ### Receiver-local cap-slot materialization
//! Caps are minted/granted into the *receiver's* cnode under the capability lock
//! only after the IPC envelope is taken under the IPC lock.
//!
//! ### Reply-cap one-shot lifecycle
//! Reply caps are minted directly (bypassing the delegation-link table) and the
//! minted `CapId` is recorded via `set_reply_cap_waiter_cap`; `ipc_reply` later
//! fast-revokes exactly that slot. Mint-then-record is atomic w.r.t. exposing
//! the cap to the receiver.
//!
//! ### Transfer-cap grant semantics
//! `grant_task_to_task_with_rights` derives the receiver-local cap with the
//! source capability's rights (attenuation order preserved).
//!
//! ### Blocked-waiter path: copy-BEFORE-materialize
//! take blocked-recv state (task) → resolve recv cap (capability) → **copy
//! payload to user (vm)** → materialize cap/reply (capability) → encode meta
//! (this module, pure) → **copy meta to user (vm)** → on meta-copy fault **roll
//! back** the freshly-minted cap → zero return GPRs (task) → clear blocked state.
//!
//! ### Queued-split path: materialize-BEFORE-copy
//! dequeue under ipc (released inside recv_core) → **materialize cap/reply
//! first** (capability) → apply sender wake (scheduler) → **user writeback (vm)**
//! → roll back cap on writeback fault → TrapFrame return-lane writeback.
//!
//! ### Rollback rules
//! Any failure after a cap has been materialized but before delivery completes
//! must roll back the freshly-minted cap (and, for reply caps, the dangling
//! waiter-cap record) to avoid a cnode-slot / refcount leak.
//!
//! ## Non-ownership
//!
//! This module never owns dispatch: `syscall.rs` decodes syscall numbers and
//! owns `pub fn dispatch`. Nothing here may decode a syscall number or introduce
//! a second dispatch layer.

use super::IPC_RECV_META_V2_ENCODED_LEN;
// Stage 158: imports for the re-homed cap-materialization cluster.
use super::SyscallError;
use crate::kernel::boot::KernelState;
use crate::kernel::capabilities::{CapObject, CapRights, Capability};
use crate::kernel::ipc::Message;

/// Serialize the 40-byte recv-v2 metadata frame.
///
/// **Pure byte codec.** This is the single implementation of the frozen recv-v2
/// metadata frame layout. Stage 155 converged all three production encoders
/// onto it: the blocked-waiter path (`complete_blocked_recv_for_waiter` in
/// `syscall.rs`), the immediate full-recv path (`syscall/ipc.rs`), and the
/// queued user-ASID split path (`kernel/recv_core.rs`).
///
/// The **field offsets** are identical across all paths and frozen by the ABI.
/// Two field *values* historically differed per delivery path, so they are
/// explicit parameters rather than hardcoded — this keeps every converged call
/// site **byte-for-byte identical** to its previous inline encoding:
///   * `status` (`[0..8]`): blocked-waiter writes `0`; the immediate and queued
///     paths write the sender/status word.
///   * `msg_flags` (`[10..12]`): blocked-waiter writes `0`; the immediate and
///     queued paths write `msg.flags`.
///
/// | offset | bytes | field                         | param            |
/// |--------|-------|-------------------------------|------------------|
/// | 0..8   | u64   | status / sender word          | `status`         |
/// | 8..10  | u16   | application opcode            | `opcode`         |
/// | 10..12 | u16   | message flags word            | `msg_flags`      |
/// | 12..16 | u32   | application payload length    | `payload_len`    |
/// | 16..24 | u64   | receiver-local cap id         | `cap_id`         |
/// | 24..32 | u64   | recv-meta flags               | `recv_meta_flags`|
/// | 32..40 | u64   | sender tid                    | `sender_tid`     |
///
/// No kernel state, no locks, no cap mutation, no user copy, no VM mutation.
///
/// `pub(crate)` (not `pub(super)`): `kernel/recv_core.rs` lives outside the
/// `syscall` module subtree and is a genuine cross-module caller (Stage 155
/// convergence). Never widen to bare `pub`.
pub(crate) fn encode_recv_v2_meta(
    status: u64,
    opcode: u16,
    msg_flags: u16,
    payload_len: u32,
    cap_id: u64,
    recv_meta_flags: u64,
    sender_tid: u64,
) -> [u8; IPC_RECV_META_V2_ENCODED_LEN] {
    let mut meta = [0u8; IPC_RECV_META_V2_ENCODED_LEN];
    meta[0..8].copy_from_slice(&status.to_le_bytes());
    meta[8..10].copy_from_slice(&opcode.to_le_bytes());
    meta[10..12].copy_from_slice(&msg_flags.to_le_bytes());
    meta[12..16].copy_from_slice(&payload_len.to_le_bytes());
    meta[16..24].copy_from_slice(&cap_id.to_le_bytes());
    meta[24..32].copy_from_slice(&recv_meta_flags.to_le_bytes());
    meta[32..40].copy_from_slice(&sender_tid.to_le_bytes());
    meta
}

// ─────────────────────────────────────────────────────────────────────────────
// Stage 158: stateful cap-materialization re-home (QEMU-validated).
//
// The recv-side cap-materialization cluster — the D1/D5 split router and its
// two canonical helpers — moved here from `syscall.rs` after the Stage 157
// oracle proved the live D1/D5 materialization markers fire on real boots
// (x86_64 extended + AArch64 manual). syscall.rs re-exports the two entry
// points so all existing call sites and sibling `super::` imports resolve
// unchanged; behaviour and log markers are byte-identical to the pre-move code.
//
// SCOPE PIN: this re-home is deliberately limited to the three functions below.
// The queued-split delivery cluster (complete_blocked_recv_for_waiter,
// try_endpoint_split_recv, try_split_recv_queued_plain_*_locked,
// clear_blocked_recv_state, and queued-split writeback) MUST stay in syscall.rs:
// the AArch64 manual oracle did NOT exercise IPC_RECV_V2_META_QUEUED_SPLIT_OK,
// so queued-split code has no cross-arch byte-identical proof and must not move.
// ─────────────────────────────────────────────────────────────────────────────

fn materialize_received_transfer_cap(
    kernel: &mut KernelState,
    transfer_handle: Option<u64>,
    endpoint: CapObject,
    receiver_tid: u64,
) -> Result<Option<u64>, SyscallError> {
    let Some(handle) = transfer_handle else {
        return Ok(None);
    };
    let envelope = kernel
        .take_transfer_envelope(handle, endpoint, crate::kernel::ipc::ThreadId(receiver_tid))
        .ok_or(SyscallError::InvalidCapability)?;
    let source_capability = kernel
        .resolve_capability_for_task(envelope.source_tid.0, envelope.source_cap)
        .map_err(SyscallError::from)?;
    let derived = kernel
        .capability_service_mut()
        .grant_task_to_task_with_rights(
            envelope.source_tid.0,
            envelope.source_cap,
            receiver_tid,
            source_capability.rights(),
        )
        .map_err(SyscallError::from)?;
    // Stage 156 IPC oracle: transfer-cap grant materialized into the receiver.
    crate::yarm_log!(
        "IPC_TRANSFER_CAP_MATERIALIZE_OK receiver_tid={} local_cap={}",
        receiver_tid,
        derived.0
    );
    Ok(Some(derived.0))
}

pub(crate) fn materialize_received_message_cap(
    kernel: &mut KernelState,
    endpoint: CapObject,
    receiver_tid: u64,
    _sender_tid: u64,
    msg: &Message,
) -> Result<Option<u64>, SyscallError> {
    let raw = msg.transferred_cap().map(|c| c.0);
    let (kind, value) = if (msg.flags & Message::FLAG_REPLY_CAP) != 0 {
        ("reply", raw)
    } else if (msg.flags & (Message::FLAG_CAP_TRANSFER | Message::FLAG_CAP_TRANSFER_PLAIN)) != 0 {
        ("transfer", raw)
    } else {
        ("none", None)
    };
    let Some(raw_value) = value else {
        return Ok(None);
    };

    if kind == "reply" {
        // ── Direct-mint path for Reply caps ───────────────────────────────────────
        // Reply caps are one-shot and non-delegatable.  We intentionally bypass
        // `grant_task_to_task_with_rights` (which would call `record_delegated_capability_link`)
        // and instead:
        //   1. Take the transfer envelope to recover the underlying Reply object.
        //   2. Verify the Reply cap is still live in the global registry.
        //   3. Mint the Reply object directly into the receiver's cnode.
        //   4. Record the resulting CapId in the global ReplyCapRecord so that
        //      `ipc_reply` can later fast-revoke the exact slot.
        //
        // This prevents delegation-link table saturation (MAX_DELEGATED_CAPABILITY_LINKS
        // entries would fill after ~1012 PM→VFS cycles on AArch64 freestanding, causing
        // `CapabilityFull` in `record_delegated_capability_link`, which left an already-
        // minted cap leaked in the receiver's cnode on every subsequent cycle, eventually
        // exhausting the 512-slot freestanding cnode).
        let envelope = match kernel.take_transfer_envelope(
            raw_value,
            endpoint,
            crate::kernel::ipc::ThreadId(receiver_tid),
        ) {
            Some(e) => e,
            None => {
                crate::yarm_log!(
                    "IPC_RECV_CAP_MATERIALIZE_FAILED kind=reply raw={} err=no_envelope",
                    raw_value
                );
                return Err(SyscallError::InvalidCapability);
            }
        };
        let (reply_index, reply_generation) = match envelope.source_object {
            CapObject::Reply { index, generation } => (index, generation),
            _ => {
                crate::yarm_log!(
                    "IPC_RECV_CAP_MATERIALIZE_FAILED kind=reply raw={} err=source_not_reply_object",
                    raw_value
                );
                return Err(SyscallError::WrongObject);
            }
        };
        let reply_object = CapObject::Reply {
            index: reply_index,
            generation: reply_generation,
        };
        crate::yarm_log!(
            "IPC_RECV_REPLY_CAP_MATERIALIZE_BEGIN waiter_tid={} raw={} reply_index={} reply_generation={}",
            receiver_tid,
            raw_value,
            reply_index,
            reply_generation
        );
        if kernel.capability_object_live(reply_object).is_none() {
            crate::yarm_log!(
                "IPC_RECV_CAP_MATERIALIZE_FAILED kind=reply raw={} err=reply_object_not_live",
                raw_value
            );
            return Err(SyscallError::InvalidCapability);
        }
        let dest_cnode = match kernel.task_cnode(receiver_tid) {
            Some(cnode) => cnode,
            None => {
                crate::yarm_log!(
                    "IPC_RECV_CAP_MATERIALIZE_FAILED kind=reply raw={} err=no_receiver_cnode",
                    raw_value
                );
                return Err(SyscallError::InvalidCapability);
            }
        };
        let minted = match kernel
            .mint_capability_in_cnode(dest_cnode, Capability::new(reply_object, CapRights::SEND))
        {
            Ok(cap) => cap,
            Err(err) => {
                let (cnode_used, cnode_capacity) = kernel
                    .cnode_slot_capacity(dest_cnode)
                    .map(|cap| {
                        let used = kernel.cnode_occupied_slots(dest_cnode).unwrap_or(0);
                        (used, cap)
                    })
                    .unwrap_or((0, 0));
                crate::yarm_log!(
                    "IPC_RECV_REPLY_CAP_MATERIALIZE_FAIL waiter_tid={} reason={:?} cnode_used={} cnode_capacity={}",
                    receiver_tid,
                    err,
                    cnode_used,
                    cnode_capacity
                );
                crate::yarm_log!(
                    "IPC_RECV_CAP_MATERIALIZE_FAILED kind=reply raw={} err={:?}",
                    raw_value,
                    err
                );
                return Err(SyscallError::from(err));
            }
        };
        // Record the materialized CapId in the global ReplyCapRecord so that
        // ipc_reply can fast-revoke the exact slot using a kernel-controlled value.
        kernel.set_reply_cap_waiter_cap(reply_index, reply_generation, minted);
        crate::yarm_log!(
            "IPC_RECV_REPLY_CAP_MATERIALIZE_OK waiter_tid={} local_reply_cap={}",
            receiver_tid,
            minted.0
        );
        // Stage 156 IPC oracle: reply-cap one-shot minted + recorded for the
        // exact-slot fast-revoke that ipc_reply performs on consumption.
        crate::yarm_log!(
            "IPC_REPLY_CAP_ONESHOT_OK waiter_tid={} local_reply_cap={}",
            receiver_tid,
            minted.0
        );
        return Ok(Some(minted.0));
    }

    // ── Transfer-cap path (FLAG_CAP_TRANSFER) ────────────────────────────────
    match materialize_received_transfer_cap(kernel, Some(raw_value), endpoint, receiver_tid) {
        Ok(local_cap) => Ok(local_cap),
        Err(first_err) => {
            crate::yarm_log!(
                "IPC_RECV_CAP_MATERIALIZE_FAILED kind={} raw={} err={:?}",
                kind,
                raw_value,
                first_err
            );
            Err(first_err)
        }
    }
}

/// Stage 104 / D1: route recv-side cap materialization through the
/// phase-separated split engine for the supported case; keep everything else
/// on the canonical global-lock path.
///
/// VALIDATION: D1_LIVE_SPLIT — live-wired at two delivery sites:
///   1. `complete_blocked_recv_for_waiter` (recv-v2 blocked-receiver delivery,
///      Stage 4K/4O seam),
///   2. `try_split_recv_queued_plain_with_snapshot_locked` (queued split-recv,
///      Stage 36/37/42+43 seam).
/// VALIDATION: FALLBACK_GLOBAL_LOCK — `FLAG_REPLY_CAP` (D5 deferred),
///   shared-region transfers (`OPCODE_SHARED_MEM`), and every
///   `FallbackRequired` outcome continue through
///   `materialize_received_message_cap` unchanged. The legacy full recv path
///   (`handle_ipc_recv_result_with_empty_error`) and the NR 30 RecvSharedV3
///   handler intentionally keep calling the canonical helper directly.
///
/// Supported case (increments `d1_split_materializations` telemetry):
/// `FLAG_CAP_TRANSFER` / `FLAG_CAP_TRANSFER_PLAIN`, non-reply, with
/// `msg.opcode != OPCODE_SHARED_MEM`. Phase A (IPC rank 3 envelope take +
/// capability rank 4 rights read) and Phase B (capability rank 4
/// `grant_task_to_task_with_rights`) run through
/// `cap_transfer_split::materialize_split_transfer_cap_equivalent`, which is
/// equivalence-tested against the canonical transfer arm (byte-equal CapId,
/// slot object, slot rights — `stage103_equivalence_split_matches_direct_take_plus_grant`).
///
/// Failure logging is byte-identical to the canonical transfer arm
/// (`IPC_RECV_CAP_MATERIALIZE_FAILED kind=transfer raw=.. err=..`) so smoke
/// log contracts are unchanged. Success additionally emits the new
/// `YARM_D1_SPLIT_MATERIALIZE` marker (additive; no script greps it as
/// forbidden).
pub(crate) fn materialize_received_message_cap_routed(
    kernel: &mut KernelState,
    endpoint: CapObject,
    receiver_tid: u64,
    sender_tid: u64,
    msg: &Message,
) -> Result<Option<u64>, SyscallError> {
    use crate::kernel::cap_transfer_split::{
        CapTransferSplitResult, materialize_split_reply_cap_equivalent,
        materialize_split_transfer_cap_equivalent,
    };
    // D1 supported scope (Pass 1 / Stage 104): non-shared-region only.
    // Shared-region transfers carry receiver-side mapping obligations outside
    // the materialize step; they keep the canonical path per the Stage 103
    // audit (doc/KERNEL_UNLOCKING.md).
    //
    // D5 supported scope (Pass 2 / Stage 105): FLAG_REPLY_CAP, non-shared-region
    // only. Phase B' uses try_set_reply_cap_waiter_cap with mint rollback on
    // the stale race window. See doc/KERNEL_UNLOCKING.md
    if msg.opcode != super::OPCODE_SHARED_MEM {
        // ── D1 transfer-cap arm ──────────────────────────────────────────────
        match materialize_split_transfer_cap_equivalent(kernel, endpoint, receiver_tid, msg) {
            CapTransferSplitResult::None => {} // not a transfer-cap; try reply arm below
            CapTransferSplitResult::Materialized(local_cap) => {
                kernel.note_d1_split_materialize();
                crate::yarm_log!(
                    "YARM_D1_SPLIT_MATERIALIZE kind=transfer receiver_tid={} local_cap={}",
                    receiver_tid,
                    local_cap
                );
                // Stage 157 IPC oracle: transfer-cap materialized on the LIVE D1
                // split path (the path real boots actually take). Same marker as
                // the canonical arm so the oracle is path-agnostic.
                crate::yarm_log!(
                    "IPC_TRANSFER_CAP_MATERIALIZE_OK receiver_tid={} local_cap={}",
                    receiver_tid,
                    local_cap
                );
                return Ok(Some(local_cap));
            }
            CapTransferSplitResult::Failed(err) => {
                // Byte-identical failure marker to the canonical transfer arm.
                crate::yarm_log!(
                    "IPC_RECV_CAP_MATERIALIZE_FAILED kind=transfer raw={} err={:?}",
                    msg.transferred_cap()
                        .map(|c| c.0)
                        .unwrap_or(super::SYSCALL_NO_TRANSFER_CAP),
                    err
                );
                return Err(err);
            }
            CapTransferSplitResult::FallbackRequired => {
                // Reserved for future fallback subcases that the transfer arm
                // cannot service; nothing produces this today, but keep the
                // arm wired so it falls through to the canonical helper.
            }
        }
        // ── D5 reply-cap arm ─────────────────────────────────────────────────
        match materialize_split_reply_cap_equivalent(kernel, endpoint, receiver_tid, msg) {
            CapTransferSplitResult::None => {} // not a reply-cap; fall to canonical
            CapTransferSplitResult::Materialized(local_cap) => {
                kernel.note_d5_split_reply_materialize();
                crate::yarm_log!(
                    "YARM_D5_SPLIT_MATERIALIZE kind=reply receiver_tid={} local_cap={}",
                    receiver_tid,
                    local_cap
                );
                // Stage 157 IPC oracle: reply-cap one-shot materialized on the
                // LIVE D5 split path (the path real boots actually take). Same
                // marker as the canonical arm so the oracle is path-agnostic.
                crate::yarm_log!(
                    "IPC_REPLY_CAP_ONESHOT_OK receiver_tid={} local_reply_cap={}",
                    receiver_tid,
                    local_cap
                );
                return Ok(Some(local_cap));
            }
            CapTransferSplitResult::Failed(err) => {
                // Byte-identical failure marker to the canonical reply arm.
                crate::yarm_log!(
                    "IPC_RECV_CAP_MATERIALIZE_FAILED kind=reply raw={} err={:?}",
                    msg.transferred_cap()
                        .map(|c| c.0)
                        .unwrap_or(super::SYSCALL_NO_TRANSFER_CAP),
                    err
                );
                return Err(err);
            }
            CapTransferSplitResult::FallbackRequired => {
                // Reserved for future reply-cap subcases the split engine
                // cannot service; nothing produces this today.
            }
        }
    }
    // VALIDATION: FALLBACK_GLOBAL_LOCK
    materialize_received_message_cap(kernel, endpoint, receiver_tid, sender_tid, msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Byte-for-byte layout guard: the recv-v2 ABI frame must encode each field
    // at its frozen offset. This mirrors the assertions the delivery-path
    // integration tests make on the user-visible meta buffer.
    #[test]
    fn encode_recv_v2_meta_matches_frozen_layout() {
        let meta = encode_recv_v2_meta(
            0x00FE_DCBA_9876_5432,
            0x1234,
            0xBEEF,
            0x0000_ABCD,
            0x1122_3344_5566_7788,
            0b10,
            0x42,
        );
        assert_eq!(meta.len(), 40, "recv-v2 meta frame must be 40 bytes");
        assert_eq!(
            &meta[0..8],
            &0x00FE_DCBA_9876_5432u64.to_le_bytes(),
            "status word"
        );
        assert_eq!(&meta[8..10], &0x1234u16.to_le_bytes(), "opcode");
        assert_eq!(&meta[10..12], &0xBEEFu16.to_le_bytes(), "msg flags word");
        assert_eq!(&meta[12..16], &0x0000_ABCDu32.to_le_bytes(), "payload len");
        assert_eq!(
            &meta[16..24],
            &0x1122_3344_5566_7788u64.to_le_bytes(),
            "cap id"
        );
        assert_eq!(&meta[24..32], &0b10u64.to_le_bytes(), "recv-meta flags");
        assert_eq!(&meta[32..40], &0x42u64.to_le_bytes(), "sender tid");
    }

    // Each converged call site must reproduce its prior inline bytes. These
    // recreate the three historical encodings field-by-field.
    #[test]
    fn encode_recv_v2_meta_reproduces_per_path_bytes() {
        // Blocked-waiter path: status=0, msg_flags=0.
        let blocked = encode_recv_v2_meta(0, 7, 0, 3, 99, 1, 5);
        assert_eq!(&blocked[0..8], &0u64.to_le_bytes());
        assert_eq!(&blocked[10..12], &0u16.to_le_bytes());
        // Immediate / queued paths: status and msg_flags carry real values.
        let immediate = encode_recv_v2_meta(5, 7, 0x0021, 3, 99, 1, 5);
        assert_eq!(&immediate[0..8], &5u64.to_le_bytes());
        assert_eq!(&immediate[10..12], &0x0021u16.to_le_bytes());
    }
}
