// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::*;

/// 199G-C §2 — proof that ONE transfer-object pin was acquired, and the authority to release
/// exactly that one.
///
/// Unforgeable: the field is private to this module and the only constructor is
/// [`KernelState::acquire_transfer_pin_locked`], which mints a token only after it has actually
/// incremented the refcount. So a token cannot name a pin that was never taken, and a release
/// driven by a token cannot underflow a counter this acquire never raised.
///
/// It is `Copy` for the same reason every other by-value proof in the split transactions is:
/// the transaction records must be snapshottable. Single-use is enforced by the ENVELOPE
/// lifecycle — an envelope owes exactly one pin and is consumed exactly once — not by move
/// semantics, which is the same discipline `take_transfer_envelope`'s `-1` has always relied on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TransferPinToken {
    object: CapObject,
}

impl TransferPinToken {
    /// The exact object whose pin this token releases.
    #[must_use]
    pub(crate) const fn object(self) -> CapObject {
        self.object
    }
}

/// 199G-C §2 — why an acquire refused. Every variant is raised BEFORE any mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TransferPinRefusal {
    /// Not a pinnable class. `MemoryObject` and `DmaRegion` are the only objects that carry a
    /// `pin_refcount`; every other capability class reaches the pin path as a no-op, which is
    /// why this is a distinct answer from a failure.
    NotPinnable,
    /// No live memory object carries this id any more — a stale or already-reclaimed object.
    Stale,
    /// The refcount is at `u32::MAX`. The broad path SATURATES here, silently returning a pin
    /// the matching release would then under-drop; the split acquire refuses instead, before
    /// touching the counter.
    Overflow,
}

impl KernelState {
    pub(crate) fn memory_object_slot_by_id(&self, id: u64) -> Option<usize> {
        self.with_memory_state(|memory| {
            memory
                .memory_objects
                .iter()
                .position(|entry| entry.is_some_and(|mem| mem.id == id))
        })
    }

    pub(crate) fn adjust_memory_object_cap_refcount(&mut self, object: CapObject, delta: i32) {
        let id = match object {
            CapObject::MemoryObject { id } | CapObject::DmaRegion { id, .. } => id,
            _ => return,
        };
        let Some(slot) = self.memory_object_slot_by_id(id) else {
            return;
        };
        self.with_memory_state_mut(|memory| {
            if let Some(memory_object) = memory.memory_objects[slot].as_mut() {
                if delta > 0 {
                    memory_object.cap_refcount =
                        memory_object.cap_refcount.saturating_add(delta as u32);
                } else {
                    memory_object.cap_refcount =
                        memory_object.cap_refcount.saturating_sub((-delta) as u32);
                }
            }
        });
    }

    /// U9-D3 §6: `&mut MemorySubsystem` sibling of [`Self::adjust_memory_object_cap_refcount`] for
    /// use inside `SharedKernel::with_memory_split_mut` (rank 6 only). Byte-identical semantics —
    /// the broad form is three `with_memory_state(_mut)` cycles (slot lookup, then mutate) and this
    /// is the same find-by-id → saturating adjust with the one acquisition the seam already holds.
    /// `adjust_memory_object_cap_refcount` itself is left unmodified.
    pub(crate) fn adjust_memory_object_cap_refcount_locked(
        memory: &mut MemorySubsystem,
        object: CapObject,
        delta: i32,
    ) {
        let id = match object {
            CapObject::MemoryObject { id } | CapObject::DmaRegion { id, .. } => id,
            _ => return,
        };
        if let Some(slot) = memory
            .memory_objects
            .iter()
            .position(|entry| entry.is_some_and(|mem| mem.id == id))
            && let Some(memory_object) = memory.memory_objects[slot].as_mut()
        {
            if delta > 0 {
                memory_object.cap_refcount =
                    memory_object.cap_refcount.saturating_add(delta as u32);
            } else {
                memory_object.cap_refcount =
                    memory_object.cap_refcount.saturating_sub((-delta) as u32);
            }
        }
    }

