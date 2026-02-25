#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtAddr(pub usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysAddr(pub usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Asid(pub u16);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressSpaceKind {
    Kernel,
    User,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageFlags {
    pub read: bool,
    pub write: bool,
    pub execute: bool,
    pub user: bool,
}

impl PageFlags {
    pub const KERNEL_RW: Self = Self {
        read: true,
        write: true,
        execute: false,
        user: false,
    };

    pub const USER_RX: Self = Self {
        read: true,
        write: false,
        execute: true,
        user: true,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mapping {
    pub phys: PhysAddr,
    pub flags: PageFlags,
}

pub const PAGE_SIZE: usize = 4096;
pub const KERNEL_SPACE_BASE: usize = 0x8000_0000;
pub const MAX_MAPPINGS: usize = 128;
pub const MAX_ADDRESS_SPACES: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmError {
    Full,
    Misaligned,
    PrivilegeViolation,
    InvalidAsid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Entry {
    virt: VirtAddr,
    mapping: Mapping,
}

#[derive(Debug)]
pub struct AddressSpace {
    kind: AddressSpaceKind,
    entries: [Option<Entry>; MAX_MAPPINGS],
}

impl AddressSpace {
    pub fn new_kernel() -> Self {
        Self {
            kind: AddressSpaceKind::Kernel,
            entries: [None; MAX_MAPPINGS],
        }
    }

    pub fn new_user() -> Self {
        Self {
            kind: AddressSpaceKind::User,
            entries: [None; MAX_MAPPINGS],
        }
    }

    pub fn kind(&self) -> AddressSpaceKind {
        self.kind
    }

    pub fn map_page(
        &mut self,
        virt: VirtAddr,
        mapping: Mapping,
    ) -> Result<Option<Mapping>, VmError> {
        if !virt.0.is_multiple_of(PAGE_SIZE) || !mapping.phys.0.is_multiple_of(PAGE_SIZE) {
            return Err(VmError::Misaligned);
        }

        if !self.mapping_is_allowed(virt, mapping.flags) {
            return Err(VmError::PrivilegeViolation);
        }

        for slot in &mut self.entries {
            if let Some(entry) = slot.as_mut() {
                if entry.virt == virt {
                    let old = entry.mapping;
                    entry.mapping = mapping;
                    return Ok(Some(old));
                }
            }
        }

        for slot in &mut self.entries {
            if slot.is_none() {
                *slot = Some(Entry { virt, mapping });
                return Ok(None);
            }
        }

        Err(VmError::Full)
    }

    fn mapping_is_allowed(&self, virt: VirtAddr, flags: PageFlags) -> bool {
        match self.kind {
            AddressSpaceKind::Kernel => virt.0 >= KERNEL_SPACE_BASE && !flags.user,
            AddressSpaceKind::User => virt.0 < KERNEL_SPACE_BASE && flags.user,
        }
    }

    pub fn unmap_page(&mut self, virt: VirtAddr) -> Option<Mapping> {
        for slot in &mut self.entries {
            if slot.as_ref().is_some_and(|entry| entry.virt == virt) {
                let old = slot.take().expect("checked is_some");
                return Some(old.mapping);
            }
        }
        None
    }

    pub fn resolve(&self, virt: VirtAddr) -> Option<Mapping> {
        for slot in &self.entries {
            if let Some(entry) = slot {
                if entry.virt == virt {
                    return Some(entry.mapping);
                }
            }
        }
        None
    }

    pub fn mappings(&self) -> usize {
        self.entries.iter().filter(|e| e.is_some()).count()
    }
}

impl Default for AddressSpace {
    fn default() -> Self {
        Self::new_kernel()
    }
}

#[derive(Debug)]
struct AsEntry {
    asid: Asid,
    aspace: AddressSpace,
}

#[derive(Debug)]
pub struct AddressSpaceManager {
    next_asid: u16,
    entries: [Option<AsEntry>; MAX_ADDRESS_SPACES],
}

impl Default for AddressSpaceManager {
    fn default() -> Self {
        Self {
            next_asid: 1,
            entries: [const { None }; MAX_ADDRESS_SPACES],
        }
    }
}

impl AddressSpaceManager {
    pub fn create_user_space(&mut self) -> Result<Asid, VmError> {
        let asid = Asid(self.next_asid);
        for slot in &mut self.entries {
            if slot.is_none() {
                *slot = Some(AsEntry {
                    asid,
                    aspace: AddressSpace::new_user(),
                });
                self.next_asid = self.next_asid.wrapping_add(1);
                return Ok(asid);
            }
        }
        Err(VmError::Full)
    }

    pub fn get(&self, asid: Asid) -> Option<&AddressSpace> {
        self.entries
            .iter()
            .flatten()
            .find(|entry| entry.asid == asid)
            .map(|entry| &entry.aspace)
    }

    pub fn get_mut(&mut self, asid: Asid) -> Option<&mut AddressSpace> {
        self.entries
            .iter_mut()
            .flatten()
            .find(|entry| entry.asid == asid)
            .map(|entry| &mut entry.aspace)
    }

    pub fn destroy(&mut self, asid: Asid) -> Result<(), VmError> {
        for slot in &mut self.entries {
            if slot.as_ref().is_some_and(|entry| entry.asid == asid) {
                *slot = None;
                return Ok(());
            }
        }
        Err(VmError::InvalidAsid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_and_resolve_page() {
        let mut aspace = AddressSpace::new_kernel();
        let va = VirtAddr(0x8000_1000);
        let mapping = Mapping {
            phys: PhysAddr(0x2000),
            flags: PageFlags::KERNEL_RW,
        };

        assert_eq!(aspace.map_page(va, mapping), Ok(None));
        assert_eq!(aspace.resolve(va), Some(mapping));
    }

    #[test]
    fn user_space_rejects_kernel_range_and_kernel_flags() {
        let mut user = AddressSpace::new_user();
        let mapping = Mapping {
            phys: PhysAddr(0x3000),
            flags: PageFlags::KERNEL_RW,
        };

        assert_eq!(
            user.map_page(VirtAddr(0x1000), mapping),
            Err(VmError::PrivilegeViolation)
        );
        assert_eq!(
            user.map_page(
                VirtAddr(KERNEL_SPACE_BASE),
                Mapping {
                    phys: PhysAddr(0x3000),
                    flags: PageFlags::USER_RX,
                }
            ),
            Err(VmError::PrivilegeViolation)
        );
    }

    #[test]
    fn manager_creates_and_destroys_user_spaces() {
        let mut mgr = AddressSpaceManager::default();
        let asid = mgr.create_user_space().expect("create");
        let user = mgr.get_mut(asid).expect("present");
        assert_eq!(user.kind(), AddressSpaceKind::User);

        let map_result = user.map_page(
            VirtAddr(0x1000),
            Mapping {
                phys: PhysAddr(0x4000),
                flags: PageFlags::USER_RX,
            },
        );
        assert_eq!(map_result, Ok(None));

        assert!(mgr.destroy(asid).is_ok());
        assert!(mgr.get(asid).is_none());
    }
}
