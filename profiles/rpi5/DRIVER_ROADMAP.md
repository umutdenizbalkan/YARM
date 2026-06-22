<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Umut Deniz Balkan -->

# Raspberry Pi 5 driver & boot roadmap

Audited status of YARM's Raspberry Pi 5-relevant drivers and the path from the
current high-half (HH) diagnostic boot to a first safe driver spawn. This is a
**conservative audit**, not a feature claim. Hardware bring-up detail is owned by
[`doc/RPI5_BRINGUP.md`](../../doc/RPI5_BRINGUP.md); driver/IRQ contracts live in
[`doc/DRIVER_PROTOCOL.md`](../../doc/DRIVER_PROTOCOL.md) and
[`doc/IRQMUX_CONTRACT.md`](../../doc/IRQMUX_CONTRACT.md).

## 1. Current verified boot status

On real Raspberry Pi 5 hardware the UART log reaches:

- `RPI5_HH3_DONE`
- `RPI5_HH4_BEGIN` → `RPI5_HH4_DTB_PTR_OK value=0x2efec600` →
  `RPI5_HH4_DTB_VIRT_OK value=0xffffff802efec600` → `RPI5_HH4_TTBR0_REPLACE_DONE`
  → `RPI5_HH4_PC_HIGH_OK` / `RPI5_HH4_SP_HIGH_OK` / `RPI5_HH4_VBAR_HIGH_OK` →
  `RPI5_HH4_DONE`
- `RPI5_HH5_BEGIN`

Last confirmed hardware blocker: the high-half flattened-devicetree `/chosen` /
initrd walk faulted (`RPI5_HH5_FAULT_BOUNDARY reason=initrd_dtb_walk`).

Fixed in-tree but **not yet re-confirmed on hardware**: the FDT walker advance
bug (it skipped the 8-byte `FDT_PROP` header), bounded phase markers
(`RPI5_HH5_FDT_HEADER_OK`, `RPI5_HH5_FDT_BLOCKS_OK`, `RPI5_HH5_FDT_CHOSEN_FOUND`,
`RPI5_HH5_FDT_CHOSEN_SCAN_DONE`), a non-fatal missing-initrd path
(`RPI5_HH5_INITRD_FAILED reason=missing`), an allocator-bridge + handoff
descriptor (`RPI5_HH5_ALLOC_BRIDGE_OK`, `RPI5_HH5_HANDOFF_OK`), and a
normal-kernel-entry bridge that brings up a real high-half physical-frame
allocator and boot-info record (`RPI5_HH5_ALLOC_ADAPTER_OK`,
`RPI5_KERNEL_PMEM_OK`, `RPI5_KERNEL_BOOTINFO_OK`). BOOT-4 builds a
high-alias-only kernel heap region, re-validates the high-half VM
(`RPI5_KERNEL_GLOBAL_HEAP_OK`, `RPI5_KERNEL_VM_OK`), then installs a gated
high-half phys↔virt direct-map offset and wires the kernel global allocator to
that heap, proving a high-half allocation (`RPI5_KERNEL_PHYSMAP_SWITCH_OK`,
`RPI5_KERNEL_GLOBAL_ALLOCATOR_HIGHMAP_OK`), before a precise deferral
(`RPI5_HH5_DEFERRED reason=kernel_state_requires_scheduler_init`,
or `reason=initrd_missing`). The earlier `normal_kernel_entry_requires_low_allocator`
blocker is resolved: `PhysicalFrameAllocator` is self-contained and needs no low
direct map, so it runs from the TTBR1-mapped HH heap.

Normal kernel entry, `ENTER_USER`, `/sbin/initramfs_srv`, `devfs`, `vfs`, and
`driver_manager` are **not reached on RPi5 yet**. Everything in the driver
inventory below is therefore *blocked by an earlier boot milestone* on RPi5,
independent of each driver's own maturity.

## 2. Classification legend