    /// U9-MO2 — THE release of one MemoryObject slot whose refcounts are already zero.
    ///
    /// Every reclaim path funnels here, so the backing-ownership rule is applied once instead
    /// of four times. Before this existed all four sites called `free_frame(object.phys)`
    /// unconditionally, which is correct only for allocator-owned backing:
    ///
    /// * `AllocatorOwned` — return the EXACT extent that was taken. The constructors allocate
    ///   `len / PAGE_SIZE` pages contiguously, so that is what is returned. The previous
    ///   `free_frame` returned a single page and under-freed every multi-page object.
    /// * `Borrowed` — remove the registry slot and touch the allocator NEVER. An initramfs
    ///   slice's phys is inside the boot initrd; handing it to the allocator would insert
    ///   memory it never owned into the free list.
    ///
    /// The caller has already verified exact identity and all three zero refcounts; this
    /// performs the release and nothing else, so the "may I?" and "do it" halves stay separate.
    pub(crate) fn release_memory_object_slot_locked(
        memory: &mut MemorySubsystem,
        slot_index: usize,
    ) {
        let Some(object) = memory.memory_objects[slot_index] else {
            return;
        };
        match object.kind.backing() {
            crate::kernel::boot::MemoryBacking::AllocatorOwned => {
                let pages = object.len / crate::kernel::vm::PAGE_SIZE;
                if pages > 0 {
                    let _ = kernel_mut(&mut memory.frame_allocator)
                        .free_contiguous(object.phys.0, pages);
                }
            }
            // Borrowed: the extent outlives this object and belongs to something else.
            crate::kernel::boot::MemoryBacking::Borrowed => {}
        }
        memory.memory_objects[slot_index] = None;
    }

    /// U9-D3 §6: `&mut MemorySubsystem` sibling of [`Self::reclaim_memory_object_if_unreferenced`]
    /// for use inside `SharedKernel::with_memory_split_mut` (rank 6 only). Same class gate, same
    /// all-three-refcounts-zero condition, same `free_frame` + slot clear.
    /// `reclaim_memory_object_if_unreferenced` itself is left unmodified.
    pub(crate) fn reclaim_memory_object_if_unreferenced_locked(
        memory: &mut MemorySubsystem,
        object: CapObject,
    ) {
        let id = match object {
            CapObject::MemoryObject { id } | CapObject::DmaRegion { id, .. } => id,
            _ => return,
        };
        let Some(slot_index) = memory
            .memory_objects
            .iter()
            .position(|entry| entry.is_some_and(|mem| mem.id == id))
        else {
            return;
        };
        let Some(memory_object) = memory.memory_objects[slot_index] else {
            return;
        };
        if memory_object.cap_refcount != 0
            || memory_object.map_refcount != 0
            || memory_object.pin_refcount != 0
        {
            return;
        }
        Self::release_memory_object_slot_locked(memory, slot_index);
    }

    pub(crate) fn adjust_memory_object_pin_refcount(&mut self, object: CapObject, delta: i32) {
        let id = match object {
            CapObject::MemoryObject { id } | CapObject::DmaRegion { id, .. } => id,
            _ => return,
        };
        let Some(slot) = self.memory_object_slot_by_id(id) else {
            return;
        };
        self.with_memory_state_mut(|memory| {
            if let Some(memory_object) = memory.memory_objects[slot].as_mut() {
                if delta > 0 {
                    memory_object.pin_refcount =
                        memory_object.pin_refcount.saturating_add(delta as u32);
                } else {
                    memory_object.pin_refcount =
                        memory_object.pin_refcount.saturating_sub((-delta) as u32);
                }
            }
        });
    }

    pub(crate) fn note_mapping_inserted(&mut self, phys: PhysAddr) {
        self.with_memory_state_mut(|memory| {
            if let Some(slot) = memory
                .memory_objects
                .iter()
                .position(|entry| entry.is_some_and(|mem| mem.phys == phys))
                && let Some(memory_object) = memory.memory_objects[slot].as_mut()
            {
                memory_object.map_refcount = memory_object.map_refcount.saturating_add(1);
            }
        });
    }

