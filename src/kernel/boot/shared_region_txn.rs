// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Stage 198E2A — Shared-region post-global-lock delivery transaction (DIRECT path).
//!
//! Architecture-neutral, bounded transaction that delivers a shared-region cap-transfer to an
//! already-blocked receiver AFTER the broad `&mut KernelState` borrow is conceptually dropped —
//! the receiver-local cap mint, the page mapping into the receiver ASID, and the user metadata
//! copy are performed by the executor, converging every failure on ONE idempotent rollback.
//!
//! This is NOT wired into an architecture retirement gate and enables no live class; queued
//! shared-region delivery stays on its current fallback path. The mechanism is proven by hosted
//! production-path tests (`stage198e2a_*`).
//!
//! Ownership model:
//! - The Phase-A snapshot CONSUMES the `TransferEnvelope` and TAKES OVER its MemoryObject lifetime
//!   pin (`take_transfer_envelope_keep_pin` — no unpin, so no reference gap). The snapshot owns the
//!   `+1` pin until a terminal outcome releases it.
//! - Sender CSpace is never re-resolved after Phase A: identity is the frozen `object` +
//!   `object_generation` captured under the lock.
//! - The receiver is identified by GENERATION-BEARING authority: the captured `receiver_asid` plus
//!   liveness (`task_asid(tid) == receiver_asid` AND the task is not Exited). A replacement task
//!   with a reused numeric TID receives a DIFFERENT ASID, so a stale transaction can never publish
//!   into it.
//! - The provisional lifecycle entry is the `ActiveTransferMapping` registered BEFORE any page is
//!   mapped, so `purge_active_transfer_mappings_for_pid` (process-exit cleanup) owns reclamation of
//!   a partially-mapped region — there is no interval with a live mapping and no registry owner.

use super::*;
use crate::kernel::ipc::Message;
use crate::kernel::vm::{CachePolicy, Mapping, PAGE_SIZE, PageFlags, PhysAddr, VirtAddr};

/// Bounded transaction state machine. Exactly one terminal transition per transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SharedRegionTxnState {
    /// Provisional ownership reserved; no receiver resource created yet.
    Reserved,
    /// A fresh receiver-local cap has been minted.
    CapMinted,
    /// The region is mapped and the provisional active-mapping entry is registered.
    Mapped,
    /// Delivery published and the receiver woken exactly once (terminal, success).
    Published,
    /// Rolled back to a clean state (terminal, failure). Idempotent.
    Cancelled,
}

/// Owned, copyable post-lock snapshot — no borrows, no `&mut KernelState`, no sender-CSpace handle
/// resolved after the lock drops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RecvBoundarySharedRegionSnapshot {
    pub(crate) receiver_cnode: crate::kernel::capabilities::CNodeId,
    /// Frozen source object identity (MemoryObject/DmaRegion) — authoritative.
    pub(crate) object: CapObject,
    /// Object generation captured under the lock (revalidated before publish).
    pub(crate) object_generation: u64,
    /// Attenuated DESTINATION rights (source rights ∩ recv-intent; WRITE dropped without intent).
    pub(crate) rights: crate::kernel::capabilities::CapRights,
    /// Shared-region descriptor (offset/len) carried by the consumed envelope.
    pub(crate) descriptor: TransferSharedRegion,
    /// Source task (delegation-parent bookkeeping only; NEVER re-resolved for authority).
    pub(crate) source_tid: u64,
    pub(crate) source_cap: CapId,
    pub(crate) receiver_tid: u64,
    pub(crate) receiver_pid: u64,
    pub(crate) receiver_asid: crate::kernel::vm::Asid,
    pub(crate) endpoint: CapObject,
    /// Receiver user VA to map the region at (also the recv-v2 payload target).
    pub(crate) map_va: u64,
    /// Receiver user VA for the recv-v2 metadata copy.
    pub(crate) meta_ptr: u64,
    /// Requested mapping intent (bit0=read, bit1=write).
    pub(crate) map_write: bool,
    /// This snapshot owns the `+1` MemoryObject pin transferred from the envelope.
    pub(crate) pin_owned: bool,
    /// Direct-delivery origin marker (vs a future queued reuse).
    pub(crate) origin_direct: bool,
    /// The dequeued message (recv-v2 metadata source).
    pub(crate) msg: Message,
}

