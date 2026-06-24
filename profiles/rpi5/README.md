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
`RPI5_KERNEL_PMEM_OK`, `RPI5_KERNEL_BOOTINFO_OK`), and BOOT-4 then builds a
high-alias-only kernel heap region and re-validates the high-half VM
(`RPI5_KERNEL_GLOBAL_HEAP_OK`, `RPI5_KERNEL_VM_OK`). BOOT-4 then installs a gated
high-half phys↔virt direct-map offset (default identity for QEMU/non-RPi5) and
wires the kernel global allocator to the high-half heap, proving a real
high-half allocation (`RPI5_KERNEL_PHYSMAP_SWITCH_OK`,
`RPI5_KERNEL_GLOBAL_ALLOCATOR_HIGHMAP_OK`). The high-half boot trampoline now
also zeroes the kernel `.bss` before any Rust runs (`RPI5_HH_BSS_CLEAR_BEGIN`,
`RPI5_HH_BSS_CLEAR_DONE`) — the default `_start` always did, but the high-half
path did not, so the frame-allocator spin-lock had a garbage state that hung the
first allocator lock; the global-allocator bring-up now emits per-step markers
(`RPI5_KERNEL_GLOBAL_ALLOCATOR_PT_STORAGE_OK`,
`…_PT_ZERO_DONE`, `…_PT_INIT_DONE`, `…_PROBE_ALLOC_OK`,
`…_PROBE_SENTINEL_OK`). The remaining blocker is now narrowed
to the scheduler/IRQ subsystem that `KernelState` bootstrap needs
(`RPI5_HH5_DEFERRED reason=kernel_state_requires_scheduler_init`, or
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
`driver_manager` and those drivers have a validated hardware path. The
firmware-property scaffold is also mock-only: no `rpi_firmware_srv` binary is
declared, and its entrypoint only logs deferred no-MMIO-grant markers. The
`driver_manager` likewise requires verified sender identity for privileged
requests, exposes only sender-scoped inert resource-query data, and will not
mint fake hardware grants when production hardware control is unavailable. DRS-2
and DRS-2B add only a hosted fake-DTB parser harness for tests, including bounded
parent-bus cell inheritance, minimal fake `ranges` translation, and limited
inert IRQ parsing; it does not parse the live boot DTB. DRS-3 adds an inert
policy-only spawn-plan generator so tests can explain which candidate services
would be eligible or blocked without calling PM, spawning, granting caps, or
touching MMIO. DRS-4 adds a mock spawn-authority decision model that turns those
plans into inert approvals/denials without calling PM/supervisor services,
spawning, granting caps, or touching MMIO. DRS-5 adds mock resource-grant bundle
descriptions; they list inert MMIO/IRQ/DMA/transport/clock/pinmux requirements
but contain no real `CapId`s and perform no grant operations. RP1 GPIO resources
remain PCIe/BAR-relative and deferred rather than direct BCM2712 MMIO. DRS-6 adds
a design-only DM↔PM live-spawn contract in
[`doc/driver-manager-pm-spawn-contract.md`](../../doc/driver-manager-pm-spawn-contract.md):
Driver Manager remains advisory/policy-only, and PM remains responsible for
validation, process creation, address-space setup, accounting, capability
minting, startup-cap delivery, and handles. DRS-7 adds only an inert
`DriverSpawnRequest` model with descriptive resource and startup-cap
requirements. DRS-8 adds an inert PM-validation simulation over those records;
it models PM checks. DRS-9 adds inert PM accounting/rollback simulation with
descriptive reservations and rollback steps. DRS-10 adds inert health and
restart-request modeling. DRS-11 adds inert PM restart validation/accounting and
rollback simulation for those restart requests. DRS-12 adds inert correlation of
mock PM process handles, verified driver registration, PM death notification,
health, and restart requests. DRS-13 adds inert dependency-health and
restart-cascade modeling: PL011 has no hard fake-hosted provider dependency,
RP1 GPIO stays deferred on PCIe/RP1 BAR discovery, mailbox stays deferred on
transport/cache/MMIO policy, irqmux crashes can mark consumers affected, and
dependency cycles fail closed. DRS-14 adds sender-scoped, verified-identity,
bounded, cap-free dependency/cascade readouts for diagnostics only; payload TIDs
are diagnostic, spoofed claims fail closed, and RP1/mailbox still report deferred
state. These stages still do not call PM, spawn/restart, teardown, grant, mint or
revoke caps, allocate address spaces, return handles, or touch MMIO. See the driver inventory in
[`DRIVER_ROADMAP.md`](DRIVER_ROADMAP.md).

## Scope warning

This profile does **not** assert that:

- Raspberry Pi 5 reaches userspace or runs any service today (it does not — it
  halts in the HH5 diagnostic path);
- the listed services have Raspberry Pi 5-specific hardware support;
- device-tree-driven device discovery, MMIO/IRQ resource assignment, or driver
  spawning is implemented (DRS-1 through DRS-5 add only a userspace-only fake
  inventory, fail-closed authorization model, inert read-only resource query
  model, hosted fake-DTB/parser-bus harness, and policy-only spawn-plan model in
  `driver_manager` tests, plus mock spawn-authority decisions and resource-grant
  bundle descriptions);
- the manifest is handed to init; or
- any listed service is spawned because this file exists.

What it *does* assert is audited and conservative: the boot markers above are
real, the manifest paths are real build targets, and the deferred driver work is
inventoried with explicit blockers in `DRIVER_ROADMAP.md`.
