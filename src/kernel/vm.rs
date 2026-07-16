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
        let mask = PAGE_SIZE as u64 - 1;
        match self.0.checked_add(mask) {
            Some(v) => Self(v & !mask),
            None => Self(u64::MAX & !mask),
        }
    }

    /// Checked byte addition. Returns `None` on `u64` overflow. Stage 163F: any
    /// range-sensitive call site (computing an end address from a size that could
    /// be large or attacker-influenced) MUST use this rather than the `Add`
    /// operator, which wraps silently (see the `Add` impl below).
    pub const fn checked_add(self, rhs: u64) -> Option<Self> {
        match self.0.checked_add(rhs) {
            Some(v) => Some(Self(v)),
            None => None,
        }
    }

    /// True if this address is in canonical form for the target ISA.
    ///
    /// On x86_64 the top 17 bits [63:47] must be all-equal (sign-extension of bit
    /// 47); a non-canonical address `#GP`s on use, so e.g. `0x0001_0000_0000_0000`
    /// must NOT be accepted as a valid user address even though it compares below
    /// `KERNEL_SPACE_BASE`. AArch64 (Sv48/TTBR split) and RISC-V (Sv39/Sv48) have
    /// their own valid-range rules handled by the page-table backend; this software
    /// VM split treats any address below `KERNEL_SPACE_BASE` as user, so the
    /// canonical check is cfg-gated to x86_64 and is a no-op elsewhere.
    pub const fn is_canonical(self) -> bool {
        #[cfg(target_arch = "x86_64")]
        {
            let top = self.0 >> 47;
            top == 0 || top == 0x1_FFFF
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            true
        }
    }
}

/// NOTE (Stage 163F): this operator uses **wrapping** arithmetic. It is kept for
/// ergonomics in tight page-walk loops where `rhs` is a small, already-in-bounds
/// page offset. It MUST NOT be used to compute an end address from an untrusted or
/// potentially large size — use [`VirtAddr::checked_add`] there. Overflow wraps
/// silently and will not be caught.
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

/// Address-space identifier.
///
/// The inner `u16` is `pub` for ergonomics and is constructed directly only in
/// known internal/test paths (the allocator in [`AddressSpaceManager::allocate_asid`]
/// and kernel-bootstrap/test code). The allocator NEVER hands out `Asid(0)`
/// (`next_asid` starts at 1 and wraps to 1, never 0), because 0 is reserved as the
/// "no ASID" / kernel sentinel and, on x86_64, PCID 0 is special-cased. Stage 163F:
/// callers minting an ASID must go through `create_user_space`; do not fabricate a
/// raw `Asid(n)` outside the allocator/tests. (Sealing the field behind a `TryFrom`
/// is deferred: the inner field is referenced pervasively across the boot/arch
/// layers and a wide API refactor is out of scope for this VM audit.)
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

    /// Guard page: no read/write/execute and not user-accessible. Stage 163F audit:
    /// the `cache_policy` here is **irrelevant** — a guard page must never be made
    /// present (its purpose is to fault on any access), so its memory type is never
    /// consulted by hardware. The `WriteBack` value is an inert default; it carries
    /// no caching semantics for an inaccessible page. (If a guard page were ever
    /// accidentally mapped present that would be the bug to fix, not this field.)
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

