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
// Conservative allocator floor for the fallback boot memory map. RISC-V QEMU
// `virt` initrd/firmware placement is not currently folded into a computed
// allocator floor, so `align_up(__kernel_end, 2 MiB)` alone is not a safe
// replacement for this target-wide fallback.
pub const NEXT_ANON_PHYS_BASE: u64 = 0x1000_0000;
pub const KERNEL_PHYS_DIRECT_MAP_BYTES: u64 = 512 * 1024 * 1024 * 1024;

pub const MAX_IRQ_LINES: usize = 64;
pub const MAX_CPUS: usize = 64;

pub const BOOTSTRAP_CPU_ID: u8 = 0;
/// BL4b — the HARDWARE timer deadline, in this port's own timer units: a `time` CSR delta on
/// QEMU `virt`'s 10 MHz timebase, so one interrupt period is 100_000 / 10 MHz = 10 ms. The re-arm
/// owner (`timer::rearm_periodic_deadline`) programs `timer::DEFAULT_TICK_INTERVAL`, and the two are
/// held equal at compile time there. Never a scheduling quantum (it was, as `10`, until BL4b).
pub const BOOTSTRAP_TIMER_DEADLINE_TICKS: u64 = 100_000;
/// BL4b — the scheduling QUANTUM, in TIMER INTERRUPTS: the number of periodic interrupts a
/// running task may consume before a tick preempts it (`Timer::new` decrements once per
/// interrupt). The contract on every port is about 100 ms, and never less than one interrupt: ten
/// 10 ms periods — the value this port already ran with.
pub const SCHED_QUANTUM_TICKS: u64 = 10;
pub const PROFILE_IS_PLACEHOLDER: bool = false;

pub const PLIC_MMIO_BASE: usize = 0x0C00_0000;
pub const PLIC_SMODE_CONTEXT_INDEX: usize = 1;
