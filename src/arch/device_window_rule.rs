// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! QEMU-IRQ1 §2 — the device-window READINESS RULE and the idle-origin external ADMISSION rule,
//! arch-neutral on purpose.
//!
//! Both are pure functions of their inputs, and both decide something the RISC-V port may only do
//! when it is true: read a controller register (readiness), and handle a supervisor external
//! interrupt taken from S-mode (admission). Keeping them out of `arch::riscv64` — which is compiled
//! only for RISC-V targets — is what lets the hosted suite execute the rule the kernel executes,
//! rather than a copy of its text. The RISC-V port calls these; it does not restate them.
//!
//! **Hosted evidence here is model evidence**: the page tables are fabricated in memory. It shows
//! the rule refuses every shape it must refuse. It does not show that a real MMU agrees — the live
//! witness does that.

/// Sv39 PTE bits.
pub const PTE_VALID: u64 = 1 << 0;
pub const PTE_READ: u64 = 1 << 1;
pub const PTE_WRITE: u64 = 1 << 2;
pub const PTE_EXECUTE: u64 = 1 << 3;
pub const PTE_USER: u64 = 1 << 4;
const PTE_ADDR_MASK: u64 = 0x003f_ffff_ffff_fc00;
const PAGE_SHIFT: u64 = 12;
const PAGE_SIZE: u64 = 1 << PAGE_SHIFT;
const PAGE_MASK: u64 = !(PAGE_SIZE - 1);
const SATP_MODE_SV39: u64 = 8;

fn pte_addr(pte: u64) -> u64 {
    ((pte & PTE_ADDR_MASK) >> 10) << PAGE_SHIFT
}

fn level_index(va: u64, shift: u64) -> usize {
    ((va >> shift) & 0x1ff) as usize
}

/// Walk an Sv39 translation for `va` the way the MMU would, reading each table through `read`.
///
/// Returns the LEAF PTE when the walk ends in a 4 KiB leaf. `None` for a non-Sv39 `satp` (bare
/// mode translates nothing), an invalid entry, a reserved W-without-R encoding, a superpage, or a
/// table `read` refuses to dereference.
pub fn walk_sv39_leaf(satp: u64, va: u64, read: impl Fn(u64, usize) -> Option<u64>) -> Option<u64> {
    if satp >> 60 != SATP_MODE_SV39 {
        return None;
    }
    let mut table = (satp & ((1u64 << 44) - 1)) << PAGE_SHIFT;
    for shift in [30u64, 21, 12] {
        let pte = read(table, level_index(va, shift))?;
        if pte & PTE_VALID == 0 {
            return None;
        }
        let rwx = pte & (PTE_READ | PTE_WRITE | PTE_EXECUTE);
        if rwx == PTE_WRITE || rwx == PTE_WRITE | PTE_EXECUTE {
            return None;
        }
        if rwx != 0 {
            return (shift == 12).then_some(pte);
        }
        if shift == 12 {
            return None;
        }
        table = pte_addr(pte);
    }
    None
}

/// The window VA that the layout `pages` (slot order, from `base`) gives `pa`. A layout
/// translation only — it says where the window WOULD put `pa`, never whether it is mapped.
pub fn window_va_for(pages: &[u64], base: u64, pa: u64) -> Option<u64> {
    let page = pa & PAGE_MASK;
    pages
        .iter()
        .position(|&p| p != 0 && p == page)
        .map(|slot| base + (slot as u64) * PAGE_SIZE + (pa - page))
}

/// **The readiness rule.** The window VA for `[pa, pa + len)` iff the translation rooted at `satp`
/// really maps it: a 4 KiB leaf that is VALID, READ and WRITE, carries neither USER nor EXECUTE,
/// and names exactly `pa`'s page. The range must lie within one page.
///
/// Every fact is read from the tables the hardware would walk; the layout only says WHICH virtual
/// address to walk. An uninstalled window, a root that never received it, bare translation, or a
/// user-accessible leaf all answer `None` — and none of them is answered by touching the device.
pub fn device_pa_reachable(
    satp: u64,
    pages: &[u64],
    base: u64,
    pa: u64,
    len: u64,
    read: impl Fn(u64, usize) -> Option<u64>,
) -> Option<u64> {
    if len == 0 {
        return None;
    }
    let page = pa & PAGE_MASK;
    if (pa.checked_add(len - 1)?) & PAGE_MASK != page {
        return None;
    }
    let va = window_va_for(pages, base, pa)?;
    let leaf = walk_sv39_leaf(satp, va, read)?;
    let want = PTE_VALID | PTE_READ | PTE_WRITE;
    if leaf & want != want || leaf & (PTE_USER | PTE_EXECUTE) != 0 || pte_addr(leaf) != page {
        return None;
    }
    Some(va)
}

