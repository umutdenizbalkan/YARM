use crate::arch::vm_layout;
use crate::kernel::topology::CpuBitmap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtAddr(pub u64);

impl VirtAddr {
    pub const fn new(addr: u64) -> Self {
        Self(addr)
    }

    pub const fn is_user(self) -> bool {
        self.0 < KERNEL_SPACE_BASE
    }

    pub const fn is_kernel(self) -> bool {
        self.0 >= KERNEL_SPACE_BASE
    }

    pub const fn page_align_down(self) -> Self {
        Self(self.0 & !(PAGE_SIZE as u64 - 1))
    }

    pub const fn page_align_up(self) -> Self {
        Self((self.0 + PAGE_SIZE as u64 - 1) & !(PAGE_SIZE as u64 - 1))
    }
}

impl core::ops::Add<u64> for VirtAddr {
    type Output = Self;

    fn add(self, rhs: u64) -> Self::Output {
        Self(self.0.wrapping_add(rhs))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysAddr(pub u64);

impl PhysAddr {
    pub const fn new(addr: u64) -> Self {
        Self(addr)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Asid(pub u16);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressSpaceKind {
    Kernel,
    User,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageFlags {
    // TODO: Add CachePolicy (WriteBack/WriteThrough/Uncached/Device) before
    // the DMA-capable driver subsystem is finalized for embedded targets.
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

#[cfg(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    target_arch = "riscv64"
))]
fn arch_register_asid(asid: Asid) -> Result<(), VmError> {
    crate::arch::selected_isa::page_table::ensure_asid_root(asid).map_err(|_| VmError::Full)
}

#[cfg(not(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    target_arch = "riscv64"
)))]
fn arch_register_asid(_asid: Asid) -> Result<(), VmError> {
    Ok(())
}

#[cfg(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    target_arch = "riscv64"
))]
fn arch_unregister_asid(asid: Asid) {
    crate::arch::selected_isa::page_table::remove_asid_root(asid);
    crate::arch::selected_isa::page_table::invalidate_asid(asid);
}

#[cfg(not(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    target_arch = "riscv64"
)))]
fn arch_unregister_asid(_asid: Asid) {}

#[cfg(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    target_arch = "riscv64"
))]
fn arch_map_page(asid: Option<Asid>, virt: VirtAddr, mapping: Mapping) -> Result<(), VmError> {
    if let Some(asid) = asid {
        crate::arch::selected_isa::page_table::map_page(asid, virt, mapping.phys, mapping.flags)
            .map_err(|_| VmError::Full)?;
    }
    Ok(())
}

#[cfg(not(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    target_arch = "riscv64"
)))]
fn arch_map_page(_asid: Option<Asid>, _virt: VirtAddr, _mapping: Mapping) -> Result<(), VmError> {
    Ok(())
}

#[cfg(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    target_arch = "riscv64"
))]
fn arch_unmap_page(asid: Option<Asid>, virt: VirtAddr) {
    if let Some(asid) = asid {
        let _ = crate::arch::selected_isa::page_table::unmap_page(asid, virt);
    }
}

#[cfg(not(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    target_arch = "riscv64"
)))]
fn arch_unmap_page(_asid: Option<Asid>, _virt: VirtAddr) {}

#[cfg(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    target_arch = "riscv64"
))]
fn arch_cr3_for_asid(asid: Asid) -> Option<u64> {
    crate::arch::selected_isa::page_table::cr3_for_asid(asid)
}