    /// Stage 198E3B2A: `&mut MemorySubsystem` sibling of [`Self::note_mapping_inserted`] for use
    /// inside `SharedKernel::with_memory_split_mut` (rank 6 only). Byte-identical semantics;
    /// `note_mapping_inserted` is left unmodified.
    pub(crate) fn note_mapping_inserted_locked(memory: &mut MemorySubsystem, phys: PhysAddr) {
        if let Some(slot) = memory
            .memory_objects
            .iter()
            .position(|entry| entry.is_some_and(|mem| mem.phys == phys))
            && let Some(memory_object) = memory.memory_objects[slot].as_mut()
        {
            memory_object.map_refcount = memory_object.map_refcount.saturating_add(1);
        }
    }

    /// Stage 198E3B2A: `&mut MemorySubsystem` sibling of [`Self::adjust_memory_object_pin_refcount`]
    /// for use inside `SharedKernel::with_memory_split_mut` (rank 6 only). Byte-identical semantics.
    pub(crate) fn adjust_memory_object_pin_refcount_locked(
        memory: &mut MemorySubsystem,
        object: CapObject,
        delta: i32,
    ) {
        let id = match object {
            CapObject::MemoryObject { id } | CapObject::DmaRegion { id, .. } => id,
            _ => return,
        };
        if let Some(slot) = memory
            .memory_objects
            .iter()
            .position(|entry| entry.is_some_and(|mem| mem.id == id))
            && let Some(memory_object) = memory.memory_objects[slot].as_mut()
        {
            if delta > 0 {
                memory_object.pin_refcount =
                    memory_object.pin_refcount.saturating_add(delta as u32);
            } else {
                memory_object.pin_refcount =
                    memory_object.pin_refcount.saturating_sub((-delta) as u32);
            }
        }
    }

    /// 199G-C §2 — THE rank-6 pin ACQUIRE, and the exact counterpart of the `-1` that
    /// `take_transfer_envelope` and `sr_release_pin_split` perform.
    ///
    /// Reachable through `SharedKernel::with_memory_split_mut` (rank 6 only), exactly as the
    /// release already is. What it does, in full:
    ///
    /// 1. classify the object — only `MemoryObject` / `DmaRegion` carry a `pin_refcount`;
    /// 2. revalidate that exact object id against the live memory table;
    /// 3. CHECKED increment;
    /// 4. mint the release authority.
    ///
    /// What it does NOT do, and what makes the AI_AGENT_RULES §14.4 D3 fence inapplicable: no
    /// page-table read or write, no map or unmap, no TLB operation, no frame allocation or
    /// reclaim, no address-space or cross-address-space inspection, and no scheduler / IPC /
    /// capability mutation while rank 6 is held. It moves one `u32` in `MemorySubsystem` and
    /// nothing else. The fence governs a seam that must reach a shootdown before reclaiming a
    /// frame; a pin raises a refcount and reclaims nothing, so there is no shootdown for it to
    /// owe. (It is also not a §14.5 live-wiring: `with_memory_split_mut` was already live for
    /// this very counter in the release direction.)
    ///
    /// The one deliberate divergence from the broad `+1` is the overflow answer. The broad path
    /// `saturating_add`s, so at `u32::MAX` it hands back a pin the matching `-1` would then
    /// under-drop; this refuses before mutation instead. Refusing is the fail-safe direction —
    /// the send fails and no envelope is created — whereas saturating silently breaks the
    /// pairing.
    pub(crate) fn acquire_transfer_pin_locked(
        memory: &mut MemorySubsystem,
        object: CapObject,
    ) -> Result<TransferPinToken, TransferPinRefusal> {
        let id = match object {
            CapObject::MemoryObject { id } | CapObject::DmaRegion { id, .. } => id,
            _ => return Err(TransferPinRefusal::NotPinnable),
        };
        let slot = memory
            .memory_objects
            .iter()
            .position(|entry| entry.is_some_and(|mem| mem.id == id))
            .ok_or(TransferPinRefusal::Stale)?;
        let memory_object = memory.memory_objects[slot]
            .as_mut()
            .ok_or(TransferPinRefusal::Stale)?;
        // Checked, and checked BEFORE the store: on refusal the counter is untouched.
        let next = memory_object
            .pin_refcount
            .checked_add(1)
            .ok_or(TransferPinRefusal::Overflow)?;
        memory_object.pin_refcount = next;
        Ok(TransferPinToken { object })
    }

