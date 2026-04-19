// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#[cfg(all(not(feature = "hosted-dev"), not(test)))]
use crate::arch::x86_64::platform_layout;
use crate::arch::x86_64::vm_layout;
use crate::kernel::frame_allocator::{alloc_pt_frame, free_pt_frame};
use crate::kernel::lock::SpinLockIrq;
use crate::kernel::vm::{Asid, CachePolicy, PageFlags, PhysAddr, VirtAddr};
#[cfg(not(feature = "hosted-dev"))]
use core::sync::atomic::{AtomicU8, Ordering};

const ENTRIES_PER_TABLE: usize = 512;
const PAGE_SIZE_U64: u64 = vm_layout::PAGE_SIZE as u64;
const PAGE_MASK: u64 = !(PAGE_SIZE_U64 - 1);
const PTE_ADDR_MASK: u64 = 0x000f_ffff_ffff_f000;
const PCID_MASK: u16 = 0x0fff;
const MAX_PCID: u16 = PCID_MASK;
const INTERMEDIATE_PT_PAGES_PER_MAPPING: usize = 4;
const MAX_PT_PAGES: usize = vm_layout::MAX_ADDRESS_SPACES
    * (1 + vm_layout::MAX_MAPPINGS * INTERMEDIATE_PT_PAGES_PER_MAPPING);
const MAX_ASID_ROOTS: usize = vm_layout::MAX_ADDRESS_SPACES * 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageTableEntry(pub u64);

impl PageTableEntry {
    pub const PRESENT: u64 = 1 << 0;
    pub const WRITABLE: u64 = 1 << 1;
    pub const USER: u64 = 1 << 2;
    pub const WRITE_THROUGH: u64 = 1 << 3;
    pub const CACHE_DISABLE: u64 = 1 << 4;
    pub const ACCESSED: u64 = 1 << 5;
    pub const DIRTY: u64 = 1 << 6;
    pub const HUGE_PAGE: u64 = 1 << 7;
    pub const GLOBAL: u64 = 1 << 8;
    pub const NO_EXECUTE: u64 = 1 << 63;

    pub const fn empty() -> Self {
        Self(0)
    }

    pub const fn is_present(self) -> bool {
        (self.0 & Self::PRESENT) != 0
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
    #[cfg(any(feature = "hosted-dev", test))]
    entries: [u64; ENTRIES_PER_TABLE],
}

impl PageTablePage {
    fn new(phys: u64) -> Self {
        Self {
            phys,
            #[cfg(any(feature = "hosted-dev", test))]
            entries: [0; ENTRIES_PER_TABLE],
        }
    }
}

#[derive(Clone, Copy)]
struct AsidCr3 {
    asid: Asid,
    root_phys: u64,
    pcid: u16,
}

struct PageTableState {
    pages: [Option<PageTablePage>; MAX_PT_PAGES],
    asids: [Option<AsidCr3>; MAX_ASID_ROOTS],
    canonical_kernel_root_phys: Option<u64>,
}

impl PageTableState {
    const fn new() -> Self {
        Self {
            pages: [const { None }; MAX_PT_PAGES],
            asids: [const { None }; MAX_ASID_ROOTS],
            canonical_kernel_root_phys: None,
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
                #[cfg(all(not(feature = "hosted-dev"), not(test)))]
                unsafe {
                    let page_ptr =
                        phys_to_virt_table_ptr(phys).ok_or(PageTableError::InvalidAddress)?;
                    core::ptr::write_bytes(page_ptr as *mut u8, 0, vm_layout::PAGE_SIZE);
                }
                *slot = Some(PageTablePage::new(phys));
                return Ok(idx);
            }
        }
        Err(PageTableError::OutOfMemory)
    }

