use crate::arch::vm_layout;
use crate::kernel::topology::CpuBitmap;

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

    pub const USER_RW: Self = Self {
        read: true,
        write: true,
        execute: false,
        user: true,
    };

    pub const GUARD: Self = Self {
        read: false,
        write: false,
        execute: false,
        user: false,
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
        let is_kernel_only = !flags.user && (flags.read || flags.write || flags.execute);
        match self.kind {
            AddressSpaceKind::Kernel => virt.0 >= KERNEL_SPACE_BASE && !flags.user,
            AddressSpaceKind::User => virt.0 < KERNEL_SPACE_BASE && !is_kernel_only,
        }
    }

    pub fn unmap_page(&mut self, virt: VirtAddr) -> Option<Mapping> {
        for slot in &mut self.entries {
            if let Some(entry) = slot.as_ref()
                && entry.virt == virt
            {
                return slot.take().map(|old| old.mapping);
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetiredAsid {
    pub asid: Asid,
    pub pending_cpu_bitmap: CpuBitmap,
}

#[derive(Debug)]
pub struct AddressSpaceManager {
    next_asid: u16,
    entries: [Option<AsEntry>; MAX_ADDRESS_SPACES],
    retired: [Option<RetiredAsid>; MAX_ADDRESS_SPACES],
}

impl Default for AddressSpaceManager {
    fn default() -> Self {
        Self {
            next_asid: 1,
            entries: [const { None }; MAX_ADDRESS_SPACES],
            retired: [None; MAX_ADDRESS_SPACES],
        }
    }
}

impl AddressSpaceManager {
    fn asid_in_use(&self, asid: Asid) -> bool {
        self.entries
            .iter()
            .flatten()
            .any(|entry| entry.asid == asid)
            || self
                .retired
                .iter()
                .flatten()
                .any(|entry| entry.asid == asid)
    }

    fn allocate_asid(&mut self) -> Result<Asid, VmError> {
        for _ in 0..MAX_ADDRESS_SPACES {
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

    pub fn destroy(&mut self, asid: Asid, pending_cpu_bitmap: CpuBitmap) -> Result<(), VmError> {
        for slot in &mut self.entries {
            if slot.as_ref().is_some_and(|entry| entry.asid == asid) {
                *slot = None;
                if pending_cpu_bitmap == 0 {
                    return Ok(());
                }
                for retired in &mut self.retired {
                    if retired.is_none() {
                        *retired = Some(RetiredAsid {
                            asid,
                            pending_cpu_bitmap,
                        });
                        return Ok(());
                    }
                }
                return Err(VmError::Full);
            }
        }
        Err(VmError::InvalidAsid)
    }

    pub fn acknowledge_shootdown(&mut self, asid: Asid, cpu_bit: CpuBitmap) -> Result<bool, VmError> {
        for slot in &mut self.retired {
            if let Some(retired) = slot.as_mut()
                && retired.asid == asid
            {
                retired.pending_cpu_bitmap &= !cpu_bit;
                if retired.pending_cpu_bitmap == 0 {
                    *slot = None;
                    return Ok(true);
                }
                return Ok(false);
            }
        }
        Err(VmError::InvalidAsid)
    }

    pub fn retired_entry(&self, asid: Asid) -> Option<RetiredAsid> {
        self.retired.iter().flatten().copied().find(|entry| entry.asid == asid)
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

        assert!(mgr.destroy(asid, 0).is_ok());
        assert!(mgr.get(asid).is_none());
    }

    #[test]
    fn destroy_retires_asid_until_all_shootdowns_acknowledge() {
        let mut mgr = AddressSpaceManager::default();
        let asid = mgr.create_user_space().expect("create");

        assert_eq!(mgr.destroy(asid, 0b11), Ok(()));
        assert!(mgr.get(asid).is_none());
        assert_eq!(mgr.retired_entry(asid).map(|entry| entry.pending_cpu_bitmap), Some(0b11));

        let replacement = mgr.create_user_space().expect("replacement asid");
        assert_ne!(replacement, asid);

        assert_eq!(mgr.acknowledge_shootdown(asid, 0b01), Ok(false));
        assert_eq!(mgr.retired_entry(asid).map(|entry| entry.pending_cpu_bitmap), Some(0b10));
        assert_eq!(mgr.acknowledge_shootdown(asid, 0b10), Ok(true));
        assert_eq!(mgr.retired_entry(asid), None);
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
        assert_eq!(mgr.destroy(Asid(777), 0), Err(VmError::InvalidAsid));

        let a = mgr.create_user_space().expect("asid a");
        let b = mgr.create_user_space().expect("asid b");
        assert_ne!(a, b);
    }
}