/// One-shot transaction wrapping the snapshot plus the intermediate resources the rollback must
/// unwind. Cleared on every outcome.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SharedRegionDirectTxn {
    pub(crate) state: SharedRegionTxnState,
    pub(crate) snapshot: RecvBoundarySharedRegionSnapshot,
    /// Fresh receiver-local cap once minted (revoked on rollback).
    pub(crate) minted_cap: Option<CapId>,
    /// Mapped (base, len) once mapped (unmapped on rollback).
    pub(crate) mapped: Option<(u64, usize)>,
}

/// Successful publication outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SharedRegionDirectPublish {
    pub(crate) receiver_local_cap: CapId,
    pub(crate) mapped_base: u64,
    pub(crate) mapped_len: usize,
    pub(crate) woke_receiver: bool,
}

/// Typed failure from `shared_region_direct_execute`. The transaction has ALREADY been rolled back
/// to `Cancelled` (idempotent) by the time this is returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SharedRegionTxnError {
    ReceiverGone,
    GenerationReplaced,
    CnodeFull,
    MissingMapRight,
    MissingWriteRight,
    BadRegion,
    MapFault,
    CopyFault,
    StalePublish,
    Internal,
}

// Test-only deterministic fault injection for the map / copy rollback cases.
#[cfg(test)]
pub(crate) static SHARED_REGION_TXN_FORCE_MAP_FAULT: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);
#[cfg(test)]
pub(crate) static SHARED_REGION_TXN_FORCE_COPY_FAULT: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);
/// Simulate the receiver exiting / generation-replacing AFTER the region is mapped but BEFORE
/// publication, exercising the phase-8 final revalidation → exactly-one unmap/revoke rollback.
#[cfg(test)]
pub(crate) static SHARED_REGION_TXN_FORCE_STALE_AFTER_MAP: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

impl KernelState {
    #[cfg(test)]
    fn shared_region_txn_force_map_fault(&self) -> bool {
        SHARED_REGION_TXN_FORCE_MAP_FAULT.load(core::sync::atomic::Ordering::SeqCst)
    }
    #[cfg(not(test))]
    fn shared_region_txn_force_map_fault(&self) -> bool {
        false
    }
    #[cfg(test)]
    fn shared_region_txn_force_copy_fault(&self) -> bool {
        SHARED_REGION_TXN_FORCE_COPY_FAULT.load(core::sync::atomic::Ordering::SeqCst)
    }
    #[cfg(not(test))]
    fn shared_region_txn_force_copy_fault(&self) -> bool {
        false
    }
    #[cfg(test)]
    fn shared_region_txn_force_stale_after_map(&self) -> bool {
        SHARED_REGION_TXN_FORCE_STALE_AFTER_MAP.load(core::sync::atomic::Ordering::SeqCst)
    }
    #[cfg(not(test))]
    fn shared_region_txn_force_stale_after_map(&self) -> bool {
        false
    }

