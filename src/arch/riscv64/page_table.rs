// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use crate::arch::riscv64::vm_layout;
use crate::kernel::frame_allocator::{alloc_pt_frame, free_pt_frame};
use crate::kernel::lock::SpinLockIrq;
use crate::kernel::vm::{Asid, CachePolicy, PageFlags, PhysAddr, VirtAddr};

const ENTRIES_PER_TABLE: usize = 512;
const PAGE_SHIFT: u64 = 12;
const PAGE_SIZE_U64: u64 = vm_layout::PAGE_SIZE as u64;
const PAGE_MASK: u64 = !(PAGE_SIZE_U64 - 1);
const PTE_ADDR_MASK: u64 = 0x003f_ffff_ffff_fc00;
const INTERMEDIATE_PT_PAGES_PER_MAPPING: usize = 4;
const MAX_PT_PAGES: usize = vm_layout::MAX_ADDRESS_SPACES
    * (1 + vm_layout::MAX_MAPPINGS * INTERMEDIATE_PT_PAGES_PER_MAPPING);
const MAX_ASID_ROOTS: usize = vm_layout::MAX_ADDRESS_SPACES * 8;

#[cfg(test)]
static LAST_INVALIDATED_ASID: SpinLock<Option<Asid>> = SpinLock::new(None);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageTableEntry(pub u64);

impl PageTableEntry {
    pub const VALID: u64 = 1 << 0;
    pub const READ: u64 = 1 << 1;
    pub const WRITE: u64 = 1 << 2;
    pub const EXECUTE: u64 = 1 << 3;
    pub const USER: u64 = 1 << 4;
    pub const GLOBAL: u64 = 1 << 5;
    pub const ACCESSED: u64 = 1 << 6;
    pub const DIRTY: u64 = 1 << 7;

    pub const fn empty() -> Self {
        Self(0)
    }

    pub const fn is_present(self) -> bool {
        (self.0 & Self::VALID) != 0
    }

    pub const fn addr(self) -> u64 {
        ((self.0 & PTE_ADDR_MASK) >> 10) << PAGE_SHIFT
    }