    /// 199G-C §2 — the release half driven by a token, so a release cannot name an object no
    /// acquire ever pinned. Delegates to the SAME counter update the broad `-1` uses; the token
    /// only decides *which* object, never *whether*.
    pub(crate) fn release_transfer_pin_locked(
        memory: &mut MemorySubsystem,
        token: TransferPinToken,
    ) {
        Self::adjust_memory_object_pin_refcount_locked(memory, token.object(), -1);
    }

    /// 199G-C §2 — THE pin policy, shared by the broad and split envelope stashes.
    ///
    /// A transfer envelope owes exactly one object pin **iff it carries a shared-region
    /// descriptor** — not because it carries a transfer capability. Every other transfer class
    /// (an ordinary Endpoint or Notification cap, a Reply cap, and even a `MemoryObject` /
    /// `DmaRegion` cap sent with a payload small enough to take an inline arm) creates an
    /// envelope with no pin at all. `stash_transfer_envelope`'s `+1` and
    /// `take_transfer_envelope`'s `-1` are both already spelled this way; naming it once is what
    /// stops the split stash from drifting into pinning "all transfer caps".
    #[must_use]
    pub(crate) const fn transfer_envelope_owes_pin(
        shared_region: Option<TransferSharedRegion>,
    ) -> bool {
        shared_region.is_some()
    }

    /// Stage 198E3B2A: `&MemorySubsystem` physical-base lookup of a frozen shared-region object for
    /// use inside `SharedKernel::with_memory_split_mut` (rank 6 only). Mirrors
    /// `KernelState::shared_region_phys_base`.
    pub(crate) fn shared_region_phys_base_locked(
        memory: &MemorySubsystem,
        object: CapObject,
    ) -> Option<PhysAddr> {
        let (id, offset) = match object {
            CapObject::MemoryObject { id } => (id, 0u64),
            CapObject::DmaRegion { id, offset, .. } => (id, offset),
            _ => return None,
        };
        memory
            .memory_objects
            .iter()
            .flatten()
            .find(|e| e.id == id)
            .map(|e| PhysAddr(e.phys.0 + offset))
    }

    pub(crate) fn note_mapping_removed(&mut self, phys: PhysAddr) {
        self.with_memory_state_mut(|memory| {
            if let Some(slot) = memory
                .memory_objects
                .iter()
                .position(|entry| entry.is_some_and(|mem| mem.phys == phys))
                && let Some(memory_object) = memory.memory_objects[slot].as_mut()
            {
                memory_object.map_refcount = memory_object.map_refcount.saturating_sub(1);
            }
        });
    }

    /// Stage 114 / D-NEXT-2: byte-identical sibling of [`Self::note_mapping_removed`]
    /// taking `&mut MemorySubsystem` directly for use inside
    /// `SharedKernel::with_memory_split_mut`'s closure. `note_mapping_removed`
    /// is left unmodified.
    pub(crate) fn note_mapping_removed_locked(memory: &mut MemorySubsystem, phys: PhysAddr) {
        if let Some(slot) = memory
            .memory_objects
            .iter()
            .position(|entry| entry.is_some_and(|mem| mem.phys == phys))
            && let Some(memory_object) = memory.memory_objects[slot].as_mut()
        {
            memory_object.map_refcount = memory_object.map_refcount.saturating_sub(1);
        }
    }

    pub(crate) fn reclaim_memory_object_if_unreferenced(&mut self, object: CapObject) {
        let id = match object {
            CapObject::MemoryObject { id } | CapObject::DmaRegion { id, .. } => id,
            _ => return,
        };

        let Some(slot_index) = self.memory_object_slot_by_id(id) else {
            return;
        };
        self.with_memory_state_mut(|memory| {
            let Some(memory_object) = memory.memory_objects[slot_index] else {
                return;
            };
            if memory_object.cap_refcount != 0
                || memory_object.map_refcount != 0
                || memory_object.pin_refcount != 0
            {
                return;
            }
            Self::release_memory_object_slot_locked(memory, slot_index);
        });
    }

