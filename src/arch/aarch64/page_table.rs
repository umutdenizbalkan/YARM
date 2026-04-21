// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use crate::arch::aarch64::vm_layout;
use crate::kernel::frame_allocator::{alloc_pt_frame, free_pt_frame};
#[cfg(test)]
use crate::kernel::lock::SpinLock;
use crate::kernel::lock::SpinLockIrq;
use crate::kernel::vm::{Asid, CachePolicy, PageFlags, PhysAddr, VirtAddr};

const ENTRIES_PER_TABLE: usize = 512;
const PAGE_SIZE_U64: u64 = vm_layout::PAGE_SIZE as u64;
const PAGE_MASK: u64 = !(PAGE_SIZE_U64 - 1);
const PTE_ADDR_MASK: u64 = 0x0000_ffff_ffff_f000;
const INTERMEDIATE_PT_PAGES_PER_MAPPING: usize = 4;
const MAX_PT_PAGES: usize = vm_layout::MAX_ADDRESS_SPACES
    * (1 + vm_layout::MAX_MAPPINGS * INTERMEDIATE_PT_PAGES_PER_MAPPING);
const MAX_ASID_ROOTS: usize = vm_layout::MAX_ADDRESS_SPACES * 8;
const EARLY_UART_MMIO_VA: u64 = 0x0900_0000;
const EARLY_UART_MMIO_PA: u64 = 0x0900_0000;

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
                clear_physical_table_page(phys)?;
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
        copy_bootstrap_kernel_root_entries(self, root_idx)?;
        ensure_early_uart_mapping(self, root_idx)?;
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

#[cfg(all(not(feature = "hosted-dev"), not(test), target_arch = "aarch64"))]
#[inline]
fn phys_to_virt_table_ptr(table_phys: u64) -> *mut u64 {
    table_phys as usize as *mut u64
}

#[cfg(any(feature = "hosted-dev", test, not(target_arch = "aarch64")))]
#[inline]
fn phys_to_virt_table_ptr(_table_phys: u64) -> *mut u64 {
    core::ptr::null_mut()
}

fn clear_physical_table_page(table_phys: u64) -> Result<(), PageTableError> {
    let ptr = phys_to_virt_table_ptr(table_phys);
    if ptr.is_null() {
        return Ok(());
    }
    for idx in 0..ENTRIES_PER_TABLE {
        unsafe {
            core::ptr::write_volatile(ptr.add(idx), 0);
        }
    }
    Ok(())
}

fn read_table_entry(
    state: &mut PageTableState,
    table_idx: usize,
    index: usize,
) -> Result<PageTableEntry, PageTableError> {
    if index >= ENTRIES_PER_TABLE {
        return Err(PageTableError::InvalidAddress);
    }
    let page = state.pages[table_idx]
        .as_mut()
        .ok_or(PageTableError::InvalidAddress)?;
    let ptr = phys_to_virt_table_ptr(page.phys);
    if ptr.is_null() {
        return Ok(page.entries[index]);
    }
    let raw = unsafe { core::ptr::read_volatile(ptr.add(index)) };
    let entry = PageTableEntry(raw);
    page.entries[index] = entry;
    Ok(entry)
}

fn write_table_entry(
    state: &mut PageTableState,
    table_idx: usize,
    index: usize,
    entry: PageTableEntry,
) -> Result<(), PageTableError> {
    if index >= ENTRIES_PER_TABLE {
        return Err(PageTableError::InvalidAddress);
    }
    let page = state.pages[table_idx]
        .as_mut()
        .ok_or(PageTableError::InvalidAddress)?;
    page.entries[index] = entry;
    let ptr = phys_to_virt_table_ptr(page.phys);
    if !ptr.is_null() {
        unsafe {
            core::ptr::write_volatile(ptr.add(index), entry.0);
        }
    }
    Ok(())
}