    pub const fn with_addr_and_flags(phys: u64, flags: u64) -> Self {
        let ppn = (phys & PAGE_MASK) >> PAGE_SHIFT;
        Self((ppn << 10) | flags)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageTableError {
    InvalidAddress,
    OutOfMemory,
}

#[derive(Clone, Copy)]
struct PageTablePage {
    phys: u64,
    entries: [PageTableEntry; ENTRIES_PER_TABLE],
}

impl PageTablePage {
    const fn new(phys: u64) -> Self {
        Self {
            phys,
            entries: [PageTableEntry::empty(); ENTRIES_PER_TABLE],
        }
    }
}

#[derive(Clone, Copy)]
struct AsidRoot {
    asid: Asid,
    root_phys: u64,
}

struct PageTableState {
    pages: [Option<PageTablePage>; MAX_PT_PAGES],
    asids: [Option<AsidRoot>; MAX_ASID_ROOTS],
}

impl PageTableState {
    const fn new() -> Self {
        Self {
            pages: [const { None }; MAX_PT_PAGES],
            asids: [const { None }; MAX_ASID_ROOTS],
        }
    }

    fn page_index_from_phys(&self, phys: u64) -> Option<usize> {
        for (idx, page) in self.pages.iter().enumerate() {
            if page.is_some_and(|entry| entry.phys == phys) {
                return Some(idx);
            }
        }
        None
    }

    fn alloc_page(&mut self) -> Result<usize, PageTableError> {
        for (idx, slot) in self.pages.iter_mut().enumerate() {
            if slot.is_none() {
                let phys = alloc_pt_frame().map_err(|_| PageTableError::OutOfMemory)?;
                // The hardware walks the *physical* frame, so it must start
                // zeroed (no stale/garbage PTEs from a recycled frame).
                zero_pt_frame(phys);
                *slot = Some(PageTablePage::new(phys));
                return Ok(idx);
            }
        }
        Err(PageTableError::OutOfMemory)
    }

    fn ensure_asid(&mut self, asid: Asid) -> Result<u64, PageTableError> {
        if let Some(root) = self
            .asids
            .iter()
            .flatten()
            .find(|entry| entry.asid == asid)
            .map(|entry| entry.root_phys)
        {
            return Ok(root);
        }

        let root_idx = self.alloc_page()?;
        let root_phys = self.pages[root_idx].expect("root page").phys;
        for slot in &mut self.asids {
            if slot.is_none() {
                *slot = Some(AsidRoot { asid, root_phys });
                return Ok(root_phys);
            }
        }
        Err(PageTableError::OutOfMemory)
    }

    fn root_for_asid(&self, asid: Asid) -> Option<u64> {
        self.asids
            .iter()
            .flatten()
            .find(|entry| entry.asid == asid)
            .map(|entry| entry.root_phys)
    }
}

static PAGE_TABLE_STATE: SpinLockIrq<PageTableState> = SpinLockIrq::new(PageTableState::new());

/// Writes a single PTE word into the actual physical page-table frame the MMU
/// walks. RISC-V identity-maps page-table frames (satp=0 bare mode during
/// setup, and the kernel-shared gigapage once a user satp is active), so the
/// frame's physical address is directly addressable. The in-memory
/// `PageTablePage::entries` shadow is kept in sync for software walks; this is
/// the half that the hardware actually reads.
#[cfg(all(not(test), not(feature = "hosted-dev"), target_arch = "riscv64"))]
fn store_pte_to_frame(frame_phys: u64, index: usize, pte: PageTableEntry) {
    unsafe {
        core::ptr::write_volatile((frame_phys as *mut u64).add(index), pte.0);
    }
}

#[cfg(any(test, feature = "hosted-dev", not(target_arch = "riscv64")))]
fn store_pte_to_frame(_frame_phys: u64, _index: usize, _pte: PageTableEntry) {}

#[cfg(all(not(test), not(feature = "hosted-dev"), target_arch = "riscv64"))]
fn zero_pt_frame(frame_phys: u64) {
    unsafe {
        let ptr = frame_phys as *mut u64;
        for i in 0..ENTRIES_PER_TABLE {
            core::ptr::write_volatile(ptr.add(i), 0);
        }
    }
}

#[cfg(any(test, feature = "hosted-dev", not(target_arch = "riscv64")))]
fn zero_pt_frame(_frame_phys: u64) {}

pub fn reset_state() {
    let mut state = PAGE_TABLE_STATE.lock();
    for page in &mut state.pages {
        *page = None;
    }
    for asid in &mut state.asids {
        *asid = None;
    }
}

#[inline]
fn level_index(va: u64, shift: u64) -> usize {
    ((va >> shift) & 0x1ff) as usize
}

fn table_flags_from_page_flags(flags: PageFlags) -> u64 {
    // Per the RISC-V Sv39 spec, a non-leaf (table-pointer) PTE has R=W=X=0
    // and "U, A, D, and G bits are reserved for future use and must be cleared
    // by software for forward compatibility." QEMU enforces this — setting U
    // on an intermediate PTE causes the walk to be classified as a leaf with
    // bad permissions, which surfaces as an instruction page fault even though
    // the actual leaf has correct flags. Only the VALID bit is allowed here.
    let _ = flags;
    PageTableEntry::VALID
}

fn leaf_flags_from_page_flags(flags: PageFlags) -> u64 {
    let mut bits = PageTableEntry::VALID | PageTableEntry::ACCESSED;
    if flags.read {
        bits |= PageTableEntry::READ;
    }
    if flags.write {
        bits |= PageTableEntry::WRITE | PageTableEntry::DIRTY;
    }
    if flags.execute {
        bits |= PageTableEntry::EXECUTE;
    }
    if flags.user {
        bits |= PageTableEntry::USER;
    }
    bits |= cache_policy_bits(flags.cache_policy);
    bits
}

fn cache_policy_bits(policy: CachePolicy) -> u64 {
    match policy {
        // Sv39 has no base per-PTE cache policy bits in the common profile.
        CachePolicy::WriteBack
        | CachePolicy::WriteThrough
        | CachePolicy::Uncached
        | CachePolicy::Device => 0,
    }
}

fn walk_or_create(
    state: &mut PageTableState,
    table_phys: u64,
    index: usize,
    flags: PageFlags,
) -> Result<u64, PageTableError> {
    let table_idx = state
        .page_index_from_phys(table_phys)
        .ok_or(PageTableError::InvalidAddress)?;
    let entry = state.pages[table_idx].as_ref().expect("table").entries[index];
    if entry.is_present() {
        return Ok(entry.addr());
    }
    let child_idx = state.alloc_page()?;
    let child_phys = state.pages[child_idx].expect("child").phys;
    let pte = PageTableEntry::with_addr_and_flags(child_phys, table_flags_from_page_flags(flags));
    state.pages[table_idx].as_mut().expect("table").entries[index] = pte;
    store_pte_to_frame(table_phys, index, pte);
    Ok(child_phys)
}

pub fn ensure_asid_root(asid: Asid) -> Result<(), PageTableError> {
    let mut state = PAGE_TABLE_STATE.lock();
    state.ensure_asid(asid)?;
    Ok(())
}

pub fn remove_asid_root(asid: Asid) {
    let mut state = PAGE_TABLE_STATE.lock();
    if let Some(slot) = state
        .asids
        .iter()
        .position(|entry| entry.is_some_and(|value| value.asid == asid))
    {
        if let Some(root) = state.asids[slot] {
            let mut stack: [(u64, usize); MAX_PT_PAGES] = [(0, 0); MAX_PT_PAGES];
            let mut sp = 0usize;
            stack[sp] = (root.root_phys, 3);
            sp += 1;
            while sp > 0 {
                sp -= 1;
                let (table_phys, level) = stack[sp];
                let Some(table_idx) = state.page_index_from_phys(table_phys) else {
                    continue;
                };
                if level > 1 {
                    let entries = state.pages[table_idx].expect("table").entries;
                    for entry in entries {
                        if !entry.is_present() {
                            continue;
                        }
                        let child_phys = entry.addr();
                        if state.page_index_from_phys(child_phys).is_some() && sp < MAX_PT_PAGES {
                            stack[sp] = (child_phys, level - 1);
                            sp += 1;
                        }
                    }
                }
                if let Some(page) = state.pages[table_idx].take() {
                    let _ = free_pt_frame(page.phys);
                }
            }
        }
        state.asids[slot] = None;
    }
}

pub fn cr3_for_asid(asid: Asid) -> Option<u64> {
    const SATP_MODE_SV39: u64 = 8u64 << 60;
    const SATP_ASID_SHIFT: u64 = 44;

    let mut state = PAGE_TABLE_STATE.lock();
    let root = state.ensure_asid(asid).ok()?;
    let asid_mask = (1u64 << vm_layout::ASID_BITS.min(16)) - 1;
    let asid_bits = ((asid.0 as u64) & asid_mask) << SATP_ASID_SHIFT;
    let root_ppn = (root & PAGE_MASK) >> PAGE_SHIFT;
    Some(SATP_MODE_SV39 | asid_bits | root_ppn)
}

/// Kernel-shared identity gigapage covering [0x8000_0000, 0xC000_0000): the
/// kernel image (text/rodata/data/bss), all frame-allocated kernel stacks, and
/// the S-mode trap vector. RISC-V userspace bring-up: installed into every user
/// address-space root so the kernel keeps executing across a `satp` switch into
/// a user page table while U-mode (no USER bit) cannot reach kernel memory.
pub const RISCV_KERNEL_SHARED_BASE: u64 = 0x8000_0000;
pub const RISCV_KERNEL_SHARED_END: u64 = 0xC000_0000;

/// Installs the kernel-shared gigapage leaf at root index 2 of `asid`'s page
/// table. Idempotent. Returns the (base, end) of the mapped window.
pub fn map_kernel_shared_into_asid(asid: Asid) -> Result<(u64, u64), PageTableError> {
    let mut state = PAGE_TABLE_STATE.lock();
    let root = state.ensure_asid(asid)?;
    let root_idx = state
        .page_index_from_phys(root)
        .ok_or(PageTableError::InvalidAddress)?;
    let l2 = level_index(RISCV_KERNEL_SHARED_BASE, 30);
    // GLOBAL + RWX leaf, ACCESSED|DIRTY pre-set, no USER: S-mode only.
    let flags = PageTableEntry::VALID
        | PageTableEntry::READ
        | PageTableEntry::WRITE
        | PageTableEntry::EXECUTE
        | PageTableEntry::GLOBAL
        | PageTableEntry::ACCESSED
        | PageTableEntry::DIRTY;
    let pte = PageTableEntry::with_addr_and_flags(RISCV_KERNEL_SHARED_BASE, flags);
    state.pages[root_idx].as_mut().expect("root").entries[l2] = pte;
    store_pte_to_frame(root, l2, pte);
    Ok((RISCV_KERNEL_SHARED_BASE, RISCV_KERNEL_SHARED_END))
}

/// Writes `satp` and flushes the TLB. Unlike [`activate_asid`] this takes a
/// pre-computed satp value (used by the userspace entry/probe paths so the
/// exact installed value can be logged).
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
pub fn write_satp(satp: u64) {
    unsafe {
        core::arch::asm!(
            "csrw satp, {value}",
            "sfence.vma x0, x0",
            value = in(reg) satp,
            options(nostack, preserves_flags)
        );
    }
}

pub fn activate_asid(asid: Asid) -> Result<u64, PageTableError> {
    let satp = cr3_for_asid(asid).ok_or(PageTableError::OutOfMemory)?;
    #[cfg(not(feature = "hosted-dev"))]
    unsafe {
        core::arch::asm!(
            "csrw satp, {value}",
            "sfence.vma x0, x0",
            value = in(reg) satp,
            options(nostack, preserves_flags)
        );
    }
    Ok(satp)
}

pub fn map_page(
    asid: Asid,
    virt: VirtAddr,
    phys: PhysAddr,
    flags: PageFlags,
) -> Result<Option<PageTableEntry>, PageTableError> {
    if !virt.0.is_multiple_of(vm_layout::PAGE_SIZE as u64)
        || !phys.0.is_multiple_of(vm_layout::PAGE_SIZE as u64)
    {
        return Err(PageTableError::InvalidAddress);
    }

    let mut state = PAGE_TABLE_STATE.lock();
    let root = state.ensure_asid(asid)?;
    let l2 = level_index(virt.0, 30);
    let l1 = level_index(virt.0, 21);
    let l0 = level_index(virt.0, 12);

    let next1 = walk_or_create(&mut state, root, l2, flags)?;
    let next2 = walk_or_create(&mut state, next1, l1, flags)?;

    let leaf_idx = state
        .page_index_from_phys(next2)
        .ok_or(PageTableError::InvalidAddress)?;
    let table = state.pages[leaf_idx].as_mut().expect("leaf");
    let prev = table.entries[l0];
    let pte = PageTableEntry::with_addr_and_flags(phys.0, leaf_flags_from_page_flags(flags));
    table.entries[l0] = pte;
    store_pte_to_frame(next2, l0, pte);
    drop(state);
    invalidate_page(virt);
    Ok(prev.is_present().then_some(prev))
}

pub fn unmap_page(asid: Asid, virt: VirtAddr) -> Option<PageTableEntry> {
    let mut state = PAGE_TABLE_STATE.lock();
    let mut table_phys = state.root_for_asid(asid)?;
    let levels = [
        level_index(virt.0, 30),
        level_index(virt.0, 21),
        level_index(virt.0, 12),
    ];

    for &level in &levels[..2] {
        let idx = state.page_index_from_phys(table_phys)?;
        let entry = state.pages[idx].as_ref()?.entries[level];
        if !entry.is_present() {
            return None;
        }
        table_phys = entry.addr();
    }

    let leaf_idx = state.page_index_from_phys(table_phys)?;
    let table = state.pages[leaf_idx].as_mut()?;
    let old = table.entries[levels[2]];
    if !old.is_present() {
        return None;
    }
    table.entries[levels[2]] = PageTableEntry::empty();
    store_pte_to_frame(table_phys, levels[2], PageTableEntry::empty());
    drop(state);
    invalidate_page(virt);
    Some(old)
}

pub fn resolve_page(asid: Asid, virt: VirtAddr) -> Option<PageTableEntry> {
    let state = PAGE_TABLE_STATE.lock();
    let mut table_phys = state.root_for_asid(asid)?;
    let levels = [
        level_index(virt.0, 30),
        level_index(virt.0, 21),
        level_index(virt.0, 12),
    ];

    for &level in &levels[..2] {
        let idx = state.page_index_from_phys(table_phys)?;
        let entry = state.pages[idx].as_ref()?.entries[level];
        if !entry.is_present() {
            return None;
        }
        table_phys = entry.addr();
    }

    let leaf_idx = state.page_index_from_phys(table_phys)?;
    let entry = state.pages[leaf_idx].as_ref()?.entries[levels[2]];
    entry.is_present().then_some(entry)
}

pub fn invalidate_page(virt: VirtAddr) {
    #[cfg(test)]
    {
        let _ = virt;
        return;
    }

    #[cfg(all(feature = "hosted-dev", not(test)))]
    {
        let _ = virt;
    }

    #[cfg(all(not(feature = "hosted-dev"), not(test)))]
    unsafe {
        core::arch::asm!(
            "sfence.vma {vaddr}, x0",
            vaddr = in(reg) virt.0 as usize,
            options(nostack, preserves_flags)
        );
    }
}

pub fn invalidate_asid(asid: Asid) {
    #[cfg(test)]
    {
        *LAST_INVALIDATED_ASID.lock() = Some(asid);
        return;
    }

    #[cfg(all(feature = "hosted-dev", not(test)))]
    {
        let _ = asid;
    }

    #[cfg(all(not(feature = "hosted-dev"), not(test)))]
    unsafe {
        core::arch::asm!(
            "sfence.vma x0, {asid}",
            asid = in(reg) asid.0 as usize,
            options(nostack, preserves_flags)
        );
    }
}

/// Stage 163I: full local TLB flush (all address translations on this hart).
///
/// Mirrors the x86_64 entry point used by the shared page-fault recovery path.
/// On a present write fault that recurs despite a per-page invalidation, this
/// drops every translation so the hart re-walks the page table.
pub fn flush_tlb_local_full() {
    #[cfg(any(test, feature = "hosted-dev"))]
    {}

    #[cfg(all(not(feature = "hosted-dev"), not(test)))]
    unsafe {
        core::arch::asm!("sfence.vma x0, x0", options(nostack, preserves_flags));
    }
}

/// Stage 163I: x86_64 needs to widen under-permissioned intermediate paging
/// entries (the AND-of-levels access check denies a permissive leaf). RISC-V
/// carries R/W/X/U permission bits only on leaf PTEs (non-leaf entries with no
/// R/W/X are pure pointers), so there is no intermediate-permission repair to
/// perform; this is a typed no-op kept so the shared fault handler can call one
/// symbol across architectures.
pub fn repair_user_path_intermediates(_asid: Asid, _virt: VirtAddr) -> u8 {
    0
}

#[cfg(test)]
pub fn take_last_invalidated_asid_for_test() -> Option<Asid> {
    LAST_INVALIDATED_ASID.lock().take()
}