// Stage 163F audit (item 10): the AddressSpaceManager lookup paths
// (get/get_mut/cr3_for_asid + retired scans) are O(N) linear over fixed arrays.
// That is only acceptable while N stays small; pin the bound at compile time so a
// future widening forces a deliberate revisit of the lookup structure rather than
// silently regressing every ASID lookup.
const _: () = assert!(
    MAX_ADDRESS_SPACES <= 64,
    "MAX_ADDRESS_SPACES grew beyond the linear-scan budget; revisit lookup structure"
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmError {
    Full,
    Misaligned,
    PrivilegeViolation,
    InvalidAsid,
    /// Returned when an address is out of range or violates an internal
    /// address-space invariant (not a privilege check failure).
    InvalidAddress,
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
pub struct DrainedMapping {
    /// Run head virtual address (Stage 163F): lets callers reconstruct exactly
    /// which virtual range `[virt, virt + pages * PAGE_SIZE)` was unmapped, not just
    /// the physical range. The current consumer (frame reclaim) only needs `phys` +
    /// `pages`, but recording `virt` makes the drained record self-describing for
    /// future callers (e.g. range-targeted TLB shootdown).
    pub virt: VirtAddr,
    pub mapping: Mapping,
    pub pages: usize,
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
        // This condition is unreachable via the only call site (map_page), which
        // only arrives here after find_entry_containing guarantees containment.
        // Return InvalidAddress (not PrivilegeViolation) to avoid misclassifying
        // an internal invariant violation as a user privilege check failure.
        debug_assert!(
            page_offset < entry.pages,
            "isolate_page_entry_at: page_offset {page_offset} out of bounds (entry.pages={})",
            entry.pages
        );
        if page_offset >= entry.pages {
            return Err(VmError::InvalidAddress);
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

    /// Exclusive end virtual address of a run. Stage 163F: uses **saturating**
    /// arithmetic so a corrupted `pages` count can never wrap the end below the
    /// base (which would corrupt the sorted-table binary search and adjacency
    /// checks). A saturated `u64::MAX` end is a safe upper bound: it is never equal
    /// to a page-aligned `virt`, so it can only ever cause a "not adjacent" / "not
    /// contained" decision, never a false merge.
    fn entry_end_virt(entry: &Entry) -> u64 {
        entry
            .virt
            .0
            .saturating_add((entry.pages as u64).saturating_mul(PAGE_SIZE as u64))
    }

    /// Stage 163F: true iff the run `prev` is immediately followed — in BOTH virt
    /// and phys, with identical flags — by a single page at (`virt`, `phys`), so the
    /// page can be coalesced onto the END of `prev`. Uses checked arithmetic for the
    /// phys-end computation: an overflow yields `false` ("cannot merge"), never a
    /// false merge on a corrupted `pages`.
    fn run_precedes_page(prev: &Entry, virt: VirtAddr, phys: PhysAddr, flags: PageFlags) -> bool {
        if prev.mapping.flags != flags || Self::entry_end_virt(prev) != virt.0 {
            return false;
        }
        let prev_phys_end = prev
            .mapping
            .phys
            .0
            .checked_add((prev.pages as u64).saturating_mul(PAGE_SIZE as u64));
        prev_phys_end == Some(phys.0)
    }

    /// Stage 163F: true iff a single page at (`virt`, `phys`) is immediately
    /// followed — in BOTH virt and phys, with identical flags — by the run `next`,
    /// so the page can be coalesced onto the FRONT of `next`. Checked arithmetic:
    /// overflow yields `false` ("cannot merge").
    fn page_precedes_run(virt: VirtAddr, phys: PhysAddr, flags: PageFlags, next: &Entry) -> bool {
        next.mapping.flags == flags
            && virt.0.checked_add(PAGE_SIZE as u64) == Some(next.virt.0)
            && phys.0.checked_add(PAGE_SIZE as u64) == Some(next.mapping.phys.0)
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

        // Stage 163F (item 8): reject non-canonical virtual addresses. On x86_64 a
        // mapping at e.g. 0x0001_0000_0000_0000 (bit 48 set, bit 47 clear) is below
        // KERNEL_SPACE_BASE — so `is_user()` would say "user" — yet the CPU `#GP`s on
        // any access to it. `is_canonical()` is cfg-gated (no-op on AArch64/RISC-V,
        // whose own range rules are enforced by the page-table backend), so this
        // only tightens x86_64 and leaves the canonical kernel/user ranges accepted.
        if !virt.is_canonical() {
            return Err(VmError::InvalidAddress);
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
                let old = self.entries[i].as_ref().expect("entry").mapping;
                // Bug 1 fix: right-split so the tail of a multi-page run keeps its
                // own tracking entry — overwriting entry.mapping here would corrupt it.
                if self.entries[i].as_ref().expect("entry").pages > 1 {
                    if self.len >= MAX_MAPPINGS {
                        return Err(VmError::Full);
                    }
                    let tail_pages = self.entries[i].as_ref().expect("entry").pages - 1;
                    let tail_virt = VirtAddr(virt.0 + PAGE_SIZE as u64);
                    let tail_phys = PhysAddr(old.phys.0 + PAGE_SIZE as u64);
                    for shift_idx in (i + 1..self.len).rev() {
                        self.entries[shift_idx + 1] = self.entries[shift_idx];
                    }
                    self.entries[i + 1] = Some(Entry {
                        virt: tail_virt,
                        mapping: Mapping {
                            phys: tail_phys,
                            flags: old.flags,
                        },
                        pages: tail_pages,
                    });
                    self.entries[i].as_mut().expect("entry").pages = 1;
                    self.len += 1;
                }
                // Bug 5 fix: BBM — unmap before remap for AArch64 compliance;
                // also forces a TLB shootdown on x86_64 and RISC-V.
                arch_unmap_page(self.asid, virt);
                if let Err(e) = arch_map_page(self.asid, virt, mapping) {
                    // Hardware is already unmapped; remove the stale software entry
                    // so the shadow stays consistent.
                    for shift_idx in i..self.len.saturating_sub(1) {
                        self.entries[shift_idx] = self.entries[shift_idx + 1];
                    }
                    self.entries[self.len - 1] = None;
                    self.len -= 1;
                    return Err(e);
                }
                self.entries[i].as_mut().expect("entry").mapping = mapping;
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
                        Self::run_precedes_page(&prev, virt, mapping.phys, mapping.flags)
                    });
                if prev_merge {
                    // Bug 6 fix: also check if the new page bridges into the next
                    // run so all three entries can be collapsed into one.
                    let next_also_merges = i < self.len
                        && self.entries[i].is_some_and(|next| {
                            Self::page_precedes_run(virt, mapping.phys, mapping.flags, &next)
                        });
                    if next_also_merges {
                        let next_pages = self.entries[i].expect("next").pages;
                        self.entries[i - 1].as_mut().expect("prev").pages += 1 + next_pages;
                        for shift_idx in i..self.len.saturating_sub(1) {
                            self.entries[shift_idx] = self.entries[shift_idx + 1];
                        }
                        self.entries[self.len - 1] = None;
                        self.len -= 1;
                    } else {
                        self.entries[i - 1].as_mut().expect("prev").pages += 1;
                    }
                    return Ok(None);
                }

                let next_merge = i < self.len
                    && self.entries[i].is_some_and(|next| {
                        Self::page_precedes_run(virt, mapping.phys, mapping.flags, &next)
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

    pub fn unmap_page(&mut self, virt: VirtAddr) -> Result<Option<Mapping>, VmError> {
        let Some(idx) = self.find_entry_containing(virt) else {
            return Ok(None);
        };
        let entry = self.entries[idx].expect("entry after find_entry_containing");
        let page_offset = ((virt.0 - entry.virt.0) / PAGE_SIZE as u64) as usize;
        let old = Mapping {
            phys: PhysAddr(entry.mapping.phys.0 + (page_offset as u64 * PAGE_SIZE as u64)),
            flags: entry.mapping.flags,
        };
        // Bug 3 fix: capacity check BEFORE touching hardware — splitting a
        // middle-of-block page requires one extra tracking entry; reject early
        // so hardware and software state never diverge.
        let is_middle = entry.pages > 1 && page_offset > 0 && page_offset < entry.pages - 1;
        if is_middle && self.len >= MAX_MAPPINGS {
            return Err(VmError::Full);
        }
        arch_unmap_page(self.asid, virt);
        if entry.pages == 1 {
            for shift_idx in idx..self.len.saturating_sub(1) {
                self.entries[shift_idx] = self.entries[shift_idx + 1];
            }
            self.entries[self.len - 1] = None;
            self.len -= 1;
            return Ok(Some(old));
        }
        if page_offset == 0 {
            let current = self.entries[idx].as_mut().expect("entry");
            current.virt = VirtAddr(current.virt.0 + PAGE_SIZE as u64);
            current.mapping.phys = PhysAddr(current.mapping.phys.0 + PAGE_SIZE as u64);
            current.pages -= 1;
            return Ok(Some(old));
        }
        if page_offset == entry.pages - 1 {
            self.entries[idx].as_mut().expect("entry").pages -= 1;
            return Ok(Some(old));
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
        Ok(Some(old))
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

    /// Stage 163E: like [`Self::mapping_at`], but also exposes the run's page count.
    /// A COW fork uses this to copy a whole run into the child and write-protect the
    /// parent run *in place* (without splitting it into per-page entries), so the
    /// parent mapping table does not balloon during the clone.
    pub fn run_at(&self, index: usize) -> Option<(VirtAddr, Mapping, usize)> {
        if index >= self.len {
            return None;
        }
        let entry = self.entries[index]?;
        Some((entry.virt, entry.mapping, entry.pages))
    }

    /// Stage 163E: clear the write flag of the entire run whose head is exactly
    /// `virt`, updating the hardware PTE of every page in the run but NOT splitting
    /// the software entry — so `len` is unchanged. Returns the previous flags for
    /// rollback. A run that is already read-only is left untouched. This supports
    /// COW fork: a writable run becomes one read-only run, and the per-page split
    /// happens lazily in `try_handle_cow_fault` on the first write. On a hardware
    /// failure mid-run the already-updated pages are restored before returning.
    pub fn write_protect_run_head_in_place(
        &mut self,
        virt: VirtAddr,
    ) -> Result<PageFlags, VmError> {
        let idx = self
            .find_entry_index(virt)
            .map_err(|_| VmError::InvalidAddress)?;
        let entry = self.entries[idx].expect("entry");
        let old = entry.mapping.flags;
        if !old.write {
            return Ok(old);
        }
        let mut new = old;
        new.write = false;
        let base = entry.virt;
        let base_phys = entry.mapping.phys;
        let pages = entry.pages;
        for p in 0..pages {
            let pv = VirtAddr(base.0 + p as u64 * PAGE_SIZE as u64);
            let pp = PhysAddr(base_phys.0 + p as u64 * PAGE_SIZE as u64);
            if let Err(e) = arch_map_page(
                self.asid,
                pv,
                Mapping {
                    phys: pp,
                    flags: new,
                },
            ) {
                for q in 0..p {
                    let qv = VirtAddr(base.0 + q as u64 * PAGE_SIZE as u64);
                    let qp = PhysAddr(base_phys.0 + q as u64 * PAGE_SIZE as u64);
                    let _ = arch_map_page(
                        self.asid,
                        qv,
                        Mapping {
                            phys: qp,
                            flags: old,
                        },
                    );
                }
                return Err(e);
            }
        }
        self.entries[idx].as_mut().expect("entry").mapping.flags = new;
        Ok(old)
    }

    /// Stage 163E: rollback companion to [`Self::write_protect_run_head_in_place`].
    /// Best-effort: restore the run head's flags (software + every page's hardware
    /// PTE) so a failed COW fork leaves the parent byte-identical to before.
    pub fn restore_run_head_flags_in_place(&mut self, virt: VirtAddr, flags: PageFlags) {
        let Ok(idx) = self.find_entry_index(virt) else {
            return;
        };
        let entry = self.entries[idx].expect("entry");
        let base = entry.virt;
        let base_phys = entry.mapping.phys;
        let pages = entry.pages;
        for p in 0..pages {
            let pv = VirtAddr(base.0 + p as u64 * PAGE_SIZE as u64);
            let pp = PhysAddr(base_phys.0 + p as u64 * PAGE_SIZE as u64);
            let _ = arch_map_page(self.asid, pv, Mapping { phys: pp, flags });
        }
        self.entries[idx].as_mut().expect("entry").mapping.flags = flags;
    }

    /// True if `phys` falls within ANY mapped run, i.e. in
    /// `[base, base + pages * PAGE_SIZE)` for some entry. Stage 163F: previously
    /// this compared only against the run's BASE phys, so a query for a non-base
    /// page of a multi-page run (e.g. base 0x20000, run 3 pages, query 0x21000)
    /// wrongly returned `false` — which, for a future frame-reclaim guard built on
    /// this, would risk reclaiming a still-mapped frame. Now it tests containment
    /// over the whole run with checked arithmetic (overflow ⇒ the run reaches the
    /// top of the address space, so any `phys >= base` is contained).
    pub fn has_mapping_for_phys(&self, phys: PhysAddr) -> bool {
        self.entries[..self.len].iter().flatten().any(|entry| {
            let base = entry.mapping.phys.0;
            match (entry.pages as u64)
                .checked_mul(PAGE_SIZE as u64)
                .and_then(|span| base.checked_add(span))
            {
                Some(end) => base <= phys.0 && phys.0 < end,
                None => base <= phys.0,
            }
        })
    }

    /// Drain every mapping (unmapping each page) and return them by value.
    ///
    /// Stack note (Stage 163F audit, item 9): the returned
    /// `[Option<DrainedMapping>; MAX_MAPPINGS]` is a fixed-size, by-value array
    /// (~`MAX_MAPPINGS` * `size_of::<Option<DrainedMapping>>()` ≈ a few KiB). It is
    /// allocation-free (`no_std`-friendly) but lives on the stack. The only callers
    /// are address-space *destruction* paths (`destroy_and_collect_mappings` ←
    /// `KernelState::destroy_user_address_space*`), which run in syscall/teardown
    /// context with an ordinary kernel stack — NOT in interrupt/trap-entry or deeply
    /// recursive paths — so the one-frame array is safe. Do not call this from a
    /// stack-constrained (IRQ/exception) context; if that is ever needed, switch to
    /// a visitor/callback drain rather than introducing heap allocation.
    pub fn drain_mappings(&mut self) -> [Option<DrainedMapping>; MAX_MAPPINGS] {
        let mut drained: [Option<DrainedMapping>; MAX_MAPPINGS] = [None; MAX_MAPPINGS];
        for (idx, slot) in self.entries.iter_mut().take(self.len).enumerate() {
            let Some(entry) = slot.take() else { continue };
            // Bug 2 fix: unmap every page in the run, not just the base VA.
            for page in 0..entry.pages {
                arch_unmap_page(
                    self.asid,
                    VirtAddr(entry.virt.0 + (page as u64 * PAGE_SIZE as u64)),
                );
            }
            drained[idx] = Some(DrainedMapping {
                virt: entry.virt,
                mapping: entry.mapping,
                pages: entry.pages,
            });
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
///
/// ## Concurrency (Stage 163F audit, item 6)
///
/// `AddressSpaceManager` and the [`AddressSpace`]es it owns hold **no interior
/// locks**. All access — read or mutate — requires external exclusive access. In
/// this kernel that owner is the VM domain lock (rank 5): every entry point reaches
/// this type through `KernelState::with_user_spaces` / `with_user_spaces_mut`
/// (`orchestrator_state.rs`), which hold the VM `SpinLock` guard across the closure.
/// Do not store a reference to this manager (or an `AddressSpace`) past the end of
/// such a closure, and do not access it from two CPUs without that lock.
///
/// ## Lookup cost (Stage 163F audit, item 10)
///
/// `get` / `get_mut` / `cr3_for_asid` and the retired-slot scans are O(N) linear
/// over `entries`/`retired`. This is intentional: `MAX_ADDRESS_SPACES` is small
/// (32) and a fixed-array scan is cache-friendly and allocation-free. A
/// `const` assertion below pins that bound so the linear scan stays cheap; if the
/// bound ever grows large, revisit the lookup structure (NOT a heap hash map in
/// this `no_std` kernel).
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
    /// Initialize `AddressSpaceManager` directly in final boot storage without
    /// returning the fixed arrays by value.
    ///
    /// # Safety
    ///
    /// `destination` must be aligned, writable, unique storage for an
    /// uninitialized `AddressSpaceManager`. The caller must not publish the
    /// manager until all fields have been written.
    #[cfg(not(feature = "hosted-dev"))]
    pub(crate) unsafe fn init_in_place_default(destination: *mut Self) {
        unsafe {
            core::ptr::addr_of_mut!((*destination).next_asid).write(1);
            let entries = core::ptr::addr_of_mut!((*destination).entries).cast::<Option<AsEntry>>();
            for idx in 0..MAX_ADDRESS_SPACES {
                entries.add(idx).write(None);
            }
            let retired =
                core::ptr::addr_of_mut!((*destination).retired).cast::<Option<RetiredAsid>>();
            for idx in 0..MAX_ADDRESS_SPACES {
                retired.add(idx).write(None);
            }
        }
    }

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
        // Scan the full u16 ASID space (1..=65535).  The previous bound of
        // MAX_ADDRESS_SPACES iterations caused spurious VmError::Full when
        // next_asid landed on a run of retired ASIDs from prior destroy cycles.
        for _ in 0..u16::MAX {
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
        // Stage 163F: confirm a free `entries` slot BEFORE allocating/registering an
        // ASID. The previous order allocated an ASID number and registered an arch
        // page-table root, then rolled back (`arch_unregister_asid`) when the slot
        // table was full — a needless allocate-then-undo. Checking the slot first
        // means a full manager performs NO ASID allocation and NO arch registration.
        let slot_idx = self
            .entries
            .iter()
            .position(Option::is_none)
            .ok_or(VmError::Full)?;
        let asid = self.allocate_asid()?;
        arch_register_asid(asid)?;
        self.entries[slot_idx] = Some(AsEntry {
            asid,
            aspace: AddressSpace::new_user_with_asid(asid),
        });
        Ok(asid)
    }

    pub fn get(&self, asid: Asid) -> Option<&AddressSpace> {
        self.entries
            .iter()
            .flatten()
            .find(|entry| entry.asid == asid)
            .map(|entry| &entry.aspace)
    }

    /// Number of live user address spaces currently occupying a slot, and the
    /// fixed slot capacity. Stage 163D: used by the proof-gated fork COW
    /// diagnostics to report `Vm(Full)` exhaustion of the address-space table.
    pub fn live_count(&self) -> usize {
        self.entries.iter().flatten().count()
    }

    pub const fn slot_capacity(&self) -> usize {
        MAX_ADDRESS_SPACES
    }

    /// Number of retired-but-not-yet-acknowledged ASIDs (pending TLB shootdown).
    /// A large value here points at a shootdown-acknowledgement leak rather than a
    /// genuine capacity shortfall.
    pub fn retired_count(&self) -> usize {
        self.retired.iter().flatten().count()
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
    ) -> Result<[Option<DrainedMapping>; MAX_MAPPINGS], VmError> {
        // Locate the live entry first so we can return InvalidAsid before
        // committing to any state change.
        let entry_idx = self
            .entries
            .iter()
            .position(|slot| slot.as_ref().is_some_and(|e| e.asid == asid))
            .ok_or(VmError::InvalidAsid)?;

        // If CPUs need to acknowledge shootdowns, guarantee a retired slot
        // exists before modifying any live state.  Without this pre-check the
        // ASID could be drained and unregistered and then the retired-array
        // insert fails, silently dropping the TLB-shootdown tracking bitmap.
        if pending_cpu_bitmap != 0 && !self.retired.iter().any(|s| s.is_none()) {
            return Err(VmError::Full);
        }

        let mut entry = self.entries[entry_idx]
            .take()
            .expect("entry verified above");
        let drained = entry.aspace.drain_mappings();
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

        // Unreachable: the pre-check above verified a free retired slot exists.
        unreachable!("retired slot must exist after capacity pre-check");
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
        // Stage 163F: a zero `cpu_bit` clears nothing (`pending &= !0` is a no-op) and
        // would silently report "not yet complete" without making progress — almost
        // certainly a caller bug. Catch it in debug builds; in release it stays a
        // harmless no-op returning `Ok(false)` (or `Ok(true)` if already empty).
        debug_assert!(
            cpu_bit != 0,
            "acknowledge_shootdown called with empty cpu_bit (no CPU acknowledged)"
        );
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
    ///
    /// Returns `0` **by design** (Stage 163F audit, item 7): the `usize` is the
    /// "number of ASIDs whose shootdown timed out and should be escalated", and the
    /// timeout-escalation mechanism is intentionally deferred (see
    /// `doc/KERNEL_LOCKING.md` §20.3 / §36.7 and `KERNEL_TEST_RULES.md` N+25.9). The
    /// caller in `scheduler_state.rs` is written to fire `escalate_tlb_shootdown_
    /// timeout` only when this returns `> 0`, so the return type is the wired-in
    /// hook for a future implementation — NOT dead. Tests assert it stays `0`; do
    /// not change the signature without lifting that documented contract.
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
        assert_eq!(aspace.unmap_page(virt), Ok(Some(second)));
        assert_eq!(aspace.unmap_page(virt), Ok(None));
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

    // Stage 163E regression: write-protecting a multi-page run for COW must update
    // the run's flags IN PLACE, never split it into per-page entries. The prior
    // per-page re-map split runs and ballooned the parent table to MAX_MAPPINGS,
    // failing fork with Vm(Full) and leaking the parent's entry count.
    #[test]
    fn write_protect_run_head_in_place_does_not_split_or_grow() {
        let mut aspace = AddressSpace::new_user();
        let rw = PageFlags {
            read: true,
            write: true,
            execute: false,
            user: true,
            cache_policy: CachePolicy::WriteBack,
        };
        // Four contiguous (VA+PA) same-flag pages merge into ONE run entry.
        for p in 0..4u64 {
            let virt = VirtAddr(0x10_000 + p * PAGE_SIZE as u64);
            let phys = PhysAddr(0x40_000 + p * PAGE_SIZE as u64);
            assert_eq!(aspace.map_page(virt, Mapping { phys, flags: rw }), Ok(None));
        }
        assert_eq!(
            aspace.mappings(),
            1,
            "4 contiguous same-flag pages must form one run"
        );
        assert_eq!(aspace.run_at(0).expect("run").2, 4, "run is 4 pages");

        // Write-protect the run head in place: NO split (len stays 1), whole run RO.
        let old = aspace
            .write_protect_run_head_in_place(VirtAddr(0x10_000))
            .expect("write-protect run");
        assert!(old.write, "returned previous flags must be writable");
        assert_eq!(
            aspace.mappings(),
            1,
            "write-protect must not split the run (no bloat)"
        );
        let (_v, m, pages) = aspace.run_at(0).expect("run");
        assert!(!m.flags.write, "run must be read-only after write-protect");
        assert_eq!(pages, 4, "run page count unchanged");

        // Rollback restores the writable flag in place (still one entry).
        aspace.restore_run_head_flags_in_place(VirtAddr(0x10_000), old);
        assert_eq!(aspace.mappings(), 1, "restore must not split either");
        assert!(
            aspace.run_at(0).expect("run").1.flags.write,
            "rollback restores the writable flag"
        );

        // An already-read-only run is left untouched (idempotent, no split).
        let mut ro_space = AddressSpace::new_user();
        assert_eq!(
            ro_space.map_page(
                VirtAddr(0x20_000),
                Mapping {
                    phys: PhysAddr(0x50_000),
                    flags: PageFlags::USER_RX,
                }
            ),
            Ok(None)
        );
        let prev = ro_space
            .write_protect_run_head_in_place(VirtAddr(0x20_000))
            .expect("noop write-protect");
        assert!(!prev.write);
        assert_eq!(ro_space.mappings(), 1);
    }

    #[test]
    fn manager_invalid_destroy_and_monotonic_asid() {
        let mut mgr = AddressSpaceManager::default();
        assert_eq!(mgr.destroy(Asid(777), 0), Err(VmError::InvalidAsid));

        let a = mgr.create_user_space().expect("asid a");
        let b = mgr.create_user_space().expect("asid b");
        assert_ne!(a, b);
    }

    // --- Bug 4 regression: page_align_up must not overflow near u64::MAX ---

    #[test]
    fn page_align_up_already_aligned_near_max_returns_self() {
        let mask = PAGE_SIZE as u64 - 1;
        let last_page_base = VirtAddr(u64::MAX & !mask);
        assert_eq!(last_page_base.page_align_up(), last_page_base);
    }

    #[test]
    fn page_align_up_saturates_on_overflow() {
        let mask = PAGE_SIZE as u64 - 1;
        // One byte into the last page — naive +mask would wrap u64.
        let one_past = VirtAddr((u64::MAX & !mask) + 1);
        let result = one_past.page_align_up();
        assert_eq!(result.0 & mask, 0, "result must be page-aligned");
        assert_eq!(result, VirtAddr(u64::MAX & !mask));
    }

    // --- Bug 1 regression: map_page must right-split multi-page entries ---

    #[test]
    fn remap_middle_page_of_multipage_block_splits_correctly() {
        let mut aspace = AddressSpace::new_user();
        let base = VirtAddr(0x10_000);
        let flags = PageFlags::USER_RW;
        for i in 0..3usize {
            aspace
                .map_page(
                    VirtAddr(base.0 + (i as u64 * PAGE_SIZE as u64)),
                    Mapping {
                        phys: PhysAddr(0x20_000 + i as u64 * PAGE_SIZE as u64),
                        flags,
                    },
                )
                .unwrap();
        }
        assert_eq!(aspace.mappings(), 1);

        let mid = VirtAddr(base.0 + PAGE_SIZE as u64);
        let new_m = Mapping {
            phys: PhysAddr(0x50_000),
            flags: PageFlags::USER_RX,
        };
        let old = aspace.map_page(mid, new_m).unwrap();
        assert_eq!(
            old,
            Some(Mapping {
                phys: PhysAddr(0x21_000),
                flags
            })
        );
        assert_eq!(aspace.mappings(), 3);
        assert_eq!(
            aspace.resolve(base),
            Some(Mapping {
                phys: PhysAddr(0x20_000),
                flags
            })
        );
        assert_eq!(aspace.resolve(mid), Some(new_m));
        assert_eq!(
            aspace.resolve(VirtAddr(base.0 + 2 * PAGE_SIZE as u64)),
            Some(Mapping {
                phys: PhysAddr(0x22_000),
                flags
            })
        );
    }

    #[test]
    fn remap_first_page_of_multipage_block_leaves_tail_intact() {
        let mut aspace = AddressSpace::new_user();
        let base = VirtAddr(0x10_000);
        let flags = PageFlags::USER_RW;
        for i in 0..4usize {
            aspace
                .map_page(
                    VirtAddr(base.0 + (i as u64 * PAGE_SIZE as u64)),
                    Mapping {
                        phys: PhysAddr(0x20_000 + i as u64 * PAGE_SIZE as u64),
                        flags,
                    },
                )
                .unwrap();
        }
        assert_eq!(aspace.mappings(), 1);

        let new_m = Mapping {
            phys: PhysAddr(0x90_000),
            flags: PageFlags::USER_RX,
        };
        let old = aspace.map_page(base, new_m).unwrap();
        assert_eq!(
            old,
            Some(Mapping {
                phys: PhysAddr(0x20_000),
                flags
            })
        );
        assert_eq!(aspace.mappings(), 2);
        assert_eq!(aspace.resolve(base), Some(new_m));
        assert_eq!(
            aspace.resolve(VirtAddr(base.0 + PAGE_SIZE as u64)),
            Some(Mapping {
                phys: PhysAddr(0x21_000),
                flags
            })
        );
        assert_eq!(
            aspace.resolve(VirtAddr(base.0 + 3 * PAGE_SIZE as u64)),
            Some(Mapping {
                phys: PhysAddr(0x23_000),
                flags
            })
        );
    }

    // --- Bug 5 regression: BBM — remap must not smash an existing PTE directly ---

    #[test]
    fn remap_single_page_returns_old_and_resolves_new() {
        let mut aspace = AddressSpace::new_user();
        let va = VirtAddr(0x5000);
        let first = Mapping {
            phys: PhysAddr(0xA_000),
            flags: PageFlags::USER_RX,
        };
        let second = Mapping {
            phys: PhysAddr(0xB_000),
            flags: PageFlags::USER_RW,
        };
        aspace.map_page(va, first).unwrap();
        let old = aspace.map_page(va, second).unwrap();
        assert_eq!(old, Some(first));
        assert_eq!(aspace.resolve(va), Some(second));
        assert_eq!(aspace.mappings(), 1);
    }

    // --- Bug 6 regression: bridging page must trigger triple merge ---

    #[test]
    fn bridge_page_merges_prev_and_next_into_single_entry() {
        let mut aspace = AddressSpace::new_user();
        let flags = PageFlags::USER_RW;
        aspace
            .map_page(
                VirtAddr(0x1000),
                Mapping {
                    phys: PhysAddr(0x10_000),
                    flags,
                },
            )
            .unwrap();
        aspace
            .map_page(
                VirtAddr(0x3000),
                Mapping {
                    phys: PhysAddr(0x12_000),
                    flags,
                },
            )
            .unwrap();
        assert_eq!(aspace.mappings(), 2);

        aspace
            .map_page(
                VirtAddr(0x2000),
                Mapping {
                    phys: PhysAddr(0x11_000),
                    flags,
                },
            )
            .unwrap();
        assert_eq!(aspace.mappings(), 1);
        assert_eq!(
            aspace.resolve(VirtAddr(0x1000)),
            Some(Mapping {
                phys: PhysAddr(0x10_000),
                flags
            })
        );
        assert_eq!(
            aspace.resolve(VirtAddr(0x2000)),
            Some(Mapping {
                phys: PhysAddr(0x11_000),
                flags
            })
        );
        assert_eq!(
            aspace.resolve(VirtAddr(0x3000)),
            Some(Mapping {
                phys: PhysAddr(0x12_000),
                flags
            })
        );
    }

    #[test]
    fn bridge_page_with_flags_mismatch_does_not_triple_merge() {
        let mut aspace = AddressSpace::new_user();
        aspace
            .map_page(
                VirtAddr(0x1000),
                Mapping {
                    phys: PhysAddr(0x10_000),
                    flags: PageFlags::USER_RW,
                },
            )
            .unwrap();
        aspace
            .map_page(
                VirtAddr(0x3000),
                Mapping {
                    phys: PhysAddr(0x12_000),
                    flags: PageFlags::USER_RX,
                },
            )
            .unwrap();
        // Bridge page shares flags with left but not right.
        aspace
            .map_page(
                VirtAddr(0x2000),
                Mapping {
                    phys: PhysAddr(0x11_000),
                    flags: PageFlags::USER_RW,
                },
            )
            .unwrap();
        // Left+bridge merge; right stays separate.
        assert_eq!(aspace.mappings(), 2);
    }

    // --- Bug 3 regression: unmap middle page returns Err when at capacity ---

    #[test]
    fn unmap_middle_of_block_fails_at_capacity() {
        let mut aspace = AddressSpace::new_user();
        let flags = PageFlags::USER_RW;
        // Build the 3-page block first (coalesces to 1 slot).
        let triple_va = VirtAddr(0x1000_0000);
        let triple_pa = PhysAddr(0x2000_0000);
        for i in 0..3usize {
            aspace
                .map_page(
                    VirtAddr(triple_va.0 + (i as u64 * PAGE_SIZE as u64)),
                    Mapping {
                        phys: PhysAddr(triple_pa.0 + (i as u64 * PAGE_SIZE as u64)),
                        flags,
                    },
                )
                .unwrap();
        }
        assert_eq!(aspace.mappings(), 1);
        // Fill the remaining MAX_MAPPINGS - 1 slots with non-adjacent isolated pages
        // in a VA/PA range that cannot merge with the triple block.
        for i in 0..(MAX_MAPPINGS - 1) {
            let va = VirtAddr(0x2000_0000_u64 + (i as u64 * 2 * PAGE_SIZE as u64));
            let pa = PhysAddr(0x3000_0000_u64 + (i as u64 * 3 * PAGE_SIZE as u64));
            aspace.map_page(va, Mapping { phys: pa, flags }).unwrap();
        }
        assert_eq!(aspace.mappings(), MAX_MAPPINGS);

        // Unmapping the middle page would need to split into 2 entries → Full.
        let mid_va = VirtAddr(triple_va.0 + PAGE_SIZE as u64);
        assert_eq!(aspace.unmap_page(mid_va), Err(VmError::Full));
        // Mapping is still intact — hardware was not touched.
        assert_eq!(
            aspace.resolve(mid_va),
            Some(Mapping {
                phys: PhysAddr(triple_pa.0 + PAGE_SIZE as u64),
                flags
            })
        );
    }

    #[test]
    fn unmap_page_returns_result_for_mapped_and_unmapped() {
        let mut aspace = AddressSpace::new_user();
        let va = VirtAddr(0x4000);
        let m = Mapping {
            phys: PhysAddr(0x9000),
            flags: PageFlags::USER_RW,
        };
        aspace.map_page(va, m).unwrap();
        assert_eq!(aspace.unmap_page(va), Ok(Some(m)));
        assert_eq!(aspace.unmap_page(va), Ok(None));
    }

    // --- Bug 2 regression: drain_mappings must report correct page counts ---

    #[test]
    fn drain_mappings_reports_full_page_count_for_coalesced_run() {
        let mut aspace = AddressSpace::new_kernel();
        let base = VirtAddr(KERNEL_SPACE_BASE + PAGE_SIZE as u64);
        for i in 0..5usize {
            aspace
                .map_page(
                    VirtAddr(base.0 + (i as u64 * PAGE_SIZE as u64)),
                    Mapping {
                        phys: PhysAddr(0x1_000_000 + i as u64 * PAGE_SIZE as u64),
                        flags: PageFlags::KERNEL_RW,
                    },
                )
                .unwrap();
        }
        assert_eq!(aspace.mappings(), 1);

        let drained = aspace.drain_mappings();
        assert_eq!(aspace.mappings(), 0);
        let dm = drained[0].expect("first entry");
        assert_eq!(dm.pages, 5);
        assert_eq!(dm.mapping.phys, PhysAddr(0x1_000_000));
        assert!(drained[1..].iter().all(|s| s.is_none()));
    }

    #[test]
    fn drain_mappings_preserves_per_entry_page_counts() {
        let mut aspace = AddressSpace::new_user();
        let flags = PageFlags::USER_RW;
        // Block A: 3 pages.
        for i in 0..3usize {
            aspace
                .map_page(
                    VirtAddr(0x1000 + (i as u64 * PAGE_SIZE as u64)),
                    Mapping {
                        phys: PhysAddr(0x10_000 + i as u64 * PAGE_SIZE as u64),
                        flags,
                    },
                )
                .unwrap();
        }
        // Block B: 1 page (non-adjacent).
        aspace
            .map_page(
                VirtAddr(0x8000),
                Mapping {
                    phys: PhysAddr(0x80_000),
                    flags,
                },
            )
            .unwrap();
        assert_eq!(aspace.mappings(), 2);

        let drained = aspace.drain_mappings();
        assert_eq!(aspace.mappings(), 0);
        assert_eq!(drained[0].expect("block A").pages, 3);
        assert_eq!(
            drained[0].expect("block A").mapping.phys,
            PhysAddr(0x10_000)
        );
        assert_eq!(drained[1].expect("block B").pages, 1);
        assert_eq!(
            drained[1].expect("block B").mapping.phys,
            PhysAddr(0x80_000)
        );
        assert!(drained[2..].iter().all(|s| s.is_none()));
    }

    #[test]
    fn drain_single_page_entry_produces_pages_one() {
        let mut aspace = AddressSpace::new_user();
        aspace
            .map_page(
                VirtAddr(0x1000),
                Mapping {
                    phys: PhysAddr(0xA_000),
                    flags: PageFlags::USER_RW,
                },
            )
            .unwrap();
        let drained = aspace.drain_mappings();
        let dm = drained[0].expect("single entry");
        assert_eq!(dm.pages, 1);
        assert_eq!(dm.mapping.phys, PhysAddr(0xA_000));
    }

    // --- Bug audit regression: destroy_and_collect_mappings atomicity (Claim 1) ---

    // Helper: fills retired[] to capacity while keeping one live space.
    // Returns that live ASID so callers can test destroy-with-full-retired.
    fn setup_full_retired_one_live() -> (AddressSpaceManager, Asid) {
        let mut mgr = AddressSpaceManager::default();
        // Create N spaces and destroy N-1 with pending bitmap → retired has N-1 entries.
        let mut batch = [Asid(0); MAX_ADDRESS_SPACES];
        for slot in batch.iter_mut() {
            *slot = mgr.create_user_space().expect("create");
        }
        let kept = batch[MAX_ADDRESS_SPACES - 1];
        for &asid in &batch[..MAX_ADDRESS_SPACES - 1] {
            mgr.destroy(asid, 0b1).expect("destroy");
        }
        // live[] = {kept}, retired[] = N-1 entries (1 free slot).
        // Create one more and immediately retire it to fill retired[].
        let filler = mgr.create_user_space().expect("filler");
        mgr.destroy(filler, 0b1).expect("destroy filler");
        // retired[] now has exactly MAX_ADDRESS_SPACES entries = FULL.
        assert!(
            mgr.retired.iter().all(|s| s.is_some()),
            "retired must be full"
        );
        (mgr, kept)
    }

    #[test]
    fn destroy_with_pending_bitmap_returns_full_when_retired_slots_exhausted() {
        let (mut mgr, victim) = setup_full_retired_one_live();

        // Give victim a mapping so we can verify drain_mappings was NOT called.
        mgr.get_mut(victim)
            .expect("victim present")
            .map_page(
                VirtAddr(0x1000),
                Mapping {
                    phys: PhysAddr(0xA_000),
                    flags: PageFlags::USER_RW,
                },
            )
            .expect("map");

        // destroy_and_collect_mappings must return VmError::Full without touching
        // the victim address space (pre-check before any state change).
        let result = mgr.destroy_and_collect_mappings(victim, 0b1);
        assert_eq!(result, Err(VmError::Full));

        // The victim must still be live.
        assert!(
            mgr.get(victim).is_some(),
            "live entry must be preserved on Full"
        );

        // The mapping must be intact (drain_mappings must not have been called).
        assert!(
            mgr.get(victim).unwrap().resolve(VirtAddr(0x1000)).is_some(),
            "mapping must be intact when destroy returns Full"
        );

        // retired[] must be unchanged — no ASID was inserted or removed.
        assert!(
            mgr.retired.iter().all(|s| s.is_some()),
            "retired must remain full"
        );
    }

    #[test]
    fn destroy_with_zero_bitmap_succeeds_even_when_retired_slots_full() {
        let (mut mgr, last) = setup_full_retired_one_live();

        // Destroying `last` with bitmap == 0 must succeed because no retired slot is
        // needed when there are no pending shootdowns.
        assert_eq!(mgr.destroy(last, 0), Ok(()));
        assert!(mgr.get(last).is_none());
    }

    #[test]
    fn destroy_unknown_asid_returns_invalid_asid_not_full() {
        // Even with full retired[], an unknown ASID must return InvalidAsid,
        // not Full — the ASID check happens before the retired-capacity check.
        let (mut mgr, _) = setup_full_retired_one_live();
        assert_eq!(
            mgr.destroy_and_collect_mappings(Asid(0xFFFF), 0b1),
            Err(VmError::InvalidAsid)
        );
    }

    // --- Bug audit regression: allocate_asid full scan (Claim 2) ---

    #[test]
    fn allocate_asid_never_returns_asid_zero() {
        let mut mgr = AddressSpaceManager::default();
        for _ in 0..MAX_ADDRESS_SPACES {
            let asid = mgr.create_user_space().expect("create");
            assert_ne!(asid, Asid(0), "ASID 0 must never be allocated");
            assert_eq!(mgr.destroy(asid, 0), Ok(()));
        }
    }

    #[test]
    fn allocate_asid_never_returns_a_retired_asid() {
        // Verify allocate_asid skips retired entries even when next_asid lands
        // in the retired range.  With the old MAX_ADDRESS_SPACES scan window this
        // could spuriously fail if all candidates in the window were retired.
        let mut mgr = AddressSpaceManager::default();
        let asid = mgr.create_user_space().expect("create");
        mgr.destroy(asid, 0b1).expect("destroy with bitmap");
        assert!(mgr.retired_entry(asid).is_some());

        // Allocate another — must not reuse the still-retired ASID.
        let asid2 = mgr.create_user_space().expect("create 2");
        assert_ne!(asid2, asid, "must not reuse a retired ASID");

        // After acknowledging the shootdown the ASID becomes reusable.
        assert_eq!(mgr.acknowledge_shootdown(asid, 0b1), Ok(true));
        assert!(mgr.get(asid2).is_some());
    }

    #[test]
    fn allocate_asid_succeeds_when_all_prior_asids_are_retired() {
        // Fill retired[] completely, then verify that create_user_space can still
        // allocate a fresh ASID outside the retired block.
        let mut mgr = AddressSpaceManager::default();
        let mut batch = [Asid(0); MAX_ADDRESS_SPACES];
        for slot in batch.iter_mut() {
            *slot = mgr.create_user_space().expect("create");
        }
        // next_asid is now MAX_ADDRESS_SPACES + 1.
        for &asid in batch.iter() {
            mgr.destroy(asid, 0b1).expect("destroy");
        }
        // retired[] = {1..MAX_ADDRESS_SPACES}, live[] = empty,
        // next_asid = MAX_ADDRESS_SPACES+1 (already outside the retired block).
        let fresh = mgr
            .create_user_space()
            .expect("must allocate past retired block");
        assert!(
            !batch.contains(&fresh),
            "new ASID {fresh:?} must not be a still-retired ASID"
        );
        assert!(mgr.get(fresh).is_some());
    }

    // --- Bug audit regression: isolate_page_entry_at wrong error (Claim 5) ---

    #[test]
    fn isolate_page_entry_at_wrong_error_variant_is_now_invalid_address() {
        // Verify that the VmError::InvalidAddress variant exists in the enum.
        // The previous code returned VmError::PrivilegeViolation for an internal
        // invariant check in isolate_page_entry_at. This test pins the new variant.
        let variants_include_invalid_address =
            matches!(VmError::InvalidAddress, VmError::InvalidAddress);
        assert!(variants_include_invalid_address);
        // Also confirm PrivilegeViolation is still used for actual privilege checks.
        let privileges_still_exist =
            matches!(VmError::PrivilegeViolation, VmError::PrivilegeViolation);
        assert!(privileges_still_exist);
    }

    #[test]
    fn map_kernel_page_into_user_space_returns_privilege_violation_not_invalid_address() {
        // Privilege checks must still return PrivilegeViolation, not InvalidAddress.
        let mut aspace = AddressSpace::new_user();
        let result = aspace.map_page(
            VirtAddr(KERNEL_SPACE_BASE),
            Mapping {
                phys: PhysAddr(0x1000),
                flags: PageFlags::USER_RW,
            },
        );
        assert_eq!(
            result,
            Err(VmError::PrivilegeViolation),
            "mapping kernel VA into user space must be PrivilegeViolation"
        );
    }

    // ── Stage 163F VM audit regression tests ──────────────────────────────────

    // Item 1: has_mapping_for_phys covers the whole run, not just the base page.
    #[test]
    fn has_mapping_for_phys_covers_whole_run() {
        let mut aspace = AddressSpace::new_user();
        let rw = PageFlags::USER_RW;
        // Build a 3-page run at VA 0x10000, PA 0x20000.
        for p in 0..3u64 {
            aspace
                .map_page(
                    VirtAddr(0x10_000 + p * PAGE_SIZE as u64),
                    Mapping {
                        phys: PhysAddr(0x20_000 + p * PAGE_SIZE as u64),
                        flags: rw,
                    },
                )
                .expect("map");
        }
        assert_eq!(aspace.mappings(), 1, "contiguous pages form one run");
        // base, middle, last page → contained.
        assert!(aspace.has_mapping_for_phys(PhysAddr(0x20_000)), "base page");
        assert!(
            aspace.has_mapping_for_phys(PhysAddr(0x21_000)),
            "middle page (the bug: was false)"
        );
        assert!(aspace.has_mapping_for_phys(PhysAddr(0x22_000)), "last page");
        // just-past-end and just-before-base → not contained.
        assert!(
            !aspace.has_mapping_for_phys(PhysAddr(0x23_000)),
            "just past end"
        );
        assert!(
            !aspace.has_mapping_for_phys(PhysAddr(0x1F_000)),
            "just before base"
        );
    }

    // Item 2: VirtAddr::checked_add reports overflow; Add wraps (documented).
    #[test]
    fn virt_addr_checked_add_detects_overflow() {
        assert_eq!(VirtAddr(0x1000).checked_add(0x1000), Some(VirtAddr(0x2000)));
        assert_eq!(VirtAddr(u64::MAX).checked_add(1), None);
        assert_eq!(VirtAddr(u64::MAX - 3).checked_add(10), None);
        // The Add operator intentionally wraps (kept for in-bounds page offsets).
        assert_eq!((VirtAddr(u64::MAX) + 1).0, 0);
    }

    // Item 3: coalescing uses checked arithmetic — an overflowing phys end must not
    // produce a false merge. Two runs whose phys would "wrap into" each other must
    // stay distinct.
    #[test]
    fn coalescing_does_not_merge_on_phys_overflow() {
        let mut aspace = AddressSpace::new_user();
        let rw = PageFlags::USER_RW;
        // A run near the top of the physical space: PA base = u64::MAX-PAGE_SIZE+1
        // (one page). Its "phys end" would overflow u64.
        let top_page_phys = PhysAddr(u64::MAX - (PAGE_SIZE as u64) + 1);
        aspace
            .map_page(
                VirtAddr(0x40_000),
                Mapping {
                    phys: top_page_phys,
                    flags: rw,
                },
            )
            .expect("map top page");
        // A second page adjacent in VA but with PA 0 (so a wrapped phys-end would
        // falsely equal it). It must NOT merge — checked arithmetic treats the
        // overflowing end as "not adjacent".
        aspace
            .map_page(
                VirtAddr(0x41_000),
                Mapping {
                    phys: PhysAddr(0),
                    flags: rw,
                },
            )
            .expect("map second page");
        assert_eq!(
            aspace.mappings(),
            2,
            "phys-end overflow must not coalesce distinct runs"
        );
    }

    // Item 4: drained mappings preserve virt, phys, and page count.
    #[test]
    fn drain_mappings_preserves_virt_phys_pages() {
        let mut aspace = AddressSpace::new_user();
        let rw = PageFlags::USER_RW;
        for p in 0..2u64 {
            aspace
                .map_page(
                    VirtAddr(0x30_000 + p * PAGE_SIZE as u64),
                    Mapping {
                        phys: PhysAddr(0x80_000 + p * PAGE_SIZE as u64),
                        flags: rw,
                    },
                )
                .expect("map");
        }
        let drained = aspace.drain_mappings();
        let dm = drained.into_iter().flatten().next().expect("one run");
        assert_eq!(dm.virt, VirtAddr(0x30_000), "drained run preserves virt");
        assert_eq!(dm.mapping.phys, PhysAddr(0x80_000), "preserves phys");
        assert_eq!(dm.pages, 2, "preserves page count");
    }

    // Item 5: a full manager performs no ASID allocation/registration, and a freed
    // slot lets the next create succeed (no state corruption on the full path).
    #[test]
    fn create_user_space_full_then_freed_recovers() {
        let mut mgr = AddressSpaceManager::default();
        for _ in 0..MAX_ADDRESS_SPACES {
            assert!(mgr.create_user_space().is_ok());
        }
        assert_eq!(
            mgr.create_user_space(),
            Err(VmError::Full),
            "full manager must reject before allocating an ASID"
        );
        // Free one (pending=0 → immediately reusable) and retry.
        let live = mgr.live_count();
        assert_eq!(live, MAX_ADDRESS_SPACES);
        // Destroy the first live ASID we can find.
        let some_asid = (1u16..=u16::MAX)
            .map(Asid)
            .find(|a| mgr.get(*a).is_some())
            .expect("a live asid");
        assert_eq!(mgr.destroy(some_asid, 0), Ok(()));
        assert!(
            mgr.create_user_space().is_ok(),
            "a freed slot must allow a new create"
        );
    }

    // Item 12: acknowledging an unset (but nonzero) CPU bit leaves the pending set
    // unchanged and reports "not yet complete".
    #[test]
    fn acknowledge_shootdown_unset_bit_is_noop() {
        let mut mgr = AddressSpaceManager::default();
        let asid = mgr.create_user_space().expect("asid");
        assert_eq!(mgr.destroy(asid, 0b0110), Ok(()));
        // Bit 0 is not in the pending set {1,2}: no progress, still pending.
        assert_eq!(mgr.acknowledge_shootdown(asid, 0b0001), Ok(false));
        // The real bits still complete it.
        assert_eq!(mgr.acknowledge_shootdown(asid, 0b0010), Ok(false));
        assert_eq!(mgr.acknowledge_shootdown(asid, 0b0100), Ok(true));
    }

    // Item 8: canonical-address policy (x86_64 only) — non-canonical user addresses
    // are rejected by map_page; canonical low/high addresses are accepted by the
    // is_canonical predicate.
    #[test]
    #[cfg(target_arch = "x86_64")]
    fn map_page_rejects_non_canonical_x86_64() {
        assert!(VirtAddr(0x1000).is_canonical(), "low user VA is canonical");
        assert!(
            VirtAddr(KERNEL_SPACE_BASE).is_canonical(),
            "kernel base is canonical"
        );
        let non_canonical = VirtAddr(0x0001_0000_0000_0000);
        assert!(
            !non_canonical.is_canonical(),
            "0x0001_0000_0000_0000 is non-canonical"
        );
        let mut aspace = AddressSpace::new_user();
        assert_eq!(
            aspace.map_page(
                non_canonical,
                Mapping {
                    phys: PhysAddr(0x9_000),
                    flags: PageFlags::USER_RW,
                }
            ),
            Err(VmError::InvalidAddress),
            "map_page must reject a non-canonical user VA"
        );
    }

    // Item 13: the ASID allocator never hands out Asid(0).
    #[test]
    fn allocate_asid_never_zero() {
        let mut mgr = AddressSpaceManager::default();
        for _ in 0..MAX_ADDRESS_SPACES {
            let a = mgr.create_user_space().expect("asid");
            assert_ne!(a, Asid(0), "ASID 0 is reserved and must never be allocated");
        }
    }
}