    fn free_table_hierarchy(&mut self, root_phys: u64, levels: usize) {
        let mut stack: [(u64, usize); MAX_PT_PAGES] = [(0, 0); MAX_PT_PAGES];
        let mut sp = 0usize;
        stack[sp] = (root_phys, levels);
        sp += 1;

        while sp > 0 {
            sp -= 1;
            let (table_phys, level) = stack[sp];
            let Some(table_idx) = self.page_index_from_phys(table_phys) else {
                continue;
            };

            if level > 1 {
                for entry_idx in 0..ENTRIES_PER_TABLE {
                    let entry = read_table_entry(self, table_phys, entry_idx)
                        .unwrap_or(PageTableEntry::empty());
                    if !entry.is_present() {
                        continue;
                    }
                    let child_phys = entry.addr();
                    if self.page_index_from_phys(child_phys).is_some() && sp < MAX_PT_PAGES {
                        stack[sp] = (child_phys, level - 1);
                        sp += 1;
                    }
                }
            }

            if let Some(page) = self.pages[table_idx].take() {
                let _ = free_pt_frame(page.phys);
            }
        }
    }

    fn find_asid_slot(&self, asid: Asid) -> Option<usize> {
        self.asids
            .iter()
            .position(|slot| slot.is_some_and(|entry| entry.asid == asid))
    }

    fn asid_root_phys(&self, asid: Asid) -> Option<u64> {
        self.find_asid_slot(asid)
            .and_then(|slot| self.asids[slot].map(|entry| entry.root_phys))
    }

    fn asid_pcid(&self, asid: Asid) -> Option<u16> {
        self.find_asid_slot(asid)
            .and_then(|slot| self.asids[slot].map(|entry| entry.pcid))
    }

    fn pcid_in_use(&self, pcid: u16) -> bool {
        self.asids.iter().flatten().any(|entry| entry.pcid == pcid)
    }

    fn allocate_pcid(&self, asid: Asid) -> Result<u16, PageTableError> {
        let preferred = asid.0 & PCID_MASK;
        if preferred != 0 && !self.pcid_in_use(preferred) {
            return Ok(preferred);
        }

        for candidate in 1..=MAX_PCID {
            if !self.pcid_in_use(candidate) {
                return Ok(candidate);
            }
        }

        Err(PageTableError::OutOfMemory)
    }

    fn ensure_asid(&mut self, asid: Asid) -> Result<u64, PageTableError> {
        if let Some(root) = self.asid_root_phys(asid) {
            return Ok(root);
        }

        let root_idx = self.alloc_page()?;
        let root_phys = self.pages[root_idx].expect("root page").phys;
        let canonical_kernel_root_phys =
            if let Some(existing) = self.canonical_kernel_root_phys {
                existing
            } else {
                let detected = detect_active_root_phys_from_cr3()?;
                self.canonical_kernel_root_phys = Some(detected);
                detected
            };
        clone_kernel_pml4_half_into_root(canonical_kernel_root_phys, root_phys)?;
        let pcid = self.allocate_pcid(asid)?;

        for slot in &mut self.asids {
            if slot.is_none() {
                *slot = Some(AsidCr3 {
                    asid,
                    root_phys,
                    pcid,
                });
                return Ok(root_phys);
            }
        }

        Err(PageTableError::OutOfMemory)
    }

    fn remove_asid(&mut self, asid: Asid) {
        if let Some(slot) = self.find_asid_slot(asid) {
            if let Some(root) = self.asids[slot] {
                self.free_table_hierarchy(root.root_phys, 4);
            }
            self.asids[slot] = None;
        }
    }
}

#[cfg(all(not(feature = "hosted-dev"), not(test)))]
fn clone_kernel_pml4_half_into_root(
    source_root_phys: u64,
    dest_root_phys: u64,
) -> Result<(), PageTableError> {
    let kernel_l4_base = pml4_index(vm_layout::KERNEL_SPACE_BASE);
    for idx in kernel_l4_base..ENTRIES_PER_TABLE {
        let entry = unsafe { read_raw_table_entry(source_root_phys, idx)? };
        unsafe { write_raw_table_entry(dest_root_phys, idx, entry)? };
    }
    Ok(())
}

