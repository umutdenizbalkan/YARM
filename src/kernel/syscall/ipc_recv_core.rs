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
