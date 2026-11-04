// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! KTHREAD-CLOSURE1 §C — the x86_64 kernel-thread stack window.
//!
//! PML4[510] (`0xFFFF_FF00_0000_0000`, kernel half, unreachable from ring 3) holds a window of
//! `(KTHREAD_CAPACITY + 1) * KSTACK_STRIDE` bytes in which only the kernel-thread stacks are
//! mapped: slot `i`'s 16 KiB at `WINDOW_BASE + (i + 1) * KSTACK_STRIDE`, aliasing the pool's
//! `.bss` storage, supervisor-only, read/write, no-execute when EFER.NXE is on. Everything else in
//! the window is unmapped, so a thread that descends below its stack takes a #PF in its own gap.
//!
//! The tables (one PDPT, one PD, one PT per slot) are static and filled once, then installed into
//! the canonical kernel root before the first user root is cloned from it (`page_table`'s
//! `ensure_asid`): every root copies PML4[256..512] by value, so every root — and every CPU,
//! whichever root it runs on — carries the same entry. Nothing tears the window down.
#![cfg(not(feature = "hosted-dev"))]

use core::sync::atomic::{AtomicU8, Ordering};

use crate::arch::x86_64::platform_layout::{KERNEL_BOOTSTRAP_VIRT_BASE, KERNEL_LINK_VIRT_BASE};
use crate::kernel::kthread::{KSTACK_STRIDE, KTHREAD_CAPACITY, KTHREAD_STACK_BYTES};

pub const WINDOW_BASE: usize = 0xFFFF_FF00_0000_0000;
const WINDOW_PML4_INDEX: usize = 510;
const PRESENT: u64 = 1;
const WRITABLE: u64 = 1 << 1;
const NO_EXECUTE: u64 = 1 << 63;
const PAGE: usize = 4096;
const PAGES_PER_STACK: usize = KTHREAD_STACK_BYTES / PAGE;

// The window is one PD (1 GiB): every stride and stack must fit, each stack in its own 2 MiB.
const _: () = assert!((KTHREAD_CAPACITY + 1) * KSTACK_STRIDE <= 1 << 30);
const _: () = assert!(KSTACK_STRIDE % (2 << 20) == 0 && KTHREAD_STACK_BYTES <= 2 << 20);

#[repr(C, align(4096))]
struct Table([u64; 512]);

static mut PDPT: Table = Table([0; 512]);
static mut PD: Table = Table([0; 512]);
static mut PTS: [Table; KTHREAD_CAPACITY] = [const { Table([0; 512]) }; KTHREAD_CAPACITY];

/// 0 = not built, 1 = building, 2 = installed.
static STATE: AtomicU8 = AtomicU8::new(0);

fn image_phys(va: usize) -> u64 {
    va as u64 - KERNEL_LINK_VIRT_BASE
}

fn nx_enabled() -> bool {
    let lo: u32;
    let _hi: u32;
    // SAFETY: reads IA32_EFER only.
    unsafe {
        core::arch::asm!("rdmsr", in("ecx") 0xC000_0080u32, out("eax") lo, out("edx") _hi,
            options(nomem, nostack, preserves_flags))
    };
    lo & (1 << 11) != 0
}

/// The window's base once installed.
pub fn window() -> Option<usize> {
    (STATE.load(Ordering::Acquire) == 2).then_some(WINDOW_BASE)
}

/// Build the window (once) over `stacks` (the pool's storage, `KTHREAD_CAPACITY` contiguous
/// stacks in the kernel image) and install it as PML4[510] of the root at `root_phys`. Called with
/// the canonical kernel root before any user root is cloned from it.
///
/// # Safety
/// `root_phys` is the canonical kernel root, reachable through the direct map; no other code
/// writes its PML4[510].
pub unsafe fn install(root_phys: u64, stacks: usize) {
    if STATE
        .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        while STATE.load(Ordering::Acquire) != 2 {
            core::hint::spin_loop();
        }
        return;
    }
    let nx = if nx_enabled() { NO_EXECUTE } else { 0 };
    // SAFETY: STATE == 1 makes this the only writer; the tables are not yet reachable.
    unsafe {
        let pdpt = &mut *core::ptr::addr_of_mut!(PDPT);
        let pd = &mut *core::ptr::addr_of_mut!(PD);
        let pts = &mut *core::ptr::addr_of_mut!(PTS);
        pdpt.0[0] = image_phys(pd as *mut Table as usize) | PRESENT | WRITABLE;
        for (slot, pt) in pts.iter_mut().enumerate() {
            let va_off = (slot + 1) * KSTACK_STRIDE;
            pd.0[va_off >> 21] = image_phys(pt as *mut Table as usize) | PRESENT | WRITABLE;
            for page in 0..PAGES_PER_STACK {
                let pa = image_phys(stacks + slot * KTHREAD_STACK_BYTES + page * PAGE);
                pt.0[((va_off >> 12) & 511) + page] = pa | PRESENT | WRITABLE | nx;
            }
        }
        let root = (KERNEL_BOOTSTRAP_VIRT_BASE + root_phys) as *mut u64;
        core::ptr::write_volatile(
            root.add(WINDOW_PML4_INDEX),
            image_phys(pdpt as *mut Table as usize) | PRESENT | WRITABLE,
        );
    }
    STATE.store(2, Ordering::Release);
    crate::yarm_log!(
        "KSTACK_WINDOW arch=x86_64 base={:#x} stride={:#x} stacks={} table_pages={} nx={}",
        WINDOW_BASE,
        KSTACK_STRIDE,
        KTHREAD_CAPACITY,
        2 + KTHREAD_CAPACITY,
        u8::from(nx != 0)
    );
}

/// The slot whose guard gap holds `addr`, if `addr` is in the window.
pub fn gap_owner(addr: usize) -> Option<usize> {
    window()?;
    crate::kernel::kthread::StackView::window(WINDOW_BASE).gap_owner(addr)
}
