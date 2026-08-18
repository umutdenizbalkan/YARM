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

// ── Runtime GIC MMIO pages, present in EVERY address-space root ──────────────────────────────
//
// `TTBR1_EL1` is always zero on this port: there is no high-half kernel, so all kernel execution
// runs from whichever TTBR0 root is active. `copy_bootstrap_kernel_root_entries` deliberately
// skips L1[0] — the 1 GiB entry covering VA 0..0x3FFF_FFFF — because user code and data live at
// low addresses and blind inheritance would collide with them. Every device the kernel must reach
// from that low window therefore needs its own leaf re-established inside each new root, which is
// exactly what the UART already does.
//
// The GIC lives in the same skipped window (`0x0800_0000` / `0x0801_0000`), and the AArch64 GIC
// helpers address it as an identity-mapped VA (`(base + offset) as *mut u32`). Without these
// leaves any GIC access taken after the first user root is activated — late boot, EL0, or an IRQ
// claim/EOI, none of which switch TTBR — faults on an unmapped address. These statics carry the
// DTB-derived PHYSICAL bases so the mapping is never a guessed constant.
static GIC_DIST_MMIO_PA: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static GIC_CPU_IF_MMIO_PA: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Publish the DTB-derived GIC physical bases for per-root mapping.
///
/// Must run before the first address-space root is created; the AArch64 DTB parse in
/// `prepare_arch_boot` precedes `Bootstrap::init`, so that ordering holds by construction. A base
/// that is zero or not 4 KiB-aligned is REFUSED and leaves the corresponding page unmapped, so an
/// unsupported controller — notably RPi5's GICv3, for which the parser deliberately yields no
/// GICv2 bases — fails closed rather than mapping a guessed address.
pub fn publish_gic_mmio_bases(dist_pa: u64, cpu_if_pa: u64) {
    if dist_pa != 0 && dist_pa.is_multiple_of(PAGE_SIZE_U64) {
        GIC_DIST_MMIO_PA.store(dist_pa, core::sync::atomic::Ordering::Relaxed);
    }
    if cpu_if_pa != 0 && cpu_if_pa.is_multiple_of(PAGE_SIZE_U64) {
        GIC_CPU_IF_MMIO_PA.store(cpu_if_pa, core::sync::atomic::Ordering::Relaxed);
    }
}

/// The GIC physical bases currently published, `0` meaning "not available, do not map".
pub fn gic_mmio_bases() -> (u64, u64) {
    (
        GIC_DIST_MMIO_PA.load(core::sync::atomic::Ordering::Relaxed),
        GIC_CPU_IF_MMIO_PA.load(core::sync::atomic::Ordering::Relaxed),
    )
}

/// Is `va` one of the privileged device leaves every root carries?
///
/// These pages are established at root creation and are not user memory. A user mapping request
/// that targeted one would silently replace a Device/XN kernel leaf with a user page — and, for
/// the GIC, hand userspace the interrupt controller. Such requests are refused.
fn is_reserved_device_va(va: u64) -> bool {
    let page = va & PAGE_MASK;
    if page == EARLY_UART_MMIO_VA {
        return true;
    }
    let (dist, cpu_if) = gic_mmio_bases();
    (dist != 0 && page == dist) || (cpu_if != 0 && page == cpu_if)
}

const AARCH64_ASID_TRACE: bool = false;
macro_rules! asid_trace {
    ($($arg:tt)*) => {
        if AARCH64_ASID_TRACE {
            crate::yarm_log!($($arg)*);
        }
    };
}

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
        ensure_reserved_device_mappings(self, root_idx)?;
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
    // TTBR0_EL1 layout (4KB granule, 48-bit PA):
    //   [63:48] ASID (when TCR_EL1.AS=1) or reserved
    //   [47:1]  base address of the translation table (BADDR)
    //   [0]     CnP
    // PAGE_MASK only clears [11:0], leaving ASID bits in [63:48] in place,
    // which corrupts any pointer derived from the result.  Use PTE_ADDR_MASK
    // (0x0000_ffff_ffff_f000) which strips both the CnP/offset low bits and
    // the ASID high bits.
    let base = ttbr0 & PTE_ADDR_MASK;
    let asid = (ttbr0 >> 48) & 0xffff;
    crate::yarm_log!(
        "AARCH64_TTBR0_DECODE raw=0x{:016x} base=0x{:016x} asid={}",
        ttbr0,
        base,
        asid
    );
    debug_assert_eq!(base & !PTE_ADDR_MASK, 0, "TTBR0 base has unexpected bits");
    base
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

