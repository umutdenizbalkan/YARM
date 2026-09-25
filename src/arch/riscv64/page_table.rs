// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use crate::arch::riscv64::vm_layout;
use crate::kernel::frame_allocator::{alloc_pt_frame, free_pt_frame};
use crate::kernel::lock::SpinLockIrq;
use crate::kernel::vm::{Asid, CachePolicy, PageFlags, PhysAddr, VirtAddr};
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

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
        // QEMU-IRQ1 §2 — a root created AFTER the device window was installed gets it here, at
        // the one place every root is born, so "every address space a trap can be taken in maps
        // the controller" cannot depend on which activation path reaches the new ASID first.
        self.install_device_window_into_root(root_phys);
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

    /// QEMU-IRQ1 §2 — point `root_phys`'s device-window slot at the shared window table.
    ///
    /// A strict no-op until [`install_device_window`] has built the window. The slot pointer is a
    /// non-leaf PTE (VALID only), exactly as `table_flags_from_page_flags` builds every other
    /// intermediate entry, so the permissions live on the window's leaves alone.
    ///
    /// The window's tables are deliberately NOT entered into `pages`: they are shared by every
    /// root and owned by none of them. `remove_asid_root` follows only children it finds in
    /// `pages`, so tearing an address space down skips the window instead of freeing a table the
    /// other roots still point at.
    fn install_device_window_into_root(&mut self, root_phys: u64) {
        let l1 = DEVICE_WINDOW_L1.load(Ordering::Acquire);
        if l1 == 0 {
            return;
        }
        let Some(root_idx) = self.page_index_from_phys(root_phys) else {
            return;
        };
        let pte = PageTableEntry::with_addr_and_flags(l1, PageTableEntry::VALID);
        self.pages[root_idx].as_mut().expect("root").entries[DEVICE_WINDOW_ROOT_SLOT] = pte;
        store_pte_to_frame(root_phys, DEVICE_WINDOW_ROOT_SLOT, pte);
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

// ═════════════════════════════════════════════════════════════════════════════════════════════
// QEMU-IRQ1 §2 — the kernel-only DEVICE WINDOW.
//
// The interrupt controller and the one device the witness drives sit below RAM (PLIC at
// `0x0C00_0000`, UART0 at `0x1000_0000`), and the only kernel mapping a user root carried was the
// RAM gigapage at root slot 2. A claim read is performed in S-mode under whatever root was active
// when the trap was taken — a user root, or the last one installed when the hart is idling — so
// the controller has to be mapped in EVERY root, not merely in one privileged table.
//
// Shape, and why:
//
// * ONE root slot, 255, the last lower-half gigabyte (`0x3F_C000_0000`). A user mapping is admitted
//   only below `0x8000_0000` (`VirtAddr::is_user`), so no user mapping can ever be placed in or
//   alias this slot, and no user root slot is taken away.
// * ONE L1 and ONE L0 table, SHARED by every root and tracked by none. The L0 holds a leaf per
//   device page and nothing else. Teardown (`remove_asid_root`) follows only tracked children, so
//   it can never free the shared tables out from under the other roots.
// * Leaves are V|R|W|A|D|G, with NO U and NO X: U-mode access faults, and nothing executes from a
//   device page. There are no PBMT bits because the executed CPU (QEMU virt, `rv64`) does not
//   implement Svpbmt — its ISA string has no `svpbmt` — so the memory type is the platform PMA,
//   which marks both regions I/O. `cache_policy_bits(Device)` is `0` for the same reason.
//
// Nothing installs the window on an ordinary boot. Until something does, `DEVICE_WINDOW_L1` is `0`,
// no root carries slot 255, and the readiness walk below answers "unreachable" from the hardware
// tables themselves rather than from a flag.
// ═════════════════════════════════════════════════════════════════════════════════════════════

/// The root slot the device window occupies.
pub const DEVICE_WINDOW_ROOT_SLOT: usize = 255;
/// First virtual address of the device window.
pub const DEVICE_WINDOW_BASE: u64 = (DEVICE_WINDOW_ROOT_SLOT as u64) << 30;
/// How many device pages the window can name. The witness uses four.
pub const DEVICE_WINDOW_MAX_PAGES: usize = 8;

/// Physical address of the shared window L1 table; `0` until [`install_device_window`] runs.
static DEVICE_WINDOW_L1: AtomicU64 = AtomicU64::new(0);
/// Physical page mapped at window slot `i`; `0` for an unused slot.
static DEVICE_WINDOW_PAGES: [AtomicU64; DEVICE_WINDOW_MAX_PAGES] =
    [const { AtomicU64::new(0) }; DEVICE_WINDOW_MAX_PAGES];
static DEVICE_WINDOW_PAGE_COUNT: AtomicUsize = AtomicUsize::new(0);

/// The flags every device leaf carries — and, by construction, the only ones.
pub const DEVICE_WINDOW_LEAF_FLAGS: u64 = PageTableEntry::VALID
    | PageTableEntry::READ
    | PageTableEntry::WRITE
    | PageTableEntry::GLOBAL
    | PageTableEntry::ACCESSED
    | PageTableEntry::DIRTY;

/// Why the window could not be installed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceWindowError {
    AlreadyInstalled,
    BadPageList,
    OutOfMemory,
}

