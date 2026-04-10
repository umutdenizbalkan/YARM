// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use crate::arch::aarch64::vm_layout;
use crate::kernel::frame_allocator::{alloc_pt_frame, free_pt_frame};
use crate::kernel::lock::SpinLock;
use crate::kernel::vm::{Asid, CachePolicy, PageFlags, PhysAddr, VirtAddr};

const ENTRIES_PER_TABLE: usize = 512;
const PAGE_SIZE_U64: u64 = vm_layout::PAGE_SIZE as u64;
const PAGE_MASK: u64 = !(PAGE_SIZE_U64 - 1);
const PTE_ADDR_MASK: u64 = 0x0000_ffff_ffff_f000;
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
    pub const TABLE_OR_PAGE: u64 = 1 << 1;
    pub const USER: u64 = 1 << 6;
    pub const READ_ONLY: u64 = 1 << 7;
    pub const ACCESSED: u64 = 1 << 10;
    pub const NO_EXECUTE: u64 = 1 << 54;
    pub const PRIV_NO_EXECUTE: u64 = 1 << 53;

    pub const fn empty() -> Self {
        Self(0)
    }

    pub const fn is_present(self) -> bool {
        (self.0 & Self::VALID) != 0
    }

    pub const fn addr(self) -> u64 {
        self.0 & PTE_ADDR_MASK
    }

    pub const fn with_addr_and_flags(phys: u64, flags: u64) -> Self {
        Self((phys & PTE_ADDR_MASK) | flags)
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

static PAGE_TABLE_STATE: SpinLock<PageTableState> = SpinLock::new(PageTableState::new());

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
    let mut bits = PageTableEntry::VALID | PageTableEntry::TABLE_OR_PAGE;
    if flags.user {
        bits |= PageTableEntry::USER;
    }
    bits |= cache_policy_bits(flags.cache_policy);
    bits
}

fn leaf_flags_from_page_flags(flags: PageFlags) -> u64 {
    let mut bits = PageTableEntry::VALID | PageTableEntry::TABLE_OR_PAGE | PageTableEntry::ACCESSED;
    if !flags.write {
        bits |= PageTableEntry::READ_ONLY;
    }
    if flags.user {
        bits |= PageTableEntry::USER;
    }
    if !flags.execute {
        bits |= PageTableEntry::NO_EXECUTE;
    }
    if flags.user {
        bits |= PageTableEntry::PRIV_NO_EXECUTE;
    }
    bits |= cache_policy_bits(flags.cache_policy);
    bits
}

fn cache_policy_bits(policy: CachePolicy) -> u64 {
    const ATTR_SHIFT: u64 = 2;
    let attr_index = match policy {
        CachePolicy::WriteBack => 0u64,
        CachePolicy::WriteThrough => 1u64,
        CachePolicy::Uncached => 2u64,
        CachePolicy::Device => 3u64,
    };
    attr_index << ATTR_SHIFT
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
    state.pages[table_idx].as_mut().expect("table").entries[index] =
        PageTableEntry::with_addr_and_flags(child_phys, table_flags_from_page_flags(flags));
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
            stack[sp] = (root.root_phys, 4);
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
    let mut state = PAGE_TABLE_STATE.lock();
    let root = state.ensure_asid(asid).ok()?;
    let asid_bits = (asid.0 as u64) & ((1u64 << vm_layout::ASID_BITS.min(16)) - 1);
    Some((root & PAGE_MASK) | asid_bits)
}

pub fn activate_asid(asid: Asid) -> Result<u64, PageTableError> {
    let ttbr0 = cr3_for_asid(asid).ok_or(PageTableError::OutOfMemory)?;
    #[cfg(not(feature = "hosted-dev"))]
    unsafe {
        core::arch::asm!(
            "msr ttbr0_el1, {value}",
            "isb",
            value = in(reg) ttbr0,
            options(nostack, preserves_flags)
        );
    }
    Ok(ttbr0)
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
    let l0 = level_index(virt.0, 39);
    let l1 = level_index(virt.0, 30);
    let l2 = level_index(virt.0, 21);
    let l3 = level_index(virt.0, 12);

    let next1 = walk_or_create(&mut state, root, l0, flags)?;
    let next2 = walk_or_create(&mut state, next1, l1, flags)?;
    let next3 = walk_or_create(&mut state, next2, l2, flags)?;

    let leaf_idx = state
        .page_index_from_phys(next3)
        .ok_or(PageTableError::InvalidAddress)?;
    let table = state.pages[leaf_idx].as_mut().expect("leaf");
    let prev = table.entries[l3];
    table.entries[l3] =
        PageTableEntry::with_addr_and_flags(phys.0, leaf_flags_from_page_flags(flags));
    drop(state);
    invalidate_page(virt);
    Ok(prev.is_present().then_some(prev))
}

pub fn unmap_page(asid: Asid, virt: VirtAddr) -> Option<PageTableEntry> {
    let mut state = PAGE_TABLE_STATE.lock();
    let mut table_phys = state.root_for_asid(asid)?;
    let levels = [
        level_index(virt.0, 39),
        level_index(virt.0, 30),
        level_index(virt.0, 21),
        level_index(virt.0, 12),
    ];

    for &level in &levels[..3] {
        let idx = state.page_index_from_phys(table_phys)?;
        let entry = state.pages[idx].as_ref()?.entries[level];
        if !entry.is_present() {
            return None;
        }
        table_phys = entry.addr();
    }

    let leaf_idx = state.page_index_from_phys(table_phys)?;
    let table = state.pages[leaf_idx].as_mut()?;
    let old = table.entries[levels[3]];
    if !old.is_present() {
        return None;
    }
    table.entries[levels[3]] = PageTableEntry::empty();
    drop(state);
    invalidate_page(virt);
    Some(old)
}

pub fn resolve_page(asid: Asid, virt: VirtAddr) -> Option<PageTableEntry> {
    let state = PAGE_TABLE_STATE.lock();
    let mut table_phys = state.root_for_asid(asid)?;
    let levels = [
        level_index(virt.0, 39),
        level_index(virt.0, 30),
        level_index(virt.0, 21),
        level_index(virt.0, 12),
    ];

    for &level in &levels[..3] {
        let idx = state.page_index_from_phys(table_phys)?;
        let entry = state.pages[idx].as_ref()?.entries[level];
        if !entry.is_present() {
            return None;
        }
        table_phys = entry.addr();
    }

    let leaf_idx = state.page_index_from_phys(table_phys)?;
    let entry = state.pages[leaf_idx].as_ref()?.entries[levels[3]];
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
        let operand = virt.0 >> 12;
        core::arch::asm!(
            "dsb ishst",
            "tlbi vaae1is, {operand}",
            "dsb ish",
            "isb",
            operand = in(reg) operand,
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
        let operand = (asid.0 as u64) << 48;
        core::arch::asm!(
            "dsb ishst",
            "tlbi aside1is, {operand}",
            "dsb ish",
            "isb",
            operand = in(reg) operand,
            options(nostack, preserves_flags)
        );
    }
}

#[cfg(test)]
pub fn take_last_invalidated_asid_for_test() -> Option<Asid> {
    LAST_INVALIDATED_ASID.lock().take()
}