#[cfg(all(not(feature = "hosted-dev"), not(test), target_arch = "aarch64"))]
#[inline]
fn current_ttbr0_root_phys() -> u64 {
    let ttbr0: u64;
    unsafe {
        core::arch::asm!("mrs {0}, ttbr0_el1", out(reg) ttbr0, options(nostack, preserves_flags));
    }
    ttbr0 & PAGE_MASK
}

#[cfg(any(feature = "hosted-dev", test, not(target_arch = "aarch64")))]
#[inline]
fn current_ttbr0_root_phys() -> u64 {
    0
}

fn copy_bootstrap_kernel_root_entries(
    state: &mut PageTableState,
    dst_root_idx: usize,
) -> Result<(), PageTableError> {
    let src_root_phys = current_ttbr0_root_phys();
    if src_root_phys == 0 {
        return Ok(());
    }
    let src_ptr = phys_to_virt_table_ptr(src_root_phys);
    if src_ptr.is_null() {
        return Ok(());
    }
    for idx in 1..ENTRIES_PER_TABLE {
        let raw = unsafe { core::ptr::read_volatile(src_ptr.add(idx)) };
        if raw != 0 {
            write_table_entry(state, dst_root_idx, idx, PageTableEntry(raw))?;
        }
    }
    Ok(())
}

fn ensure_early_uart_mapping(
    state: &mut PageTableState,
    root_idx: usize,
) -> Result<(), PageTableError> {
    let root_phys = state.pages[root_idx]
        .as_ref()
        .ok_or(PageTableError::InvalidAddress)?
        .phys;
    let l1 = level_index(EARLY_UART_MMIO_VA, 30);
    let l2 = level_index(EARLY_UART_MMIO_VA, 21);
    let l3 = level_index(EARLY_UART_MMIO_VA, 12);
    let l2_phys = walk_or_create(state, root_phys, l1, PageFlags::KERNEL_RW)?;
    let l3_phys = walk_or_create(state, l2_phys, l2, PageFlags::KERNEL_RW)?;
    let l3_idx = state
        .page_index_from_phys(l3_phys)
        .ok_or(PageTableError::InvalidAddress)?;
    write_table_entry(
        state,
        l3_idx,
        l3,
        PageTableEntry::with_addr_and_flags(
            EARLY_UART_MMIO_PA,
            leaf_flags_from_page_flags(PageFlags::DEVICE_RW),
        ),
    )
}

#[cfg(all(not(feature = "hosted-dev"), not(test), target_arch = "aarch64"))]
#[inline]
fn raw_uart_marker(tag: u8) {
    const UART_BASE: usize = 0x0900_0000;
    const PL011_DR: usize = 0x000;
    const PL011_FR: usize = 0x018;
    const PL011_FR_TXFF: u32 = 1 << 5;
    unsafe {
        while core::ptr::read_volatile((UART_BASE + PL011_FR) as *const u32) & PL011_FR_TXFF != 0 {}
        core::ptr::write_volatile((UART_BASE + PL011_DR) as *mut u32, tag as u32);
    }
}

#[cfg(any(feature = "hosted-dev", test, not(target_arch = "aarch64")))]
#[inline]
fn raw_uart_marker(_tag: u8) {}

static PAGE_TABLE_STATE: SpinLockIrq<PageTableState> = SpinLockIrq::new(PageTableState::new());

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
    let entry = read_table_entry(state, table_idx, index)?;
    if entry.is_present() {
        return Ok(entry.addr());
    }
    let child_idx = state.alloc_page()?;
    let child_phys = state.pages[child_idx].expect("child").phys;
    write_table_entry(
        state,
        table_idx,
        index,
        PageTableEntry::with_addr_and_flags(child_phys, table_flags_from_page_flags(flags)),
    )?;
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
                    let mut entries = [PageTableEntry::empty(); ENTRIES_PER_TABLE];
                    for (idx, entry) in entries.iter_mut().enumerate() {
                        *entry = read_table_entry(&mut state, table_idx, idx)
                            .unwrap_or(PageTableEntry::empty());
                    }
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
    Some((root & PAGE_MASK) | (asid_bits << 48))
}

