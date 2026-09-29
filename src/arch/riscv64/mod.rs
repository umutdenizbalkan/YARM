// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

pub mod boot;
pub mod console;
pub mod context_switch;
pub mod ipi;
pub mod irq;
pub mod page_table;
pub mod platform_layout;
pub mod plic;
pub mod sbi;
/// QEMU-SMP3 — the two-hart IPI / remote-fence / context witness. Compile-time gated by
/// `riscv64-smp3-witness`; no default or production profile carries it.
#[cfg(all(
    feature = "riscv64-smp3-witness",
    not(feature = "hosted-dev"),
    target_arch = "riscv64"
))]
pub mod smp3_witness;
pub mod syscall_abi;
pub mod timer;
pub mod trap;
#[cfg(all(
    feature = "riscv-uart-irq-witness",
    not(feature = "hosted-dev"),
    target_arch = "riscv64"
))]
pub mod uart_irq_witness;
pub mod user_status;
pub mod vm_layout;

pub mod topology;
