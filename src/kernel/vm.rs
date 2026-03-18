use crate::arch::vm_layout;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtAddr(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysAddr(pub u64);

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

pub const PAGE_SIZE: usize = vm_layout::PAGE_SIZE;
pub const KERNEL_SPACE_BASE: u64 = vm_layout::KERNEL_SPACE_BASE;
pub const MAX_MAPPINGS: usize = vm_layout::MAX_MAPPINGS;
pub const MAX_ADDRESS_SPACES: usize = vm_layout::MAX_ADDRESS_SPACES;

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
        if !virt.0.is_multiple_of(PAGE_SIZE as u64)
            || !mapping.phys.0.is_multiple_of(PAGE_SIZE as u64)
        {
            return Err(VmError::Misaligned);
        }

        if !self.mapping_is_allowed(virt, mapping.flags) {
            return Err(VmError::PrivilegeViolation);
        }

        let mut first_free: Option<usize> = None;
        for i in 0..MAX_MAPPINGS {
            match self.entries[i].as_mut() {
                Some(entry) if entry.virt == virt => {
                    let old = entry.mapping;
                    entry.mapping = mapping;
                    return Ok(Some(old));
                }
                Some(_) => {}
                None if first_free.is_none() => first_free = Some(i),
                None => {}
            }
        }

        if let Some(i) = first_free {
            self.entries[i] = Some(Entry { virt, mapping });
            return Ok(None);
        }

        Err(VmError::Full)
    }

    fn mapping_is_allowed(&self, virt: VirtAddr, flags: PageFlags) -> bool {
        // Current policy is intentionally strict: user address spaces only accept user pages
        // below the split; kernel mappings are supervisor-only at/above the split.
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
    fn asid_in_use(&self, asid: Asid) -> bool {
        self.entries
            .iter()
            .flatten()
            .any(|entry| entry.asid == asid)
    }

    fn allocate_asid(&mut self) -> Result<Asid, VmError> {
        let asid_capacity = (1u32 << vm_layout::ASID_BITS) - 1;
        for _ in 0..asid_capacity {
            if self.next_asid == 0 {
                self.next_asid = 1;
            }

            let candidate = Asid(self.next_asid);
            self.next_asid = self.next_asid.wrapping_add(1);
            if !self.asid_in_use(candidate) {
                return Ok(candidate);
            }
        }
        Err(VmError::Full)
    }

    pub fn create_user_space(&mut self) -> Result<Asid, VmError> {
        let asid = self.allocate_asid()?;
        for slot in &mut self.entries {
            if slot.is_none() {
                *slot = Some(AsEntry {
                    asid,
                    aspace: AddressSpace::new_user(),
                });
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
    fn vm_constants_are_arch_sourced() {
        assert_eq!(PAGE_SIZE, vm_layout::PAGE_SIZE);
        assert_eq!(KERNEL_SPACE_BASE, vm_layout::KERNEL_SPACE_BASE);
        assert_eq!(MAX_MAPPINGS, vm_layout::MAX_MAPPINGS);
        assert_eq!(MAX_ADDRESS_SPACES, vm_layout::MAX_ADDRESS_SPACES);
    }

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

    #[test]
    fn map_rejects_misaligned_virt_and_phys() {
        let mut aspace = AddressSpace::new_user();
        let flags = PageFlags::USER_RX;

        let bad_virt = aspace.map_page(
            VirtAddr(0x1001),
            Mapping {
                phys: PhysAddr(0x4000),
                flags,
            },
        );
        assert_eq!(bad_virt, Err(VmError::Misaligned));

        let bad_phys = aspace.map_page(
            VirtAddr(0x2000),
            Mapping {
                phys: PhysAddr(0x4001),
                flags,
            },
        );
        assert_eq!(bad_phys, Err(VmError::Misaligned));
    }

    #[test]
    fn map_remap_and_unmap_paths() {
        let mut aspace = AddressSpace::new_user();
        let virt = VirtAddr(0x3000);
        let first = Mapping {
            phys: PhysAddr(0x5000),
            flags: PageFlags::USER_RX,
        };
        let second = Mapping {
            phys: PhysAddr(0x6000),
            flags: PageFlags {
                read: true,
                write: true,
                execute: false,
                user: true,
            },
        };

        assert_eq!(aspace.map_page(virt, first), Ok(None));
        assert_eq!(aspace.map_page(virt, second), Ok(Some(first)));
        assert_eq!(aspace.unmap_page(virt), Some(second));
        assert_eq!(aspace.unmap_page(virt), None);
    }

    #[test]
    fn map_space_and_manager_capacity_limits() {
        let mut aspace = AddressSpace::new_user();
        for i in 0..MAX_MAPPINGS {
            let virt = VirtAddr(((i + 1) * PAGE_SIZE) as u64);
            let phys = PhysAddr(((i + 100) * PAGE_SIZE) as u64);
            assert_eq!(
                aspace.map_page(
                    virt,
                    Mapping {
                        phys,
                        flags: PageFlags::USER_RX,
                    }
                ),
                Ok(None)
            );
        }

        assert_eq!(
            aspace.map_page(
                VirtAddr(((MAX_MAPPINGS + 1) * PAGE_SIZE) as u64),
                Mapping {
                    phys: PhysAddr(0x9000_0000),
                    flags: PageFlags::USER_RX,
                }
            ),
            Err(VmError::Full)
        );

        let mut mgr = AddressSpaceManager::default();
        for _ in 0..MAX_ADDRESS_SPACES {
            assert!(mgr.create_user_space().is_ok());
        }
        assert_eq!(mgr.create_user_space(), Err(VmError::Full));
    }

    #[test]
    fn manager_invalid_destroy_and_monotonic_asid() {
        let mut mgr = AddressSpaceManager::default();
        assert_eq!(mgr.destroy(Asid(777)), Err(VmError::InvalidAsid));

        let a = mgr.create_user_space().expect("asid a");
        let b = mgr.create_user_space().expect("asid b");
        assert_ne!(a, b);
    }
}