/// Build the device window over `pages` (page-aligned physical addresses, one leaf each, in slot
/// order) and point every existing root at it. Roots created later receive it in `ensure_asid`.
///
/// Called once, from the kernel idle safe point, before any controller register is written.
/// Returns the number of pages mapped.
pub fn install_device_window(pages: &[u64]) -> Result<usize, DeviceWindowError> {
    if pages.is_empty() || pages.len() > DEVICE_WINDOW_MAX_PAGES {
        return Err(DeviceWindowError::BadPageList);
    }
    for &pa in pages {
        // A device page must be page-aligned, non-zero and OUTSIDE the RAM gigapage: the window
        // exists for what that mapping does not cover, and mapping RAM here would give it a second,
        // differently-attributed alias.
        if pa == 0
            || !pa.is_multiple_of(PAGE_SIZE_U64)
            || (RISCV_KERNEL_SHARED_BASE..RISCV_KERNEL_SHARED_END).contains(&pa)
        {
            return Err(DeviceWindowError::BadPageList);
        }
    }
    let mut state = PAGE_TABLE_STATE.lock();
    if DEVICE_WINDOW_L1.load(Ordering::Acquire) != 0 {
        return Err(DeviceWindowError::AlreadyInstalled);
    }
    let l1 = alloc_pt_frame().map_err(|_| DeviceWindowError::OutOfMemory)?;
    let l0 = match alloc_pt_frame() {
        Ok(f) => f,
        Err(_) => {
            let _ = free_pt_frame(l1);
            return Err(DeviceWindowError::OutOfMemory);
        }
    };
    zero_pt_frame(l1);
    zero_pt_frame(l0);
    for (slot, &pa) in pages.iter().enumerate() {
        store_pte_to_frame(
            l0,
            slot,
            PageTableEntry::with_addr_and_flags(pa, DEVICE_WINDOW_LEAF_FLAGS),
        );
        DEVICE_WINDOW_PAGES[slot].store(pa, Ordering::Release);
    }
    DEVICE_WINDOW_PAGE_COUNT.store(pages.len(), Ordering::Release);
    store_pte_to_frame(
        l1,
        level_index(DEVICE_WINDOW_BASE, 21),
        PageTableEntry::with_addr_and_flags(l0, PageTableEntry::VALID),
    );
    // Published only once both tables are complete, so a root can never point at a half-built
    // window.
    DEVICE_WINDOW_L1.store(l1, Ordering::Release);
    for i in 0..MAX_ASID_ROOTS {
        if let Some(root) = state.asids[i] {
            state.install_device_window_into_root(root.root_phys);
        }
    }
    drop(state);
    flush_tlb_local_full();
    Ok(pages.len())
}

/// `true` once [`install_device_window`] has published the window.
pub fn device_window_installed() -> bool {
    DEVICE_WINDOW_L1.load(Ordering::Acquire) != 0
}

/// The window VA that WOULD name `pa`, from the recorded page list alone.
///
/// This is a layout translation, not a readiness answer: it says where the window puts `pa`,
/// never whether the active root actually maps it. Readiness is
/// [`device_pa_reachable_under_satp`].
pub fn device_window_va(pa: u64) -> Option<u64> {
    let page = pa & PAGE_MASK;
    let count = DEVICE_WINDOW_PAGE_COUNT.load(Ordering::Acquire);
    (0..count.min(DEVICE_WINDOW_MAX_PAGES))
        .find(|&slot| DEVICE_WINDOW_PAGES[slot].load(Ordering::Acquire) == page)
        .map(|slot| DEVICE_WINDOW_BASE + (slot as u64) * PAGE_SIZE_U64 + (pa - page))
}

