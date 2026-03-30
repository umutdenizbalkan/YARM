use crate::arch::x86_64::vm_layout;
use crate::kernel::lock::SpinLock;
use crate::kernel::vm::{Asid, PageFlags, PhysAddr, VirtAddr};

const ENTRIES_PER_TABLE: usize = 512;
const PAGE_SHIFT: u64 = 12;
const PAGE_SIZE_U64: u64 = vm_layout::PAGE_SIZE as u64;
const PAGE_MASK: u64 = !(PAGE_SIZE_U64 - 1);
const PTE_ADDR_MASK: u64 = 0x000f_ffff_ffff_f000;
const PT_POOL_BASE: u64 = 0x0010_0000;
const MAX_PT_PAGES: usize = vm_layout::MAX_ADDRESS_SPACES * 64;
const MAX_ASID_ROOTS: usize = vm_layout::MAX_ADDRESS_SPACES * 64;

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
struct AsidCr3 {
    asid: Asid,
    root_phys: u64,
}

struct PageTableState {
    pages: [Option<PageTablePage>; MAX_PT_PAGES],
    asids: [Option<AsidCr3>; MAX_ASID_ROOTS],
}

impl PageTableState {
    const fn new() -> Self {
        Self {
            pages: [const { None }; MAX_PT_PAGES],
            asids: [const { None }; MAX_ASID_ROOTS],
        }
    }

    fn page_index_from_phys(&self, phys: u64) -> Option<usize> {
        if phys < PT_POOL_BASE {
            return None;
        }
        let index = ((phys - PT_POOL_BASE) >> PAGE_SHIFT) as usize;
        self.pages.get(index)?.as_ref()?;
        Some(index)
    }

    fn alloc_page(&mut self) -> Result<usize, PageTableError> {
        for (idx, slot) in self.pages.iter_mut().enumerate() {
            if slot.is_none() {
                let phys = PT_POOL_BASE + ((idx as u64) << PAGE_SHIFT);
                *slot = Some(PageTablePage::new(phys));
                return Ok(idx);
            }
        }
        Err(PageTableError::OutOfMemory)
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

    fn ensure_asid(&mut self, asid: Asid) -> Result<u64, PageTableError> {
        if let Some(root) = self.asid_root_phys(asid) {
            return Ok(root);
        }

        let root_idx = self.alloc_page()?;
        let root_phys = self.pages[root_idx].expect("root page").phys;

        for slot in &mut self.asids {
            if slot.is_none() {
                *slot = Some(AsidCr3 { asid, root_phys });
                return Ok(root_phys);
            }
        }

        Err(PageTableError::OutOfMemory)
    }

    fn remove_asid(&mut self, asid: Asid) {
        if let Some(slot) = self.find_asid_slot(asid) {
            self.asids[slot] = None;
        }
    }
}

static PAGE_TABLE_STATE: SpinLock<PageTableState> = SpinLock::new(PageTableState::new());

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
    bits
}

fn walk_or_create_table(
    state: &mut PageTableState,
    table_phys: u64,
    index: usize,
    flags: PageFlags,
) -> Result<u64, PageTableError> {
    let table_idx = state
        .page_index_from_phys(table_phys)
        .ok_or(PageTableError::InvalidAddress)?;
    let entry = state.pages[table_idx].as_ref().expect("table page").entries[index];
    if entry.is_present() {
        return Ok(entry.addr());
    }

    let child_idx = state.alloc_page()?;
    let child_phys = state.pages[child_idx].expect("child page").phys;
    state.pages[table_idx].as_mut().expect("table page").entries[index] =
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
    state.remove_asid(asid);
}

pub fn cr3_for_asid(asid: Asid) -> Option<u64> {
    let mut state = PAGE_TABLE_STATE.lock();
    let root_phys = state.ensure_asid(asid).ok()?;
    let pcid = (asid.0 as u64) & 0x0fff;
    Some((root_phys & PAGE_MASK) | pcid)
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

    let pt_idx = state
        .page_index_from_phys(pt_phys)
        .ok_or(PageTableError::InvalidAddress)?;
    let table = state.pages[pt_idx].as_mut().expect("pt page");
    let previous = table.entries[l1];
    table.entries[l1] = PageTableEntry::with_addr_and_flags(phys.0, leaf_flags_from_page_flags(flags));
    drop(state);
    invalidate_page(virt);
    Ok(previous.is_present().then_some(previous))
}

pub fn unmap_page(asid: Asid, virt: VirtAddr) -> Option<PageTableEntry> {
    let mut state = PAGE_TABLE_STATE.lock();
    let root_phys = state.asid_root_phys(asid)?;

    let levels = [pml4_index(virt.0), pdpt_index(virt.0), pd_index(virt.0), pt_index(virt.0)];
    let mut table_phys = root_phys;
    for &level in &levels[..3] {
        let idx = state.page_index_from_phys(table_phys)?;
        let entry = state.pages[idx].as_ref()?.entries[level];
        if !entry.is_present() {
            return None;
        }
        table_phys = entry.addr();
    }

    let pt_idx = state.page_index_from_phys(table_phys)?;
    let table = state.pages[pt_idx].as_mut()?;
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
    let root_phys = state.asid_root_phys(asid)?;
    let levels = [pml4_index(virt.0), pdpt_index(virt.0), pd_index(virt.0), pt_index(virt.0)];
    let mut table_phys = root_phys;
    for &level in &levels[..3] {
        let idx = state.page_index_from_phys(table_phys)?;
        let entry = state.pages[idx].as_ref()?.entries[level];
        if !entry.is_present() {
            return None;
        }
        table_phys = entry.addr();
    }
    let pt_idx = state.page_index_from_phys(table_phys)?;
    let entry = state.pages[pt_idx].as_ref()?.entries[levels[3]];
    entry.is_present().then_some(entry)
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

pub fn invalidate_asid(asid: Asid) {
    #[cfg(feature = "hosted-dev")]
    {
        let _ = asid;
    }

    #[cfg(not(feature = "hosted-dev"))]
    unsafe {
        let descriptor = InvpcidDescriptor {
            pcid: (asid.0 as u64) & 0x0fff,
            addr: 0,
        };
        core::arch::asm!(
            "invpcid {kind:r}, [{desc}]",
            kind = in(reg) 1u64,
            desc = in(reg) &descriptor,
            options(nostack, preserves_flags)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_and_resolve_4_level_page() {
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
        let asid = Asid(12);
        ensure_asid_root(asid).expect("root");
        let va = VirtAddr(0x4000_0000);
        map_page(asid, va, PhysAddr(0x2000_0000), PageFlags::USER_RX).expect("map");
        assert!(unmap_page(asid, va).is_some());
        assert!(resolve_page(asid, va).is_none());
    }

    #[test]
    fn cr3_includes_low_pcid_bits() {
        let asid = Asid(0x1234);
        let cr3 = cr3_for_asid(asid).expect("cr3");
        assert_eq!(cr3 & 0x0fff, 0x234);
    }
}
