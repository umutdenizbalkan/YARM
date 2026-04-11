// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

// AArch64 prototype platform layout constants.
pub const KERNEL_BOOTSTRAP_VIRT_BASE: u64 = 0x4008_0000;
pub const KERNEL_BOOTSTRAP_PHYS_BASE: u64 = 0x4008_0000;
pub const NEXT_ANON_PHYS_BASE: u64 = 0x5000_0000;
pub const KERNEL_PHYS_DIRECT_MAP_BYTES: u64 = 512 * 1024 * 1024 * 1024;

pub const MAX_IRQ_LINES: usize = 64;
pub const MAX_CPUS: usize = 64;

pub const BOOTSTRAP_CPU_ID: u8 = 0;
pub const BOOTSTRAP_TIMER_DEADLINE_TICKS: u64 = 3_125_000;
pub const PROFILE_IS_PLACEHOLDER: bool = true;

pub const GIC_CPU_IF_BASE: usize = 0x0801_0000;
