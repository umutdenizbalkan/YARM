<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Umut Deniz Balkan -->

# Raspberry Pi 5 profile

This directory is the Raspberry Pi 5 **service-profile** scaffold: a strict
example service list plus a driver/boot roadmap. It is documentation and
prospective profile data only. It does **not** enable a Raspberry Pi 5 driver
stack and it does **not** cause any service to be spawned.

Hardware bring-up detail (the high-half boot stages, markers, and scripts) is
owned by [`doc/RPI5_BRINGUP.md`](../../doc/RPI5_BRINGUP.md). This file does not
duplicate it; it only summarizes status and points at the driver roadmap.

## Current real-hardware status (conservative)

YARM boots farther on real Raspberry Pi 5 than earlier revisions of this profile
claimed, but it does **not** yet reach normal kernel entry, userspace, or live
driver spawning. The high-half (HH) diagnostic path is what runs on hardware
today.

Proven on real RPi5 hardware (UART log markers):

- `RPI5_HH3_DONE` — high-half register/UART proof complete.
- `RPI5_HH4_BEGIN` … `RPI5_HH4_DONE` — DTB pointer preserved
  (`RPI5_HH4_DTB_PTR_OK value=0x2efec600`, `RPI5_HH4_DTB_VIRT_OK
  value=0xffffff802efec600`), low TTBR0 identity map retired, high PC/SP/VBAR
  validated, UART still alive after the TTBR0 replacement.
- `RPI5_HH5_BEGIN` — high-half initrd/allocator bridge stage entered.

Last confirmed hardware blocker: HH5 stopped inside the flattened-devicetree
`/chosen` / initrd walk (`RPI5_HH5_FAULT_BOUNDARY reason=initrd_dtb_walk`).

In-tree but **not yet re-confirmed on hardware**: the FDT walker advance bug was
fixed and the walk now emits precise phase markers
(`RPI5_HH5_FDT_HEADER_OK`, `RPI5_HH5_FDT_BLOCKS_OK`, `RPI5_HH5_FDT_CHOSEN_FOUND`,
…), a missing initrd is non-fatal (`RPI5_HH5_INITRD_FAILED reason=missing`), and
the bridge now brings up a **real high-half physical-frame allocator** in the
TTBR1-mapped HH heap plus a boot-info record (`RPI5_HH5_ALLOC_ADAPTER_OK`,
`RPI5_KERNEL_PMEM_OK`, `RPI5_KERNEL_BOOTINFO_OK`). The old
`normal_kernel_entry_requires_low_allocator` deferral is therefore replaced; the
remaining blocker is the kernel global heap / full VM layout
(`RPI5_HH5_DEFERRED reason=kernel_bootstrap_requires_global_heap_and_full_vm`, or
`reason=initrd_missing` when no initrd is present). This still needs a hardware
run to confirm. See `doc/RPI5_BRINGUP.md` and
[`DRIVER_ROADMAP.md`](DRIVER_ROADMAP.md).

## First userspace target

Normal kernel bootstrap, `ENTER_USER`, and `/sbin/initramfs_srv` are **not yet
reached on RPi5**. The blocker is that the normal kernel bootstrap still requires
a low-physical frame allocator and low identity mappings that HH4 deliberately
retired. The next milestone is a high-half handoff into normal kernel init; see
the milestone ladder in [`DRIVER_ROADMAP.md`](DRIVER_ROADMAP.md).

## `services-core.manifest`

`services-core.manifest` is a strict MANIFEST-1 v1 example:

- UTF-8 text; one absolute service path per nonempty line;
- blank lines and full-line `#` comments only; no inline comments;
- no duplicate, relative, or whitespace-containing paths.

It intentionally lists only services that already have a build-declared binary
and an `image_id` in the runtime spawn table (`initramfs_srv`, `devfs_srv`,
`vfs_server`, `driver_manager`, `blkcache_srv`). It is **not** a startup-order
claim and is **not** consumed by init today — no code selects or applies it.
Raspberry Pi-specific driver binaries (`uart_srv`, `irqmux_srv`, `rp1_gpio_srv`)
are deliberately **omitted** until RPi5 userspace actually reaches
`driver_manager` and those drivers have a validated hardware path. See the
driver inventory in [`DRIVER_ROADMAP.md`](DRIVER_ROADMAP.md).

## Scope warning

This profile does **not** assert that:

- Raspberry Pi 5 reaches userspace or runs any service today (it does not — it
  halts in the HH5 diagnostic path);
- the listed services have Raspberry Pi 5-specific hardware support;
- device-tree-driven device discovery, MMIO/IRQ resource assignment, or driver
  spawning is implemented;
- the manifest is handed to init; or
- any listed service is spawned because this file exists.

What it *does* assert is audited and conservative: the boot markers above are
real, the manifest paths are real build targets, and the deferred driver work is
inventoried with explicit blockers in `DRIVER_ROADMAP.md`.
