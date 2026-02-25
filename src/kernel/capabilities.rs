#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CapId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapRights {
    Read,
    Write,
    Map,
    Send,
    Receive,
    Schedule,
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
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapObject {
    Kernel,
    Endpoint { index: usize, generation: u64 },
    AddressSpace { asid: u16 },
    MemoryObject { id: u64 },
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
    id: CapId,
    capability: Capability,
}

#[derive(Debug)]
pub struct CapabilitySpace {
    next_id: u64,
    entries: [Option<CapEntry>; MAX_CAPABILITIES],
}

impl Default for CapabilitySpace {
    fn default() -> Self {
        Self {
            next_id: 0,
            entries: [None; MAX_CAPABILITIES],
        }
    }
}

impl CapabilitySpace {
    pub fn mint(&mut self, capability: Capability) -> Result<CapId, Capability> {
        let id = CapId(self.next_id);

        for slot in &mut self.entries {
            if slot.is_none() {
                *slot = Some(CapEntry { id, capability });
                self.next_id += 1;
                return Ok(id);
            }
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

        self.mint(derived)
            .map_err(|_| CapabilityDeriveError::SpaceFull)
    }

    pub fn revoke(&mut self, id: CapId) -> bool {
        for slot in &mut self.entries {
            if slot.as_ref().is_some_and(|entry| entry.id == id) {
                *slot = None;
                return true;
            }
        }
        false
    }

    pub fn get(&self, id: CapId) -> Option<Capability> {
        for slot in &self.entries {
            if let Some(entry) = slot {
                if entry.id == id {
                    return Some(entry.capability);
                }
            }
        }
        None
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

        assert!(cspace.revoke(child));
        assert!(cspace.get(child).is_none());
    }
}
