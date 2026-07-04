// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Stage 186D3 — cap-transfer delegation-link seam.
//!
//! Stage 186D2 built the first seam-based cap-transfer materializer
//! (`materialize_received_cap_snapshot_split`) but stopped short of a
//! live-equivalent: the legacy grant (`grant_task_to_task_with_rights`) also
//! records a **sender→receiver delegation link** so that revoking the sender's
//! source capability propagates to the derived receiver cap. Without that link,
//! a seam-materialized cap would be an orphan the revoke tree can't reach — so
//! 186D2's helper was explicitly barred from live wiring until the link is
//! preserved. This module closes that gap as seam-only infrastructure.
//!
//! The delegation link is **pure capability-domain (rank 4)** metadata
//! (`CapabilitySubsystem::delegated_capability_links`); recording it needs no
//! IPC, task, or memory lock. So this seam records the link via the rank-4
//! capability seam only, then — on failure — rolls the freshly-minted receiver
//! cap all the way back through the atomic-mint inverse
//! (`rollback_minted_cap_split`: clear slot + drop refcount + reclaim).
//!
//! # Atomicity / rollback model
//!
//! 1. Materialize the ordinary object cap via the Stage 186D2 seam
//!    (`materialize_received_cap_snapshot_split` → the Stage 186D-proper atomic
//!    mint: pre-bump refcount, publish slot, rollback-on-publish-failure).
//! 2. If a delegation is required (`source_tid != dest_tid`, mirroring legacy),
//!    record the link under the rank-4 capability seam.
//! 3. If link recording fails (`CapabilityFull` — link table full), roll the
//!    mint back via `rollback_minted_cap_split` (clear the receiver cnode slot,
//!    drop the memory-object `cap_refcount`, reclaim if unreferenced) and return
//!    the real error.
//!
//! Success ⇒ receiver slot + refcount + delegation metadata are all consistent.
//! Failure ⇒ no published cap, no refcount leak, no stale cnode slot, and no
//! stale delegation edge (the link is never recorded on the failure path).
//!
//! The delegation carries the source's **bookkeeping identity**
//! (`source_tid`, `source_cap`) purely to record the revoke edge — it is never
//! resolved-to-mint and never treated as receiver authority. The receiver-local
//! CapId is freshly minted by the atomic mint.
//!
//! # Reply-cap arm — still DEFERRED
//!
//! Reply objects are never delegated (reply caps are one-shot, not part of the
//! delegation tree), and their materialization still needs a post-mint IPC
//! (rank 3) waiter-cap record — the `reply_cap_ipc_rank_inversion` blocker
//! unchanged from Stage 186D-prereq. Reply objects route to
//! [`CapTransferMaterializeOutcome::DeferredReplyCap`]; this stage does NOT
//! solve that and emits no reply-cap success marker.
//!
//! # Status / scope
//!
//! `M2_SEAM_HELPER_ONLY` — NOT wired into
//! `materialize_received_message_cap_routed`, `ipc_reply`,
//! `ipc_send`/`recv`/`call`, or any live delivery path. It does not by itself
//! convert any live path or retire the global lock. It brings ordinary
//! cap-transfer materialization to seam **live-equivalence** (mint + delegation
//! link + rollback), which a future live-wiring stage can build on. See
//! `doc/KERNEL_UNLOCKING.md` (Stage 186D3).

use super::cap_transfer_materialize_split::{CapTransferMaterializeOutcome, TransferCapSnapshot};
use super::*;

/// Bookkeeping identity of the transfer source, recorded in the sender→receiver
/// delegation link so revoke/delete can propagate. **Not authority:** `source_cap`
/// is only recorded as a revoke-tree edge, never resolved-to-mint here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TransferCapDelegation {
    /// Source task TID (from the consumed transfer envelope).
    pub(crate) source_tid: u64,
    /// Source CapId in the sender's cnode — recorded as the delegation edge's
    /// parent. Never used as receiver authority.
    pub(crate) source_cap: CapId,
    /// Receiver task TID (the delegation edge's child owner).
    pub(crate) dest_tid: u64,
}