1. Present and architecture-neutral
2. Present and Raspberry Pi 5-specific
3. Trait/API scaffold only
4. Mock-safe / unit-testable but not hardware-proven
5. Build-declared service binary
6. Live-spawned in current QEMU/runtime flow
7. Not live-spawned on RPi5 yet
8. Missing hardware integration
9. Blocked by earlier boot milestone

These overlap: a driver is usually several states at once (e.g. a Pi-specific
trait-backed mock that is build-declared but not live-spawned and missing
hardware integration). On RPi5 specifically, **all** userspace drivers are also
state 9 today.

## 3. Driver / service inventory

| Service / module | States | Live in QEMU flow? | RPi5-relevant? | Notes |
| --- | --- | --- | --- | --- |
| `uart_srv` (generic) | 1,3,4,5,7,8 | No (no `image_id`) | Yes | `UartDeviceOps` trait; `run()` logs `UART_SRV_DEFERRED_NO_MMIO_GRANT`. |
| PL011 backend | 2,3,4 | n/a | Yes | `Pl011UartDevice<B: UartRegisterIo>`; DR/FR/IBRD/FBRD/LCRH/CR/IMSC/ICR; MMIO abstracted, mock-safe, nonblocking. |
| `console_driver` bin | 5,7 | No | Indirect | Build-declared bin; its `run()` calls `run_devfs()`. Not in spawn table; the live devfs is `devfs_srv`. |
| mailbox / RPi firmware property (`drivers/firmware/rpi`) | 2,3,4,7,8 | No (no bin) | Yes | `RpiPropertyTransport` trait + property tags; `run()` is empty; real MMIO transport deferred; **no `rpi_firmware_srv` bin**. |
| generic GPIO (`drivers/gpio`) | 1,3 | n/a | Yes | `GpioDeviceOps` trait + pin/mode/pull/direction types; no registers/discovery. |
| `rp1_gpio_srv` | 2,3,4,5,7,8 | No (no `image_id`) | Yes | `Rp1GpioDevice<B: RegisterIo>` over RP1 GPIO_CTRL/SYS_RIO/PADS (54 pins); `run()` logs `RP1_GPIO_SRV_DEFERRED_NO_MMIO_GRANT`. |
| `irqmux_srv` | 1,3,4,5,7,8 | No (no `image_id`) | Yes (software model) | `IrqMuxService` software route/grant authorization; real recv loop; **no GIC / RP1 hardware wiring**. See `doc/IRQMUX_CONTRACT.md`. |
| `driver_manager` | 1,5,6,9 | **Yes** (`image_id=7`) | Yes | Registry + `REGISTER`/`GRANT_IRQ`/`GRANT_DMA`/`RESTARTED`; does **not** parse DTB or spawn drivers. |
| `devfs_srv` | 1,5,6,9 | **Yes** (`image_id=5`) | Yes | Device namespace fs-server; arch-neutral. |
| `vfs_server` | 1,5,6,9 | **Yes** (`image_id=6`) | Yes | VFS routing/mount; arch-neutral. |
| `initramfs_srv` | 1,5,6,9 | **Yes** (`image_id=4`) | Yes | Read-only CPIO access; arch-neutral. |
| `blkcache_srv` | 1,5,6,9 | Yes (`image_id=8`, late) | Yes | Platform-neutral block cache above a future block backend. |
| `ramfs_srv` / `fat_srv` / `ext4_srv` | 1,5,6,9 | Yes (`image_id` 11/10/12) | Later | Filesystem servers; need a real block backend on RPi5. |
| `virtio_blk_srv` / `virtio_gpu_srv` / `virtio_net_srv` / `input_srv` | 1,4,5,6 | Partly (virtio_blk `image_id=9`) | No | QEMU virtio backends; not RPi5 hardware. |
| net stack (`netmgr_srv`/`tcpip_srv`/`socket_srv`/`dhcp_srv`/`dns_srv`) | 1,5 | No | Later | Generic; needs a real RPi5 NIC (RP1 gigabit) backend. |
| SD/eMMC/MMC block driver | — | No | Later | **Missing**: no SDHCI/eMMC driver exists. |
| xHCI / USB host | — | No | Later | **Missing**: no xHCI/USB driver exists. |