    pub(crate) fn reclaim_memory_object_for_phys(&mut self, phys: PhysAddr) {
        let maybe_id = self.with_memory_state(|memory| {
            memory
                .memory_objects
                .iter()
                .flatten()
                .find(|entry| entry.phys == phys)
                .map(|obj| obj.id)
        });
        if let Some(id) = maybe_id {
            self.reclaim_memory_object_if_unreferenced(CapObject::MemoryObject { id });
        }
    }

    /// Stage 114 / D-NEXT-2: sibling of [`Self::reclaim_memory_object_for_phys`]
    /// taking `&mut MemorySubsystem` directly for use inside
    /// `SharedKernel::with_memory_split_mut`'s closure. The global-lock version
    /// composes find-by-phys → id → `reclaim_memory_object_if_unreferenced`
    /// (find-by-id → slot) as three separate `with_memory_state(_mut)` cycles;
    /// since this helper already holds the one memory-domain lock acquisition
    /// the seam took, it fuses straight to a single find-by-phys → mutate pass.
    /// Same refcount/free-frame semantics, strictly fewer redundant scans — not
    /// a behavior change. `reclaim_memory_object_for_phys` itself is left
    /// unmodified.
    pub(crate) fn reclaim_memory_object_for_phys_locked(
        memory: &mut MemorySubsystem,
        phys: PhysAddr,
    ) {
        let Some(slot_index) = memory
            .memory_objects
            .iter()
            .position(|entry| entry.is_some_and(|mem| mem.phys == phys))
        else {
            return;
        };
        let Some(memory_object) = memory.memory_objects[slot_index] else {
            return;
        };
        if memory_object.cap_refcount != 0
            || memory_object.map_refcount != 0
            || memory_object.pin_refcount != 0
        {
            return;
        }
        Self::release_memory_object_slot_locked(memory, slot_index);
    }

    /// U6/199C test accessor: `(cap_refcount, map_refcount, pin_refcount)` for the MemoryObject
    /// with `id`, or `None` if it has been reclaimed. Keyed by id because the shared-region
    /// blocking-send proofs hold a `CapObject::MemoryObject { id }`, not a physical address.
    #[cfg(test)]
    pub(crate) fn memory_object_refcounts_by_id(&self, id: u64) -> Option<(u32, u32, u32)> {
        let slot = self.memory_object_slot_by_id(id)?;
        self.with_memory_state(|memory| {
            memory.memory_objects[slot]
                .map(|obj| (obj.cap_refcount, obj.map_refcount, obj.pin_refcount))
        })
    }

    /// Return `(cap_refcount, map_refcount, pin_refcount)` for the MemoryObject
    /// backing `phys`, or `None` if the object has been reclaimed.
    #[cfg(test)]
    pub(crate) fn memory_object_refcounts(&self, phys: PhysAddr) -> Option<(u32, u32, u32)> {
        self.with_memory_state(|memory| {
            memory
                .memory_objects
                .iter()
                .flatten()
                .find(|obj| obj.phys == phys)
                .map(|obj| (obj.cap_refcount, obj.map_refcount, obj.pin_refcount))
        })
    }

    /// Stage 198E3C1 test accessor: the byte length of the MemoryObject backing `phys` (used to
    /// prove the shared-region oracle provisions exactly a two-page object).
    #[cfg(test)]
    pub(crate) fn memory_object_len_for_test(&self, phys: PhysAddr) -> Option<usize> {
        self.with_memory_state(|memory| {
            memory
                .memory_objects
                .iter()
                .flatten()
                .find(|obj| obj.phys == phys)
                .map(|obj| obj.len)
        })
    }

    /// Return true if a MemoryObject slot exists for `phys` (i.e., not reclaimed).
    #[cfg(test)]
    pub(crate) fn memory_object_exists_for_phys(&self, phys: PhysAddr) -> bool {
        self.memory_object_refcounts(phys).is_some()
    }

    /// Stage 198E3C1B rollback accessor: the number of live (non-reclaimed) MemoryObject slots.
    /// A leak-free provisioning rollback must leave this UNCHANGED versus the pre-attempt count.
    #[cfg(test)]
    pub(crate) fn live_memory_object_count_for_test(&self) -> usize {
        self.with_memory_state(|memory| memory.memory_objects.iter().flatten().count())
    }
}
