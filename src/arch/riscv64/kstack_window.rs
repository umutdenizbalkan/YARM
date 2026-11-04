// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! KTHREAD-CLOSURE1 §C — the RISC-V kernel-thread root and stack window.
//!
//! Before this, a kernel thread ran with `satp` Bare (the dispatcher leaves the user root first),
//! so nothing could fault below its stack. It now runs under the KERNEL-THREAD ROOT (Sv39, ASID 0,
//! which the address-space allocator never hands out): slot 0 an identity gigapage over the devices
//! below RAM (PLIC, UART, CLINT; R|W, never X), slot 2 the kernel-shared RAM gigapage every user
//! root carries (`RISCV_KERNEL_SHARED_BASE`, global R|W|X, S-mode only), slot 3 the window: slot
//! `i`'s 16 KiB at `WINDOW_BASE + (i + 1) * KSTACK_STRIDE`, aliasing the pool's `.bss` storage,
//! R|W without X or U. Everything else in the window is invalid, so a thread that descends below
//! its stack takes a page fault in its own gap. Slot 255 is the device window when a boot
//! installs one (`page_table::install_device_window`): under any translation the kernel reaches
//! a registered device page only through that slot (`mmio_va_under_active_satp` walks the live
//! root), as every user root carries it, so this root carries it too. Built once, at the pool's
//! first creation, before any kernel thread can run; afterwards only slot 255 may change, once,
//! from invalid to the window's entry (the window never changes once installed), and only in
//! `enter_root`, before its `satp` write and full fence. The dispatcher installs the root right
//! before switching into a thread and returns the hart to Bare right after the thread switches
//! back (`enter_root` / `leave_root`), all with interrupts masked; residency bookkeeping is
//! untouched (the hart left the user root before, as it always did).
#![cfg(not(feature = "hosted-dev"))]

use core::sync::atomic::{AtomicU8, AtomicU64, Ordering};

use crate::arch::riscv64::page_table::{
    self, DEVICE_WINDOW_ROOT_SLOT, RISCV_KERNEL_SHARED_BASE, RISCV_KERNEL_SHARED_END,
};
use crate::kernel::kthread::{KSTACK_STRIDE, KTHREAD_CAPACITY, KTHREAD_STACK_BYTES};

pub const WINDOW_BASE: usize = 0xC000_0000;
const V: u64 = 1;
const R: u64 = 1 << 1;
const W: u64 = 1 << 2;
const X: u64 = 1 << 3;
const G: u64 = 1 << 5;
const A: u64 = 1 << 6;
const D: u64 = 1 << 7;
const PAGE: usize = 4096;
const PAGES_PER_STACK: usize = KTHREAD_STACK_BYTES / PAGE;
const SATP_SV39: u64 = 8 << 60;

const _: () = assert!((KTHREAD_CAPACITY + 1) * KSTACK_STRIDE <= 1 << 30);
const _: () = assert!(KSTACK_STRIDE % (2 << 20) == 0 && KTHREAD_STACK_BYTES <= 2 << 20);
const _: () = assert!(RISCV_KERNEL_SHARED_END as usize == WINDOW_BASE);
const _: () = assert!(DEVICE_WINDOW_ROOT_SLOT > WINDOW_BASE >> 30 && DEVICE_WINDOW_ROOT_SLOT < 512);

#[repr(C, align(4096))]
struct Table([u64; 512]);

static mut ROOT: Table = Table([0; 512]);
static mut L1: Table = Table([0; 512]);
static mut L0S: [Table; KTHREAD_CAPACITY] = [const { Table([0; 512]) }; KTHREAD_CAPACITY];

/// 0 = not built, 1 = building, 2 = built.
static STATE: AtomicU8 = AtomicU8::new(0);

fn pte_table(pa: u64) -> u64 {
    ((pa >> 12) << 10) | V
}

fn pte_leaf(pa: u64, flags: u64) -> u64 {
    ((pa >> 12) << 10) | flags | V | A | D
}

/// Build the kernel-thread root (once) and return the window's base. Called by the pool at its
/// first creation, under its lock, before any kernel thread exists.
pub fn window() -> Option<usize> {
    match STATE.compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => {
            let stacks = crate::kernel::kthread::kstacks_storage();
            // SAFETY: STATE == 1 makes this the only writer; no hart uses the root yet.
            unsafe {
                let root = &mut *core::ptr::addr_of_mut!(ROOT);
                let l1 = &mut *core::ptr::addr_of_mut!(L1);
                let l0s = &mut *core::ptr::addr_of_mut!(L0S);
                root.0[0] = pte_leaf(0, R | W);
                root.0[(RISCV_KERNEL_SHARED_BASE >> 30) as usize] =
                    pte_leaf(RISCV_KERNEL_SHARED_BASE, R | W | X | G);
                root.0[WINDOW_BASE >> 30] = pte_table(l1 as *mut Table as u64);
                for (slot, l0) in l0s.iter_mut().enumerate() {
                    let off = (slot + 1) * KSTACK_STRIDE;
                    l1.0[off >> 21] = pte_table(l0 as *mut Table as u64);
                    for page in 0..PAGES_PER_STACK {
                        let pa = (stacks + slot * KTHREAD_STACK_BYTES + page * PAGE) as u64;
                        l0.0[((off >> 12) & 511) + page] = pte_leaf(pa, R | W);
                    }
                }
            }
            STATE.store(2, Ordering::Release);
            crate::yarm_log!(
                "KSTACK_WINDOW arch=riscv64 base={:#x} stride={:#x} stacks={} table_pages={} root_asid=0",
                WINDOW_BASE,
                KSTACK_STRIDE,
                KTHREAD_CAPACITY,
                2 + KTHREAD_CAPACITY
            );
            Some(WINDOW_BASE)
        }
        Err(_) => {
            while STATE.load(Ordering::Acquire) == 1 {
                core::hint::spin_loop();
            }
            Some(WINDOW_BASE)
        }
    }
}

fn root_satp() -> u64 {
    SATP_SV39 | ((core::ptr::addr_of!(ROOT) as u64) >> 12)
}

/// The dispatcher is about to switch into a kernel thread on this hart (interrupts masked, the
/// hart already in Bare): install the kernel-thread root. The flush drops any translation another
/// root left under ASID 0's tag (none is ever made, but nothing is assumed).
pub fn enter_root() {
    if STATE.load(Ordering::Acquire) != 2 {
        return;
    }
    if let Some(entry) = page_table::device_window_root_entry() {
        // SAFETY: an aligned u64 of the static root, accessed only atomically once built (harts
        // may enter concurrently; each stores the same value). The fence below orders it for this
        // hart's walks.
        let slot = unsafe {
            AtomicU64::from_ptr(core::ptr::addr_of_mut!(ROOT.0[DEVICE_WINDOW_ROOT_SLOT]))
        };
        if slot.load(Ordering::Acquire) != entry {
            slot.store(entry, Ordering::Release);
        }
    }
    // SAFETY: the root maps everything the kernel touches while a thread runs (RAM, the devices
    // below RAM, the device window if installed, the stack window); interrupts are masked.
    unsafe {
        core::arch::asm!("csrw satp, {v}", "sfence.vma zero, zero", v = in(reg) root_satp(),
            options(nostack, preserves_flags))
    };
}

/// The thread switched back to the dispatcher: return the hart to Bare, as before.
pub fn leave_root() {
    if STATE.load(Ordering::Acquire) != 2 {
        return;
    }
    crate::arch::riscv64::page_table::enter_bare_kernel_space();
}
