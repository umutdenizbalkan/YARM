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
    /// Mapping is in progress; `mapped_prefix_len` on the txn is the authoritative page prefix.
    Mapping,
    /// The full region is mapped and the provisional active-mapping entry is registered.
    Mapped,
    /// Cancellation became authoritative (teardown marked it, or a checkpoint observed a dead
    /// receiver / generation replacement). No further page/writeback/wake is allowed.
    CancelRequested,
    /// The single cleanup owner (the executor, protocol A) has claimed the unwind — a one-shot
    /// transition that makes any second cleanup a no-op.
    CleanupOwned,
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
    /// Registered mapping base + full authorized length (registry entry span).
    pub(crate) mapped: Option<(u64, usize)>,
    /// AUTHORITATIVE mapping progress in bytes: exactly the successfully-mapped page prefix,
    /// updated after EACH page. Rollback unmaps exactly this prefix — it does not depend on the
    /// txn reaching the terminal `Mapped` state.
    pub(crate) mapped_prefix_len: usize,
}

/// Successful publication outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SharedRegionDirectPublish {
    pub(crate) receiver_local_cap: CapId,
    pub(crate) mapped_base: u64,
    pub(crate) mapped_len: usize,
    pub(crate) woke_receiver: bool,
}

/// Typed failure from `shared_region_execute`. The transaction has ALREADY been rolled back
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
    /// Cancellation became authoritative at a checkpoint (teardown request or dead receiver); the
    /// executor performed the full cleanup.
    Cancelled,
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
/// Fire a cancellation at a specific executor checkpoint (1..=6; 0 = none) — simulates teardown
/// marking `CancelRequested` while the executor is active at that checkpoint.
#[cfg(test)]
pub(crate) static SHARED_REGION_TXN_CANCEL_AT: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
/// Inject a map fault at a specific page index (usize::MAX = none) — exercises the page-N failure
/// → unmap-exactly-prefix path.
#[cfg(test)]
pub(crate) static SHARED_REGION_TXN_MAP_FAULT_AT_PAGE: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(usize::MAX);
/// Fire a between-page cancellation just before mapping this page index (usize::MAX = none) — lets a
/// test cancel between later pages of a multi-page region.
#[cfg(test)]
pub(crate) static SHARED_REGION_TXN_CANCEL_AT_PAGE: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(usize::MAX);

// ── Deterministic test-fault hooks (read as module free functions by the generic transaction
// runner, so BOTH the broad-borrow and off-lock contexts observe the same injected faults). ──
#[cfg(test)]
fn hook_force_map_fault() -> bool {
    SHARED_REGION_TXN_FORCE_MAP_FAULT.load(core::sync::atomic::Ordering::SeqCst)
}
#[cfg(not(test))]
fn hook_force_map_fault() -> bool {
    false
}
#[cfg(test)]
fn hook_force_copy_fault() -> bool {
    SHARED_REGION_TXN_FORCE_COPY_FAULT.load(core::sync::atomic::Ordering::SeqCst)
}
#[cfg(not(test))]
fn hook_force_copy_fault() -> bool {
    false
}
#[cfg(test)]
fn hook_force_stale_after_map() -> bool {
    SHARED_REGION_TXN_FORCE_STALE_AFTER_MAP.load(core::sync::atomic::Ordering::SeqCst)
}
#[cfg(not(test))]
fn hook_force_stale_after_map() -> bool {
    false
}
#[cfg(test)]
fn hook_cancel_at(checkpoint: usize) -> bool {
    SHARED_REGION_TXN_CANCEL_AT.load(core::sync::atomic::Ordering::SeqCst) == checkpoint
}
#[cfg(not(test))]
fn hook_cancel_at(_checkpoint: usize) -> bool {
    false
}
#[cfg(test)]
fn hook_map_fault_at_page(page: usize) -> bool {
    SHARED_REGION_TXN_MAP_FAULT_AT_PAGE.load(core::sync::atomic::Ordering::SeqCst) == page
}
#[cfg(not(test))]
fn hook_map_fault_at_page(_page: usize) -> bool {
    false
}
#[cfg(test)]
fn hook_cancel_at_page(page: usize) -> bool {
    SHARED_REGION_TXN_CANCEL_AT_PAGE.load(core::sync::atomic::Ordering::SeqCst) == page
}
#[cfg(not(test))]
fn hook_cancel_at_page(_page: usize) -> bool {
    false
}