    /// Phase A (UNDER the broad lock): consume the shared-region transfer envelope, TAKE OVER its
    /// object pin, resolve + attenuate the destination rights, and capture the receiver's
    /// generation-bearing authority. Fails closed (envelope reclaimed by the caller path) on any
    /// mismatch. Sender CSpace is resolved exactly ONCE here, never again.
    pub(crate) fn shared_region_direct_phase_a(
        &mut self,
        handle: u64,
        endpoint: CapObject,
        receiver_tid: u64,
        map_va: u64,
        map_write: bool,
        meta_ptr: u64,
        msg: Message,
    ) -> Result<RecvBoundarySharedRegionSnapshot, KernelError> {
        // Consume the envelope KEEPING the pin (no reference gap): the snapshot owns it now.
        let envelope = self
            .take_transfer_envelope_keep_pin(handle, endpoint, ThreadId(receiver_tid))
            .ok_or(KernelError::InvalidCapability)?;
        let Some(descriptor) = envelope.shared_region else {
            // Not a shared-region envelope: release the pin we kept is unnecessary (none kept for
            // non-shared), just reject. Restore nothing — keep-pin only kept it for shared.
            return Err(KernelError::WrongObject);
        };
        // Object must be a shared-region variant.
        let (object, object_generation) = match envelope.source_object {
            CapObject::MemoryObject { .. } | CapObject::DmaRegion { .. } => (
                envelope.source_object,
                capobject_generation(envelope.source_object),
            ),
            _ => {
                // Release the kept pin before rejecting.
                self.adjust_memory_object_pin_refcount(envelope.source_object, -1);
                return Err(KernelError::WrongObject);
            }
        };
        // Source rights (resolved ONCE, here) → attenuated destination rights.
        let source_capability =
            self.resolve_capability_for_task(envelope.source_tid.0, envelope.source_cap)?;
        let mut rights = source_capability.rights();
        if !map_write {
            rights = rights.intersect(CapRights::READ | CapRights::MAP);
        }
        let receiver_cnode = self
            .task_cnode(receiver_tid)
            .ok_or(KernelError::InvalidCapability)?;
        let receiver_asid = self
            .task_asid(receiver_tid)
            .ok_or(KernelError::UserMemoryFault)?;
        let receiver_pid = self.process_id(receiver_tid).unwrap_or(receiver_tid);
        Ok(RecvBoundarySharedRegionSnapshot {
            receiver_cnode,
            object,
            object_generation,
            rights,
            descriptor,
            source_tid: envelope.source_tid.0,
            source_cap: envelope.source_cap,
            receiver_tid,
            receiver_pid,
            receiver_asid,
            endpoint,
            map_va,
            meta_ptr,
            map_write,
            pin_owned: true,
            origin_direct: true,
            msg,
        })
    }