/// Walk an Sv39 translation for `va` the way the MMU would, reading each table through `read`.
///
/// Returns the LEAF PTE when the walk ends in a 4 KiB leaf. Returns `None` for a non-Sv39 `satp`
/// (bare mode translates nothing), an invalid entry, a reserved W-without-R encoding, a superpage,
/// or a table `read` refuses to dereference. Pure, so the readiness rule is testable off target.
pub fn walk_sv39_leaf(
    satp: u64,
    va: u64,
    read: impl Fn(u64, usize) -> Option<u64>,
) -> Option<PageTableEntry> {
    const SATP_MODE_SV39: u64 = 8;
    if satp >> 60 != SATP_MODE_SV39 {
        return None;
    }
    let mut table = (satp & ((1u64 << 44) - 1)) << PAGE_SHIFT;
    for shift in [30u64, 21, 12] {
        let pte = PageTableEntry(read(table, level_index(va, shift))?);
        if !pte.is_present() {
            return None;
        }
        let rwx = pte.0 & (PageTableEntry::READ | PageTableEntry::WRITE | PageTableEntry::EXECUTE);
        if rwx == PageTableEntry::WRITE || rwx == PageTableEntry::WRITE | PageTableEntry::EXECUTE {
            return None;
        }
        if rwx != 0 {
            return (shift == 12).then_some(pte);
        }
        if shift == 12 {
            return None;
        }
        table = pte.addr();
    }
    None
}

/// **The readiness rule.** The window VA for `[pa, pa + len)` iff the translation rooted at
/// `satp` really maps it: a 4 KiB leaf that is VALID, READ and WRITE, carries neither USER nor
/// EXECUTE, and names exactly `pa`'s page. The range must lie within one page.
///
/// Every fact is read from the page tables the hardware would walk; the recorded page list only
/// says WHICH virtual address to walk. So an uninstalled window, a root that never received it,
/// or bare translation all answer `None` — and none of them is ever answered by touching the
/// device.
pub fn device_pa_reachable_under_satp(
    satp: u64,
    pa: u64,
    len: u64,
    read: impl Fn(u64, usize) -> Option<u64>,
) -> Option<u64> {
    if len == 0 {
        return None;
    }
    let page = pa & PAGE_MASK;
    if (pa + len - 1) & PAGE_MASK != page {
        return None;
    }
    let va = device_window_va(pa)?;
    let leaf = walk_sv39_leaf(satp, va, read)?;
    let want = PageTableEntry::VALID | PageTableEntry::READ | PageTableEntry::WRITE;
    let forbidden = PageTableEntry::USER | PageTableEntry::EXECUTE;
    if leaf.0 & want != want || leaf.0 & forbidden != 0 || leaf.addr() != page {
        return None;
    }
    Some(va)
}

/// [`device_pa_reachable_under_satp`] against the LIVE `satp` of this hart, reading each table
/// frame through the identity RAM mapping every root carries. A frame outside that mapping is
/// refused rather than dereferenced.
#[cfg(all(not(test), not(feature = "hosted-dev"), target_arch = "riscv64"))]
pub fn device_pa_reachable_under_active_satp(pa: u64, len: u64) -> Option<u64> {
    let satp: u64;
    unsafe {
        core::arch::asm!("csrr {0}, satp", out(reg) satp, options(nomem, nostack, preserves_flags));
    }
    device_pa_reachable_under_satp(satp, pa, len, |table, index| {
        let in_ram = table >= RISCV_KERNEL_SHARED_BASE
            && table + PAGE_SIZE_U64 <= RISCV_KERNEL_SHARED_END
            && table.is_multiple_of(PAGE_SIZE_U64)
            && index < ENTRIES_PER_TABLE;
        in_ram.then(|| unsafe { core::ptr::read_volatile((table as *const u64).add(index)) })
    })
}

/// No live translation to walk off target.
#[cfg(any(test, feature = "hosted-dev", not(target_arch = "riscv64")))]
pub fn device_pa_reachable_under_active_satp(_pa: u64, _len: u64) -> Option<u64> {
    None
}

#[cfg(test)]
pub(crate) fn reset_device_window_for_test() {
    DEVICE_WINDOW_L1.store(0, Ordering::Release);
    DEVICE_WINDOW_PAGE_COUNT.store(0, Ordering::Release);
    for page in &DEVICE_WINDOW_PAGES {
        page.store(0, Ordering::Release);
    }
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
