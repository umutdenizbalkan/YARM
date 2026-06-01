// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

// x86_64 QEMU q35/PVH platform layout constants.
//
// This profile is not a placeholder for the supported x86_64 smoke target. It
// deliberately encodes the PVH higher-half/direct-map bootstrap aliases and the
// standard PC-compatible LAPIC/IOAPIC physical MMIO locations used by QEMU q35.
// Boot-time memory availability is still discovered from the PVH memmap; these
// constants are target-specific anchors, not generic ACPI-discovered hardware.

pub const KERNEL_BOOTSTRAP_VIRT_BASE: u64 = 0xFFFF_FF80_0000_0000;
pub const KERNEL_BOOTSTRAP_PHYS_BASE: u64 = 0x0;
pub const KERNEL_PHYS_DIRECT_MAP_BYTES: u64 = 512 * 1024 * 1024 * 1024;
pub const NEXT_ANON_PHYS_BASE: u64 = 0x1000_0000;

// Link base for the kernel image's high-VA suffix.  The linker script
// places .text/.rodata/.data/.bss at VMA = KERNEL_LINK_VIRT_BASE + LMA so
// every link-time absolute address (vtables, function-pointer tables,
// statically-stored fn pointers) lands in the canonical-top-2 GiB window
// that PML4[511]/PDPT[510] maps to physical 0..64 MiB.  Subtracting this
// base from a kernel-image VA yields the corresponding physical address.
pub const KERNEL_LINK_VIRT_BASE: u64 = 0xFFFF_FFFF_8000_0000;

pub const MAX_IRQ_LINES: usize = 64;
pub const MAX_CPUS: usize = 64;

pub const BOOTSTRAP_CPU_ID: u8 = 0;
pub const BOOTSTRAP_TIMER_DEADLINE_TICKS: u64 = 50_000_000;
pub const PROFILE_IS_PLACEHOLDER: bool = false;

// MMIO is reached through the higher-half PML4[511]/PDPT[511] window backed
// by boot_pd_hi, where the 2 MiB pages set PCD (uncacheable) - the right
// caching for LAPIC/IOAPIC. The KERNEL_BOOTSTRAP_VIRT_BASE direct map (1 GiB
// huge pages without PCD) would alias the same physical addresses but with
// write-back caching, which is incorrect for MMIO; we deliberately use the
// PDPT[511] alias instead.
//
// PDPT[511] of PML4[511] starts at VA 0xFFFF_FFFF_C000_0000 and maps phys
// 0xC000_0000+, so phys X (3 GiB <= X < 4 GiB) is at VA 0xFFFF_FFFF_0000_0000 + X.
pub const IOAPIC_MMIO_BASE: usize = 0xFFFF_FFFF_FEC0_0000;
pub const LAPIC_MMIO_BASE: usize = 0xFFFF_FFFF_FEE0_0000;

// Original physical addresses, kept for documentation and tooling that needs
// to know the underlying hardware location.
pub const IOAPIC_MMIO_PHYS: usize = 0xFEC0_0000;
pub const LAPIC_MMIO_PHYS: usize = 0xFEE0_0000;
