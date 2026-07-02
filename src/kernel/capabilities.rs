// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use alloc::vec::Vec;
pub use yarm_kernel::capability::{CNodeId, CapId, CapRights, CapabilityDeriveError};

/// Capability object identity remains a monolithic enum for now.
///
/// Long term, an opaque `{ kind, handle }` representation would decouple this
/// module from the full kernel object taxonomy, but the current enum is kept
/// until the object set stabilizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapObject {
    Kernel,
    Endpoint { index: usize, generation: u64 },
    AddressSpace { asid: u16 },
    IovaSpace { id: u64 },
    MemoryObject { id: u64 },
    DmaRegion { id: u64, offset: u64, len: u64 },
    Notification { index: usize, generation: u64 },
    Reply { index: usize, generation: u64 },
    Irq { line: u16 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capability {
    pub object: CapObject,
    rights: CapRights,
}

impl Capability {
    pub const fn new(object: CapObject, rights: CapRights) -> Self {
        Self { object, rights }
    }

    pub const fn has_right(self, right: CapRights) -> bool {
        self.rights.contains(right)
    }

    pub const fn rights_bits(self) -> u8 {
        self.rights.bits()
    }

    pub const fn rights(self) -> CapRights {
        self.rights
    }

    pub const fn can_derive(self, rights: CapRights) -> bool {
        rights.is_subset_of(self.rights)
    }

    pub const fn derive(self, rights: CapRights) -> Result<Self, CapabilityDeriveError> {
        if !self.can_derive(rights) {
            return Err(CapabilityDeriveError::RightsEscalation);
        }
        Ok(Self::new(self.object, rights))
    }
}

/// Runtime default CNode slot capacity for hosted profile policy.
///
/// This is *not* a hard storage ceiling: CNodes are allocator-backed and may
/// be resized at runtime subject to policy/accounting and `CapId` encodability.
#[cfg(feature = "hosted-dev")]
pub const MAX_CAPABILITIES_PER_CSPACE: usize = 1024;
/// Runtime default CNode slot capacity for non-hosted profile policy.
///
/// This is *not* a hard storage ceiling: CNodes are allocator-backed and may
/// be resized at runtime subject to policy/accounting and `CapId` encodability.
#[cfg(not(feature = "hosted-dev"))]
pub const MAX_CAPABILITIES_PER_CSPACE: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CapEntry {
    capability: Capability,
    parent: Option<CapId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CapSlot {
    generation: u64,
    entry: Option<CapEntry>,
}

#[derive(Debug, Clone)]
pub struct CapabilitySpace {
    slots: CNodeSlots,
    revoke_scratch_cache: Option<RevokeScratch>,
    revoke_scratch_cache_hits: u64,
    revoke_scratch_cache_misses: u64,
    revoke_scratch_cache_drops: u64,
}

#[derive(Debug, Clone)]
struct CNodeSlots {
    slots: Vec<CapSlot>,
}

impl CNodeSlots {
    fn try_new(slot_capacity: usize) -> Result<Self, CapabilityDeriveError> {
        if !Self::is_valid_slot_capacity(slot_capacity) {
            return Err(CapabilityDeriveError::InvalidSlot);
        }
        let mut slots = Vec::new();
        slots
            .try_reserve_exact(slot_capacity)
            .map_err(|_| CapabilityDeriveError::AllocFailed)?;
        slots.resize(
            slot_capacity,
            CapSlot {
                generation: 1,
                entry: None,
            },
        );
        Ok(Self { slots })
    }

    fn len(&self) -> usize {
        self.slots.len()
    }

    fn as_slice(&self) -> &[CapSlot] {
        &self.slots
    }

    fn get(&self, index: usize) -> Option<&CapSlot> {
        self.slots.get(index)
    }

    fn get_mut(&mut self, index: usize) -> Option<&mut CapSlot> {
        self.slots.get_mut(index)
    }

    fn try_resize(&mut self, target_len: usize) -> Result<(), CapabilityDeriveError> {
        if !Self::is_valid_slot_capacity(target_len) {
            return Err(CapabilityDeriveError::InvalidSlot);
        }
        if target_len == self.len() {
            return Ok(());
        }
        if target_len < self.len()
            && self.slots[target_len..]
                .iter()
                .any(|slot| slot.entry.is_some())
        {
            return Err(CapabilityDeriveError::SpaceFull);
        }
        let mut next = Vec::new();
        next.try_reserve_exact(target_len)
            .map_err(|_| CapabilityDeriveError::AllocFailed)?;
        next.extend_from_slice(&self.slots[..self.len().min(target_len)]);
        if next.len() < target_len {
            next.resize(
                target_len,
                CapSlot {
                    generation: 1,
                    entry: None,
                },
            );
        }
        self.slots = next;
        Ok(())
    }

    fn is_valid_slot_capacity(slot_capacity: usize) -> bool {
        slot_capacity != 0 && slot_capacity <= (CapId::INDEX_MASK as usize).saturating_add(1)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RevokeScratchTelemetry {
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub cache_drops: u64,
}

impl Default for CapabilitySpace {
    fn default() -> Self {
        Self::try_with_slots(MAX_CAPABILITIES_PER_CSPACE)
            .expect("default capability space allocation should succeed")
    }
}

#[derive(Debug, Clone)]
struct RevokeScratch {
    child_heads: Vec<Option<usize>>,
    next_sibling: Vec<Option<usize>>,
    marked: Vec<bool>,
    stack: Vec<usize>,
}

impl RevokeScratch {
    fn try_with_capacity(capacity: usize) -> Result<Self, CapabilityDeriveError> {
        fn try_vec_clone<T: Clone>(
            capacity: usize,
            value: T,
        ) -> Result<Vec<T>, CapabilityDeriveError> {
            let mut out = Vec::new();
            out.try_reserve_exact(capacity)
                .map_err(|_| CapabilityDeriveError::AllocFailed)?;
            out.resize(capacity, value);
            Ok(out)
        }

        let mut stack = Vec::new();
        stack
            .try_reserve_exact(capacity)
            .map_err(|_| CapabilityDeriveError::AllocFailed)?;
        Ok(Self {
            child_heads: try_vec_clone(capacity, None)?,
            next_sibling: try_vec_clone(capacity, None)?,
            marked: try_vec_clone(capacity, false)?,
            stack,
        })
    }

    fn capacity(&self) -> usize {
        self.child_heads.len()
    }

    fn clear_for_len(&mut self, len: usize) {
        self.child_heads[..len].fill(None);
        self.next_sibling[..len].fill(None);
        self.marked[..len].fill(false);
        self.stack.clear();
    }
}

impl CapabilitySpace {
    pub fn try_with_slots(slot_capacity: usize) -> Result<Self, CapabilityDeriveError> {
        let slots = CNodeSlots::try_new(slot_capacity)?;
        Ok(Self {
            slots,
            revoke_scratch_cache: None,
            revoke_scratch_cache_hits: 0,
            revoke_scratch_cache_misses: 0,
            revoke_scratch_cache_drops: 0,
        })
    }

    pub fn with_slots(slot_capacity: usize) -> Self {
        Self::try_with_slots(slot_capacity).expect("capability space allocation should succeed")
    }

    pub fn capacity(&self) -> usize {
        self.slots.len()
    }

    pub fn resize_slots(&mut self, slot_capacity: usize) -> Result<(), CapabilityDeriveError> {
        self.slots.try_resize(slot_capacity)?;
        if self
            .revoke_scratch_cache
            .as_ref()
            .is_some_and(|scratch| scratch.capacity() < self.capacity())
        {
            self.revoke_scratch_cache = None;
            self.revoke_scratch_cache_drops = self.revoke_scratch_cache_drops.saturating_add(1);
        }
        Ok(())
    }

    /// Stage 181C: release the cached revoke-scratch working set (the per-cspace
    /// `RevokeScratch` Vecs sized to `capacity()`), returning its backing pages to
    /// the allocator. `revoke()` lazily builds and CACHES this scratch on first use;
    /// for a large (e.g. 512-slot) cspace it is ~12 pages drawn from the PT frame
    /// pool that back the kernel heap. A one-shot diagnostic that only revokes a
    /// couple of scratch caps should not leave that cache resident (it would steal
    /// PT-pool headroom a later fork needs). Returns `true` if a cache was dropped.
    /// The next real revoke rebuilds it on demand, so correctness is unchanged.
    pub fn drop_revoke_scratch_cache(&mut self) -> bool {
        if self.revoke_scratch_cache.take().is_some() {
            self.revoke_scratch_cache_drops = self.revoke_scratch_cache_drops.saturating_add(1);
            true
        } else {
            false
        }
    }

    pub fn occupied_slots(&self) -> usize {
        self.slots
            .as_slice()
            .iter()
            .filter(|slot| slot.entry.is_some())
            .count()
    }

    pub fn mint(&mut self, capability: Capability) -> Result<CapId, CapabilityDeriveError> {
        self.mint_with_parent(capability, None)
    }

    fn mint_with_parent(
        &mut self,
        capability: Capability,
        parent: Option<CapId>,
    ) -> Result<CapId, CapabilityDeriveError> {
        for index in 0..self.capacity() {
            let slot = self
                .slots
                .get_mut(index)
                .expect("index validated by capacity loop");
            if slot.entry.is_none() {
                let id = CapId::new(index, slot.generation);
                slot.entry = Some(CapEntry { capability, parent });
                return Ok(id);
            }
        }
        Err(CapabilityDeriveError::SpaceFull)
    }

    pub fn mint_derived(
        &mut self,
        parent: CapId,
        rights: CapRights,
    ) -> Result<CapId, CapabilityDeriveError> {
        let parent_cap = self
            .get(parent)
            .ok_or(CapabilityDeriveError::ParentMissing)?;
        let derived = parent_cap.derive(rights)?;
        self.mint_with_parent(derived, Some(parent))
    }

    pub fn mint_at(
        &mut self,
        slot_index: usize,
        capability: Capability,
    ) -> Result<CapId, CapabilityDeriveError> {
        if slot_index >= self.capacity() {
            return Err(CapabilityDeriveError::InvalidSlot);
        }
        let slot = self
            .slots
            .get_mut(slot_index)
            .expect("slot index validated above");
        if slot.entry.is_some() {
            return Err(CapabilityDeriveError::SlotOccupied);
        }
        let id = CapId::new(slot_index, slot.generation);
        slot.entry = Some(CapEntry {
            capability,
            parent: None,
        });
        Ok(id)
    }

    pub fn grant_to(
        &self,
        source: CapId,
        rights: CapRights,
        destination: &mut CapabilitySpace,
    ) -> Result<CapId, CapabilityDeriveError> {
        let source_cap = self
            .get(source)
            .ok_or(CapabilityDeriveError::ParentMissing)?;
        let derived = source_cap.derive(rights)?;
        destination.mint(derived)
    }

    pub fn grant_to_slot(
        &self,
        source: CapId,
        rights: CapRights,
        destination: &mut CapabilitySpace,
        destination_slot: usize,
    ) -> Result<CapId, CapabilityDeriveError> {
        let source_cap = self
            .get(source)
            .ok_or(CapabilityDeriveError::ParentMissing)?;
        let derived = source_cap.derive(rights)?;
        destination.mint_at(destination_slot, derived)
    }

    /// Revoke `id` and all descendants derived from it.
    ///
    /// Complexity is `O(n)` in slot count for each revoke operation:
    /// a single pass builds a parent->children index, then a stack walk marks
    /// the transitive closure.
    pub fn revoke(&mut self, id: CapId) -> Result<(), CapabilityDeriveError> {
        if self.get(id).is_none() {
            return Err(CapabilityDeriveError::NotFound);
        }

        let mut scratch = self.take_revoke_scratch()?;
        scratch.clear_for_len(self.capacity());
        for idx in 0..self.capacity() {
            let Some(entry) = self.slots.as_slice()[idx].entry else {
                continue;
            };
            let Some(parent) = entry.parent else {
                continue;
            };
            let parent_idx = parent.index();
            if parent_idx >= self.capacity() {
                continue;
            }
            if self.slots.as_slice()[parent_idx].generation != parent.generation() {
                continue;
            }
            if self.slots.as_slice()[parent_idx].entry.is_none() {
                continue;
            }
            scratch.next_sibling[idx] = scratch.child_heads[parent_idx];
            scratch.child_heads[parent_idx] = Some(idx);
        }

        scratch.stack.push(id.index());

        while let Some(node) = scratch.stack.pop() {
            if scratch.marked[node] {
                continue;
            }
            scratch.marked[node] = true;
            let mut child = scratch.child_heads[node];
            while let Some(idx) = child {
                scratch.stack.push(idx);
                child = scratch.next_sibling[idx];
            }
        }

        for (idx, is_marked) in scratch
            .marked
            .iter()
            .copied()
            .take(self.capacity())
            .enumerate()
        {
            if !is_marked {
                continue;
            }
            let slot = self
                .slots
                .get_mut(idx)
                .expect("idx derived from marked bounds");
            if slot.entry.is_some() {
                slot.entry = None;
                let next = slot.generation.wrapping_add(1);
                slot.generation = if next == 0 { 1 } else { next };
            }
        }

        self.revoke_scratch_cache = Some(scratch);
        Ok(())
    }

    fn take_revoke_scratch(&mut self) -> Result<RevokeScratch, CapabilityDeriveError> {
        if let Some(scratch) = self.revoke_scratch_cache.take()
            && scratch.capacity() >= self.capacity()
        {
            self.revoke_scratch_cache_hits = self.revoke_scratch_cache_hits.saturating_add(1);
            return Ok(scratch);
        }
        self.revoke_scratch_cache_misses = self.revoke_scratch_cache_misses.saturating_add(1);
        RevokeScratch::try_with_capacity(self.capacity())
    }

    pub fn revoke_scratch_telemetry(&self) -> RevokeScratchTelemetry {
        RevokeScratchTelemetry {
            cache_hits: self.revoke_scratch_cache_hits,
            cache_misses: self.revoke_scratch_cache_misses,
            cache_drops: self.revoke_scratch_cache_drops,
        }
    }

    pub fn get(&self, id: CapId) -> Option<Capability> {
        let index = id.index();
        if index >= self.capacity() {
            return None;
        }
        let slot = *self
            .slots
            .get(index)
            .expect("index validated by capacity check");
        if slot.generation != id.generation() {
            return None;
        }
        slot.entry.map(|entry| entry.capability)
    }

    pub fn has_right(&self, id: CapId, right: CapRights) -> bool {
        self.get(id)
            .map(|capability| capability.has_right(right))
            .unwrap_or(false)
    }

    pub fn contains(&self, id: CapId) -> bool {
        self.get(id).is_some()
    }

    pub fn object_refcount(&self, object: CapObject) -> usize {
        self.slots
            .as_slice()
            .iter()
            .filter_map(|slot| slot.entry)
            .filter(|entry| entry.capability.object == object)
            .count()
    }

    pub fn memory_object_id_refcount(&self, id: u64) -> usize {
        self.slots
            .as_slice()
            .iter()
            .filter_map(|slot| slot.entry)
            .filter(|entry| {
                matches!(
                    entry.capability.object,
                    CapObject::MemoryObject { id: object_id }
                        | CapObject::DmaRegion { id: object_id, .. }
                        if object_id == id
                )
            })
            .count()
    }

    pub fn live_cap_ids(&self) -> impl Iterator<Item = CapId> + '_ {
        self.slots
            .as_slice()
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| {
                slot.entry
                    .as_ref()
                    .map(|_| CapId::new(index, slot.generation))
            })
    }

    /// Fast revoke a single Reply cap slot — **no heap allocation**.
    ///
    /// Removes the slot entry only if all of these conditions hold:
    /// - `cap.index()` is within the space's capacity
    /// - The slot's generation matches `cap.generation()` (the CapId is live)
    /// - The entry contains a capability whose `object` equals `expected_object`
    ///
    /// When a match is found the entry is cleared and the slot generation is
    /// bumped, invalidating any outstanding copies of this CapId.
    ///
    /// Returns `true` if the slot was cleared, `false` if it was already gone
    /// (stale generation, empty, or wrong object).  The return value is
    /// diagnostic only — callers must not abort work based on a `false` result.
    ///
    /// # No-alloc guarantee
    /// This function allocates nothing on the heap.  It is safe to call from
    /// the hot IPC reply path on freestanding kernel configurations.
    pub fn fast_revoke_reply_slot(&mut self, cap: CapId, expected_object: CapObject) -> bool {
        let index = cap.index();
        if index >= self.capacity() {
            return false;
        }
        let slot = match self.slots.get_mut(index) {
            Some(s) => s,
            None => return false,
        };
        if slot.generation != cap.generation() {
            return false;
        }
        let entry = match slot.entry {
            Some(e) => e,
            None => return false,
        };
        if entry.capability.object != expected_object {
            return false;
        }
        slot.entry = None;
        let next = slot.generation.wrapping_add(1);
        slot.generation = if next == 0 { 1 } else { next };
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minted_capability_can_be_checked() {
        let mut cspace = CapabilitySpace::default();
        let cap = Capability::new(
            CapObject::Endpoint {
                index: 0,
                generation: 1,
            },
            CapRights::SEND | CapRights::RECEIVE,
        );

        let id = cspace.mint(cap).expect("cspace should have room");

        assert!(cspace.has_right(id, CapRights::SEND));
        assert!(!cspace.has_right(id, CapRights::SCHEDULE));
        assert_eq!(
            cspace.get(id).expect("cap should exist").object,
            CapObject::Endpoint {
                index: 0,
                generation: 1
            }
        );
    }

    #[test]
    fn capability_can_derive_subset_without_escalation() {
        let parent = Capability::new(
            CapObject::Endpoint {
                index: 0,
                generation: 1,
            },
            CapRights::SEND | CapRights::RECEIVE,
        );
        assert!(parent.can_derive(CapRights::SEND));
        assert_eq!(
            parent.derive(CapRights::SEND).expect("subset"),
            Capability::new(
                CapObject::Endpoint {
                    index: 0,
                    generation: 1,
                },
                CapRights::SEND,
            )
        );
        assert_eq!(
            parent.derive(CapRights::SCHEDULE),
            Err(CapabilityDeriveError::RightsEscalation)
        );
    }

    #[test]
    fn derived_capability_cannot_escalate_rights_and_can_be_revoked() {
        let mut cspace = CapabilitySpace::default();
        let parent = cspace
            .mint(Capability::new(
                CapObject::Endpoint {
                    index: 0,
                    generation: 1,
                },
                CapRights::SEND | CapRights::RECEIVE,
            ))
            .expect("mint parent");

        let child = cspace
            .mint_derived(parent, CapRights::SEND)
            .expect("derive subset rights");
        assert!(cspace.has_right(child, CapRights::SEND));
        assert!(!cspace.has_right(child, CapRights::RECEIVE));

        assert_eq!(
            cspace.mint_derived(parent, CapRights::SCHEDULE),
            Err(CapabilityDeriveError::RightsEscalation)
        );

        assert_eq!(cspace.revoke(parent), Ok(()));
        assert!(cspace.get(parent).is_none());
        assert!(cspace.get(child).is_none());
    }

    #[test]
    fn mint_fails_when_space_full_and_returns_capability() {
        let mut cspace = CapabilitySpace::default();
        for _ in 0..MAX_CAPABILITIES_PER_CSPACE {
            let _ = cspace
                .mint(Capability::new(CapObject::Kernel, CapRights::READ))
                .expect("room");
        }
        let cap = Capability::new(CapObject::Kernel, CapRights::WRITE);
        assert_eq!(cspace.mint(cap), Err(CapabilityDeriveError::SpaceFull));
    }

    #[test]
    fn mint_derived_on_revoked_parent_returns_parent_missing() {
        let mut cspace = CapabilitySpace::default();
        let parent = cspace
            .mint(Capability::new(CapObject::Kernel, CapRights::READ))
            .expect("mint");
        assert_eq!(cspace.revoke(parent), Ok(()));
        assert_eq!(
            cspace.mint_derived(parent, CapRights::READ),
            Err(CapabilityDeriveError::ParentMissing)
        );
    }

    #[test]
    fn revoke_missing_and_double_revoke_behave() {
        let mut cspace = CapabilitySpace::default();
        let id = cspace
            .mint(Capability::new(CapObject::Kernel, CapRights::READ))
            .expect("mint");
        assert_eq!(cspace.revoke(id), Ok(()));
        assert_eq!(cspace.revoke(id), Err(CapabilityDeriveError::NotFound));
        assert_eq!(
            cspace.revoke(CapId(u64::MAX)),
            Err(CapabilityDeriveError::NotFound)
        );
    }

    #[test]
    fn revoked_slot_reuse_produces_fresh_cap_id() {
        let mut cspace = CapabilitySpace::default();
        let first = cspace
            .mint(Capability::new(CapObject::Kernel, CapRights::READ))
            .expect("mint");
        assert_eq!(cspace.revoke(first), Ok(()));
        let second = cspace
            .mint(Capability::new(CapObject::Kernel, CapRights::READ))
            .expect("mint2");
        assert_ne!(first, second);
        assert!(cspace.get(first).is_none());
        assert!(cspace.get(second).is_some());
        assert!(cspace.contains(second));
        assert!(!cspace.contains(first));
    }

    #[test]
    fn explicit_grant_to_destination_space_requires_subset_rights() {
        let mut src = CapabilitySpace::default();
        let mut dst = CapabilitySpace::default();

        let root = src
            .mint(Capability::new(
                CapObject::Endpoint {
                    index: 7,
                    generation: 1,
                },
                CapRights::SEND | CapRights::RECEIVE,
            ))
            .expect("mint");

        let granted = src
            .grant_to(root, CapRights::SEND, &mut dst)
            .expect("grant");
        assert!(dst.has_right(granted, CapRights::SEND));
        assert!(!dst.has_right(granted, CapRights::RECEIVE));

        assert_eq!(
            src.grant_to(root, CapRights::SCHEDULE, &mut dst),
            Err(CapabilityDeriveError::RightsEscalation)
        );
    }

    #[test]
    fn explicit_grant_to_specific_slot_rejects_occupied_or_invalid_slot() {
        let mut src = CapabilitySpace::default();
        let mut dst = CapabilitySpace::default();
        let root = src
            .mint(Capability::new(CapObject::Kernel, CapRights::READ))
            .expect("mint");

        let first = src
            .grant_to_slot(root, CapRights::READ, &mut dst, 3)
            .expect("grant slot");
        assert_eq!(first.index(), 3);

        assert_eq!(
            src.grant_to_slot(root, CapRights::READ, &mut dst, 3),
            Err(CapabilityDeriveError::SlotOccupied)
        );
        let invalid_slot = dst.capacity();
        assert_eq!(
            src.grant_to_slot(root, CapRights::READ, &mut dst, invalid_slot),
            Err(CapabilityDeriveError::InvalidSlot)
        );
    }

    #[test]
    fn custom_capacity_limits_minting() {
        let mut cspace = CapabilitySpace::with_slots(4);
        for _ in 0..4 {
            let _ = cspace
                .mint(Capability::new(CapObject::Kernel, CapRights::READ))
                .expect("room");
        }
        assert_eq!(
            cspace.mint(Capability::new(CapObject::Kernel, CapRights::READ)),
            Err(CapabilityDeriveError::SpaceFull)
        );
    }

    #[test]
    fn custom_capacity_bounds_slot_mint_at() {
        let mut cspace = CapabilitySpace::with_slots(4);
        assert_eq!(cspace.capacity(), 4);
        assert_eq!(
            cspace.mint_at(4, Capability::new(CapObject::Kernel, CapRights::READ)),
            Err(CapabilityDeriveError::InvalidSlot)
        );
    }

    #[test]
    fn live_cap_ids_iterator_reports_only_live_entries() {
        let mut cspace = CapabilitySpace::with_slots(4);
        let cap = Capability::new(CapObject::Kernel, CapRights::READ);
        let first = cspace.mint_at(0, cap).expect("slot 0");
        let second = cspace.mint_at(2, cap).expect("slot 2");

        let mut ids = cspace.live_cap_ids();
        assert_eq!(ids.next(), Some(first));
        assert_eq!(ids.next(), Some(second));
        assert_eq!(ids.next(), None);
    }

    #[test]
    fn resize_slots_rejects_shrink_below_occupied() {
        let mut cspace = CapabilitySpace::with_slots(4);
        let cap = Capability::new(CapObject::Kernel, CapRights::READ);
        cspace.mint_at(0, cap).expect("slot 0");
        cspace.mint_at(1, cap).expect("slot 1");
        assert_eq!(
            cspace.resize_slots(1),
            Err(CapabilityDeriveError::SpaceFull)
        );
    }

    #[test]
    fn cspace_can_grow_beyond_profile_default_max() {
        let cspace = CapabilitySpace::try_with_slots(MAX_CAPABILITIES_PER_CSPACE + 1)
            .expect("allocation should succeed");
        assert_eq!(cspace.capacity(), MAX_CAPABILITIES_PER_CSPACE + 1);
    }

    #[test]
    fn cspace_creation_rejects_unencodable_slot_capacity() {
        let limit = (CapId::INDEX_MASK as usize).saturating_add(1);
        let err = CapabilitySpace::try_with_slots(limit.saturating_add(1))
            .expect_err("slot capacity above CapId index range must fail");
        assert_eq!(err, CapabilityDeriveError::InvalidSlot);
    }

    #[test]
    fn resize_slots_grow_preserves_live_entries() {
        let mut cspace = CapabilitySpace::with_slots(4);
        let cap = Capability::new(CapObject::Kernel, CapRights::READ);
        let root = cspace.mint_at(3, cap).expect("slot 3");

        cspace.resize_slots(9).expect("grow should succeed");
        assert_eq!(cspace.capacity(), 9);
        assert_eq!(cspace.get(root), Some(cap));
    }

    #[test]
    fn resize_slots_shrink_succeeds_when_tail_is_empty() {
        let mut cspace = CapabilitySpace::with_slots(8);
        cspace
            .mint_at(2, Capability::new(CapObject::Kernel, CapRights::READ))
            .expect("slot 2");
        cspace.resize_slots(4).expect("shrink should succeed");
        assert_eq!(cspace.capacity(), 4);
        assert_eq!(
            cspace.mint_at(5, Capability::new(CapObject::Kernel, CapRights::READ)),
            Err(CapabilityDeriveError::InvalidSlot)
        );
    }

    #[test]
    fn revoke_scratch_cache_telemetry_tracks_hits_and_misses() {
        let mut cspace = CapabilitySpace::with_slots(4);
        let cap = Capability::new(CapObject::Kernel, CapRights::READ);

        let root1 = cspace.mint_at(0, cap).expect("slot 0");
        cspace.revoke(root1).expect("first revoke");

        let root2 = cspace.mint_at(1, cap).expect("slot 1");
        cspace.revoke(root2).expect("second revoke");

        let telemetry = cspace.revoke_scratch_telemetry();
        assert!(telemetry.cache_misses >= 1);
        assert!(telemetry.cache_hits >= 1);
    }

    // Stage 181C: dropping the cached revoke scratch releases it (freeing its backing
    // pages) and is idempotent; the next revoke rebuilds it (a fresh cache miss).
    #[test]
    fn drop_revoke_scratch_cache_releases_and_rebuilds() {
        let mut cspace = CapabilitySpace::with_slots(8);
        let cap = Capability::new(CapObject::Kernel, CapRights::READ);
        let root = cspace.mint_at(0, cap).expect("slot 0");
        cspace.revoke(root).expect("revoke builds + caches scratch");

        // A cache is now resident; dropping it reports true, then false (idempotent).
        assert!(
            cspace.drop_revoke_scratch_cache(),
            "cached revoke scratch must be dropped"
        );
        assert!(
            !cspace.drop_revoke_scratch_cache(),
            "second drop is a no-op (nothing cached)"
        );
        let before = cspace.revoke_scratch_telemetry().cache_misses;

        // The next revoke rebuilds the scratch (another miss) — correctness intact.
        let root2 = cspace.mint_at(1, cap).expect("slot 1");
        cspace
            .revoke(root2)
            .expect("revoke after drop rebuilds scratch");
        assert!(
            cspace.revoke_scratch_telemetry().cache_misses > before,
            "revoke after a drop must rebuild the scratch (a fresh miss)"
        );
    }

    #[test]
    fn revoke_scratch_worklists_scale_with_runtime_slot_capacity() {
        let slot_capacity = MAX_CAPABILITIES_PER_CSPACE + 64;
        let mut cspace =
            CapabilitySpace::try_with_slots(slot_capacity).expect("allocation should succeed");
        let cap = Capability::new(CapObject::Kernel, CapRights::READ);

        let root = cspace
            .mint_at(slot_capacity - 1, cap)
            .expect("root at high slot index");
        let child = cspace
            .mint_derived(root, CapRights::READ)
            .expect("derived child");

        cspace
            .revoke(root)
            .expect("revoke should traverse dynamic worklists");
        assert!(cspace.get(root).is_none());
        assert!(cspace.get(child).is_none());
    }

    #[test]
    fn pass_b_capability_ids_and_rights_are_reexported_from_yarm_kernel() {
        use core::mem;

        assert_eq!(
            mem::size_of::<CapId>(),
            mem::size_of::<yarm_kernel::capability::CapId>()
        );
        assert_eq!(
            mem::size_of::<CNodeId>(),
            mem::size_of::<yarm_kernel::capability::CNodeId>()
        );
        assert_eq!(
            CapRights::READ.bits(),
            yarm_kernel::capability::CapRights::READ.bits()
        );
        let _err: yarm_kernel::capability::CapabilityDeriveError =
            CapabilityDeriveError::RightsEscalation;
    }
}