pub fn activate_asid(asid: Asid) -> Result<u64, PageTableError> {
    #[cfg(not(feature = "hosted-dev"))]
    crate::yarm_log!("ASW0 before computing TTBR0 asid={}", asid.0);
    let ttbr0 = cr3_for_asid(asid).ok_or(PageTableError::OutOfMemory)?;
    #[cfg(not(feature = "hosted-dev"))]
    crate::yarm_log!("ASW1 after computing TTBR0 asid={} ttbr0=0x{:x}", asid.0, ttbr0);
    #[cfg(not(feature = "hosted-dev"))]
    unsafe {
        let sp: u64;
        core::arch::asm!("mov {0}, sp", out(reg) sp, options(nostack, preserves_flags));
        let pc_sym = activate_asid as usize as u64;
        let asw3_msg_ptr = b"ASW3_RAW".as_ptr() as u64;
        crate::yarm_log!(
            "ASW1V switch_va_snapshot asid={} pc_sym=0x{:x} sp=0x{:x} asw3_msg_ptr=0x{:x}",
            asid.0,
            pc_sym,
            sp,
            asw3_msg_ptr
        );
        crate::yarm_log!("ASW2 before msr ttbr0_el1 asid={} ttbr0=0x{:x}", asid.0, ttbr0);
        core::arch::asm!(
            "msr ttbr0_el1, {value}",
            value = in(reg) ttbr0,
            options(nostack, preserves_flags)
        );
        raw_uart_marker(b'3');
        crate::yarm_log!("ASW3 immediately after msr ttbr0_el1 asid={}", asid.0);
        core::arch::asm!("dsb ish", "isb", options(nostack, preserves_flags));
        raw_uart_marker(b'4');
        crate::yarm_log!("ASW4 after barriers/isb asid={}", asid.0);
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
    let l1 = level_index(virt.0, 30);
    let l2 = level_index(virt.0, 21);
    let l3 = level_index(virt.0, 12);

    let next1 = walk_or_create(&mut state, root, l1, flags)?;
    let next2 = walk_or_create(&mut state, next1, l2, flags)?;

    let leaf_idx = state
        .page_index_from_phys(next2)
        .ok_or(PageTableError::InvalidAddress)?;
    let prev = read_table_entry(&mut state, leaf_idx, l3)?;
    write_table_entry(
        &mut state,
        leaf_idx,
        l3,
        PageTableEntry::with_addr_and_flags(phys.0, leaf_flags_from_page_flags(flags)),
    )?;
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
        let entry = read_table_entry(&mut state, idx, level).ok()?;
        if !entry.is_present() {
            return None;
        }
        table_phys = entry.addr();
    }

    let leaf_idx = state.page_index_from_phys(table_phys)?;
    let old = read_table_entry(&mut state, leaf_idx, levels[2]).ok()?;
    if !old.is_present() {
        return None;
    }
    write_table_entry(&mut state, leaf_idx, levels[2], PageTableEntry::empty()).ok()?;
    drop(state);
    invalidate_page(virt);
    Some(old)
}

pub fn resolve_page(asid: Asid, virt: VirtAddr) -> Option<PageTableEntry> {
    let mut state = PAGE_TABLE_STATE.lock();
    let mut table_phys = state.root_for_asid(asid)?;
    let levels = [
        level_index(virt.0, 30),
        level_index(virt.0, 21),
        level_index(virt.0, 12),
    ];

    for &level in &levels[..2] {
        let idx = state.page_index_from_phys(table_phys)?;
        let entry = read_table_entry(&mut state, idx, level).ok()?;
        if !entry.is_present() {
            return None;
        }
        table_phys = entry.addr();
    }

    let leaf_idx = state.page_index_from_phys(table_phys)?;
    let entry = read_table_entry(&mut state, leaf_idx, levels[2]).ok()?;
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