#[cfg(not(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    target_arch = "riscv64"
)))]
fn arch_cr3_for_asid(_asid: Asid) -> Option<u64> {
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Entry {
    virt: VirtAddr,
    mapping: Mapping,
}

/// Software shadow of the hardware page table.
///
/// `map_page` and `unmap_page` keep this in-kernel record in sync with the
/// selected-ISA page-table backend. Architecture hooks perform page-table
/// updates and TLB invalidations as part of mapping changes.
#[derive(Debug)]
pub struct AddressSpace {
    kind: AddressSpaceKind,
    asid: Option<Asid>,
    entries: [Option<Entry>; MAX_MAPPINGS],
    len: usize,
}

impl AddressSpace {
    pub fn new_kernel() -> Self {
        Self {
            kind: AddressSpaceKind::Kernel,
            asid: None,
            entries: [None; MAX_MAPPINGS],
            len: 0,
        }
    }

    pub fn new_user() -> Self {
        Self {
            kind: AddressSpaceKind::User,
            asid: None,
            entries: [None; MAX_MAPPINGS],
            len: 0,
        }
    }

    pub fn new_user_with_asid(asid: Asid) -> Self {
        Self {
            kind: AddressSpaceKind::User,
            asid: Some(asid),
            entries: [None; MAX_MAPPINGS],
            len: 0,
        }
    }

    pub fn kind(&self) -> AddressSpaceKind {
        self.kind
    }

    pub fn asid(&self) -> Option<Asid> {
        self.asid
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
                    arch_map_page(self.asid, virt, mapping)?;
                    entry.mapping = mapping;
                    return Ok(Some(old));
                }
                Some(_) => {}
                None if first_free.is_none() => first_free = Some(i),
                None => {}
            }
        }

        if let Some(i) = first_free {
            arch_map_page(self.asid, virt, mapping)?;
            self.entries[i] = Some(Entry { virt, mapping });
            self.len += 1;
            return Ok(None);
        }

        Err(VmError::Full)
    }

    fn mapping_is_allowed(&self, virt: VirtAddr, flags: PageFlags) -> bool {
        let is_kernel_only = !flags.user && (flags.read || flags.write || flags.execute);
        match self.kind {
            AddressSpaceKind::Kernel => virt.is_kernel() && !flags.user,
            AddressSpaceKind::User => virt.is_user() && !is_kernel_only,
        }
    }

    pub fn unmap_page(&mut self, virt: VirtAddr) -> Option<Mapping> {
        for slot in &mut self.entries {
            if let Some(entry) = slot.as_ref()
                && entry.virt == virt
            {
                self.len = self.len.saturating_sub(1);
                let old = slot.take().map(|old| old.mapping);
                arch_unmap_page(self.asid, virt);
                return old;
            }
        }
        None
    }

    /// Resolves a virtual address to its mapping.
    ///
    /// Complexity: O(N) where N is `MAX_MAPPINGS`. For the current prototype
    /// this remains acceptable, but a larger-scale implementation should use a
    /// more efficient indexed structure.
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
        self.len
    }

    pub fn has_mapping_for_phys(&self, phys: PhysAddr) -> bool {
        self.entries
            .iter()
            .flatten()
            .any(|entry| entry.mapping.phys == phys)
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
    pub age_ticks: u32,
}