/// Map ONE 4 KiB device page identity (VA == PA) into `root_idx`.
///
/// Generalized from the early-UART mapping so the GIC pages ride the same bounded path. Exactly
/// one page is mapped per call — never the enclosing 2 MiB block and never the whole bootstrap
/// 1 GiB entry — so no device space beyond what the kernel actually touches becomes reachable.
/// The identity relation is required: the AArch64 GIC and UART helpers address these bases as raw
/// pointers, so VA must equal the DTB-derived PA.
///
/// `PageFlags::DEVICE_RW` yields Device-nGnRE (MAIR AttrIdx 3), read/write, execute-never and
/// privileged — `leaf_flags_from_page_flags` omits the USER bit and sets NO_EXECUTE.
///
/// A zero or misaligned base maps nothing and is not an error: the caller is fail-closed by
/// construction, so an unsupported interrupt controller simply has no leaf.
fn ensure_early_device_mapping(
    state: &mut PageTableState,
    root_idx: usize,
    pa: u64,
) -> Result<(), PageTableError> {
    if pa == 0 || !pa.is_multiple_of(PAGE_SIZE_U64) {
        return Ok(());
    }
    let root_phys = state.pages[root_idx]
        .as_ref()
        .ok_or(PageTableError::InvalidAddress)?
        .phys;
    // Identity mapping: the virtual address IS the physical base.
    let va = pa;
    let l1 = level_index(va, 30);
    let l2 = level_index(va, 21);
    let l3 = level_index(va, 12);
    let l2_phys = walk_or_create(state, root_phys, l1, PageFlags::KERNEL_RW)?;
    let l3_phys = walk_or_create(state, l2_phys, l2, PageFlags::KERNEL_RW)?;
    let l3_idx = state
        .page_index_from_phys(l3_phys)
        .ok_or(PageTableError::InvalidAddress)?;
    write_table_entry(
        state,
        l3_idx,
        l3,
        PageTableEntry::with_addr_and_flags(pa, leaf_flags_from_page_flags(PageFlags::DEVICE_RW)),
    )
}