## 4. Per-driver missing features

### UART / PL011
- Trait: `UartDeviceOps` (configure 8N1 with integer/fractional divisors, FIFO,
  TX/RX, nonblocking, `clear_interrupts`). Register I/O abstracted behind
  `UartRegisterIo`; MMIO-safe and mock-testable; not tied to QEMU only.
- Missing: DTB node discovery (`reg`/`interrupts`/`clocks`), baud→divisor clock
  policy (divisors are passed in, not computed), pinmux ownership, and a
  capability-granted MMIO mapping. Interrupt-driven RX has no IRQ delivery path
  (depends on irqmux + a real interrupt domain). `uart_srv` is not live-spawned
  and embeds no Pi UART address. Kernel early console is a separate facility and
  must never depend on `uart_srv`.

### Mailbox / RPi firmware property
- Present: `RpiPropertyTransport` trait, `RpiFirmwareClient`, property tags
  (firmware/board model/revision/serial, ARM/VC memory). Hosted mock validates
  request/response in place. Compatibility `mailbox` aliases re-export it.
- Missing: real MMIO mailbox transport (channel-8 doorbell/status polling), bus
  vs. physical address translation, cache maintenance and 16-byte buffer
  alignment, and post-HH4 high-VA safety. No service binary is declared and
  `run()` is empty — it is a scaffold, not a driver. Note that BCM2712/Pi 5
  firmware-interface specifics differ from the legacy VideoCore mailbox and must
  be validated against the Pi 5 firmware before any transport is enabled.

### Generic GPIO + RP1 GPIO
- Present: `GpioDeviceOps` trait; `Rp1GpioDevice<B>` implements it over the RP1
  GPIO_CTRL / SYS_RIO (atomic SET/CLR/XOR aliases) / PADS banks for 54 pins;
  direction, level read/write, pull, and alternate functions 1–4 / 6–8 are
  device-level and mock-tested with exact offsets (BAR-relative, not physical).
- Missing: RP1 **PCIe discovery and BAR sizing/validation**, capability-
  controlled MMIO mapping/grant, an explicit startup contract carrying that
  grant, and interrupt config/status/ack (absent from both ABI v1 and the
  verified register model). `rp1_gpio_srv` is mock/protocol-ready, **not
  hardware-proven**, and not live-spawned.

### irqmux
- Present: a software interrupt-routing/authorization model — routes (line →
  vector, trigger, polarity, owner, target, enable/mask) and grants with
  REGISTER/BIND/ENABLE/MASK/ACK rights, bounded to 32 each, with an IPC recv
  loop. Safe before hardware interrupts are enabled (it performs no MMIO).
- Missing: a concrete interrupt-domain model bound to real hardware — GIC-400 /
  GICv2 on BCM2712, and RP1's own interrupt aggregation delivered over PCIe MSI.
  It does not yet deliver real interrupt notifications/caps to drivers from
  hardware. Not live-spawned.

### driver_manager
- Present and **live-spawned in the QEMU flow** (`image_id=7`): a driver
  registry with `REGISTER`, `GRANT_IRQ`, `GRANT_DMA`, `RESTARTED`, backed by
  kernel runtime ops that mint/grant IRQ and DMA-region capabilities and restart
  tasks. Emits `DRIVER_MANAGER_READY`.
- Missing for an RPi5 first driver spawn: it does **not** parse the DTB, does
  **not** enumerate or classify devices (`DriverClass::Unknown` only; MMIO/IOPORT
  bind opcodes, device-enumeration, and heartbeat/watchdog are explicit TODOs),
  and does **not** spawn driver binaries (PM remains the spawn authority). On
  RPi5 it is additionally blocked because userspace is not reached.