    /// Post-lock executor: phases 2..10. Every failure converges on the single idempotent rollback
    /// and returns the typed error AFTER the transaction is `Cancelled`.
    pub(crate) fn shared_region_direct_execute(
        &mut self,
        snapshot: RecvBoundarySharedRegionSnapshot,
    ) -> Result<SharedRegionDirectPublish, SharedRegionTxnError> {
        let mut txn = SharedRegionDirectTxn {
            state: SharedRegionTxnState::Reserved,
            snapshot,
            minted_cap: None,
            mapped: None,
        };

        // Phase 1: revalidate receiver generation-authority BEFORE reserving anything.
        if !self.shared_region_receiver_alive(&txn.snapshot) {
            self.rollback_shared_region_direct_txn(&mut txn);
            return Err(SharedRegionTxnError::ReceiverGone);
        }
        // Phase 1b: object generation still live.
        if self.capability_object_live(txn.snapshot.object).is_none() {
            self.rollback_shared_region_direct_txn(&mut txn);
            return Err(SharedRegionTxnError::GenerationReplaced);
        }

        // Bounds check (DmaRegion / region len) up front.
        let region_len = match usize::try_from(txn.snapshot.descriptor.len) {
            Ok(v) if v > 0 => v,
            _ => {
                self.rollback_shared_region_direct_txn(&mut txn);
                return Err(SharedRegionTxnError::BadRegion);
            }
        };
        if txn.snapshot.map_va == 0 || !txn.snapshot.map_va.is_multiple_of(PAGE_SIZE as u64) {
            self.rollback_shared_region_direct_txn(&mut txn);
            return Err(SharedRegionTxnError::BadRegion);
        }
        // Rights gates (object-authoritative): MAP required; WRITE only with canonical WRITE.
        if !txn.snapshot.rights.contains(CapRights::MAP) {
            self.rollback_shared_region_direct_txn(&mut txn);
            return Err(SharedRegionTxnError::MissingMapRight);
        }
        if txn.snapshot.map_write && !txn.snapshot.rights.contains(CapRights::WRITE) {
            self.rollback_shared_region_direct_txn(&mut txn);
            return Err(SharedRegionTxnError::MissingWriteRight);
        }

        // Phase 3: mint ONE fresh receiver-local cap with the attenuated rights.
        let minted = match self.mint_capability_in_cnode(
            txn.snapshot.receiver_cnode,
            Capability::new(txn.snapshot.object, txn.snapshot.rights),
        ) {
            Ok(cap) => cap,
            Err(_) => {
                self.rollback_shared_region_direct_txn(&mut txn);
                return Err(SharedRegionTxnError::CnodeFull);
            }
        };
        txn.minted_cap = Some(minted);
        txn.state = SharedRegionTxnState::CapMinted;

        // Phase 4: map ONLY the authorized region. Register the provisional active mapping BEFORE
        // the first page maps, so process-exit cleanup owns any partial mapping (no untracked
        // window). NX enforced (execute=false); writable only with canonical WRITE.
        let mapped_len = page_round_up(region_len);
        if self
            .register_active_transfer_mapping(
                ThreadId(txn.snapshot.receiver_tid),
                minted,
                VirtAddr(txn.snapshot.map_va),
                mapped_len,
            )
            .is_err()
        {
            self.rollback_shared_region_direct_txn(&mut txn);
            return Err(SharedRegionTxnError::Internal);
        }
        // From here a (future) mapping is owned by the active-mapping registry AND the txn.
        txn.mapped = Some((txn.snapshot.map_va, mapped_len));

        let phys_start = match self.shared_region_phys_base(txn.snapshot.object) {
            Some(p) => p,
            None => {
                self.rollback_shared_region_direct_txn(&mut txn);
                return Err(SharedRegionTxnError::BadRegion);
            }
        };
        let map_flags = PageFlags {
            read: true,
            write: txn.snapshot.map_write,
            execute: false, // NX ALWAYS.
            user: true,
            cache_policy: CachePolicy::WriteBack,
        };
        let num_pages = mapped_len / PAGE_SIZE;
        let mut mapped_ok = !self.shared_region_txn_force_map_fault();
        if mapped_ok {
            for i in 0..num_pages {
                let virt = VirtAddr(txn.snapshot.map_va + (i * PAGE_SIZE) as u64);
                let phys = crate::kernel::vm::PhysAddr(phys_start.0 + (i * PAGE_SIZE) as u64);
                if self
                    .map_user_page_in_asid_raw(
                        txn.snapshot.receiver_asid,
                        virt,
                        crate::kernel::vm::Mapping {
                            phys,
                            flags: map_flags,
                        },
                    )
                    .is_err()
                {
                    mapped_ok = false;
                    break;
                }
            }
        }
        if !mapped_ok {
            self.rollback_shared_region_direct_txn(&mut txn);
            return Err(SharedRegionTxnError::MapFault);
        }
        // Phase 5/6: mapping complete (fresh maps need no TLB shootdown; only rollback unmaps do).
        txn.state = SharedRegionTxnState::Mapped;

        // Phase 7: user metadata copy OUTSIDE all locks (recv-v2 meta).
        let meta = crate::kernel::syscall::ipc_recv_core::encode_recv_v2_meta(
            txn.snapshot.msg.sender_tid.0,
            txn.snapshot.msg.opcode,
            txn.snapshot.msg.flags,
            txn.snapshot.msg.as_slice().len() as u32,
            minted.0,
            crate::kernel::syscall::SYSCALL_RECV_META_TRANSFERRED_CAP as u64,
            txn.snapshot.msg.sender_tid.0,
        );
        let copy_ok = !self.shared_region_txn_force_copy_fault()
            && self
                .copy_to_user(
                    txn.snapshot.receiver_asid,
                    VirtAddr(txn.snapshot.meta_ptr),
                    &meta,
                )
                .is_ok();
        if !copy_ok {
            self.rollback_shared_region_direct_txn(&mut txn);
            return Err(SharedRegionTxnError::CopyFault);
        }

        // Phase 8: revalidate receiver generation + transaction ownership BEFORE publishing.
        if self.shared_region_txn_force_stale_after_map()
            || !self.shared_region_receiver_alive(&txn.snapshot)
            || self.capability_object_live(txn.snapshot.object).is_none()
        {
            self.rollback_shared_region_direct_txn(&mut txn);
            return Err(SharedRegionTxnError::StalePublish);
        }

        // Phase 9/10: publish + wake receiver exactly once, then release the transfer pin (the
        // receiver-local cap now owns the object reference), and clear the post-work state.
        let woke = self
            .apply_split_receiver_wake_plan(ThreadId(txn.snapshot.receiver_tid))
            .is_ok();
        if txn.snapshot.pin_owned {
            self.adjust_memory_object_pin_refcount(txn.snapshot.object, -1);
            txn.snapshot.pin_owned = false;
        }
        txn.state = SharedRegionTxnState::Published;
        crate::yarm_log!(
            "SHARED_REGION_DIRECT_PUBLISH receiver_tid={} cap={} base=0x{:x} len={} wake={}",
            txn.snapshot.receiver_tid,
            minted.0,
            txn.snapshot.map_va,
            mapped_len,
            woke
        );
        Ok(SharedRegionDirectPublish {
            receiver_local_cap: minted,
            mapped_base: txn.snapshot.map_va,
            mapped_len,
            woke_receiver: woke,
        })
    }