/// Establish every privileged device leaf a fresh root must carry: the early UART, and — when the
/// DTB published GICv2 bases — the GIC distributor and CPU-interface pages.
///
/// Runs at root creation, BEFORE the root can be activated, so no live TLB shootdown is
/// introduced: nothing can have cached a translation for a root that has never been installed.
fn ensure_reserved_device_mappings(
    state: &mut PageTableState,
    root_idx: usize,
) -> Result<(), PageTableError> {
    ensure_early_device_mapping(state, root_idx, EARLY_UART_MMIO_PA)?;
    let (dist_pa, cpu_if_pa) = gic_mmio_bases();
    ensure_early_device_mapping(state, root_idx, dist_pa)?;
    ensure_early_device_mapping(state, root_idx, cpu_if_pa)
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
    asid_trace!("ASW0 before computing TTBR0 asid={}", asid.0);
    let ttbr0 = cr3_for_asid(asid).ok_or(PageTableError::OutOfMemory)?;
    #[cfg(not(feature = "hosted-dev"))]
    crate::yarm_log!(
        "ASW1 after computing TTBR0 asid={} ttbr0=0x{:x}",
        asid.0,
        ttbr0
    );
    #[cfg(not(feature = "hosted-dev"))]
    unsafe {
        let mut current_ttbr0: u64 = 0;
        core::arch::asm!(
            "mrs {value}, ttbr0_el1",
            value = out(reg) current_ttbr0,
            options(nostack, preserves_flags)
        );
        if current_ttbr0 == ttbr0 {
            asid_trace!(
                "ADDRESS_SPACE_SWITCH_SKIPPED_SAME_ASID asid={} ttbr0=0x{:x}",
                asid.0,
                ttbr0
            );
            return Ok(ttbr0);
        }
        let sp: u64;
        core::arch::asm!("mov {0}, sp", out(reg) sp, options(nostack, preserves_flags));
        let pc_sym = activate_asid as *const () as usize as u64;
        let asw3_msg_ptr = b"ASW3_RAW".as_ptr() as u64;
        crate::yarm_log!(
            "ASW1V switch_va_snapshot asid={} pc_sym=0x{:x} sp=0x{:x} asw3_msg_ptr=0x{:x}",
            asid.0,
            pc_sym,
            sp,
            asw3_msg_ptr
        );
        crate::yarm_log!(
            "ASW2 before msr ttbr0_el1 asid={} ttbr0=0x{:x}",
            asid.0,
            ttbr0
        );
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
    // A reserved device leaf (early UART, GIC distributor, GIC CPU interface) is kernel MMIO that
    // every root carries. Replacing one would drop a Device/XN privileged mapping and, for the
    // GIC, hand userspace the interrupt controller — so the request is refused rather than
    // silently overwriting it.
    if is_reserved_device_va(virt.0) {
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

/// Stage 163I: full local TLB flush (all stage-1 EL1 entries on this CPU).
///
/// Mirrors the x86_64 entry point used by the shared page-fault recovery path.
/// On a present write fault that recurs despite a per-page invalidation, this
/// drops every translation so the CPU re-walks the page table.
pub fn flush_tlb_local_full() {
    #[cfg(any(test, feature = "hosted-dev"))]
    {}

    #[cfg(all(not(feature = "hosted-dev"), not(test)))]
    unsafe {
        core::arch::asm!(
            "dsb ishst",
            "tlbi vmalle1is",
            "dsb ish",
            "isb",
            options(nostack, preserves_flags)
        );
    }
}

/// Stage 163I: x86_64 needs to widen under-permissioned intermediate paging
/// entries (the AND-of-levels access check denies a permissive leaf). AArch64
/// applies access permissions only at the leaf descriptor, so there is no
/// intermediate-permission repair to perform; this is a typed no-op kept so the
/// shared fault handler can call one symbol across architectures.
pub fn repair_user_path_intermediates(_asid: Asid, _virt: VirtAddr) -> u8 {
    0
}

#[cfg(test)]
pub fn take_last_invalidated_asid_for_test() -> Option<Asid> {
    LAST_INVALIDATED_ASID.lock().take()
}

/// Canonical 199E prerequisite — per-root GIC device mappings.
///
/// `TTBR1_EL1` is always zero on this port, so all kernel execution runs from the active TTBR0
/// root, and `copy_bootstrap_kernel_root_entries` deliberately skips `L1[0]` (VA 0..1 GiB) because
/// user code occupies low addresses. Every device the kernel reaches through that window must
/// therefore have its own leaf re-established in each root — previously only the UART did, which
/// is why any GIC access after the first user root was activated faulted.
///
/// These fixtures live here because they exercise `PageTableState` directly. `src/arch/aarch64/**`
/// is `#[cfg(target_arch = "aarch64")]`, so they compile in an AArch64 test build; the canonical
/// hosted suite carries matching structural guards in `kernel/boot/tests.rs`.
#[cfg(test)]
mod gic_device_mapping_tests {
    use super::*;

    const QEMU_GICD_PA: u64 = 0x0800_0000;
    const QEMU_GICC_PA: u64 = 0x0801_0000;

    fn leaf_for(state: &mut PageTableState, root_idx: usize, va: u64) -> Option<PageTableEntry> {
        let root_phys = state.pages[root_idx].as_ref()?.phys;
        let l1 = level_index(va, 30);
        let l2 = level_index(va, 21);
        let l3 = level_index(va, 12);
        let root_i = state.page_index_from_phys(root_phys)?;
        let e1 = read_table_entry(state, root_i, l1).ok()?;
        let i2 = state.page_index_from_phys(e1.addr())?;
        let e2 = read_table_entry(state, i2, l2).ok()?;
        let i3 = state.page_index_from_phys(e2.addr())?;
        read_table_entry(state, i3, l3).ok()
    }

    fn fresh_root(state: &mut PageTableState) -> usize {
        let root_idx = state.alloc_page().expect("root page");
        ensure_reserved_device_mappings(state, root_idx).expect("device mappings");
        root_idx
    }

    /// Every new root receives the UART leaf AND both GIC leaves once bases are published.
    #[test]
    fn a_new_root_receives_uart_and_both_gic_leaves() {
        publish_gic_mmio_bases(QEMU_GICD_PA, QEMU_GICC_PA);
        let mut state = PageTableState::new();
        let root = fresh_root(&mut state);
        for (name, va) in [
            ("uart", EARLY_UART_MMIO_VA),
            ("gicd", QEMU_GICD_PA),
            ("gicc", QEMU_GICC_PA),
        ] {
            let leaf = leaf_for(&mut state, root, va).unwrap_or(PageTableEntry::empty());
            assert!(leaf.is_present(), "{name} leaf must be present");
            // Identity: the mapped physical address IS the virtual address.
            assert_eq!(leaf.addr(), va, "{name} must be identity-mapped");
        }
    }

    /// Both GIC leaves are Device-nGnRE, execute-never and privileged (non-user).
    #[test]
    fn gic_leaves_are_device_xn_and_privileged() {
        publish_gic_mmio_bases(QEMU_GICD_PA, QEMU_GICC_PA);
        let mut state = PageTableState::new();
        let root = fresh_root(&mut state);
        let device_attr = cache_policy_bits(CachePolicy::Device);
        for va in [QEMU_GICD_PA, QEMU_GICC_PA] {
            let leaf = leaf_for(&mut state, root, va).expect("leaf");
            assert_eq!(
                leaf.0 & device_attr,
                device_attr,
                "Device-nGnRE memory type (MAIR AttrIdx 3)"
            );
            assert_ne!(leaf.0 & PageTableEntry::NO_EXECUTE, 0, "execute-never");
            assert_eq!(leaf.0 & PageTableEntry::USER, 0, "privileged, not user");
            assert_ne!(leaf.0 & PageTableEntry::VALID, 0, "present");
        }
    }

    /// The distributor and CPU-interface pages stay DISTINCT 4 KiB leaves — one page each, never
    /// the enclosing 2 MiB block and never a shared entry.
    #[test]
    fn gicd_and_gicc_are_distinct_pages() {
        publish_gic_mmio_bases(QEMU_GICD_PA, QEMU_GICC_PA);
        let mut state = PageTableState::new();
        let root = fresh_root(&mut state);
        let d = leaf_for(&mut state, root, QEMU_GICD_PA).expect("gicd");
        let c = leaf_for(&mut state, root, QEMU_GICC_PA).expect("gicc");
        assert_ne!(d.addr(), c.addr());
        assert_eq!(d.addr(), QEMU_GICD_PA);
        assert_eq!(c.addr(), QEMU_GICC_PA);
        // The page immediately above the distributor is NOT mapped: only one page was taken.
        let neighbour = leaf_for(&mut state, root, QEMU_GICD_PA + PAGE_SIZE_U64)
            .unwrap_or(PageTableEntry::empty());
        assert!(!neighbour.is_present(), "exactly one page per device");
    }

    /// A zero or misaligned base maps nothing — fail closed, never a guessed address. This is the
    /// RPi5/GICv3 case: the parser yields no GICv2 bases, so no GIC leaf exists.
    #[test]
    fn absent_or_misaligned_bases_map_nothing() {
        let mut state = PageTableState::new();
        let root_idx = state.alloc_page().expect("root");
        ensure_early_device_mapping(&mut state, root_idx, 0).expect("zero base is a no-op");
        ensure_early_device_mapping(&mut state, root_idx, QEMU_GICD_PA + 0x40)
            .expect("misaligned base is a no-op");
        for va in [0u64, QEMU_GICD_PA, QEMU_GICD_PA + 0x40] {
            let leaf = leaf_for(&mut state, root_idx, va).unwrap_or(PageTableEntry::empty());
            assert!(!leaf.is_present(), "no leaf for an invalid base");
        }
    }

    /// The UART leaf keeps its exact previous behaviour.
    #[test]
    fn uart_mapping_is_unchanged() {
        let mut state = PageTableState::new();
        let root_idx = state.alloc_page().expect("root");
        ensure_early_device_mapping(&mut state, root_idx, EARLY_UART_MMIO_PA).expect("uart");
        let leaf = leaf_for(&mut state, root_idx, EARLY_UART_MMIO_VA).expect("uart leaf");
        assert!(leaf.is_present());
        assert_eq!(leaf.addr(), EARLY_UART_MMIO_PA);
        assert_eq!(
            leaf.0 & cache_policy_bits(CachePolicy::Device),
            cache_policy_bits(CachePolicy::Device)
        );
    }

    /// A user mapping request cannot replace a reserved device leaf.
    #[test]
    fn user_mapping_cannot_overwrite_a_reserved_device_leaf() {
        publish_gic_mmio_bases(QEMU_GICD_PA, QEMU_GICC_PA);
        for va in [EARLY_UART_MMIO_VA, QEMU_GICD_PA, QEMU_GICC_PA] {
            assert!(is_reserved_device_va(va), "0x{va:x} is reserved");
            // An offset inside the same page is equally reserved.
            assert!(is_reserved_device_va(va + 0x40));
            assert_eq!(
                map_page(
                    Asid(9),
                    VirtAddr(va),
                    PhysAddr(0x4100_0000),
                    PageFlags::USER_RW
                ),
                Err(PageTableError::InvalidAddress),
                "user mapping at 0x{va:x} must be refused"
            );
        }
        // An ordinary user VA is still mappable.
        assert!(
            map_page(
                Asid(9),
                VirtAddr(0x4000_0000),
                PhysAddr(0x4100_0000),
                PageFlags::USER_RW
            )
            .is_ok()
        );
    }
}