impl KernelState {
    /// Teardown API (protocol A): mark a generation-bearing cancellation request for an in-flight
    /// shared-region transaction (direct OR queued). The executor observes it at its next checkpoint
    /// and performs ALL cleanup itself (executor is the single cleanup owner). Matched on BOTH the
    /// numeric receiver TID and the captured ASID, so a delayed action for an old TID cannot cancel
    /// a replacement process's transaction.
    ///
    /// FAIL-CLOSED (Stage 198E2B): returns `true` when the request is recorded (or an identical one
    /// already exists). When the table is full it first evicts a STALE entry — one whose `(tid,asid)`
    /// no longer names a live receiver (so it can never be consumed by any transaction) — and
    /// records into the freed slot. If (and only if) every occupant is still live, it sets the
    /// `shared_region_cancel_overflow` latch (a permanent per-instance fail-closed fuse) and returns
    /// `false`; while that latch is set EVERY executor checkpoint treats cancellation as authoritative,
    /// so no transaction can proceed past a cancellation that could not be recorded. Silent
    /// cancellation loss is therefore impossible.
    pub(crate) fn shared_region_request_cancel(
        &mut self,
        receiver_tid: u64,
        receiver_asid: crate::kernel::vm::Asid,
    ) -> bool {
        // A cancel-request occupant is STALE if its (tid, asid) can no longer belong to any live
        // transaction (the task is gone / exited / has a different ASID now).
        let occupant_is_stale = |ipc_tid: u64, ipc_asid: crate::kernel::vm::Asid| -> bool {
            match self.task_status(ipc_tid) {
                None | Some(TaskStatus::Exited(_)) | Some(TaskStatus::Dead) => true,
                _ => self.task_asid(ipc_tid) != Some(ipc_asid),
            }
        };
        // Precompute staleness (needs &self) before the &mut borrow.
        let stale_flags: [bool; MAX_SHARED_REGION_CANCEL_REQUESTS] = core::array::from_fn(|i| {
            self.with_ipc_state(|ipc| ipc.shared_region_cancel_requests[i])
                .map(|r| occupant_is_stale(r.tid, r.asid))
                .unwrap_or(true)
        });
        // Returns (recorded, fuse_newly_set): fuse_newly_set is true ONLY on the clear → set
        // overflow transition, so the diagnostic (emitted below, outside the IPC borrow) fires once.
        let (recorded, fuse_newly_set) = self.with_ipc_state_mut(|ipc| {
            if ipc
                .shared_region_cancel_requests
                .iter()
                .flatten()
                .any(|r| r.tid == receiver_tid && r.asid == receiver_asid)
            {
                return (true, false);
            }
            // Prefer a free slot, otherwise a stale one.
            let target = ipc
                .shared_region_cancel_requests
                .iter()
                .position(|s| s.is_none())
                .or_else(|| stale_flags.iter().position(|&stale| stale));
            if let Some(idx) = target {
                ipc.shared_region_cancel_requests[idx] = Some(SharedRegionCancelReq {
                    tid: receiver_tid,
                    asid: receiver_asid,
                });
                (true, false)
            } else {
                // Cannot record → FAIL CLOSED: no transaction may publish past this cancellation.
                let newly_set = !ipc.shared_region_cancel_overflow;
                ipc.shared_region_cancel_overflow = true;
                (false, newly_set)
            }
        });
        if fuse_newly_set {
            crate::kernel::boot::maybe_log_shared_region_cancel_fuse_set();
        }
        recorded
    }

