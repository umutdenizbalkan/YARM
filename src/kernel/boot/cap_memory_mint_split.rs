// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Stage 186D-proper — capability↔memory atomic mint/refcount discipline.
//!
//! Stage 186D-prereq audited the received-cap materialization engine and stopped
//! honestly: minting a cap that references a memory-backed object couples the
//! capability/cnode domain (rank 4) with the memory-object `cap_refcount` domain
//! (rank 6). Under the global `SpinLock<KernelState>` those two mutations sit in
//! one critical section, so nothing can observe a half-state. A future
//! cap-transfer materialization *seam* runs under per-domain marker locks
//! instead, which makes the window between "cnode slot installed" and
//! "memory-object refcount bumped" real — and a concurrent
//! `reclaim_memory_object_if_unreferenced` in that window could free an object
//! that a freshly-published cnode slot already references (use-after-free).
//!
//! This module builds the **atomicity discipline** for that mint as reusable,
//! seam-only infrastructure — it does **not** convert any live path.
//!
//! # Atomicity model — Model A ("pre-bump then install")
//!
//! 1. **Phase 1 (rank 6, memory):** under `with_memory_split_mut`, validate the
//!    target memory-object is still live and increment its `cap_refcount` by one.
//!    The object is now protected against reclaim *before any cnode slot can
//!    reference it*. For objects that carry no memory refcount (`Reply`,
//!    `Endpoint`, `Notification`, …) this is a no-op — their liveness is the
//!    caller's / IPC's concern, out of scope here.
//! 2. **Phase 2 (rank 4, capability):** under `with_capability_state_split_mut`,
//!    publish the capability into an existing destination cnode slot, minting a
//!    fresh **receiver-local** `CapId`.
//! 3. **Rollback:** if Phase 2 fails (`CapabilityFull`, or the destination cnode
//!    space is absent → `TaskMissing`), drop the Phase-1 `cap_refcount` bump so a
//!    failed mint leaks nothing.
//!
//! Why pre-bump: at every instant a published cnode slot exists, the object's
//! `cap_refcount` already protects it, so no reclaim race is possible. The only
//! transient is a briefly over-counted refcount when Phase 2 fails — harmless (it
//! can only *delay* a reclaim, never cause a premature free) and always rolled
//! back.
//!
//! Why this is deadlock-free despite acquiring rank 6 before rank 4: the two
//! critical sections are **disjoint** — Phase 1 fully releases the memory lock
//! before Phase 2 acquires the capability lock. This helper never holds two
//! subsystem locks at once, so it cannot be part of any lock-ordering cycle. It
//! also never acquires or holds `ipc_state_lock` (rank 3), so it introduces no
//! cap→IPC rank inversion and materializes no cap under the IPC lock.
//!
//! # Scope / status
//!
//! `M2_SEAM_HELPER_ONLY` — NOT wired into `ipc_reply`, `ipc_send`/`recv`/`call`,
//! or the cap-transfer materialization path. It does not by itself retire the
//! global lock and does not solve the reply-cap IPC rank-inversion blocker (the
//! reply arm still records waiter-cap metadata back into IPC rank 3 after the
//! rank-4 mint — deferred). A future cap-transfer seam is meant to be built *on
//! top of* this helper. See `doc/KERNEL_UNLOCKING.md` (Stage 186D-proper).

use super::*;

/// Outcome of the rank-6 memory-object protection phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MintMemoryRef {
    /// The object carries no memory-object `cap_refcount` (`Reply` / `Endpoint`
    /// / `Notification` / …). Nothing was protected and nothing must be rolled
    /// back on the memory side.
    NotMemoryBacked,
    /// A live `MemoryObject` / `DmaRegion` whose `cap_refcount` was incremented
    /// by exactly one. Must be rolled back if the subsequent slot install fails.
    Bumped,
}

