// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

pub mod boot;
pub mod console;
pub mod context_switch;
/// QEMU-CONTEXT1 §3 — the execution-state witness's kernel hooks. Compiled only with
/// `context1-witness`; no default or production profile carries it.
#[cfg(all(
    feature = "context1-witness",
    not(feature = "hosted-dev"),
    target_arch = "aarch64"
))]
pub mod context_witness;
pub mod dtb;
pub mod irq;
/// QEMU-SMP3-ACCEPTANCE §2 — the overtaken-deferral witness's AArch64 provisioning. Built only with
/// `aarch64-overtaken-witness`; no default or production profile carries it.
#[cfg(all(
    feature = "aarch64-overtaken-witness",
    not(feature = "hosted-dev"),
    target_arch = "aarch64"
))]
pub mod overtaken_witness;
pub mod page_table;
/// QEMU-IRQ2 — the PL011 external-interrupt witness fixture. Compiled only with
/// `aarch64-pl011-irq-witness`; no default or production profile carries it.
#[cfg(all(
    feature = "aarch64-pl011-irq-witness",
    not(feature = "hosted-dev"),
    target_arch = "aarch64"
))]
pub mod pl011_irq_witness;
pub mod platform_layout;
/// QEMU-SMP2 — AP interrupt/dispatch wiring on the existing GICv2 (default off:
/// `yarm.ap_user_dispatch=1`).
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
pub mod smp;
/// QEMU-SMP2 — the two-CPU witness's programs, provisioning and dump. Compiled only with
/// `aarch64-smp2-witness`; no default or production profile carries it.
#[cfg(all(
    feature = "aarch64-smp2-witness",
    not(feature = "hosted-dev"),
    target_arch = "aarch64"
))]
pub mod smp2_witness;
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
