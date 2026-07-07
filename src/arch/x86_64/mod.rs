// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

pub mod ap_dispatch;
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
pub mod vm_layout;

pub mod topology;
