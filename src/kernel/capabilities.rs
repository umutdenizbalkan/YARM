#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CapId(pub u64);

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
pub enum CapRights {
    Read,
    Write,
    Map,
    Send,
    Receive,
    Schedule,
    Signal,
    Wait,
}

impl CapRights {
    const fn bit(self) -> u8 {
        match self {
            Self::Read => 1 << 0,
            Self::Write => 1 << 1,
            Self::Map => 1 << 2,
            Self::Send => 1 << 3,
            Self::Receive => 1 << 4,
            Self::Schedule => 1 << 5,
            Self::Signal => 1 << 6,
            Self::Wait => 1 << 7,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapObject {
    Kernel,
    Endpoint { index: usize, generation: u64 },
    AddressSpace { asid: u16 },
    IovaSpace { id: u64 },
    MemoryObject { id: u64 },
    DmaRegion { id: u64, offset: usize, len: usize },
    Notification { index: usize, generation: u64 },
    Irq { line: u16 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capability {
    pub name: &'static str,
    pub object: CapObject,
    rights_bits: u8,
}

impl Capability {
    pub const fn new(name: &'static str, object: CapObject, rights: &[CapRights]) -> Self {
        let mut idx = 0;
        let mut bits = 0;
        while idx < rights.len() {
            bits |= rights[idx].bit();
            idx += 1;
        }
        Self {
            name,
            object,
            rights_bits: bits,
        }
    }

    pub const fn has_right(self, right: CapRights) -> bool {
        (self.rights_bits & right.bit()) != 0
    }

    pub const fn rights_bits(self) -> u8 {
        self.rights_bits
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityDeriveError {
    ParentMissing,
    RightsEscalation,
    SpaceFull,
}

const MAX_CAPABILITIES: usize = 128;

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
    slots: [CapSlot; MAX_CAPABILITIES],
}

impl Default for CapabilitySpace {
    fn default() -> Self {
        Self {
            slots: [const {
                CapSlot {
                    generation: 1,
                    entry: None,
                }
            }; MAX_CAPABILITIES],
        }
    }
}

impl CapabilitySpace {
    pub fn mint(&mut self, capability: Capability) -> Result<CapId, Capability> {
        self.mint_with_parent(capability, None)
    }

    fn mint_with_parent(
        &mut self,
        capability: Capability,
        parent: Option<CapId>,
    ) -> Result<CapId, Capability> {
        let mut index = 0usize;
        while index < MAX_CAPABILITIES {
            let slot = &mut self.slots[index];
            if slot.entry.is_none() {
                let id = CapId::new(index, slot.generation);
                slot.entry = Some(CapEntry { capability, parent });
                return Ok(id);
            }
            index += 1;
        }
        Err(capability)
    }

    pub fn mint_derived(
        &mut self,
        parent: CapId,
        name: &'static str,
        rights: &[CapRights],
    ) -> Result<CapId, CapabilityDeriveError> {
        let parent_cap = self
            .get(parent)
            .ok_or(CapabilityDeriveError::ParentMissing)?;
        let derived = Capability::new(name, parent_cap.object, rights);
        if (derived.rights_bits() & !parent_cap.rights_bits()) != 0 {
            return Err(CapabilityDeriveError::RightsEscalation);
        }

        self.mint_with_parent(derived, Some(parent))
            .map_err(|_| CapabilityDeriveError::SpaceFull)
    }

    pub fn revoke(&mut self, id: CapId) -> bool {
        if self.get(id).is_none() {
            return false;
        }

        // Recursive revoke for derived lineage.
        let mut stack = [None; MAX_CAPABILITIES];
        let mut stack_len = 0usize;
        stack[stack_len] = Some(id);
        stack_len += 1;

        while stack_len > 0 {
            stack_len -= 1;
            let Some(current) = stack[stack_len] else {
                continue;
            };

            let mut idx = 0usize;
            while idx < MAX_CAPABILITIES {
                if let Some(entry) = self.slots[idx].entry {
                    if entry.parent == Some(current) && stack_len < MAX_CAPABILITIES {
                        let child_id = CapId::new(idx, self.slots[idx].generation);
                        stack[stack_len] = Some(child_id);
                        stack_len += 1;
                    }
                }
                idx += 1;
            }

            let slot_idx = current.index();
            if slot_idx < MAX_CAPABILITIES {
                let slot = &mut self.slots[slot_idx];
                if slot.entry.is_some() && slot.generation == current.generation() {
                    slot.entry = None;
                    slot.generation = slot.generation.wrapping_add(1).max(1);
                }
            }
        }

        true
    }

    pub fn get(&self, id: CapId) -> Option<Capability> {
        let index = id.index();
        if index >= MAX_CAPABILITIES {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minted_capability_can_be_checked() {
        let mut cspace = CapabilitySpace::default();
        let cap = Capability::new(
            "ipc_endpoint",
            CapObject::Endpoint {
                index: 0,
                generation: 1,
            },
            &[CapRights::Send, CapRights::Receive],
        );

        let id = cspace.mint(cap).expect("cspace should have room");

        assert!(cspace.has_right(id, CapRights::Send));
        assert!(!cspace.has_right(id, CapRights::Schedule));
        assert_eq!(
            cspace.get(id).expect("cap should exist").object,
            CapObject::Endpoint {
                index: 0,
                generation: 1
            }
        );
    }

    #[test]
    fn derived_capability_cannot_escalate_rights_and_can_be_revoked() {
        let mut cspace = CapabilitySpace::default();
        let parent = cspace
            .mint(Capability::new(
                "ep_full",
                CapObject::Endpoint {
                    index: 0,
                    generation: 1,
                },
                &[CapRights::Send, CapRights::Receive],
            ))
            .expect("mint parent");

        let child = cspace
            .mint_derived(parent, "ep_send_only", &[CapRights::Send])
            .expect("derive subset rights");
        assert!(cspace.has_right(child, CapRights::Send));
        assert!(!cspace.has_right(child, CapRights::Receive));

        assert_eq!(
            cspace.mint_derived(parent, "bad", &[CapRights::Schedule]),
            Err(CapabilityDeriveError::RightsEscalation)
        );

        assert!(cspace.revoke(parent));
        assert!(cspace.get(parent).is_none());
        assert!(cspace.get(child).is_none());
    }

    #[test]
    fn mint_fails_when_space_full_and_returns_capability() {
        let mut cspace = CapabilitySpace::default();
        for _ in 0..MAX_CAPABILITIES {
            let _ = cspace
                .mint(Capability::new("x", CapObject::Kernel, &[CapRights::Read]))
                .expect("room");
        }
        let cap = Capability::new("overflow", CapObject::Kernel, &[CapRights::Write]);
        assert_eq!(cspace.mint(cap), Err(cap));
    }

    #[test]
    fn mint_derived_on_revoked_parent_returns_parent_missing() {
        let mut cspace = CapabilitySpace::default();
        let parent = cspace
            .mint(Capability::new("p", CapObject::Kernel, &[CapRights::Read]))
            .expect("mint");
        assert!(cspace.revoke(parent));
        assert_eq!(
            cspace.mint_derived(parent, "c", &[CapRights::Read]),
            Err(CapabilityDeriveError::ParentMissing)
        );
    }

    #[test]
    fn revoke_missing_and_double_revoke_behave() {
        let mut cspace = CapabilitySpace::default();
        let id = cspace
            .mint(Capability::new("p", CapObject::Kernel, &[CapRights::Read]))
            .expect("mint");
        assert!(cspace.revoke(id));
        assert!(!cspace.revoke(id));
        assert!(!cspace.revoke(CapId(u64::MAX)));
    }

    #[test]
    fn revoked_slot_reuse_produces_fresh_cap_id() {
        let mut cspace = CapabilitySpace::default();
        let first = cspace
            .mint(Capability::new("a", CapObject::Kernel, &[CapRights::Read]))
            .expect("mint");
        assert!(cspace.revoke(first));
        let second = cspace
            .mint(Capability::new("b", CapObject::Kernel, &[CapRights::Read]))
            .expect("mint2");
        assert_ne!(first, second);
        assert!(cspace.get(first).is_none());
        assert!(cspace.get(second).is_some());
    }
}
