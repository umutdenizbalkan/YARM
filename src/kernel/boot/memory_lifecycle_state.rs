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
        if let Some(memory_object) = self.memory.memory_objects[slot].as_mut() {
            if delta > 0 {
                memory_object.cap_refcount =
                    memory_object.cap_refcount.saturating_add(delta as u32);
            } else {
                memory_object.cap_refcount =
                    memory_object.cap_refcount.saturating_sub((-delta) as u32);
            }
        }
    }

    pub(crate) fn adjust_memory_object_pin_refcount(&mut self, object: CapObject, delta: i32) {
        let id = match object {
            CapObject::MemoryObject { id } | CapObject::DmaRegion { id, .. } => id,
            _ => return,
        };
        let Some(slot) = self.memory_object_slot_by_id(id) else {
            return;
        };
        if let Some(memory_object) = self.memory.memory_objects[slot].as_mut() {
            if delta > 0 {
                memory_object.pin_refcount =
                    memory_object.pin_refcount.saturating_add(delta as u32);
            } else {
                memory_object.pin_refcount =
                    memory_object.pin_refcount.saturating_sub((-delta) as u32);
            }
        }
    }

    pub(crate) fn note_mapping_inserted(&mut self, phys: PhysAddr) {
        if let Some(slot) = self
            .memory
            .memory_objects
            .iter()
            .position(|entry| entry.is_some_and(|mem| mem.phys == phys))
            && let Some(memory_object) = self.memory.memory_objects[slot].as_mut()
        {
            memory_object.map_refcount = memory_object.map_refcount.saturating_add(1);
        }
    }

    pub(crate) fn note_mapping_removed(&mut self, phys: PhysAddr) {
        if let Some(slot) = self
            .memory
            .memory_objects
            .iter()
            .position(|entry| entry.is_some_and(|mem| mem.phys == phys))
            && let Some(memory_object) = self.memory.memory_objects[slot].as_mut()
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
        let Some(memory_object) = self.memory.memory_objects[slot_index] else {
            return;
        };

        if memory_object.cap_refcount != 0
            || memory_object.map_refcount != 0
            || memory_object.pin_refcount != 0
        {
            return;
        }

        let _ = kernel_mut(&mut self.memory.frame_allocator).free_frame(memory_object.phys.0);
        self.memory.memory_objects[slot_index] = None;
    }

    pub(crate) fn reclaim_memory_object_for_phys(&mut self, phys: PhysAddr) {
        let maybe_object = self
            .memory
            .memory_objects
            .iter()
            .flatten()
            .find(|entry| entry.phys == phys)
            .copied();
        if let Some(object) = maybe_object {
            self.reclaim_memory_object_if_unreferenced(CapObject::MemoryObject { id: object.id });
        }
    }
}
