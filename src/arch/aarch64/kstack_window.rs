// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! KTHREAD-CLOSURE1 §C — the AArch64 kernel-thread stack window.
//!
//! The bootstrap root maps VA = PA with 1 GiB blocks in L1[1..511]; L1[64] (`0x10_0000_0000`,
//! above every user address and over no RAM or device on QEMU virt) instead points at one L2 and
//! one L3 per slot, in which only the kernel-thread stacks are mapped: slot `i`'s 16 KiB at
//! `WINDOW_BASE + (i + 1) * KSTACK_STRIDE`, aliasing the pool's `.bss` storage, EL1-only
//! read/write (AP = 0b00), never executable (PXN | UXN), normal write-back memory. The rest of the
//! window is invalid, so a thread that descends below its stack takes a translation fault in its
//! own gap. Written by `setup_bootstrap_mmu` before the MMU is enabled; every root copies
//! L1[1..511] from the bootstrap root, so every root shares these tables. Nothing changes them.
#![cfg(not(feature = "hosted-dev"))]

use core::sync::atomic::{AtomicBool, Ordering};

use crate::kernel::kthread::{KSTACK_STRIDE, KTHREAD_CAPACITY, KTHREAD_STACK_BYTES};

pub const WINDOW_BASE: usize = 0x10_0000_0000;
pub const WINDOW_L1_INDEX: usize = WINDOW_BASE >> 30;
const VALID: u64 = 1;
const TABLE: u64 = 1 << 1;
const PAGE_DESC: u64 = 0b11;
const AF: u64 = 1 << 10;
const SH_INNER: u64 = 0b11 << 8;
const PXN: u64 = 1 << 53;
const UXN: u64 = 1 << 54;
const ATTR_NORMAL_WB: u64 = 0 << 2;
const PAGE: usize = 4096;
const PAGES_PER_STACK: usize = KTHREAD_STACK_BYTES / PAGE;

const _: () = assert!((KTHREAD_CAPACITY + 1) * KSTACK_STRIDE <= 1 << 30);
const _: () = assert!(KSTACK_STRIDE % (2 << 20) == 0 && KTHREAD_STACK_BYTES <= 2 << 20);
// The vector entries' overflow test (bit 14 of the stack pointer) needs 16 KiB stacks at the
// bottom of strides aligned to more than 32 KiB.
const _: () = assert!(KTHREAD_STACK_BYTES == 1 << 14 && WINDOW_BASE % KSTACK_STRIDE == 0);

#[repr(C, align(4096))]
struct Table([u64; 512]);

static mut L2: Table = Table([0; 512]);
static mut L3S: [Table; KTHREAD_CAPACITY] = [const { Table([0; 512]) }; KTHREAD_CAPACITY];
static INSTALLED: AtomicBool = AtomicBool::new(false);

/// The window's base once installed.
pub fn window() -> Option<usize> {
    INSTALLED.load(Ordering::Acquire).then_some(WINDOW_BASE)
}

/// Fill the window's tables over the pool's storage and point `l1[WINDOW_L1_INDEX]` at them.
///
/// # Safety
/// Called once, from `setup_bootstrap_mmu`, before the MMU is enabled (VA = PA), with `l1` the
/// bootstrap root.
pub unsafe fn install(l1: *mut u64) {
    let stacks = crate::kernel::kthread::kstacks_storage();
    // SAFETY: the only writer, before any translation through these tables.
    unsafe {
        let l2 = &mut *core::ptr::addr_of_mut!(L2);
        let l3s = &mut *core::ptr::addr_of_mut!(L3S);
        for (slot, l3) in l3s.iter_mut().enumerate() {
            let off = (slot + 1) * KSTACK_STRIDE;
            l2.0[off >> 21] = (l3 as *mut Table as u64) | VALID | TABLE;
            for page in 0..PAGES_PER_STACK {
                let pa = (stacks + slot * KTHREAD_STACK_BYTES + page * PAGE) as u64;
                l3.0[((off >> 12) & 511) + page] =
                    pa | PAGE_DESC | AF | SH_INNER | ATTR_NORMAL_WB | PXN | UXN;
            }
        }
        core::ptr::write(
            l1.add(WINDOW_L1_INDEX),
            (l2 as *mut Table as u64) | VALID | TABLE,
        );
    }
    INSTALLED.store(true, Ordering::Release);
}

/// Per-CPU emergency stacks for the kernel-thread sync entry and the overflow path (16 KiB each,
/// room for the panic report of an ordinary kernel-thread fault).
pub const EMERGENCY_STACK_BYTES: usize = 16384;
pub const EMERGENCY_CPUS: usize = 8;

#[repr(C, align(16))]
pub struct EmergencyStacks(pub [[u8; EMERGENCY_STACK_BYTES]; EMERGENCY_CPUS]);

#[unsafe(no_mangle)]
pub static mut YARM_AARCH64_KSTACK_EMERGENCY: EmergencyStacks =
    EmergencyStacks([[0; EMERGENCY_STACK_BYTES]; EMERGENCY_CPUS]);
