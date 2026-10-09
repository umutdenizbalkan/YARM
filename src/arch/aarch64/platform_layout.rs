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
// Conservative allocator floor for anonymous/page-table frames. Do not derive
// this from `align_up(__kernel_end, 2 MiB)` alone: QEMU `virt` can place the
// DTB/initrd above the kernel image, and those boot ranges must stay reserved.
pub const NEXT_ANON_PHYS_BASE: u64 = 0x5000_0000;
pub const KERNEL_PHYS_DIRECT_MAP_BYTES: u64 = 512 * 1024 * 1024 * 1024;

pub const MAX_IRQ_LINES: usize = 64;
pub const MAX_CPUS: usize = 64;

pub const BOOTSTRAP_CPU_ID: u8 = 0;
/// BL4b — the HARDWARE timer deadline, in this port's own timer units: the EL1 physical timer's
/// `CNTP_TVAL_EL0` down-count (`irq::program_timer_deadline`). QEMU `virt` runs the generic timer
/// at 62.5 MHz (`CNTFRQ_EL0`), so one interrupt period is 3_125_000 / 62.5 MHz = 50 ms.
/// Programmed by the timer route's single re-arm; never a scheduling quantum.
pub const BOOTSTRAP_TIMER_DEADLINE_TICKS: u64 = 3_125_000;
/// BL4b — the scheduling QUANTUM, in TIMER INTERRUPTS: the number of periodic interrupts a
/// running task may consume before a tick preempts it (`Timer::new` decrements once per
/// interrupt). The contract on every port is about 100 ms, and never less than one interrupt: two
/// 50 ms periods.
pub const SCHED_QUANTUM_TICKS: u64 = 2;
pub const PROFILE_IS_PLACEHOLDER: bool = false;

pub const GIC_CPU_IF_BASE: usize = 0x0801_0000;