#[cfg(any(feature = "hosted-dev", test))]
fn clone_kernel_pml4_half_into_root(
    _source_root_phys: u64,
    _dest_root_phys: u64,
) -> Result<(), PageTableError> {
    Ok(())
}

#[cfg(all(not(feature = "hosted-dev"), not(test)))]
fn detect_active_root_phys_from_cr3() -> Result<u64, PageTableError> {
    let mut active_cr3: u64;
    unsafe {
        core::arch::asm!("mov {}, cr3", out(reg) active_cr3, options(nostack, preserves_flags));
    }
    Ok(active_cr3 & PAGE_MASK)
}

#[cfg(any(feature = "hosted-dev", test))]
fn detect_active_root_phys_from_cr3() -> Result<u64, PageTableError> {
    Ok(0)
}

#[cfg(all(not(feature = "hosted-dev"), not(test)))]
fn pcide_enabled() -> bool {
    let mut cr4: u64 = 0;
    unsafe {
        core::arch::asm!("mov {}, cr4", out(reg) cr4, options(nostack, preserves_flags));
    }
    (cr4 & (1 << 17)) != 0
}

#[cfg(any(feature = "hosted-dev", test))]
fn pcide_enabled() -> bool {
    true
}

static PAGE_TABLE_STATE: SpinLockIrq<PageTableState> = SpinLockIrq::new(PageTableState::new());
#[cfg(test)]
static LAST_INVALIDATED_ASID: crate::kernel::lock::SpinLock<Option<Asid>> =
    crate::kernel::lock::SpinLock::new(None);
#[cfg(test)]
static PAGE_TABLE_TEST_LOCK: crate::kernel::lock::SpinLock<()> =
    crate::kernel::lock::SpinLock::new(());

pub fn reset_state() {
    let mut state = PAGE_TABLE_STATE.lock();
    for page in &mut state.pages {
        *page = None;
    }
    for asid in &mut state.asids {
        *asid = None;
    }
    state.canonical_kernel_root_phys = None;
}

fn pml4_index(va: u64) -> usize {
    ((va >> 39) & 0x1ff) as usize
}

fn pdpt_index(va: u64) -> usize {
    ((va >> 30) & 0x1ff) as usize
}

fn pd_index(va: u64) -> usize {
    ((va >> 21) & 0x1ff) as usize
}

fn pt_index(va: u64) -> usize {
    ((va >> 12) & 0x1ff) as usize
}

fn table_flags_from_page_flags(flags: PageFlags) -> u64 {
    let mut bits = PageTableEntry::PRESENT | PageTableEntry::WRITABLE;
    if flags.user {
        bits |= PageTableEntry::USER;
    }
    bits |= cache_policy_bits(flags.cache_policy);
    bits
}

fn leaf_flags_from_page_flags(flags: PageFlags) -> u64 {
    let mut bits = PageTableEntry::PRESENT;
    if flags.write {
        bits |= PageTableEntry::WRITABLE;
    }
    if flags.user {
        bits |= PageTableEntry::USER;
    }
    if !flags.execute {
        bits |= PageTableEntry::NO_EXECUTE;
    }
    bits |= cache_policy_bits(flags.cache_policy);
    bits
}

fn cache_policy_bits(policy: CachePolicy) -> u64 {
    match policy {
        CachePolicy::WriteBack => 0,
        CachePolicy::WriteThrough => PageTableEntry::WRITE_THROUGH,
        CachePolicy::Uncached | CachePolicy::Device => PageTableEntry::CACHE_DISABLE,
    }
}

