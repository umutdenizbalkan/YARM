// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::*;

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
            let _ = kernel_mut(&mut memory.frame_allocator).free_frame(memory_object.phys.0);
            memory.memory_objects[slot_index] = None;
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
        let _ = kernel_mut(&mut memory.frame_allocator).free_frame(memory_object.phys.0);
        memory.memory_objects[slot_index] = None;
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
