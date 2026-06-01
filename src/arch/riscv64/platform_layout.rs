// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

// RISC-V 64 QEMU `virt`/OpenSBI platform layout constants.
//
// This profile is concrete for YARM's supported riscv64 smoke target. It keeps
// the fixed QEMU `virt` PLIC base/context and current bootstrap VA/PA anchors;
// production firmware-table discovery remains future work, but the constants are
// not placeholders for the current target.

pub const KERNEL_BOOTSTRAP_VIRT_BASE: u64 = 0xFFFF_0000;
pub const KERNEL_BOOTSTRAP_PHYS_BASE: u64 = 0x0;
pub const KERNEL_LINK_VIRT_BASE: u64 = 0x0;
pub const NEXT_ANON_PHYS_BASE: u64 = 0x1000_0000;
pub const KERNEL_PHYS_DIRECT_MAP_BYTES: u64 = 512 * 1024 * 1024 * 1024;

pub const MAX_IRQ_LINES: usize = 64;
pub const MAX_CPUS: usize = 64;

pub const BOOTSTRAP_CPU_ID: u8 = 0;
pub const BOOTSTRAP_TIMER_DEADLINE_TICKS: u64 = 10;
pub const PROFILE_IS_PLACEHOLDER: bool = false;

pub const PLIC_MMIO_BASE: usize = 0x0C00_0000;
pub const PLIC_SMODE_CONTEXT_INDEX: usize = 1;
