// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Stage 188A — typed dispatch-return delivery channel.
//!
//! Stage 187D established the architectural blocker: the shared blocked-waiter
//! delivery engine (`complete_blocked_recv_for_waiter`) and the other IPC
//! delivery paths run inside the single main-dispatch `with_cpu` closure with no
//! `SharedKernel`-level owner, so they cannot drop the broad `&mut KernelState`
//! borrow to call the Stage 186E / 186D2 / 186D3 seams. This module introduces
//! the prerequisite: a **typed, by-value channel** by which a syscall/IPC handler
//! running under the broad borrow can hand *post-boundary work* back to runtime,
//! which executes it **after** the borrow is dropped, through `&SharedKernel`
//! seams.
//!
//! The channel reuses the proven Stage 117 per-CPU stash idiom
//! (`PerCpuSwitchPlanStash`): a handler stashes a [`DispatchPostWork`] under the
//! broad borrow; the trap entry drains it right after `with_cpu` returns (the
//! same post-`with_cpu` execution point the D2/D6 drains already use).
//!
//! # Stage 188A scope — infrastructure only
//!
//! No live handler produces post-work in this stage: every syscall path leaves
//! the stash empty, so the drain is a no-op and there is **zero runtime behavior
//! change** (the drain emits only a one-shot `DISPATCH_RETURN_CHANNEL_READY
//! mode=helper_only`). The [`DispatchPostWork::BlockedWaiterPlainDelivery`]
//! variant and its executor are complete and unit-tested (via the 186E copy
//! seam) but are **produced by nothing live** — a future stage wires the
//! blocked-waiter call sites to produce it. Reply-cap materialization stays
//! blocked by `reply_cap_ipc_rank_inversion`; cap-transfer / fault delivery
//! variants are future extensions.
//!
//! # Invariants
//!
//! [`DispatchPostWork`] is **by value only**: no `&mut KernelState`, no borrowed
//! subsystem references, and no sender-local `CapId` as receiver authority (the
//! only wired variant is a *plain, no-cap* delivery, so it carries no cap at
//! all).

use super::capabilities::{CNodeId, CapId, CapObject, CapRights};
use super::ipc::{Message, ThreadId};
use super::vm::Asid;

/// Encoded recv-v2 metadata length (mirrors `IPC_RECV_META_V2_ENCODED_LEN`).
pub(crate) const DISPATCH_POST_WORK_META_LEN: usize = 40;

/// Typed post-dispatch work returned (via the per-CPU stash) by a syscall/IPC
/// handler running under the broad `with_cpu` / `&mut KernelState` borrow, to be
/// executed by runtime **after** that borrow is dropped, through `&SharedKernel`
/// seams.
///
/// By value only — see module docs. `#[non_exhaustive]`-style extension is
/// expressed by the documented future variants; new variants are added together
/// with their live producer and executor arm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DispatchPostWork {
    /// No post-boundary work — the syscall completed entirely under the broad
    /// borrow. This is the ONLY variant any live path produces in Stage 188A, so
    /// the channel is inert (zero behavior change).
    None,
    /// A plain (no-cap, no-reply) blocked-waiter payload+meta delivery deferred
    /// past the broad borrow. **Produced by no live handler in Stage 188A**
    /// (infrastructure only); the executor is exercised by unit tests through the
    /// 186E copy seam. A future stage wires the blocked-waiter call sites to
    /// produce this instead of copying under the broad borrow.
    #[cfg_attr(not(test), allow(dead_code))]
    BlockedWaiterPlainDelivery(BlockedWaiterPlainDeliverySnapshot),
    /// An ordinary (non-reply, non-shared-region) single-cap-transfer
    /// blocked-waiter delivery deferred past the broad borrow (Stage 188C). The
    /// executor materializes the receiver-local cap through the 186D2/186D3 seam
    /// (atomic mint + delegation link + rollback), encodes the recv-v2 meta with
    /// that fresh receiver-local CapId, copies payload+meta through the 186E seam,
    /// then clears the waiter and wakes it once. **Reply caps are excluded** — the
    /// snapshot's `object` is never a `Reply` (`reply_cap_ipc_rank_inversion`
    /// keeps reply-cap materialization deferred).
    #[cfg_attr(not(test), allow(dead_code))]
    BlockedWaiterOrdinaryCapDelivery(BlockedWaiterOrdinaryCapDeliverySnapshot),
}

impl DispatchPostWork {
    /// True for the inert [`DispatchPostWork::None`].
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) const fn is_none(&self) -> bool {
        matches!(self, Self::None)
    }

    /// Stable kind slug for markers/telemetry.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) const fn kind(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::BlockedWaiterPlainDelivery(_) => "blocked_waiter_plain",
            Self::BlockedWaiterOrdinaryCapDelivery(_) => "blocked_waiter_ordinary_cap",
        }
    }
}

