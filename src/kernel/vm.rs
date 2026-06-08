// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

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
pub enum CachePolicy {
    WriteBack,
    WriteThrough,
    Uncached,
    Device,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageFlags {
    pub read: bool,
    pub write: bool,
    pub execute: bool,
    pub user: bool,
    pub cache_policy: CachePolicy,
}

impl PageFlags {
    pub const KERNEL_RW: Self = Self {
        read: true,
        write: true,
        execute: false,
        user: false,
        cache_policy: CachePolicy::WriteBack,
    };

    pub const USER_RX: Self = Self {
        read: true,
        write: false,
        execute: true,
        user: true,
        cache_policy: CachePolicy::WriteBack,
    };

    pub const USER_RW: Self = Self {
        read: true,
        write: true,
        execute: false,
        user: true,
        cache_policy: CachePolicy::WriteBack,
    };

    pub const DEVICE_RW: Self = Self {
        read: true,
        write: true,
        execute: false,
        user: false,
        cache_policy: CachePolicy::Device,
    };

    pub const GUARD: Self = Self {
        read: false,
        write: false,
        execute: false,
        user: false,
        cache_policy: CachePolicy::WriteBack,
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
    crate::arch::selected_isa::page_table::ensure_asid_root(asid).map_err(|err| {
        crate::yarm_log!(
            "VM_FULL reason=ensure_asid_root_failed asid={} err={:?}",
            asid.0,
            err
        );
        VmError::Full
    })
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
    crate::arch::selected_isa::page_table::invalidate_asid(asid);
    crate::arch::selected_isa::page_table::remove_asid_root(asid);
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
            .map_err(|err| {
                crate::yarm_log!(
                    "VM_FULL reason=arch_map_page_failed asid={} va=0x{:x} pa=0x{:x} err={:?}",
                    asid.0,
                    virt.0,
                    mapping.phys.0,
                    err
                );
                VmError::Full
            })?;
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
    pages: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MappingEntry {
    pub virt: VirtAddr,
    pub mapping: Mapping,
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
    fn isolate_page_entry_at(&mut self, idx: usize, virt: VirtAddr) -> Result<usize, VmError> {
        let entry = self.entries[idx].expect("entry");
        let page_offset = ((virt.0 - entry.virt.0) / PAGE_SIZE as u64) as usize;
        if page_offset >= entry.pages {
            return Err(VmError::PrivilegeViolation);
        }
        if page_offset == 0 {
            return Ok(idx);
        }
        if self.len >= MAX_MAPPINGS {
            return Err(VmError::Full);
        }
        for shift_idx in (idx + 1..self.len).rev() {
            self.entries[shift_idx + 1] = self.entries[shift_idx];
        }
        let left_pages = page_offset;
        let right_pages = entry.pages - page_offset;
        self.entries[idx] = Some(Entry {
            virt: entry.virt,
            mapping: entry.mapping,
            pages: left_pages,
        });
        self.entries[idx + 1] = Some(Entry {
            virt,
            mapping: Mapping {
                phys: PhysAddr(entry.mapping.phys.0 + (page_offset as u64 * PAGE_SIZE as u64)),
                flags: entry.mapping.flags,
            },
            pages: right_pages,
        });
        self.len += 1;
        Ok(idx + 1)
    }
    fn find_entry_index(&self, virt: VirtAddr) -> Result<usize, usize> {
        let mut lo = 0usize;
        let mut hi = self.len;
        while lo < hi {
            let mid = lo + ((hi - lo) / 2);
            let mid_virt = self.entries[mid].expect("dense mapping table").virt;
            if mid_virt.0 < virt.0 {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        if lo < self.len && self.entries[lo].is_some_and(|entry| entry.virt == virt) {
            Ok(lo)
        } else {
            Err(lo)
        }
    }

    fn entry_end_virt(entry: &Entry) -> u64 {
        entry.virt.0 + (entry.pages as u64 * PAGE_SIZE as u64)
    }

    fn find_entry_containing(&self, virt: VirtAddr) -> Option<usize> {
        if self.len == 0 {
            return None;
        }
        let mut lo = 0usize;
        let mut hi = self.len;
        while lo < hi {
            let mid = lo + ((hi - lo) / 2);
            let entry = self.entries[mid].expect("dense mapping table");
            if Self::entry_end_virt(&entry) <= virt.0 {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        if lo < self.len {
            let entry = self.entries[lo].expect("dense mapping table");
            if entry.virt.0 <= virt.0 && virt.0 < Self::entry_end_virt(&entry) {
                return Some(lo);
            }
        }
        None
    }

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

        #[cfg(not(feature = "hosted-dev"))]
        if self.kind == AddressSpaceKind::User {
            let pa = mapping.phys.0;
            let reserved = crate::kernel::frame_allocator::is_pa_reserved(pa).is_some();
            crate::yarm_log!(
                "USER_MAP_PA_CHECK asid={} va=0x{:x} pa=0x{:x} reserved={}",
                self.asid.map(|a| a.0).unwrap_or(0),
                virt.0,
                pa,
                reserved
            );
        }

        let exact_idx = match self.find_entry_index(virt) {
            Ok(i) => Some(i),
            Err(_) => None,
        };
        let containing_idx = self.find_entry_containing(virt);
        let effective_exact_idx = match (exact_idx, containing_idx) {
            (some @ Some(_), _) => some,
            (None, Some(idx)) => Some(self.isolate_page_entry_at(idx, virt)?),
            (None, None) => None,
        };
        match effective_exact_idx {
            Some(i) => {
                let entry = self.entries[i].as_mut().expect("entry");
                let old = entry.mapping;
                arch_map_page(self.asid, virt, mapping)?;
                entry.mapping = mapping;
                Ok(Some(old))
            }
            None => {
                let i = self.find_entry_index(virt).err().expect("insert idx");
                if self.len >= MAX_MAPPINGS {
                    crate::yarm_log!(
                        "VM_FULL reason=mapping_bookkeeping_full asid={:?} len={} max_mappings={} va=0x{:x}",
                        self.asid.map(|v| v.0),
                        self.len,
                        MAX_MAPPINGS,
                        virt.0
                    );
                    return Err(VmError::Full);
                }
                arch_map_page(self.asid, virt, mapping)?;
                let prev_merge = i > 0
                    && self.entries[i - 1].is_some_and(|prev| {
                        prev.mapping.flags == mapping.flags
                            && Self::entry_end_virt(&prev) == virt.0
                            && prev.mapping.phys.0 + (prev.pages as u64 * PAGE_SIZE as u64)
                                == mapping.phys.0
                    });
                if prev_merge {
                    let prev = self.entries[i - 1].as_mut().expect("prev");
                    prev.pages += 1;
                    return Ok(None);
                }

                let next_merge = i < self.len
                    && self.entries[i].is_some_and(|next| {
                        next.mapping.flags == mapping.flags
                            && virt.0 + PAGE_SIZE as u64 == next.virt.0
                            && mapping.phys.0 + PAGE_SIZE as u64 == next.mapping.phys.0
                    });
                if next_merge {
                    let next = self.entries[i].as_mut().expect("next");
                    next.virt = virt;
                    next.mapping = mapping;
                    next.pages += 1;
                    return Ok(None);
                }

                for shift_idx in (i..self.len).rev() {
                    self.entries[shift_idx + 1] = self.entries[shift_idx];
                }
                self.entries[i] = Some(Entry {
                    virt,
                    mapping,
                    pages: 1,
                });
                self.len += 1;
                Ok(None)
            }
        }
    }

    fn mapping_is_allowed(&self, virt: VirtAddr, flags: PageFlags) -> bool {
        let is_kernel_only = !flags.user && (flags.read || flags.write || flags.execute);
        match self.kind {
            AddressSpaceKind::Kernel => virt.is_kernel() && !flags.user,
            AddressSpaceKind::User => virt.is_user() && !is_kernel_only,
        }
    }

    pub fn unmap_page(&mut self, virt: VirtAddr) -> Option<Mapping> {
        let idx = self.find_entry_containing(virt)?;
        let entry = self.entries[idx]?;
        let page_offset = ((virt.0 - entry.virt.0) / PAGE_SIZE as u64) as usize;
        let old = Mapping {
            phys: PhysAddr(entry.mapping.phys.0 + (page_offset as u64 * PAGE_SIZE as u64)),
            flags: entry.mapping.flags,
        };
        arch_unmap_page(self.asid, virt);
        if entry.pages == 1 {
            for shift_idx in idx..self.len.saturating_sub(1) {
                self.entries[shift_idx] = self.entries[shift_idx + 1];
            }
            self.entries[self.len - 1] = None;
            self.len -= 1;
            return Some(old);
        }
        if page_offset == 0 {
            let current = self.entries[idx].as_mut().expect("entry");
            current.virt = VirtAddr(current.virt.0 + PAGE_SIZE as u64);
            current.mapping.phys = PhysAddr(current.mapping.phys.0 + PAGE_SIZE as u64);
            current.pages -= 1;
            return Some(old);
        }
        if page_offset == entry.pages - 1 {
            self.entries[idx].as_mut().expect("entry").pages -= 1;
            return Some(old);
        }
        if self.len >= MAX_MAPPINGS {
            return Some(old);
        }
        for shift_idx in (idx + 1..self.len).rev() {
            self.entries[shift_idx + 1] = self.entries[shift_idx];
        }
        let left_pages = page_offset;
        let right_pages = entry.pages - page_offset - 1;
        self.entries[idx] = Some(Entry {
            virt: entry.virt,
            mapping: entry.mapping,
            pages: left_pages,
        });
        self.entries[idx + 1] = Some(Entry {
            virt: VirtAddr(virt.0 + PAGE_SIZE as u64),
            mapping: Mapping {
                phys: PhysAddr(old.phys.0 + PAGE_SIZE as u64),
                flags: old.flags,
            },
            pages: right_pages,
        });
        self.len += 1;
        Some(old)
    }

    /// Resolves a virtual address to its mapping.
    ///
    /// Complexity: O(log N) over the sorted fixed-size mapping table.
    pub fn resolve(&self, virt: VirtAddr) -> Option<Mapping> {
        let idx = self.find_entry_containing(virt)?;
        let entry = self.entries[idx]?;
        let page_offset = (virt.0 - entry.virt.0) / PAGE_SIZE as u64;
        Some(Mapping {
            phys: PhysAddr(entry.mapping.phys.0 + page_offset * PAGE_SIZE as u64),
            flags: entry.mapping.flags,
        })
    }

    pub fn mappings(&self) -> usize {
        self.len
    }

    pub fn mapping_at(&self, index: usize) -> Option<MappingEntry> {
        if index >= self.len {
            return None;
        }
        let entry = self.entries[index]?;
        Some(MappingEntry {
            virt: entry.virt,
            mapping: entry.mapping,
        })
    }

    pub fn has_mapping_for_phys(&self, phys: PhysAddr) -> bool {
        self.entries[..self.len]
            .iter()
            .flatten()
            .any(|entry| entry.mapping.phys == phys)
    }

    pub fn drain_mappings(&mut self) -> [Option<Mapping>; MAX_MAPPINGS] {
        let mut drained = [None; MAX_MAPPINGS];
        for (idx, slot) in self.entries.iter_mut().take(self.len).enumerate() {
            let Some(entry) = slot.take() else { continue };
            arch_unmap_page(self.asid, entry.virt);
            drained[idx] = Some(entry.mapping);
        }
        for slot in self.entries.iter_mut().skip(self.len) {
            *slot = None;
        }
        self.len = 0;
        drained
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
        let _ = self.destroy_and_collect_mappings(asid, pending_cpu_bitmap)?;
        Ok(())
    }

    /// Destroys a software address space and returns all drained mappings.
    ///
    /// Callers can use the returned mappings to update mapping refcounts and
    /// reclaim physical memory objects after the ASID is retired.
    pub fn destroy_and_collect_mappings(
        &mut self,
        asid: Asid,
        pending_cpu_bitmap: CpuBitmap,
    ) -> Result<[Option<Mapping>; MAX_MAPPINGS], VmError> {
        for slot in &mut self.entries {
            if slot.as_ref().is_some_and(|entry| entry.asid == asid)
                && let Some(mut entry) = slot.take()
            {
                let drained = entry.aspace.drain_mappings();
                *slot = None;
                arch_unregister_asid(asid);
                if pending_cpu_bitmap == 0 {
                    return Ok(drained);
                }
                for retired in &mut self.retired {
                    if retired.is_none() {
                        *retired = Some(RetiredAsid {
                            asid,
                            pending_cpu_bitmap,
                            age_ticks: 0,
                        });
                        return Ok(drained);
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

    /// Advances age counters for retired ASIDs.
    ///
    /// Retired ASIDs are no longer force-freed on timeout; they remain retired
    /// until every targeted CPU explicitly acknowledges shootdown completion.
    pub fn tick_retired_shootdowns(&mut self) -> usize {
        for slot in &mut self.retired {
            let Some(retired) = slot.as_mut() else {
                continue;
            };
            retired.age_ticks = retired.age_ticks.saturating_add(1);
        }
        0
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
            KERNEL_SPACE_BASE > 0,
            "KERNEL_SPACE_BASE must leave some lower user virtual space"
        );
        assert_eq!(
            KERNEL_SPACE_BASE & ((PAGE_SIZE as u64) - 1),
            0,
            "KERNEL_SPACE_BASE must be page-aligned"
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
        let va = VirtAddr(KERNEL_SPACE_BASE + PAGE_SIZE as u64);
        let mapping = Mapping {
            phys: PhysAddr(0x2000),
            flags: PageFlags::KERNEL_RW,
        };

        assert_eq!(aspace.map_page(va, mapping), Ok(None));
        assert_eq!(aspace.resolve(va), Some(mapping));
    }

    #[test]
    fn resolve_contract_unmapped_and_mapped_addresses() {
        let mut aspace = AddressSpace::new_user();
        let va = VirtAddr(0x4000);
        assert_eq!(aspace.resolve(va), None);
        assert_eq!(
            aspace.map_page(
                va,
                Mapping {
                    phys: PhysAddr(0x9000),
                    flags: PageFlags::USER_RW,
                }
            ),
            Ok(None)
        );
        assert_eq!(
            aspace.resolve(va),
            Some(Mapping {
                phys: PhysAddr(0x9000),
                flags: PageFlags::USER_RW
            })
        );
        assert_eq!(aspace.resolve(VirtAddr(0x5000)), None);
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
    fn retired_asid_requires_explicit_acknowledgment() {
        let mut mgr = AddressSpaceManager::default();
        let asid = mgr.create_user_space().expect("create");

        assert_eq!(mgr.destroy(asid, 0b11), Ok(()));
        assert!(mgr.retired_entry(asid).is_some());

        for _ in 0..64 {
            assert_eq!(mgr.tick_retired_shootdowns(), 0);
        }

        assert!(mgr.retired_entry(asid).is_some());
        assert_eq!(mgr.acknowledge_shootdown(asid, 0b01), Ok(false));
        assert_eq!(mgr.acknowledge_shootdown(asid, 0b10), Ok(true));
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
                for _ in 0..4 {
                    let _ = mgr.tick_retired_shootdowns();
                }
                assert_eq!(mgr.acknowledge_shootdown(asid, 0b01), Ok(false));
                assert_eq!(mgr.acknowledge_shootdown(asid, 0b10), Ok(true));
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
    fn contiguous_user_stack_sized_mapping_coalesces() {
        let mut aspace = AddressSpace::new_user();
        let base = VirtAddr(0x3ff8_0000);
        for i in 0..128usize {
            let va = VirtAddr(base.0 + (i as u64 * PAGE_SIZE as u64));
            let pa = PhysAddr(0x2000_0000 + (i as u64 * PAGE_SIZE as u64));
            assert_eq!(
                aspace.map_page(
                    va,
                    Mapping {
                        phys: pa,
                        flags: PageFlags::USER_RW
                    }
                ),
                Ok(None)
            );
        }
        assert_eq!(aspace.mappings(), 1);
        assert!(aspace.resolve(base).is_some());
        assert!(
            aspace
                .resolve(VirtAddr(base.0 + (127 * PAGE_SIZE) as u64))
                .is_some()
        );
        assert_eq!(aspace.resolve(VirtAddr(base.0 - PAGE_SIZE as u64)), None);
    }

    #[test]
    fn adjacent_pages_with_different_permissions_do_not_coalesce() {
        let mut aspace = AddressSpace::new_user();
        assert_eq!(
            aspace.map_page(
                VirtAddr(0x2000),
                Mapping {
                    phys: PhysAddr(0x3000),
                    flags: PageFlags::USER_RX
                }
            ),
            Ok(None)
        );
        assert_eq!(
            aspace.map_page(
                VirtAddr(0x3000),
                Mapping {
                    phys: PhysAddr(0x4000),
                    flags: PageFlags::USER_RW
                }
            ),
            Ok(None)
        );
        assert_eq!(aspace.mappings(), 2);
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
                cache_policy: CachePolicy::WriteBack,
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
            // Use virtual addresses that skip one page between each mapping so consecutive
            // pages are never adjacent in both VA and PA at the same time, preventing the
            // entry-merge optimisation from collapsing all mappings into one entry.
            let virt = VirtAddr(((i * 2 + 1) * PAGE_SIZE) as u64);
            // Use a scrambled physical address (not consecutive) to further defeat merging.
            let phys = PhysAddr(((i * 3 + 100) * PAGE_SIZE) as u64);
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

        // The (MAX_MAPPINGS + 1)-th distinct mapping must be rejected.
        // Use a virtual address that was not previously mapped (i.e. an even-numbered page
        // since we used odd-numbered pages above) and a completely different physical address.
        assert_eq!(
            aspace.map_page(
                VirtAddr(((MAX_MAPPINGS * 2 + 2) * PAGE_SIZE) as u64),
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