    /// The SINGLE idempotent rollback executor. Safe to call from EVERY state; each undo step is
    /// guarded so nothing is unmapped or revoked twice. Reverse order:
    /// prevent-wake → unmap → TLB shootdown → remove active mapping → revoke cap → release pin →
    /// clear state.
    pub(crate) fn rollback_shared_region_direct_txn(&mut self, txn: &mut SharedRegionDirectTxn) {
        if txn.state == SharedRegionTxnState::Cancelled
            || txn.state == SharedRegionTxnState::Published
        {
            // Already terminal: nothing to unwind (and never after a successful publish).
            if txn.state == SharedRegionTxnState::Published {
                return;
            }
        }
        // (publication/wake is simply never performed on this path)
        // Unmap the region (two-phase; tolerates absent pages) then drop the mapping record.
        if let Some((base, len)) = txn.mapped.take() {
            self.unmap_range_two_phase(txn.snapshot.receiver_asid, base as usize, len);
        }
        // Remove the provisional/active mapping registry entry (guarded: remove returns false if
        // already gone; the CapId key is generation-bearing so a reused slot cannot be hit).
        if let Some(cap) = txn.minted_cap {
            let _ = self.remove_active_transfer_mapping(ThreadId(txn.snapshot.receiver_tid), cap);
        }
        // Revoke the provisional receiver cap (once).
        if let Some(cap) = txn.minted_cap.take() {
            let _ = self.revoke_capability_in_cnode(txn.snapshot.receiver_cnode, cap);
        }
        // Release the transferred object pin (once).
        if txn.snapshot.pin_owned {
            self.adjust_memory_object_pin_refcount(txn.snapshot.object, -1);
            txn.snapshot.pin_owned = false;
        }
        txn.state = SharedRegionTxnState::Cancelled;
    }

    /// Receiver liveness + generation-bearing authority: the task must exist, not be Exited, and
    /// still hold the SAME ASID captured in the snapshot. A replacement task reusing the numeric
    /// TID gets a different ASID and therefore fails this check.
    fn shared_region_receiver_alive(&self, snap: &RecvBoundarySharedRegionSnapshot) -> bool {
        // `task_status` returns None for a non-existent TCB; Exited/Dead are terminal.
        match self.task_status(snap.receiver_tid) {
            None | Some(TaskStatus::Exited(_)) | Some(TaskStatus::Dead) => return false,
            _ => {}
        }
        // Generation-bearing authority: the ASID must be unchanged (a replacement task reusing the
        // numeric TID receives a different ASID).
        self.task_asid(snap.receiver_tid) == Some(snap.receiver_asid)
    }

    fn shared_region_phys_base(&self, object: CapObject) -> Option<crate::kernel::vm::PhysAddr> {
        let (id, offset) = match object {
            CapObject::MemoryObject { id } => (id, 0u64),
            CapObject::DmaRegion { id, offset, .. } => (id, offset),
            _ => return None,
        };
        self.with_memory_state(|m| {
            m.memory_objects
                .iter()
                .flatten()
                .find(|e| e.id == id)
                .map(|e| crate::kernel::vm::PhysAddr(e.phys.0 + offset))
        })
    }
}

fn page_round_up(len: usize) -> usize {
    (len + PAGE_SIZE - 1) & !(PAGE_SIZE - 1)
}

fn capobject_generation(object: CapObject) -> u64 {
    match object {
        CapObject::MemoryObject { .. } => 0,
        CapObject::DmaRegion { .. } => 0,
        CapObject::Endpoint { generation, .. }
        | CapObject::Notification { generation, .. }
        | CapObject::Reply { generation, .. } => generation,
        _ => 0,
    }
}
