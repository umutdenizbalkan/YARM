use core::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CapId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CNodeId(pub u16);

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
    NotFound,
}

impl fmt::Display for CapabilityDeriveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::ParentMissing => "parent capability does not exist",
            Self::RightsEscalation => "derived capability would escalate rights",
            Self::SpaceFull => "capability space is full",
            Self::NotFound => "capability does not exist",
        };
        f.write_str(message)
    }
}

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

#[derive(Debug)]
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

    pub fn revoke(&mut self, id: CapId) -> Result<(), CapabilityDeriveError> {
        if self.get(id).is_none() {
            return Err(CapabilityDeriveError::NotFound);
        }

        let mut marked = [false; MAX_CAPABILITIES_PER_CSPACE];
        marked[id.index()] = true;

        loop {
            let mut changed = false;
            for idx in 0..MAX_CAPABILITIES_PER_CSPACE {
                if marked[idx] {
                    continue;
                }
                if let Some(entry) = self.slots[idx].entry
                    && let Some(parent) = entry.parent
                    && parent.index() < MAX_CAPABILITIES_PER_CSPACE
                    && marked[parent.index()]
                    && self.slots[parent.index()].generation == parent.generation()
                {
                    marked[idx] = true;
                    changed = true;
                }
            }
            if !changed {
                break;
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
}