fn walk_or_create_table(
    state: &mut PageTableState,
    table_phys: u64,
    index: usize,
    flags: PageFlags,
) -> Result<u64, PageTableError> {
    let entry = read_table_entry(state, table_phys, index).ok_or(PageTableError::InvalidAddress)?;
    if entry.is_present() {
        return Ok(entry.addr());
    }

    let child_idx = state.alloc_page()?;
    let child_phys = state.pages[child_idx].expect("child page").phys;
    write_table_entry(
        state,
        table_phys,
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
    state.remove_asid(asid);
}

pub fn cr3_for_asid(asid: Asid) -> Option<u64> {
    let mut state = PAGE_TABLE_STATE.lock();
    let root_phys = state.ensure_asid(asid).ok()?;
    if pcide_enabled() {
        // x86_64 PCID is 12 bits; software ASID is wider. Keep an explicit
        // per-ASID PCID assignment so simultaneously-live ASIDs never alias.
        let pcid = state.asid_pcid(asid)? as u64;
        Some((root_phys & PAGE_MASK) | pcid)
    } else {
        // CR4.PCIDE is not enabled; CR3 low bits must remain clear (except
        // legacy PWT/PCD), so do not encode software ASID in CR3.
        Some(root_phys & PAGE_MASK)
    }
}

pub fn activate_asid(asid: Asid) -> Result<u64, PageTableError> {
    let cr3 = cr3_for_asid(asid).ok_or(PageTableError::OutOfMemory)?;
    #[cfg(not(feature = "hosted-dev"))]
    unsafe {
        core::arch::asm!("mov cr3, {}", in(reg) cr3, options(nostack, preserves_flags));
    }
    Ok(cr3)
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
    let root_phys = state.ensure_asid(asid)?;
    let l4 = pml4_index(virt.0);
    let l3 = pdpt_index(virt.0);
    let l2 = pd_index(virt.0);
    let l1 = pt_index(virt.0);

    let pdpt_phys = walk_or_create_table(&mut state, root_phys, l4, flags)?;
    let pd_phys = walk_or_create_table(&mut state, pdpt_phys, l3, flags)?;
    let pt_phys = walk_or_create_table(&mut state, pd_phys, l2, flags)?;

    let previous = read_table_entry(&state, pt_phys, l1).ok_or(PageTableError::InvalidAddress)?;
    write_table_entry(
        &mut state,
        pt_phys,
        l1,
        PageTableEntry::with_addr_and_flags(phys.0, leaf_flags_from_page_flags(flags)),
    )?;
    drop(state);
    invalidate_page(virt);
    Ok(previous.is_present().then_some(previous))
}

pub fn unmap_page(asid: Asid, virt: VirtAddr) -> Option<PageTableEntry> {
    let mut state = PAGE_TABLE_STATE.lock();
    let root_phys = state.asid_root_phys(asid)?;

    let levels = [
        pml4_index(virt.0),
        pdpt_index(virt.0),
        pd_index(virt.0),
        pt_index(virt.0),
    ];
    let mut table_phys = root_phys;
    for &level in &levels[..3] {
        let entry = read_table_entry(&state, table_phys, level)?;
        if !entry.is_present() {
            return None;
        }
        table_phys = entry.addr();
    }

    let old = read_table_entry(&state, table_phys, levels[3])?;
    if !old.is_present() {
        return None;
    }
    if write_table_entry(&mut state, table_phys, levels[3], PageTableEntry::empty()).is_err() {
        return None;
    }
    drop(state);
    invalidate_page(virt);
    Some(old)
}

pub fn resolve_page(asid: Asid, virt: VirtAddr) -> Option<PageTableEntry> {
    let state = PAGE_TABLE_STATE.lock();
    let root_phys = state.asid_root_phys(asid)?;
    let levels = [
        pml4_index(virt.0),
        pdpt_index(virt.0),
        pd_index(virt.0),
        pt_index(virt.0),
    ];
    let mut table_phys = root_phys;
    for &level in &levels[..3] {
        let entry = read_table_entry(&state, table_phys, level)?;
        if !entry.is_present() {
            return None;
        }
        table_phys = entry.addr();
    }
    let entry = read_table_entry(&state, table_phys, levels[3])?;
    entry.is_present().then_some(entry)
}

fn read_table_entry(
    state: &PageTableState,
    table_phys: u64,
    index: usize,
) -> Option<PageTableEntry> {
    if index >= ENTRIES_PER_TABLE {
        return None;
    }
    #[cfg(any(feature = "hosted-dev", test))]
    {
        let table_idx = state.page_index_from_phys(table_phys)?;
        let entry = state.pages[table_idx].as_ref()?.entries[index];
        return Some(PageTableEntry(entry));
    }
    #[cfg(all(not(feature = "hosted-dev"), not(test)))]
    unsafe {
        state.page_index_from_phys(table_phys)?;
        let table_ptr = phys_to_virt_table_ptr(table_phys)?;
        let ptr = (table_ptr as usize + index * core::mem::size_of::<u64>()) as *const u64;
        Some(PageTableEntry(core::ptr::read_volatile(ptr)))
    }
}

fn write_table_entry(
    state: &mut PageTableState,
    table_phys: u64,
    index: usize,
    entry: PageTableEntry,
) -> Result<(), PageTableError> {
    if index >= ENTRIES_PER_TABLE {
        return Err(PageTableError::InvalidAddress);
    }
    #[cfg(any(feature = "hosted-dev", test))]
    {
        let table_idx = state
            .page_index_from_phys(table_phys)
            .ok_or(PageTableError::InvalidAddress)?;
        if let Some(table) = state.pages[table_idx].as_mut() {
            table.entries[index] = entry.0;
            return Ok(());
        }
        return Err(PageTableError::InvalidAddress);
    }
    #[cfg(all(not(feature = "hosted-dev"), not(test)))]
    {
        state
            .page_index_from_phys(table_phys)
            .ok_or(PageTableError::InvalidAddress)?;
        unsafe {
            let table_ptr =
                phys_to_virt_table_ptr(table_phys).ok_or(PageTableError::InvalidAddress)?;
            let ptr = (table_ptr as usize + index * core::mem::size_of::<u64>()) as *mut u64;
            core::ptr::write_volatile(ptr, entry.0);
        }
        Ok(())
    }
}

#[cfg(all(not(feature = "hosted-dev"), not(test)))]
fn phys_to_virt_table_ptr(table_phys: u64) -> Option<*mut u8> {
    let phys_off = table_phys.checked_sub(platform_layout::KERNEL_BOOTSTRAP_PHYS_BASE)?;
    if phys_off >= platform_layout::KERNEL_PHYS_DIRECT_MAP_BYTES {
        return None;
    }
    let virt = platform_layout::KERNEL_BOOTSTRAP_VIRT_BASE.checked_add(phys_off)?;
    Some(virt as usize as *mut u8)
}

#[cfg(all(not(feature = "hosted-dev"), not(test)))]
unsafe fn read_raw_table_entry(table_phys: u64, index: usize) -> Result<PageTableEntry, PageTableError> {
    if index >= ENTRIES_PER_TABLE {
        return Err(PageTableError::InvalidAddress);
    }
    let table_ptr = phys_to_virt_table_ptr(table_phys).ok_or(PageTableError::InvalidAddress)?;
    let ptr = (table_ptr as usize + index * core::mem::size_of::<u64>()) as *const u64;
    Ok(PageTableEntry(unsafe { core::ptr::read_volatile(ptr) }))
}

#[cfg(all(not(feature = "hosted-dev"), not(test)))]
unsafe fn write_raw_table_entry(
    table_phys: u64,
    index: usize,
    entry: PageTableEntry,
) -> Result<(), PageTableError> {
    if index >= ENTRIES_PER_TABLE {
        return Err(PageTableError::InvalidAddress);
    }
    let table_ptr = phys_to_virt_table_ptr(table_phys).ok_or(PageTableError::InvalidAddress)?;
    let ptr = (table_ptr as usize + index * core::mem::size_of::<u64>()) as *mut u64;
    unsafe { core::ptr::write_volatile(ptr, entry.0) };
    Ok(())
}

pub fn invalidate_page(virt: VirtAddr) {
    #[cfg(feature = "hosted-dev")]
    {
        let _ = virt;
    }

    #[cfg(not(feature = "hosted-dev"))]
    unsafe {
        core::arch::asm!("invlpg [{addr}]", addr = in(reg) virt.0 as usize, options(nostack, preserves_flags));
    }
}

#[cfg(not(feature = "hosted-dev"))]
#[repr(C, align(16))]
struct InvpcidDescriptor {
    pcid: u64,
    addr: u64,
}

#[cfg(not(feature = "hosted-dev"))]
const INVPCID_SUPPORT_UNKNOWN: u8 = 0;
#[cfg(not(feature = "hosted-dev"))]
const INVPCID_SUPPORT_AVAILABLE: u8 = 1;
#[cfg(not(feature = "hosted-dev"))]
const INVPCID_SUPPORT_UNAVAILABLE: u8 = 2;
#[cfg(not(feature = "hosted-dev"))]
static INVPCID_SUPPORT: AtomicU8 = AtomicU8::new(INVPCID_SUPPORT_UNKNOWN);

#[cfg(not(feature = "hosted-dev"))]
#[allow(unused_unsafe)]
fn cpu_supports_invpcid() -> bool {
    match INVPCID_SUPPORT.load(Ordering::Relaxed) {
        INVPCID_SUPPORT_AVAILABLE => true,
        INVPCID_SUPPORT_UNAVAILABLE => false,
        _ => {
            let max_leaf = unsafe { core::arch::x86_64::__cpuid(0) }.eax;
            let supported = if max_leaf >= 7 {
                let leaf7 = unsafe { core::arch::x86_64::__cpuid_count(7, 0) };
                (leaf7.ebx & (1 << 10)) != 0
            } else {
                false
            };
            INVPCID_SUPPORT.store(
                if supported {
                    INVPCID_SUPPORT_AVAILABLE
                } else {
                    INVPCID_SUPPORT_UNAVAILABLE
                },
                Ordering::Relaxed,
            );
            supported
        }
    }
}

#[cfg(not(feature = "hosted-dev"))]
unsafe fn fallback_flush_tlb_via_cr3() {
    let mut cr3: u64;
    unsafe {
        core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nostack, preserves_flags));
    }
    // Clear the no-flush bit (bit 63) to force an architectural flush.
    let flushed_cr3 = cr3 & !(1u64 << 63);
    unsafe {
        core::arch::asm!("mov cr3, {}", in(reg) flushed_cr3, options(nostack, preserves_flags));
    }
}

