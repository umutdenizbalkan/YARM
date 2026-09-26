// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

pub mod ap_dispatch;
pub mod ap_sched;
pub mod boot;
pub mod console;
pub mod context_switch;
pub mod descriptor_tables;
pub mod irq;
pub mod page_table;
pub mod percpu;
pub mod platform_layout;
pub mod smp;
pub(crate) mod smp_trampoline;
pub mod syscall_abi;
pub mod tlb_shootdown;
pub mod trap;
/// QEMU-IRQ3 — the COM1 / I/O APIC external-interrupt witness fixture. Compiled only with
/// `x86_64-uart-irq-witness`; no default or production profile carries it.
#[cfg(all(
    feature = "x86_64-uart-irq-witness",
    not(feature = "hosted-dev"),
    target_arch = "x86_64"
))]
pub mod uart_irq_witness;
pub mod vm_layout;

pub mod topology;