/// Supervisor external interrupt cause code (`scause` low bits, with the interrupt bit set).
pub const IRQ_SUPERVISOR_EXTERNAL_CODE: usize = 9;

/// **The admission rule** for the one S-origin external interrupt the RISC-V bridge may accept
/// (witness builds only): a supervisor EXTERNAL interrupt taken at the audited kernel-idle
/// boundary, after a source has been enabled.
///
/// * `scause` carries the interrupt bit and names the supervisor external interrupt — so no
///   exception (a supervisor page fault is scause 12/13/15 with the interrupt bit clear) can ever
///   satisfy it;
/// * `SPP` says Supervisor;
/// * the idle-boundary latch is armed;
/// * a source has actually been enabled by the witness owner.
pub fn is_accepted_s_mode_external_trap(
    scause: usize,
    sstatus: usize,
    boundary_armed: bool,
    source_enabled: bool,
) -> bool {
    const INTERRUPT_BIT: usize = 1usize << (usize::BITS - 1);
    const SPP_BIT: usize = 1usize << 8;
    let is_interrupt = (scause & INTERRUPT_BIT) != 0;
    let code = scause & !INTERRUPT_BIT;
    let from_supervisor = (sstatus & SPP_BIT) != 0;
    is_interrupt
        && code == IRQ_SUPERVISOR_EXTERNAL_CODE
        && from_supervisor
        && boundary_armed
        && source_enabled
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::BTreeMap;

    const BASE: u64 = 255 << 30;
    const ROOT: u64 = 0x8010_0000;
    const L1: u64 = 0x8010_1000;
    const L0: u64 = 0x8010_2000;
    const PLIC_CTX: u64 = 0x0c20_1000;
    const UART: u64 = 0x1000_0000;
    const LEAF: u64 = PTE_VALID | PTE_READ | PTE_WRITE | (1 << 5) | (1 << 6) | (1 << 7);

    fn satp(root: u64) -> u64 {
        (8u64 << 60) | (root >> 12)
    }

    fn ptr(pa: u64) -> u64 {
        ((pa >> 12) << 10) | PTE_VALID
    }

    fn leaf(pa: u64, flags: u64) -> u64 {
        ((pa >> 12) << 10) | flags
    }

    /// A fabricated window: root slot 255 → L1[0] → L0[0..] = the given leaves.
    fn tables(leaves: &[u64]) -> BTreeMap<(u64, usize), u64> {
        let mut m = BTreeMap::new();
        m.insert((ROOT, 255), ptr(L1));
        m.insert((L1, 0), ptr(L0));
        for (i, &l) in leaves.iter().enumerate() {
            m.insert((L0, i), l);
        }
        m
    }

    fn reader(m: &BTreeMap<(u64, usize), u64>) -> impl Fn(u64, usize) -> Option<u64> + '_ {
        move |t, i| Some(m.get(&(t, i)).copied().unwrap_or(0))
    }

    const PAGES: [u64; 2] = [PLIC_CTX, UART];

    #[test]
    fn an_installed_kernel_only_leaf_is_ready_at_the_window_address() {
        let m = tables(&[leaf(PLIC_CTX, LEAF), leaf(UART, LEAF)]);
        assert_eq!(
            device_pa_reachable(satp(ROOT), &PAGES, BASE, PLIC_CTX + 4, 4, reader(&m)),
            Some(BASE + 4),
            "the claim register, one word into the context page"
        );
        assert_eq!(
            device_pa_reachable(satp(ROOT), &PAGES, BASE, UART + 5, 1, reader(&m)),
            Some(BASE + 4096 + 5),
            "the UART LSR, in window slot 1"
        );
    }

    #[test]
    fn readiness_refuses_every_shape_it_must() {
        let good = tables(&[leaf(PLIC_CTX, LEAF), leaf(UART, LEAF)]);
        let claim = PLIC_CTX + 4;
        // No window in this root at all: the root slot is empty.
        let empty = BTreeMap::new();
        assert_eq!(
            device_pa_reachable(satp(ROOT), &PAGES, BASE, claim, 4, reader(&empty)),
            None
        );
        // Bare translation.
        assert_eq!(
            device_pa_reachable(0, &PAGES, BASE, claim, 4, reader(&good)),
            None
        );
        // A USER-accessible leaf is NOT ready: the window must be kernel-only.
        let user = tables(&[leaf(PLIC_CTX, LEAF | PTE_USER)]);
        assert_eq!(
            device_pa_reachable(satp(ROOT), &PAGES, BASE, claim, 4, reader(&user)),
            None
        );
        // An executable leaf is not a device leaf.
        let exec = tables(&[leaf(PLIC_CTX, LEAF | PTE_EXECUTE)]);
        assert_eq!(
            device_pa_reachable(satp(ROOT), &PAGES, BASE, claim, 4, reader(&exec)),
            None
        );
        // A read-only leaf cannot take the completion write.
        let ro = tables(&[leaf(PLIC_CTX, LEAF & !PTE_WRITE)]);
        assert_eq!(
            device_pa_reachable(satp(ROOT), &PAGES, BASE, claim, 4, reader(&ro)),
            None
        );
        // A leaf naming a different page.
        let wrong = tables(&[leaf(UART, LEAF)]);
        assert_eq!(
            device_pa_reachable(satp(ROOT), &PAGES, BASE, claim, 4, reader(&wrong)),
            None
        );
        // A physical address the layout does not name.
        assert_eq!(
            device_pa_reachable(satp(ROOT), &PAGES, BASE, 0x0c00_0028, 4, reader(&good)),
            None
        );
        // A range crossing the page end.
        assert_eq!(
            device_pa_reachable(satp(ROOT), &PAGES, BASE, PLIC_CTX + 4094, 4, reader(&good)),
            None
        );
        // A zero-length range.
        assert_eq!(
            device_pa_reachable(satp(ROOT), &PAGES, BASE, claim, 0, reader(&good)),
            None
        );
        // A table the reader refuses to dereference.
        assert_eq!(
            device_pa_reachable(satp(ROOT), &PAGES, BASE, claim, 4, |_, _| None),
            None
        );
        // A 1 GiB superpage at the root slot is not the window.
        let mut huge = BTreeMap::new();
        huge.insert((ROOT, 255), leaf(PLIC_CTX & !0x3fff_ffff, LEAF));
        assert_eq!(
            device_pa_reachable(satp(ROOT), &PAGES, BASE, claim, 4, reader(&huge)),
            None
        );
    }

    #[test]
    fn the_layout_translation_is_not_a_readiness_answer() {
        // `window_va_for` names an address whether or not anything maps it.
        assert_eq!(window_va_for(&PAGES, BASE, PLIC_CTX + 4), Some(BASE + 4));
        assert_eq!(window_va_for(&PAGES, BASE, 0x2000_0000), None);
        assert_eq!(
            window_va_for(&[0, 0], BASE, 0),
            None,
            "an empty slot names nothing"
        );
    }

    #[test]
    fn idle_origin_external_admission_requires_all_conditions() {
        const INT: usize = 1usize << 63;
        const SPP: usize = 1 << 8;
        assert!(is_accepted_s_mode_external_trap(INT | 9, SPP, true, true));
        assert!(
            !is_accepted_s_mode_external_trap(9, SPP, true, true),
            "not an interrupt"
        );
        assert!(
            !is_accepted_s_mode_external_trap(INT | 5, SPP, true, true),
            "timer"
        );
        assert!(
            !is_accepted_s_mode_external_trap(INT | 1, SPP, true, true),
            "software"
        );
        assert!(
            !is_accepted_s_mode_external_trap(INT | 9, 0, true, true),
            "from U-mode"
        );
        assert!(
            !is_accepted_s_mode_external_trap(INT | 9, SPP, false, true),
            "latch not armed"
        );
        assert!(
            !is_accepted_s_mode_external_trap(INT | 9, SPP, true, false),
            "no source"
        );
        for exception in [2usize, 5, 7, 8, 9, 12, 13, 15] {
            assert!(
                !is_accepted_s_mode_external_trap(exception, SPP, true, true),
                "exception {exception} must never be admitted from S-mode"
            );
        }
    }
}