impl crate::runtime::SharedKernel {
    /// Stage 186D-proper — atomically mint `capability` into an **existing**
    /// destination cnode while keeping the referenced memory-object's
    /// `cap_refcount` and the published cnode slot mutually consistent.
    ///
    /// Seam-only infrastructure (`M2_SEAM_HELPER_ONLY`): uses ONLY the rank-6
    /// memory seam and the rank-4 capability seam — never a broad
    /// `&mut KernelState`, never `ipc_state_lock`. Returns a fresh
    /// **receiver-local** `CapId`; it never echoes a sender-local CapId as
    /// authority (it takes an already-formed `Capability`, i.e. object + rights,
    /// not a foreign `CapId`).
    ///
    /// Rights derivation / `WrongObject` / `MissingRight` are the caller's
    /// concern and happen *before* this helper (which mints an already-attenuated
    /// `Capability`); this helper never converts any error into `Ok`.
    ///
    /// Errors (all real, never hidden):
    /// - `StaleCapability` — the memory-backed object is no longer live (Phase 1).
    /// - `CapabilityFull` — the destination cnode's cspace is full (Phase 2).
    /// - `TaskMissing` — the destination cnode space is not provisioned. Space
    ///   provisioning (`ensure_cnode_space`) is a caller precondition and is
    ///   deliberately out of this helper's atomic scope.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn mint_capability_with_memory_ref_split(
        &self,
        cnode: CNodeId,
        capability: Capability,
    ) -> Result<CapId, KernelError> {
        // Phase 1 (rank 6): protect the object BEFORE any slot can reference it.
        let mem_ref = self.bump_memory_ref_for_mint_split(capability.object)?;

        // Phase 2 (rank 4): publish the receiver-local cnode slot.
        match self.install_cnode_slot_for_mint_split(cnode, capability) {
            Ok(cap_id) => Ok(cap_id),
            Err(install_err) => {
                // Rollback the Phase-1 protection so a failed mint leaks no
                // refcount. Reply/endpoint objects took no memory ref → no-op.
                if matches!(mem_ref, MintMemoryRef::Bumped) {
                    self.rollback_memory_ref_for_mint_split(capability.object);
                }
                Err(install_err)
            }
        }
    }

    /// Phase 1 (rank 6): validate liveness and bump `cap_refcount` atomically
    /// under the memory lock. A memory-backed object that is no longer live is
    /// rejected with `StaleCapability` — the helper never publishes a cap for a
    /// reclaimed object. Non-memory-backed objects are a no-op.
    fn bump_memory_ref_for_mint_split(
        &self,
        object: CapObject,
    ) -> Result<MintMemoryRef, KernelError> {
        let id = match object {
            CapObject::MemoryObject { id } | CapObject::DmaRegion { id, .. } => id,
            _ => return Ok(MintMemoryRef::NotMemoryBacked),
        };
        self.with_memory_split_mut(|memory| {
            let Some(slot) = memory
                .memory_objects
                .iter()
                .position(|entry| entry.is_some_and(|mem| mem.id == id))
            else {
                return Err(KernelError::StaleCapability);
            };
            // `slot` came from a positive `is_some_and` match, so the entry is
            // `Some`; bump its cap_refcount by exactly one under this lock.
            if let Some(mem) = memory.memory_objects[slot].as_mut() {
                mem.cap_refcount = mem.cap_refcount.saturating_add(1);
            }
            Ok(MintMemoryRef::Bumped)
        })
    }

    /// Phase 2 (rank 4): mint `capability` into an existing destination cnode
    /// slot, producing a fresh receiver-local `CapId`. A missing cnode space is a
    /// real `TaskMissing` (precondition unmet), never hidden as success; a full
    /// cspace is a real `CapabilityFull`.
    fn install_cnode_slot_for_mint_split(
        &self,
        cnode: CNodeId,
        capability: Capability,
    ) -> Result<CapId, KernelError> {
        self.with_capability_state_split_mut(|cap| {
            let space = cap
                .cnode_spaces
                .iter_mut()
                .flatten()
                .find(|space| space.id == cnode)
                .ok_or(KernelError::TaskMissing)?;
            kernel_mut(&mut space.cspace)
                .mint(capability)
                .map_err(|_| KernelError::CapabilityFull)
        })
    }

    /// Rollback of Phase 1 (rank 6): drop the `cap_refcount` bump when Phase 2
    /// failed, so the increment/decrement stays symmetric (exactly one each).
    fn rollback_memory_ref_for_mint_split(&self, object: CapObject) {
        let id = match object {
            CapObject::MemoryObject { id } | CapObject::DmaRegion { id, .. } => id,
            _ => return,
        };
        self.with_memory_split_mut(|memory| {
            if let Some(slot) = memory
                .memory_objects
                .iter()
                .position(|entry| entry.is_some_and(|mem| mem.id == id))
                && let Some(mem) = memory.memory_objects[slot].as_mut()
            {
                mem.cap_refcount = mem.cap_refcount.saturating_sub(1);
            }
        });
    }
}
