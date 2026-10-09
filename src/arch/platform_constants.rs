// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Kernel-consumed platform constants facade.
//!
//! PR C keeps platform-layout constant usage explicit and centralized on the
//! selected-ISA re-export boundary so kernel code does not reach into
//! ISA-specific modules directly.

pub const MAX_CPUS: usize = super::platform_layout::MAX_CPUS;
pub const MAX_IRQ_LINES: usize = super::platform_layout::MAX_IRQ_LINES;

pub const BOOTSTRAP_CPU_ID: u8 = super::platform_layout::BOOTSTRAP_CPU_ID;
/// The hardware timer deadline, in the selected port's own timer units (see its layout).
pub const BOOTSTRAP_TIMER_DEADLINE_TICKS: u64 =
    super::platform_layout::BOOTSTRAP_TIMER_DEADLINE_TICKS;
/// BL4b — the scheduling quantum, in timer interrupts (see the selected port's layout). A separate
/// constant from the hardware deadline: the two are in incompatible units.
pub const SCHED_QUANTUM_TICKS: u64 = super::platform_layout::SCHED_QUANTUM_TICKS;

pub const KERNEL_BOOTSTRAP_VIRT_BASE: u64 = super::platform_layout::KERNEL_BOOTSTRAP_VIRT_BASE;
pub const KERNEL_BOOTSTRAP_PHYS_BASE: u64 = super::platform_layout::KERNEL_BOOTSTRAP_PHYS_BASE;
pub const KERNEL_LINK_VIRT_BASE: u64 = super::platform_layout::KERNEL_LINK_VIRT_BASE;
pub const NEXT_ANON_PHYS_BASE: u64 = super::platform_layout::NEXT_ANON_PHYS_BASE;
