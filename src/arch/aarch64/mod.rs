// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

pub mod boot;
pub mod console;
pub mod context_switch;
pub mod dtb;
pub mod irq;
pub mod page_table;
pub mod platform_layout;
pub mod syscall_abi;
pub mod trap;
pub mod vm_layout;

pub mod topology;

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
#[inline]
pub fn read_mpidr_el1() -> u64 {
    let mpidr: u64;
    unsafe {
        core::arch::asm!("mrs {0}, MPIDR_EL1", out(reg) mpidr, options(nomem, preserves_flags));
    }
    mpidr
}

#[cfg(any(feature = "hosted-dev", not(target_arch = "aarch64")))]
#[inline]
pub fn read_mpidr_el1() -> u64 {
    0
}