## 5. Driver-manager integration plan (RPi5)

1. Reach userspace on RPi5 (see milestones below) so `driver_manager` is spawned
   at all.
2. Give `driver_manager` (or a platform-inventory step before it) the DTB high
   virtual pointer already proven at HH4 (`RPI5_HH4_DTB_VIRT_OK`), and a
   read-only device inventory derived from it — **without** starting any
   hardware-heavy driver.
3. Extend the driver ABI/registry with device records (compatible string, MMIO
   region, IRQ line, DMA constraints) instead of the current `Unknown` class.
4. Only then register/grant resources to a driver and let PM spawn it, gated per
   the safe-driver ordering in milestone RPi5-DRV-2.

## 6. Hardware-reference notes (conceptual, no GPL code copied)

- **RP1** is the Pi 5 I/O controller (GPIO, UART, I²C, SPI, PWM, SD/eMMC, USB,
  Ethernet) attached to BCM2712 over a PCIe 2.0 ×4 link; peripherals are reached
  through a PCIe BAR window, not through assumed BCM2712 MMIO. Any RP1 driver
  must first discover RP1 over PCIe, size/validate the BAR, and receive a
  capability-granted MMIO mapping. RP1 GPIO uses per-bank GPIO_CTRL/STATUS, an
  atomic SYS_RIO block (direct/XOR/SET/CLR alias pages), and PADS cells.
  (Source: Raspberry Pi RP1 peripherals datasheet; `pinctrl-rp1` concepts only.)
- **PL011** UART: standard ARM PrimeCell registers (DR, FR with TXFF/RXFE,
  IBRD/FBRD for the baud divisor, LCRH for 8N1/FIFO, CR for enable, IMSC/ICR for
  interrupts). Baud divisor depends on the UART reference clock, which on Pi 5 is
  supplied via RP1/clock tree and must come from DTB, not be hard-coded.
- **Firmware property mailbox**: ARM↔firmware property channel; message buffers
  use bus addresses (physical aliased at `0x4000_0000` when L2 is enabled),
  require 16-byte alignment, and need cache flush/invalidate around the
  transaction. Pi 5 (BCM2712) differs from earlier VideoCore parts; validate
  against current firmware. (Source: raspberrypi/firmware mailbox property wiki.)
- **DTB**: the firmware passes the DTB physical pointer in `x0` (preserved by
  YARM through `x20` into HH4); `/chosen` carries `bootargs` and optional
  `linux,initrd-start` / `linux,initrd-end` (end exclusive), cell width per the
  root `#address-cells`. UART/GPIO/interrupt-controller/mailbox nodes provide the
  `reg`/`interrupts`/`clocks` needed for resource assignment.

## 7. Milestone roadmap

Boot path to userspace:

- **RPi5-BOOT-1** — Finish HH5 FDT `/chosen` and initrd discovery (walker
  advance fix + bounded phase markers). *In-tree; needs hardware confirmation.*
- **RPi5-BOOT-2** — Initrd/load bridge works after the HH4 no-low-VA TTBR0
  retirement (allocator bridge + handoff descriptor at a high VA). *In-tree;
  needs hardware confirmation.*
- **RPi5-BOOT-3** — Enter normal kernel bootstrap from the HH5 handoff. *Partly
  done in-tree:* a high-half-safe `PhysicalFrameAllocator` + boot-info record now
  run from the HH heap (`RPI5_KERNEL_PMEM_OK`, `RPI5_KERNEL_BOOTINFO_OK`),
  resolving the low-allocator blocker. Needs hardware confirmation.