impl crate::runtime::SharedKernel {
    /// Record the sender→receiver delegation link under the rank-4 capability
    /// seam only. Byte-for-byte the same table write as the legacy
    /// `record_delegated_capability_link` (idempotent on an existing identical
    /// link; `CapabilityFull` when the link table is full). No IPC, task, or
    /// memory lock.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn record_cap_delegation_link_split(
        &self,
        delegation: TransferCapDelegation,
        dest_cap: CapId,
    ) -> Result<(), KernelError> {
        self.with_capability_state_split_mut(|capability| {
            let links = kernel_mut(&mut capability.delegated_capability_links);
            if links.iter().flatten().any(|link| {
                link.source_tid == delegation.source_tid
                    && link.source_cap == delegation.source_cap
                    && link.dest_tid == delegation.dest_tid
                    && link.dest_cap == dest_cap
            }) {
                return Ok(());
            }
            if let Some(slot) = links.iter_mut().find(|slot| slot.is_none()) {
                *slot = Some(DelegatedCapabilityLink {
                    source_tid: delegation.source_tid,
                    source_cap: delegation.source_cap,
                    dest_tid: delegation.dest_tid,
                    dest_cap,
                });
                Ok(())
            } else {
                Err(KernelError::CapabilityFull)
            }
        })
    }

    /// Stage 186D3 — materialize an ordinary transferred cap AND record its
    /// delegation link, the seam **live-equivalent** of
    /// `grant_task_to_task_with_rights` (mint + link) for ordinary object caps.
    ///
    /// Goes through the Stage 186D2 seam (atomic mint) and, on delegation-record
    /// failure, rolls the mint back via `rollback_minted_cap_split`. Never
    /// touches IPC, never a broad `&mut KernelState`. Preserves receiver-local
    /// CapId allocation, object identity + rights, generation/stale checks, and
    /// the real `StaleCapability`/`CapabilityFull`/`TaskMissing` errors
    /// (`WrongObject`/`MissingRight` are upstream).
    ///
    /// `delegation == None` (or `source_tid == dest_tid`) records no link,
    /// matching the legacy `if source_tid != dest_tid` guard.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn materialize_received_cap_snapshot_with_delegation_split(
        &self,
        snapshot: TransferCapSnapshot,
        delegation: Option<TransferCapDelegation>,
    ) -> Result<CapId, KernelError> {
        let minted = self.materialize_received_cap_snapshot_split(snapshot)?;

        if let Some(delegation) = delegation
            && delegation.source_tid != delegation.dest_tid
            && let Err(link_err) = self.record_cap_delegation_link_split(delegation, minted)
        {
            // Delegation recording failed: roll the successful mint all the way
            // back (slot + refcount + reclaim) so nothing is left behind — no
            // published cap, no refcount leak, no stale delegation edge.
            self.rollback_minted_cap_split(snapshot.receiver_cnode, minted, snapshot.object);
            return Err(link_err);
        }
        Ok(minted)
    }

    /// Route a snapshot with optional delegation: ordinary object caps →
    /// atomic mint + delegation link; reply-cap objects → explicit
    /// `DeferredReplyCap` (`reply_cap_ipc_rank_inversion`), never faked, and never
    /// delegated (reply caps are not part of the delegation tree).
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn materialize_received_message_cap_routed_with_delegation_split(
        &self,
        snapshot: TransferCapSnapshot,
        delegation: Option<TransferCapDelegation>,
    ) -> Result<CapTransferMaterializeOutcome, KernelError> {
        if matches!(snapshot.object, CapObject::Reply { .. }) {
            return Ok(CapTransferMaterializeOutcome::DeferredReplyCap);
        }
        let cap =
            self.materialize_received_cap_snapshot_with_delegation_split(snapshot, delegation)?;
        Ok(CapTransferMaterializeOutcome::Materialized(cap))
    }
}