/// Tracks live and retired software address spaces.
///
/// NOTE: `retired` has the same capacity as `entries`. If all
/// `MAX_ADDRESS_SPACES` are destroyed simultaneously with pending shootdowns,
/// no further `destroy()` call can retire another ASID until an existing
/// retired slot is cleared by `acknowledge_shootdown()`.
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
    const SHOOTDOWN_TIMEOUT_TICKS: u32 = 16;

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
        arch_register_asid(asid)?;
        for slot in &mut self.entries {
            if slot.is_none() {
                *slot = Some(AsEntry {
                    asid,
                    aspace: AddressSpace::new_user_with_asid(asid),
                });
                return Ok(asid);
            }
        }
        arch_unregister_asid(asid);
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

    pub fn cr3_for_asid(&self, asid: Asid) -> Option<u64> {
        if self.get(asid).is_none() {
            return None;
        }
        arch_cr3_for_asid(asid)
    }

    /// Destroys a software address space shadow and optionally retires its ASID
    /// until all CPUs in `pending_cpu_bitmap` acknowledge a shootdown.
    ///
    /// If `pending_cpu_bitmap == 0`, the ASID is immediately reusable and
    /// callers must not later invoke `acknowledge_shootdown()` for that ASID.
    pub fn destroy(&mut self, asid: Asid, pending_cpu_bitmap: CpuBitmap) -> Result<(), VmError> {
        for slot in &mut self.entries {
            if slot.as_ref().is_some_and(|entry| entry.asid == asid) {
                *slot = None;
                arch_unregister_asid(asid);
                if pending_cpu_bitmap == 0 {
                    return Ok(());
                }
                for retired in &mut self.retired {
                    if retired.is_none() {
                        *retired = Some(RetiredAsid {
                            asid,
                            pending_cpu_bitmap,
                            age_ticks: 0,
                        });
                        return Ok(());
                    }
                }
                return Err(VmError::Full);
            }
        }
        Err(VmError::InvalidAsid)
    }

    /// Acknowledges one CPU's shootdown for a retired ASID.
    ///
    /// This is only valid for ASIDs retired by `destroy()` with a non-zero
    /// `pending_cpu_bitmap`.
    pub fn acknowledge_shootdown(
        &mut self,
        asid: Asid,
        cpu_bit: CpuBitmap,
    ) -> Result<bool, VmError> {
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
        self.retired
            .iter()
            .flatten()
            .copied()
            .find(|entry| entry.asid == asid)
    }

    pub fn tick_retired_shootdowns(&mut self) -> usize {
        let mut timed_out = 0usize;
        for slot in &mut self.retired {
            let Some(retired) = slot.as_mut() else {
                continue;
            };
            retired.age_ticks = retired.age_ticks.saturating_add(1);
            if retired.age_ticks >= Self::SHOOTDOWN_TIMEOUT_TICKS {
                *slot = None;
                timed_out += 1;
            }
        }
        timed_out
    }

    pub fn any_mapping_for_phys(&self, phys: PhysAddr) -> bool {
        self.entries
            .iter()
            .flatten()
            .any(|entry| entry.aspace.has_mapping_for_phys(phys))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vm_constants_obey_basic_invariants() {
        assert!(
            PAGE_SIZE.is_power_of_two(),
            "PAGE_SIZE must be a power of two"
        );
        assert!(PAGE_SIZE >= 4096, "PAGE_SIZE must be at least 4096");
        assert!(
            KERNEL_SPACE_BASE.is_power_of_two(),
            "KERNEL_SPACE_BASE must be power-of-two aligned"
        );
        assert!(
            KERNEL_SPACE_BASE > 0,
            "KERNEL_SPACE_BASE must leave some lower user virtual space"
        );
        assert!(MAX_MAPPINGS > 0, "MAX_MAPPINGS must be non-zero");
        assert!(
            MAX_ADDRESS_SPACES > 0,
            "MAX_ADDRESS_SPACES must be non-zero"
        );
    }

    #[test]
    fn virt_addr_helpers_cover_common_queries() {
        let user = VirtAddr::new(0x1003);
        let kernel = VirtAddr::new(KERNEL_SPACE_BASE);

        assert!(user.is_user());
        assert!(!user.is_kernel());
        assert!(kernel.is_kernel());
        assert!(!kernel.is_user());
        assert_eq!(user.page_align_down(), VirtAddr(0x1000));
        assert_eq!(user.page_align_up(), VirtAddr(0x2000));
        assert_eq!(user + 5, VirtAddr(0x1008));
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
        assert_eq!(
            mgr.retired_entry(asid)
                .map(|entry| entry.pending_cpu_bitmap),
            Some(0b11)
        );

        let replacement = mgr.create_user_space().expect("replacement asid");
        assert_ne!(replacement, asid);

        assert_eq!(mgr.acknowledge_shootdown(asid, 0b01), Ok(false));
        assert_eq!(
            mgr.retired_entry(asid)
                .map(|entry| entry.pending_cpu_bitmap),
            Some(0b10)
        );
        assert_eq!(mgr.acknowledge_shootdown(asid, 0b10), Ok(true));
        assert_eq!(mgr.retired_entry(asid), None);
    }

    #[test]
    fn retired_asid_timeout_prevents_indefinite_stall() {
        let mut mgr = AddressSpaceManager::default();
        let asid = mgr.create_user_space().expect("create");

        assert_eq!(mgr.destroy(asid, 0b11), Ok(()));
        assert!(mgr.retired_entry(asid).is_some());

        let mut timed_out = 0usize;
        for _ in 0..AddressSpaceManager::SHOOTDOWN_TIMEOUT_TICKS {
            timed_out += mgr.tick_retired_shootdowns();
        }

        assert_eq!(timed_out, 1);
        assert_eq!(mgr.retired_entry(asid), None);
    }

    #[test]
    fn repeated_destroy_recreate_cycles_with_pending_shootdowns() {
        let mut mgr = AddressSpaceManager::default();

        for cycle in 0..64usize {
            let asid = mgr.create_user_space().expect("create");
            assert_eq!(mgr.destroy(asid, 0b11), Ok(()));
            assert!(mgr.retired_entry(asid).is_some());

            if cycle % 2 == 0 {
                assert_eq!(mgr.acknowledge_shootdown(asid, 0b01), Ok(false));
                assert_eq!(mgr.acknowledge_shootdown(asid, 0b10), Ok(true));
            } else {
                for _ in 0..AddressSpaceManager::SHOOTDOWN_TIMEOUT_TICKS {
                    let _ = mgr.tick_retired_shootdowns();
                }
            }

            assert_eq!(mgr.retired_entry(asid), None);
        }
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