pub fn invalidate_asid(asid: Asid) {
    #[cfg(test)]
    {
        *LAST_INVALIDATED_ASID.lock() = Some(asid);
    }

    #[cfg(feature = "hosted-dev")]
    {
        let _ = asid;
    }

    #[cfg(not(feature = "hosted-dev"))]
    unsafe {
        if cpu_supports_invpcid() {
            let pcid = {
                let state = PAGE_TABLE_STATE.lock();
                state.asid_pcid(asid).unwrap_or_else(|| asid.0 & PCID_MASK) as u64
            };
            let descriptor = InvpcidDescriptor { pcid, addr: 0 };
            core::arch::asm!(
                "invpcid {kind:r}, [{desc}]",
                kind = in(reg) 1u64,
                desc = in(reg) &descriptor,
                options(nostack, preserves_flags)
            );
        } else {
            fallback_flush_tlb_via_cr3();
        }
    }
}

#[cfg(test)]
pub fn take_last_invalidated_asid_for_test() -> Option<Asid> {
    LAST_INVALIDATED_ASID.lock().take()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_and_resolve_4_level_page() {
        let _guard = PAGE_TABLE_TEST_LOCK.lock();
        reset_state();
        let asid = Asid(11);
        ensure_asid_root(asid).expect("root");
        let va = VirtAddr(0x0000_7f00_1234_5000);
        let pa = PhysAddr(0x0000_0000_0100_0000);
        map_page(asid, va, pa, PageFlags::USER_RW).expect("map");
        let entry = resolve_page(asid, va).expect("present");
        assert_eq!(entry.addr(), pa.0 & PAGE_MASK);
        assert!(entry.0 & PageTableEntry::USER != 0);
    }

    #[test]
    fn unmap_clears_leaf_entry() {
        let _guard = PAGE_TABLE_TEST_LOCK.lock();
        reset_state();
        let asid = Asid(12);
        ensure_asid_root(asid).expect("root");
        let va = VirtAddr(0x4000_0000);
        map_page(asid, va, PhysAddr(0x2000_0000), PageFlags::USER_RX).expect("map");
        assert!(unmap_page(asid, va).is_some());
        assert!(resolve_page(asid, va).is_none());
    }

    #[test]
    fn cr3_includes_low_pcid_bits() {
        let _guard = PAGE_TABLE_TEST_LOCK.lock();
        reset_state();
        let asid = Asid(0x1234);
        let cr3 = cr3_for_asid(asid).expect("cr3");
        assert_eq!(cr3 & 0x0fff, 0x234);
    }

    #[test]
    fn pcid_remains_unique_when_asid_low_bits_collide() {
        let _guard = PAGE_TABLE_TEST_LOCK.lock();
        reset_state();
        let cr3_a = cr3_for_asid(Asid(1)).expect("cr3 a");
        let cr3_b = cr3_for_asid(Asid(0x1001)).expect("cr3 b");
        assert_ne!(cr3_a & 0x0fff, cr3_b & 0x0fff);
    }

    #[test]
    fn cache_policy_maps_to_leaf_cache_bits() {
        let _guard = PAGE_TABLE_TEST_LOCK.lock();
        reset_state();
        let asid = Asid(13);
        ensure_asid_root(asid).expect("root");
        let va_wt = VirtAddr(0x0000_7f00_2000_0000);
        let va_uc = VirtAddr(0x0000_7f00_2000_1000);

        map_page(
            asid,
            va_wt,
            PhysAddr(0x3000_0000),
            PageFlags {
                cache_policy: CachePolicy::WriteThrough,
                ..PageFlags::USER_RW
            },
        )
        .expect("map wt");
        map_page(
            asid,
            va_uc,
            PhysAddr(0x3000_1000),
            PageFlags {
                cache_policy: CachePolicy::Uncached,
                ..PageFlags::USER_RW
            },
        )
        .expect("map uc");

        let wt_entry = resolve_page(asid, va_wt).expect("wt");
        let uc_entry = resolve_page(asid, va_uc).expect("uc");
        assert!(wt_entry.0 & PageTableEntry::WRITE_THROUGH != 0);
        assert!(uc_entry.0 & PageTableEntry::CACHE_DISABLE != 0);
    }

    #[test]
    fn non_executable_mappings_set_nx_bit() {
        let _guard = PAGE_TABLE_TEST_LOCK.lock();
        reset_state();
        let asid = Asid(14);
        ensure_asid_root(asid).expect("root");
        let va_nx = VirtAddr(0x0000_7f00_3000_0000);
        let va_x = VirtAddr(0x0000_7f00_3000_1000);

        map_page(asid, va_nx, PhysAddr(0x4000_0000), PageFlags::USER_RW).expect("map nx");
        map_page(asid, va_x, PhysAddr(0x4000_1000), PageFlags::USER_RX).expect("map x");

        let nx_entry = resolve_page(asid, va_nx).expect("nx");
        let x_entry = resolve_page(asid, va_x).expect("x");
        assert!(nx_entry.0 & PageTableEntry::NO_EXECUTE != 0);
        assert!(x_entry.0 & PageTableEntry::NO_EXECUTE == 0);
    }
}