- **RPi5-BOOT-4** — Global heap + kernel VM + global-allocator highmap toward
  `KernelState`. *Done in-tree:* a high-alias-only kernel heap region is built,
  the high-half VM re-validated (`RPI5_KERNEL_GLOBAL_HEAP_OK`,
  `RPI5_KERNEL_VM_OK`), and a gated high-half phys↔virt direct-map offset (0 ==
  identity for QEMU/default) is installed so the kernel global allocator now
  hands out high-half memory — proven by a real allocation probe
  (`RPI5_KERNEL_PHYSMAP_SWITCH_OK`, `RPI5_KERNEL_GLOBAL_ALLOCATOR_HIGHMAP_OK`).
  *Remaining:* `KernelState` bootstrap needs the scheduler/IRQ subsystem, not yet
  brought up on this path (`reason=kernel_state_requires_scheduler_init`). Needs
  hardware confirmation.
- **RPi5-BOOT-5** — Reach `/sbin/initramfs_srv`. *Open.*
- **RPi5-BOOT-6** — Reach `devfs` + `vfs` + `driver_manager`
  (`DRIVER_MANAGER_READY`) on RPi5. *Open.*

Driver path (each strictly after RPi5-BOOT-6):

- **RPi5-DRV-1** — `driver_manager` consumes the DTB/platform inventory and
  registers devices **without** starting hardware-heavy drivers.
- **RPi5-DRV-2** — Spawn/register safe early drivers, in order:
  1. `uart_srv` / console only once the PL011 backend gets a DTB-derived,
     capability-granted MMIO region and clock/divisor policy;
  2. mailbox/firmware service only after the property transport is validated on
     Pi 5;
  3. `irqmux` only after an interrupt-domain model (GIC + RP1) is defined;
  4. `rp1_gpio_srv` only after RP1 PCIe discovery, MMIO mapping, and the
     pin/line model are validated.
- **RPi5-DRV-3** — Storage path: SD/eMMC/MMC driver → `blkcache_srv` → `ramfs` /
  `fat` / `ext4` once a block backend exists.
- **RPi5-DRV-4** — Later: xHCI/USB; network device + `netmgr`/`tcpip`/`socket`;
  richer DTB matching and hotplug policy.

## 8. TODO

- [ ] Confirm RPi5-BOOT-1/2 on real hardware (expect `RPI5_HH5_FDT_CHOSEN_FOUND`,
      `RPI5_HH5_INITRD_FAILED reason=missing` or initrd range, `RPI5_HH5_HANDOFF_OK`,
      `RPI5_HH5_DEFERRED`).
- [x] High-half-safe boot allocator + boot-info record (RPi5-BOOT-3, allocator
      half): `PhysicalFrameAllocator` now runs from the HH heap via its high
      alias (`RPI5_KERNEL_PMEM_OK`, `RPI5_KERNEL_BOOTINFO_OK`).
- [x] High-half kernel heap region + VM re-validation (RPi5-BOOT-4 first half):
      `RPI5_KERNEL_GLOBAL_HEAP_OK`, `RPI5_KERNEL_VM_OK`.
- [x] Gated high-half phys↔virt direct map (RPi5-BOOT-4): the kernel global
      allocator is wired to the BOOT-4 heap region and proven via a high-half
      allocation probe (`RPI5_KERNEL_GLOBAL_ALLOCATOR_HIGHMAP_OK`); QEMU/default
      stays identity.
- [ ] Bring up the scheduler/IRQ subsystem so `KernelState` can be built / init
      loaded, and stop deferring at `kernel_state_requires_scheduler_init`
      (RPi5-BOOT-4 remainder → ENTER_USER).
- [ ] Define the RP1 PCIe discovery + BAR grant path before any RP1 driver.
- [ ] Define the interrupt-domain model (GIC + RP1 over PCIe MSI) before irqmux
      hardware wiring.
- [ ] Extend the driver ABI/registry beyond `DriverClass::Unknown` with device
      records (compatible, MMIO, IRQ, DMA).
- [x] Restored the `doc/driver-layering-audit.md` referenced by
      `crates/yarm-driver-servers/src/lib.rs` tests; its `include_str!` target was
      missing, which had broken that crate's lib-test compilation.
- [ ] Keep `services-core.manifest` minimal until RPi5 reaches `driver_manager`.
