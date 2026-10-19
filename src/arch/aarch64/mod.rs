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

/// AP-SCHED3 §B — THIS CPU's LOGICAL id, the scheduler's `CpuId`.
///
/// Read from `TPIDR_EL1`, which each CPU writes exactly once at its own entry, before any
/// exception can be taken there: the boot CPU writes 0 at `_start`, and a secondary writes the
/// logical id the boot CPU planned for it (`aarch64_cpu_admission::plan`), which it then
/// validates against its own MPIDR_EL1 affinity before it touches shared state. The register is
/// EL1-only, so no task can change it, and it survives every exception and `eret`.
///
/// This replaces `MPIDR_EL1 & 0xff`, which named a CPU by `Aff0` alone: it assumed logical =
/// hardware identity and would alias `0x000` and `0x100` (two CPUs in two clusters) onto one id.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
#[inline]
pub fn this_cpu_index() -> usize {
    let id: u64;
    unsafe {
        core::arch::asm!("mrs {0}, TPIDR_EL1", out(reg) id, options(nomem, nostack, preserves_flags));
    }
    id as usize
}

#[cfg(any(feature = "hosted-dev", not(target_arch = "aarch64")))]
#[inline]
pub fn this_cpu_index() -> usize {
    0
}