/// By-value snapshot of a plain (no-cap, no-reply) blocked-waiter delivery.
///
/// Contains ONLY owned values — no `&mut KernelState`, no borrows, and **no
/// `CapId` at all** (a plain delivery transfers no capability, so there is
/// nothing that could be mistaken for sender-local authority). The receiver's
/// payload/meta are captured by value under the broad borrow (Phase A); the
/// executor writes them to the waiter's user buffers through the 186E seam
/// (Phase B), then clears the waiter's return registers and wakes it (Phase C).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BlockedWaiterPlainDeliverySnapshot {
    /// The blocked waiter (delivery target).
    pub(crate) waiter_tid: u64,
    /// The waiter's ASID (resolved in Phase A; the copy target address space).
    pub(crate) waiter_asid: Asid,
    /// User pointer for the payload buffer.
    pub(crate) payload_user_ptr: usize,
    /// Number of valid payload bytes in `payload`.
    pub(crate) payload_len: usize,
    /// Payload bytes, by value.
    pub(crate) payload: [u8; Message::MAX_PAYLOAD],
    /// User pointer for the recv-v2 meta buffer.
    pub(crate) meta_user_ptr: usize,
    /// Pre-encoded 40-byte recv-v2 meta, by value.
    pub(crate) meta: [u8; DISPATCH_POST_WORK_META_LEN],
    /// Endpoint index whose receiver-waiter slot the executor clears in Phase C
    /// (the legacy `ipc_clear_plain_receiver_waiter_only` step).
    pub(crate) endpoint_idx: usize,
    /// Optional task to wake exactly once after delivery completes.
    pub(crate) wake_tid: Option<ThreadId>,
}

/// By-value snapshot of an ordinary (non-reply, non-shared-region) single
/// cap-transfer blocked-waiter delivery (Stage 188C).
///
/// Contains ONLY owned values — no `&mut KernelState`, no borrowed subsystem
/// references. It carries the transferred **object identity + rights** (the
/// authority the receiver actually gets, freshly minted by the seam) plus the
/// source `(source_tid, source_cap)` **as a delegation-link parent edge only**
/// (bookkeeping): `source_cap` is NEVER resolved-to-mint and NEVER handed to the
/// receiver as authority — the receiver-local CapId is minted fresh by the
/// executor's 186D2/186D3 seam. The `object` is guaranteed non-`Reply` by the
/// producer (reply caps stay deferred by `reply_cap_ipc_rank_inversion`).
///
/// Unlike the plain snapshot, the recv-v2 meta is NOT pre-encoded: the
/// receiver-local CapId is only known after the seam mint, so the executor
/// encodes the meta in Phase B (after materialization) from `app_opcode`,
/// `payload_len`, and `sender_tid`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BlockedWaiterOrdinaryCapDeliverySnapshot {
    /// The blocked waiter (delivery target + delegation child owner).
    pub(crate) waiter_tid: u64,
    /// The waiter's ASID (resolved in Phase A; the copy target address space).
    pub(crate) waiter_asid: Asid,
    /// User pointer for the payload buffer.
    pub(crate) payload_user_ptr: usize,
    /// Number of valid payload bytes in `payload`.
    pub(crate) payload_len: usize,
    /// Payload bytes, by value.
    pub(crate) payload: [u8; Message::MAX_PAYLOAD],
    /// User pointer for the recv-v2 meta buffer.
    pub(crate) meta_user_ptr: usize,
    /// Application opcode for the recv-v2 meta (encoded in Phase B).
    pub(crate) app_opcode: u16,
    /// Sender TID for the recv-v2 meta (encoded in Phase B).
    pub(crate) sender_tid: u64,
    /// Receiver's destination cnode (resolved in Phase A). Seam mint target.
    pub(crate) receiver_cnode: CNodeId,
    /// The transferred object identity (resolved from the consumed envelope's
    /// source capability). Never a `Reply` object on this path.
    pub(crate) object: CapObject,
    /// Attenuated rights the receiver-local cap is minted with (byte-identical
    /// to the legacy grant's `derive(source_rights)`).
    pub(crate) rights: CapRights,
    /// Source task TID — delegation-link parent owner (bookkeeping only).
    pub(crate) source_tid: u64,
    /// Source CapId — delegation-link parent edge (bookkeeping only; NEVER
    /// receiver authority, NEVER resolved-to-mint by the executor).
    pub(crate) source_cap: CapId,
    /// Endpoint index whose receiver-waiter slot the executor clears in Phase C.
    pub(crate) endpoint_idx: usize,
    /// Optional task to wake exactly once after delivery completes.
    pub(crate) wake_tid: Option<ThreadId>,
}