    /// Consume (one-shot) a matching cancellation request for `(receiver_tid, receiver_asid)`.
    /// Generation-bearing: BOTH the TID and the ASID must match.
    ///
    /// This does NOT clear the fail-closed overflow latch: `shared_region_cancel_now` checks the
    /// latch BEFORE calling consume, so a set latch already cancels every transaction unconditionally.
    /// Clearing it here on an unrelated consume would be unsafe — the specific cancellation that
    /// overflowed was never recorded, so once the latch cleared that receiver could publish (silent
    /// cancellation loss). The latch is therefore a permanent per-kernel-instance safety fuse.
    fn shared_region_consume_cancel(
        &mut self,
        receiver_tid: u64,
        receiver_asid: crate::kernel::vm::Asid,
    ) -> bool {
        self.with_ipc_state_mut(|ipc| {
            for slot in ipc.shared_region_cancel_requests.iter_mut() {
                if let Some(req) = *slot {
                    if req.tid == receiver_tid && req.asid == receiver_asid {
                        *slot = None;
                        return true;
                    }
                }
            }
            false
        })
    }

    fn shared_region_cancel_overflowed(&self) -> bool {
        self.with_ipc_state(|ipc| ipc.shared_region_cancel_overflow)
    }

    /// Phase A (UNDER the broad lock): consume the shared-region transfer envelope, TAKE OVER its
    /// object pin, resolve + attenuate the destination rights, and capture the receiver's
    /// generation-bearing authority. Fails closed (envelope reclaimed by the caller path) on any
    /// mismatch. Sender CSpace is resolved exactly ONCE here, never again.
    ///
    /// Origin-neutral (Stage 198E2B): `origin_direct` records whether this snapshot came from the
    /// direct (no-waiter, sender-side) or the queued (receiver-side dequeue) delivery path. It sets
    /// ONLY the `origin_direct` proof marker on the snapshot — it never influences classification,
    /// rights attenuation, mapping, rollback, lifecycle, or wake. Both origins converge on the SAME
    /// `shared_region_execute` executor with identical security/mapping semantics.
    pub(crate) fn shared_region_phase_a(
        &mut self,
        handle: u64,
        endpoint: CapObject,
        receiver_tid: u64,
        map_va: u64,
        map_write: bool,
        meta_ptr: u64,
        msg: Message,
        origin_direct: bool,
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
            origin_direct,
            msg,
        })
    }

    /// Post-lock executor: phases 2..10. Every failure converges on the single idempotent rollback
    /// and returns the typed error AFTER the transaction is `Cancelled`.
    ///
    /// Origin-neutral (Stage 198E2B): the SAME executor serves both the direct (no-waiter) and the
    /// queued (receiver-side dequeue) delivery paths. `snapshot.origin_direct` is a proof marker
    /// only — every security, classification, mapping-permission, rollback, lifecycle, and wake
    /// decision below is identical regardless of origin. There is exactly one shared-region
    /// transfer mechanism.
    pub(crate) fn shared_region_execute(
        &mut self,
        snapshot: RecvBoundarySharedRegionSnapshot,
    ) -> Result<SharedRegionDirectPublish, SharedRegionTxnError> {
        // The transaction logic lives ONCE in `run_shared_region_txn`, parameterised over a
        // `SharedRegionExecCtx`. This broad-borrow (`&mut KernelState`) context is used by the
        // hosted transaction/cancellation/rollback proofs; the production drain uses the off-lock
        // `SharedKernel` context (Stage 198E3B) — SAME logic, SAME single rollback.
        run_shared_region_txn(self, snapshot)
    }

    /// The SINGLE idempotent rollback executor. Safe to call from EVERY state; each undo step is
    /// guarded so nothing is unmapped or revoked twice. Reverse order:
    /// prevent-wake → unmap → TLB shootdown → remove active mapping → revoke cap → release pin →
    /// clear state.
    pub(crate) fn rollback_shared_region_direct_txn(&mut self, txn: &mut SharedRegionDirectTxn) {
        // The single idempotent rollback logic lives ONCE in `rollback_shared_region_txn`,
        // parameterised over a `SharedRegionExecCtx`. This broad-borrow context delegates to the
        // KernelState methods; the off-lock production context (Stage 198E3B) delegates to seams.
        rollback_shared_region_txn(self, txn);
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

// ── Stage 198E3B: single transaction implementation over a bounded execution context ──────────
//
// The shared-region post-lock transaction logic is written EXACTLY ONCE (`run_shared_region_txn`)
// plus ONE idempotent rollback (`rollback_shared_region_txn`), both parameterised over the
// `SharedRegionExecCtx` trait — there is no second transaction representation or rollback.
//
// Two contexts implement the trait:
//  - `KernelState` (broad borrow): used by the hosted transaction / cancellation / partial-map /
//    rollback proofs. The primitives delegate to the existing KernelState methods.
//  - the off-lock production context (Stage 198E3B, `SharedKernel`): each primitive acquires ONE
//    ranked domain lock (scheduler 1 / task 2 / IPC 3 / capability 4 / VM 5 / memory 6) and releases
//    it before the next phase, so the user copy and TLB completion run with NO lock held.
//
// The runner NEVER holds a broad borrow itself: it only calls context primitives + reads the
// deterministic test-fault statics (module free functions above).

/// Bounded execution primitives for the shared-region transaction. Each primitive is a single
/// ranked-lock operation (or an off-lock user copy); the runner sequences them with no lock held
/// across primitives. No primitive returns or retains a `&mut KernelState` / field reference.
pub(crate) trait SharedRegionExecCtx {
    /// rank 2: receiver TID live (not Dead/Exited) AND its ASID still equals the captured ASID.
    fn ctx_receiver_alive(&self, snap: &RecvBoundarySharedRegionSnapshot) -> bool;
    /// rank 4/6: the frozen source object is still live.
    fn ctx_object_live(&self, object: CapObject) -> bool;
    /// rank 3: the fail-closed cancellation overflow fuse is set.
    fn ctx_cancel_overflowed(&self) -> bool;
    /// rank 3: one-shot consume a matching (tid, asid) cancellation request.
    fn ctx_consume_cancel(&mut self, tid: u64, asid: crate::kernel::vm::Asid) -> bool;
    /// rank 4 (+6): mint one attenuated receiver-local cap. `Err(())` = cnode full / stale object.
    fn ctx_mint(
        &mut self,
        cnode: crate::kernel::capabilities::CNodeId,
        cap: Capability,
    ) -> Result<CapId, ()>;
    /// rank 3: register the provisional active-mapping entry BEFORE the first page maps.
    fn ctx_register_active_mapping(&mut self, tid: u64, cap: CapId, va: u64, len: usize) -> bool;
    /// rank 6: physical base of the shared region.
    fn ctx_phys_base(&self, object: CapObject) -> Option<PhysAddr>;
    /// rank 5 (+6): map exactly ONE user page (NX enforced by the caller's flags).
    fn ctx_map_page(
        &mut self,
        asid: crate::kernel::vm::Asid,
        virt: VirtAddr,
        mapping: Mapping,
    ) -> bool;
    /// OFF-LOCK: copy the recv-v2 meta to user memory with NO kernel lock held.
    fn ctx_copy_meta(&mut self, asid: crate::kernel::vm::Asid, va: VirtAddr, bytes: &[u8]) -> bool;
    /// rank 1: publish exactly one receiver wake through the scheduler.
    fn ctx_wake(&mut self, tid: u64) -> bool;
    /// rank 6: release the transferred object pin (once).
    fn ctx_release_pin(&mut self, object: CapObject);
    /// rank 5 + TLB: unmap the mapped prefix via the two-phase shootdown-before-reclaim contract.
    fn ctx_unmap_prefix(&mut self, asid: crate::kernel::vm::Asid, base: usize, len: usize);
    /// rank 3: remove the provisional active-mapping entry (guarded).
    fn ctx_remove_active_mapping(&mut self, tid: u64, cap: CapId) -> bool;
    /// rank 4 (+6): revoke the provisional receiver-local cap (guarded).
    fn ctx_revoke_cap(&mut self, cnode: crate::kernel::capabilities::CNodeId, cap: CapId);
}

/// Cancellation-authoritative fold (single definition): overflow fuse → test hook → one-shot
/// consume → dead/generation-replaced receiver. Consumes any pending matching request.
fn cancel_now<C: SharedRegionExecCtx>(
    ctx: &mut C,
    snap: &RecvBoundarySharedRegionSnapshot,
    checkpoint: usize,
) -> bool {
    if ctx.ctx_cancel_overflowed() {
        return true;
    }
    if hook_cancel_at(checkpoint) {
        return true;
    }
    if ctx.ctx_consume_cancel(snap.receiver_tid, snap.receiver_asid) {
        return true;
    }
    !ctx.ctx_receiver_alive(snap)
}

/// The SINGLE post-lock transaction runner (phases 1..10). Every failure converges on the single
/// idempotent rollback and returns the typed error AFTER the transaction is `Cancelled`.
pub(crate) fn run_shared_region_txn<C: SharedRegionExecCtx>(
    ctx: &mut C,
    snapshot: RecvBoundarySharedRegionSnapshot,
) -> Result<SharedRegionDirectPublish, SharedRegionTxnError> {
    let mut txn = SharedRegionDirectTxn {
        state: SharedRegionTxnState::Reserved,
        snapshot,
        minted_cap: None,
        mapped: None,
        mapped_prefix_len: 0,
    };

    // Phase 1: revalidate receiver generation-authority BEFORE reserving anything.
    if !ctx.ctx_receiver_alive(&txn.snapshot) {
        rollback_shared_region_txn(ctx, &mut txn);
        return Err(SharedRegionTxnError::ReceiverGone);
    }
    // Phase 1b: object generation still live.
    if !ctx.ctx_object_live(txn.snapshot.object) {
        rollback_shared_region_txn(ctx, &mut txn);
        return Err(SharedRegionTxnError::GenerationReplaced);
    }
    // Checkpoint 1 — before cap mint.
    if cancel_now(ctx, &txn.snapshot, 1) {
        txn.state = SharedRegionTxnState::CancelRequested;
        rollback_shared_region_txn(ctx, &mut txn);
        return Err(SharedRegionTxnError::Cancelled);
    }

    // Bounds check (DmaRegion / region len) up front.
    let region_len = match usize::try_from(txn.snapshot.descriptor.len) {
        Ok(v) if v > 0 => v,
        _ => {
            rollback_shared_region_txn(ctx, &mut txn);
            return Err(SharedRegionTxnError::BadRegion);
        }
    };
    if txn.snapshot.map_va == 0 || !txn.snapshot.map_va.is_multiple_of(PAGE_SIZE as u64) {
        rollback_shared_region_txn(ctx, &mut txn);
        return Err(SharedRegionTxnError::BadRegion);
    }
    // Rights gates (object-authoritative): MAP required; WRITE only with canonical WRITE.
    if !txn.snapshot.rights.contains(CapRights::MAP) {
        rollback_shared_region_txn(ctx, &mut txn);
        return Err(SharedRegionTxnError::MissingMapRight);
    }
    if txn.snapshot.map_write && !txn.snapshot.rights.contains(CapRights::WRITE) {
        rollback_shared_region_txn(ctx, &mut txn);
        return Err(SharedRegionTxnError::MissingWriteRight);
    }

    // Phase 3: mint ONE fresh receiver-local cap with the attenuated rights.
    let minted = match ctx.ctx_mint(
        txn.snapshot.receiver_cnode,
        Capability::new(txn.snapshot.object, txn.snapshot.rights),
    ) {
        Ok(cap) => cap,
        Err(()) => {
            rollback_shared_region_txn(ctx, &mut txn);
            return Err(SharedRegionTxnError::CnodeFull);
        }
    };
    txn.minted_cap = Some(minted);
    txn.state = SharedRegionTxnState::CapMinted;

    // Phase 4: register the provisional active mapping BEFORE the first page maps, so process-exit
    // cleanup owns any partial mapping (no untracked window). NX enforced; writable only with WRITE.
    let mapped_len = page_round_up(region_len);
    if !ctx.ctx_register_active_mapping(
        txn.snapshot.receiver_tid,
        minted,
        txn.snapshot.map_va,
        mapped_len,
    ) {
        rollback_shared_region_txn(ctx, &mut txn);
        return Err(SharedRegionTxnError::Internal);
    }
    txn.mapped = Some((txn.snapshot.map_va, mapped_len));
    txn.state = SharedRegionTxnState::Mapping;

    let phys_start = match ctx.ctx_phys_base(txn.snapshot.object) {
        Some(p) => p,
        None => {
            rollback_shared_region_txn(ctx, &mut txn);
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

    // Checkpoint 2 — before the FIRST map.
    if cancel_now(ctx, &txn.snapshot, 2) {
        txn.state = SharedRegionTxnState::CancelRequested;
        rollback_shared_region_txn(ctx, &mut txn);
        return Err(SharedRegionTxnError::Cancelled);
    }

    for i in 0..num_pages {
        // Checkpoint 3 — BETWEEN page mappings (before mapping page `i`). Once cancellation is
        // authoritative, NO further page is mapped: rollback unmaps exactly the prefix so far.
        if i > 0 && (hook_cancel_at_page(i) || cancel_now(ctx, &txn.snapshot, 3)) {
            txn.state = SharedRegionTxnState::CancelRequested;
            rollback_shared_region_txn(ctx, &mut txn);
            return Err(SharedRegionTxnError::Cancelled);
        }
        let virt = VirtAddr(txn.snapshot.map_va + (i * PAGE_SIZE) as u64);
        let phys = PhysAddr(phys_start.0 + (i * PAGE_SIZE) as u64);
        let fault = hook_force_map_fault() || hook_map_fault_at_page(i);
        if fault
            || !ctx.ctx_map_page(
                txn.snapshot.receiver_asid,
                virt,
                Mapping {
                    phys,
                    flags: map_flags,
                },
            )
        {
            // Page `i` failed after pages 0..i succeeded: rollback unmaps exactly that prefix.
            rollback_shared_region_txn(ctx, &mut txn);
            return Err(SharedRegionTxnError::MapFault);
        }
        // AUTHORITATIVE progress update AFTER each successful page, BEFORE the next.
        txn.mapped_prefix_len = (i + 1) * PAGE_SIZE;
    }
    // Phase 5/6: mapping complete (fresh maps need no TLB shootdown; only rollback unmaps do).
    txn.state = SharedRegionTxnState::Mapped;

    // Checkpoint 4 — after mapping, before writeback.
    if cancel_now(ctx, &txn.snapshot, 4) {
        txn.state = SharedRegionTxnState::CancelRequested;
        rollback_shared_region_txn(ctx, &mut txn);
        return Err(SharedRegionTxnError::Cancelled);
    }
    // Checkpoint 5 — before the user writeback. After cancellation no writeback may occur.
    if cancel_now(ctx, &txn.snapshot, 5) {
        txn.state = SharedRegionTxnState::CancelRequested;
        rollback_shared_region_txn(ctx, &mut txn);
        return Err(SharedRegionTxnError::Cancelled);
    }

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
    let copy_ok = !hook_force_copy_fault()
        && ctx.ctx_copy_meta(
            txn.snapshot.receiver_asid,
            VirtAddr(txn.snapshot.meta_ptr),
            &meta,
        );
    if !copy_ok {
        rollback_shared_region_txn(ctx, &mut txn);
        return Err(SharedRegionTxnError::CopyFault);
    }

    // Phase 8 / checkpoint 6 — IMMEDIATELY before publication and wake: revalidate receiver
    // generation + transaction ownership + any pending cancellation. Nothing is published if
    // cancellation is authoritative here.
    if hook_force_stale_after_map()
        || !ctx.ctx_object_live(txn.snapshot.object)
        || cancel_now(ctx, &txn.snapshot, 6)
    {
        rollback_shared_region_txn(ctx, &mut txn);
        return Err(SharedRegionTxnError::StalePublish);
    }

    // Phase 9/10: publish + wake receiver exactly once, then release the transfer pin (the
    // receiver-local cap now owns the object reference).
    let woke = ctx.ctx_wake(txn.snapshot.receiver_tid);
    if txn.snapshot.pin_owned {
        ctx.ctx_release_pin(txn.snapshot.object);
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

/// The SINGLE idempotent rollback. Safe from EVERY state; each undo step is guarded so nothing is
/// unmapped, revoked, or pin-released twice. Reverse order: prevent-wake → unmap prefix (two-phase
/// shootdown-before-reclaim) → remove active mapping → revoke cap → release pin → clear state.
pub(crate) fn rollback_shared_region_txn<C: SharedRegionExecCtx>(
    ctx: &mut C,
    txn: &mut SharedRegionDirectTxn,
) {
    // One-shot ownership: a Published txn is NEVER rolled back; a Cancelled txn is already fully
    // unwound, so a second/third call is a no-op.
    if txn.state == SharedRegionTxnState::Published || txn.state == SharedRegionTxnState::Cancelled
    {
        return;
    }
    // Claim cleanup ownership (protocol A: the executor is the single cleanup owner).
    txn.state = SharedRegionTxnState::CleanupOwned;

    // (1) Publication/wake is simply never performed on this path.
    // (2) Unmap EXACTLY the successfully-mapped page prefix (two-phase; the shootdown completes
    // before frames are freed). Does not depend on reaching the terminal `Mapped` state.
    if let Some((base, _)) = txn.mapped {
        if txn.mapped_prefix_len > 0 {
            ctx.ctx_unmap_prefix(
                txn.snapshot.receiver_asid,
                base as usize,
                txn.mapped_prefix_len,
            );
        }
        txn.mapped_prefix_len = 0;
        txn.mapped = None;
    }
    // (3) Remove the provisional/active mapping registry entry (guarded).
    if let Some(cap) = txn.minted_cap {
        let _ = ctx.ctx_remove_active_mapping(txn.snapshot.receiver_tid, cap);
    }
    // (4) Revoke the provisional receiver cap (once — guarded by take()).
    if let Some(cap) = txn.minted_cap.take() {
        ctx.ctx_revoke_cap(txn.snapshot.receiver_cnode, cap);
    }
    // (5) Release the transferred object pin (once — guarded by pin_owned). NEVER before the unmap
    // + required shootdown above complete.
    if txn.snapshot.pin_owned {
        ctx.ctx_release_pin(txn.snapshot.object);
        txn.snapshot.pin_owned = false;
    }
    txn.state = SharedRegionTxnState::Cancelled;
}

impl SharedRegionExecCtx for KernelState {
    fn ctx_receiver_alive(&self, snap: &RecvBoundarySharedRegionSnapshot) -> bool {
        self.shared_region_receiver_alive(snap)
    }
    fn ctx_object_live(&self, object: CapObject) -> bool {
        self.capability_object_live(object).is_some()
    }
    fn ctx_cancel_overflowed(&self) -> bool {
        self.shared_region_cancel_overflowed()
    }
    fn ctx_consume_cancel(&mut self, tid: u64, asid: crate::kernel::vm::Asid) -> bool {
        self.shared_region_consume_cancel(tid, asid)
    }
    fn ctx_mint(
        &mut self,
        cnode: crate::kernel::capabilities::CNodeId,
        cap: Capability,
    ) -> Result<CapId, ()> {
        self.mint_capability_in_cnode(cnode, cap).map_err(|_| ())
    }
    fn ctx_register_active_mapping(&mut self, tid: u64, cap: CapId, va: u64, len: usize) -> bool {
        self.register_active_transfer_mapping(ThreadId(tid), cap, VirtAddr(va), len)
            .is_ok()
    }
    fn ctx_phys_base(&self, object: CapObject) -> Option<PhysAddr> {
        self.shared_region_phys_base(object)
    }
    fn ctx_map_page(
        &mut self,
        asid: crate::kernel::vm::Asid,
        virt: VirtAddr,
        mapping: Mapping,
    ) -> bool {
        self.map_user_page_in_asid_raw(asid, virt, mapping).is_ok()
    }
    fn ctx_copy_meta(&mut self, asid: crate::kernel::vm::Asid, va: VirtAddr, bytes: &[u8]) -> bool {
        self.copy_to_user(asid, va, bytes).is_ok()
    }
    fn ctx_wake(&mut self, tid: u64) -> bool {
        self.apply_split_receiver_wake_plan(ThreadId(tid)).is_ok()
    }
    fn ctx_release_pin(&mut self, object: CapObject) {
        self.adjust_memory_object_pin_refcount(object, -1);
    }
    fn ctx_unmap_prefix(&mut self, asid: crate::kernel::vm::Asid, base: usize, len: usize) {
        self.unmap_range_two_phase(asid, base, len);
    }
    fn ctx_remove_active_mapping(&mut self, tid: u64, cap: CapId) -> bool {
        self.remove_active_transfer_mapping(ThreadId(tid), cap)
    }
    fn ctx_revoke_cap(&mut self, cnode: crate::kernel::capabilities::CNodeId, cap: CapId) {
        let _ = self.revoke_capability_in_cnode(cnode, cap);
    }
}
