// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

// x86_64 platform layout constants.

pub const KERNEL_BOOTSTRAP_VIRT_BASE: u64 = 0xFFFF_8000_0000_0000;
pub const KERNEL_BOOTSTRAP_PHYS_BASE: u64 = 0x0;
pub const KERNEL_PHYS_DIRECT_MAP_BYTES: u64 = 512 * 1024 * 1024 * 1024;
pub const NEXT_ANON_PHYS_BASE: u64 = 0x1000_0000;

pub const MAX_IRQ_LINES: usize = 64;
pub const MAX_CPUS: usize = 64;

pub const BOOTSTRAP_CPU_ID: u8 = 0;
pub const BOOTSTRAP_TIMER_DEADLINE_TICKS: u64 = 50_000_000;
pub const PROFILE_IS_PLACEHOLDER: bool = false;

pub const IOAPIC_MMIO_BASE: usize = 0xFEC0_0000;
pub const LAPIC_MMIO_BASE: usize = 0xFEE0_0000;
