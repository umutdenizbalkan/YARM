// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Stage 186D2 — cap-transfer materialization seam, first slice.
//!
//! Builds the first **seam-based** cap-transfer materialization on top of the
//! Stage 186D-proper atomic cap↔memory mint
//! (`mint_capability_with_memory_ref_split`). It materializes an *ordinary*
//! transferred object capability (endpoint / notification / memory-object /
//! DMA-region) into the receiver's cnode with a fresh receiver-local `CapId`,
//! using ONLY the rank-4 capability seam and rank-6 memory seam — never
//! `ipc_state_lock`, never a broad `&mut KernelState`.
//!
//! # IPC / capability boundary (snapshot model)
//!
//! The one-shot transfer envelope is consumed under `ipc_state_lock` (rank 3) by
//! the IPC phase, the source capability is resolved to a concrete
//! `(object, rights)` pair, and the receiver's destination cnode is resolved —
//! all *before* this seam runs. The result is captured into a plain
//! [`TransferCapSnapshot`] value; the IPC lock is then dropped. This seam takes
//! that snapshot **by value** and touches no IPC state at all. Because the
//! envelope was already consumed to produce the snapshot, one-shot semantics are
//! preserved at the interface boundary: this seam never sees, re-takes, or
//! reuses an envelope.
//!
//! The snapshot deliberately carries **object identity + rights**, not a
//! sender-local `CapId`. A local CapId is never transferable authority; the
//! receiver-local CapId is freshly minted here.
//!
//! # Reply-cap arm — DEFERRED
//!
//! Reply objects (`CapObject::Reply`) are **not** seam-supported by this slice.
//! A reply-cap materialization must record the receiver-local CapId back into the
//! IPC reply registry (rank 3) *after* the rank-4 mint — a cap→IPC rank
//! inversion (see `doc/KERNEL_UNLOCKING.md` Stage 186D-prereq / 186D2). This
//! slice routes reply objects to an explicit
//! [`CapTransferMaterializeOutcome::DeferredReplyCap`] (reason
//! `reply_cap_ipc_rank_inversion`); it never fakes a reply-cap seam and emits no
//! reply-cap success marker.
//!
//! # Status / scope
//!
//! `M2_SEAM_HELPER_ONLY` — NOT wired into `materialize_received_message_cap_routed`,
//! `ipc_reply`, `ipc_send`/`recv`/`call`, or any live delivery path. It does not
//! by itself convert any live path or retire the global lock.
//!
//! **Not yet a live-equivalent of `grant_task_to_task_with_rights`.** This slice
//! materializes the cap (object + rights + fresh receiver-local CapId + atomic
//! refcount), but does **not** yet record the source→dest delegation link that
//! the legacy grant records for revocation propagation. Recording that link is a
//! rank-4-only follow-on slice; until it lands, this helper MUST NOT be wired
//! into the live delivery path. See `doc/KERNEL_UNLOCKING.md` (Stage 186D2).

use super::*;

/// Plain, IPC-lock-free snapshot of an ordinary transferred capability, captured
/// AFTER the transfer envelope was consumed under `ipc_state_lock`.
///
/// Carries object identity + rights + the resolved destination cnode — **never**
/// a sender-local `CapId` (local CapIds are not transferable authority).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TransferCapSnapshot {
    /// Receiver's destination cnode, resolved during the IPC/capability phase.
    pub(crate) receiver_cnode: CNodeId,
    /// The concrete transferred object, resolved from the (now-consumed) source
    /// capability during the IPC/capability phase.
    pub(crate) object: CapObject,
    /// Attenuated rights, derived during the IPC/capability phase. Byte-identical
    /// to what the legacy grant would attenuate to.
    pub(crate) rights: CapRights,
}

/// Outcome of routing a [`TransferCapSnapshot`] through the seam materializer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CapTransferMaterializeOutcome {
    /// Ordinary object cap materialized: fresh receiver-local `CapId`.
    Materialized(CapId),
    /// Reply-cap objects are not seam-supported in this slice (they need a
    /// post-mint IPC rank-3 waiter-cap record — a rank inversion). Reason:
    /// `reply_cap_ipc_rank_inversion`. Caller keeps the legacy path.
    DeferredReplyCap,
}

impl CapTransferMaterializeOutcome {
    /// Stable reason string for the deferred reply-cap arm (telemetry / docs).
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) const DEFERRED_REPLY_CAP_REASON: &'static str = "reply_cap_ipc_rank_inversion";
}

impl crate::runtime::SharedKernel {
    /// Stage 186D2 — first-slice seam materialization of an ordinary transferred
    /// capability from a post-IPC snapshot.
    ///
    /// Uses the Stage 186D-proper atomic mint
    /// (`mint_capability_with_memory_ref_split`): pre-bump the memory-object
    /// `cap_refcount` (rank 6), publish the receiver-local cnode slot (rank 4),
    /// roll the bump back if publish fails. Never touches IPC state, never holds
    /// `ipc_state_lock`, never forms a broad `&mut KernelState`.
    ///
    /// Returns a fresh receiver-local `CapId`, or a real error
    /// (`StaleCapability` for a dead memory object, `CapabilityFull` for a full
    /// destination cspace, `TaskMissing` for an absent destination cnode space).
    /// `WrongObject` / `MissingRight` are produced upstream (source resolution /
    /// rights derivation, before the snapshot) and are never converted to `Ok`
    /// here.
    ///
    /// Precondition: `snapshot.object` must NOT be a `CapObject::Reply`
    /// (route reply objects through [`Self::materialize_received_message_cap_routed_split`]
    /// which defers them). Reply objects are still minted structurally here, but
    /// the reply *registry* record is not performed — callers must not use this
    /// path for reply caps.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn materialize_received_cap_snapshot_split(
        &self,
        snapshot: TransferCapSnapshot,
    ) -> Result<CapId, KernelError> {
        self.mint_capability_with_memory_ref_split(
            snapshot.receiver_cnode,
            Capability::new(snapshot.object, snapshot.rights),
        )
    }

    /// Route a transfer-cap snapshot: ordinary object caps go through the atomic
    /// seam mint; reply-cap objects are explicitly deferred
    /// (`reply_cap_ipc_rank_inversion`) — never faked as seam-supported.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn materialize_received_message_cap_routed_split(
        &self,
        snapshot: TransferCapSnapshot,
    ) -> Result<CapTransferMaterializeOutcome, KernelError> {
        if matches!(snapshot.object, CapObject::Reply { .. }) {
            // Reply caps need a post-mint IPC (rank 3) waiter-cap record — a
            // cap→IPC rank inversion this slice does not solve. Defer, do NOT
            // fake a seam materialization.
            return Ok(CapTransferMaterializeOutcome::DeferredReplyCap);
        }
        let cap = self.materialize_received_cap_snapshot_split(snapshot)?;
        Ok(CapTransferMaterializeOutcome::Materialized(cap))
    }
}
