// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

// AArch64 QEMU `virt` platform layout constants.
//
// This profile is concrete for YARM's supported AArch64 smoke target:
// QEMU `virt`, RAM at 0x4000_0000, direct `-kernel` load at 0x4008_0000,
// and GICv2 CPU interface at 0x0801_0000. Early boot parses the DTB for RAM,
// initrd, CPU bitmap, PSCI conduit, and GIC handoff where available; these
// constants remain fallback/static bootstrap anchors rather than placeholders.
pub const KERNEL_BOOTSTRAP_VIRT_BASE: u64 = 0x4008_0000;
pub const KERNEL_BOOTSTRAP_PHYS_BASE: u64 = 0x4008_0000;
pub const KERNEL_LINK_VIRT_BASE: u64 = 0x0;
pub const NEXT_ANON_PHYS_BASE: u64 = 0x5000_0000;
pub const KERNEL_PHYS_DIRECT_MAP_BYTES: u64 = 512 * 1024 * 1024 * 1024;

pub const MAX_IRQ_LINES: usize = 64;
pub const MAX_CPUS: usize = 64;

pub const BOOTSTRAP_CPU_ID: u8 = 0;
pub const BOOTSTRAP_TIMER_DEADLINE_TICKS: u64 = 3_125_000;
pub const PROFILE_IS_PLACEHOLDER: bool = false;

pub const GIC_CPU_IF_BASE: usize = 0x0801_0000;
