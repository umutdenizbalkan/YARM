// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use core::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CapId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CNodeId(pub u64);

impl CapId {
    const INDEX_BITS: u64 = 16;
    const INDEX_MASK: u64 = (1 << Self::INDEX_BITS) - 1;

    const fn new(index: usize, generation: u64) -> Self {
        Self((generation << Self::INDEX_BITS) | (index as u64))
    }

    const fn index(self) -> usize {
        (self.0 & Self::INDEX_MASK) as usize
    }

    const fn generation(self) -> u64 {
        self.0 >> Self::INDEX_BITS
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapRights(u8);

impl CapRights {
    pub const NONE: Self = Self(0);
    pub const READ: Self = Self(1 << 0);
    pub const WRITE: Self = Self(1 << 1);
    pub const MAP: Self = Self(1 << 2);
    pub const SEND: Self = Self(1 << 3);
    pub const RECEIVE: Self = Self(1 << 4);
    pub const SCHEDULE: Self = Self(1 << 5);
    pub const SIGNAL: Self = Self(1 << 6);
    pub const WAIT: Self = Self(1 << 7);

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn intersect(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    pub const fn is_subset_of(self, other: Self) -> bool {
        (self.0 & !other.0) == 0
    }

    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    pub const fn bits(self) -> u8 {
        self.0
    }
}

impl core::ops::BitOr for CapRights {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        self.union(rhs)
    }
}

impl core::ops::BitOrAssign for CapRights {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityDeriveError {
    ParentMissing,
    RightsEscalation,
    SpaceFull,
    SlotOccupied,
    InvalidSlot,
    NotFound,
}

impl fmt::Display for CapabilityDeriveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::ParentMissing => "parent capability does not exist",
            Self::RightsEscalation => "derived capability would escalate rights",
            Self::SpaceFull => "capability space is full",
            Self::SlotOccupied => "destination slot is occupied",
            Self::InvalidSlot => "invalid destination slot",
            Self::NotFound => "capability does not exist",
        };
        f.write_str(message)
    }
}

#[cfg(feature = "hosted-dev")]
pub const MAX_CAPABILITIES_PER_CSPACE: usize = 1024;
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
    slots: [CapSlot; MAX_CAPABILITIES_PER_CSPACE],
}

impl Default for CapabilitySpace {
    fn default() -> Self {
        Self {
            slots: [const {
                CapSlot {
                    generation: 1,
                    entry: None,
                }
            }; MAX_CAPABILITIES_PER_CSPACE],
        }
    }
}

impl CapabilitySpace {
    pub fn occupied_slots(&self) -> usize {
        self.slots
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
        for index in 0..MAX_CAPABILITIES_PER_CSPACE {
            let slot = &mut self.slots[index];
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
        if slot_index >= MAX_CAPABILITIES_PER_CSPACE {
            return Err(CapabilityDeriveError::InvalidSlot);
        }
        let slot = &mut self.slots[slot_index];
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

        let mut child_heads: [Option<usize>; MAX_CAPABILITIES_PER_CSPACE] =
            [None; MAX_CAPABILITIES_PER_CSPACE];
        let mut next_sibling: [Option<usize>; MAX_CAPABILITIES_PER_CSPACE] =
            [None; MAX_CAPABILITIES_PER_CSPACE];
        for idx in 0..MAX_CAPABILITIES_PER_CSPACE {
            let Some(entry) = self.slots[idx].entry else {
                continue;
            };
            let Some(parent) = entry.parent else {
                continue;
            };
            let parent_idx = parent.index();
            if parent_idx >= MAX_CAPABILITIES_PER_CSPACE {
                continue;
            }
            if self.slots[parent_idx].generation != parent.generation() {
                continue;
            }
            if self.slots[parent_idx].entry.is_none() {
                continue;
            }
            next_sibling[idx] = child_heads[parent_idx];
            child_heads[parent_idx] = Some(idx);
        }

        let mut marked = [false; MAX_CAPABILITIES_PER_CSPACE];
        let mut stack = [0usize; MAX_CAPABILITIES_PER_CSPACE];
        let mut sp = 0usize;
        stack[sp] = id.index();
        sp += 1;

        while sp != 0 {
            sp -= 1;
            let node = stack[sp];
            if marked[node] {
                continue;
            }
            marked[node] = true;
            let mut child = child_heads[node];
            while let Some(idx) = child {
                stack[sp] = idx;
                sp += 1;
                child = next_sibling[idx];
            }
        }

        for (idx, is_marked) in marked.iter().copied().enumerate() {
            if !is_marked {
                continue;
            }
            let slot = &mut self.slots[idx];
            if slot.entry.is_some() {
                slot.entry = None;
                let next = slot.generation.wrapping_add(1);
                slot.generation = if next == 0 { 1 } else { next };
            }
        }

        Ok(())
    }

    pub fn get(&self, id: CapId) -> Option<Capability> {
        let index = id.index();
        if index >= MAX_CAPABILITIES_PER_CSPACE {
            return None;
        }
        let slot = self.slots[index];
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
            .iter()
            .filter_map(|slot| slot.entry)
            .filter(|entry| entry.capability.object == object)
            .count()
    }

    pub fn memory_object_id_refcount(&self, id: u64) -> usize {
        self.slots
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

    pub fn live_cap_ids(&self) -> [Option<CapId>; MAX_CAPABILITIES_PER_CSPACE] {
        let mut ids = [None; MAX_CAPABILITIES_PER_CSPACE];
        let mut used = 0usize;
        for (index, slot) in self.slots.iter().enumerate() {
            if slot.entry.is_none() || used >= ids.len() {
                continue;
            }
            ids[used] = Some(CapId::new(index, slot.generation));
            used += 1;
        }
        ids
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
        assert_eq!(
            src.grant_to_slot(root, CapRights::READ, &mut dst, MAX_CAPABILITIES_PER_CSPACE),
            Err(CapabilityDeriveError::InvalidSlot)
        );
    }
}
