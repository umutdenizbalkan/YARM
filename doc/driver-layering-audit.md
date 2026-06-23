<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Umut Deniz Balkan -->

# Driver layering audit

Audit of how YARM's userspace driver servers separate a backend-neutral service
contract from hardware/IP-block-specific backends, and which paths are
mock-tested versus hardware-proven. This document is referenced by compile-time
guard tests in `crates/yarm-driver-servers/src/lib.rs`; keep the marker phrases
below intact when editing.

Raspberry Pi 5-specific status and the boot/driver milestone roadmap live in
[`profiles/rpi5/DRIVER_ROADMAP.md`](../profiles/rpi5/DRIVER_ROADMAP.md). This
file is the cross-architecture layering audit.

## Layering principle

Each driver server splits into:

- a generic, backend-neutral operations trait and ABI dispatch that is pure and
  mock-testable (no syscalls, no MMIO construction, no startup-slot access); and
- one or more explicitly namespaced hardware backends that implement the trait
  over a register-I/O abstraction.

The generic service layer must never import a concrete backend type. Hosted
(`hosted-dev`) builds compile out volatile MMIO and can only use trait-backed
mocks.

## UART

- Generic contract: `UartDeviceOps` in `drivers/uart/service.rs` (8N1 divisor
  configuration, byte/slice TX, nonblocking RX, interrupt clear). The service
  imports no `Pl011`/`UartDevice` concrete type.
- Backend: `drivers/uart/backend/pl011` provides `Pl011UartDevice<B:
  UartRegisterIo>` and the PL011 register model. MMIO is abstracted behind
  `UartRegisterIo`; the volatile backend is excluded from hosted builds.
- `uart_srv` is **not live-spawned**: `run()` logs
  `UART_SRV_DEFERRED_NO_MMIO_GRANT` and consumes no MMIO. It is mock/protocol-
  ready and **not hardware-proven**; it needs DTB discovery, clock/divisor
  policy, pinmux ownership, and a capability-granted MMIO mapping.

## Mailbox / Raspberry Pi firmware property interface

- The canonical scaffold lives under `drivers/firmware/rpi`
  (Raspberry Pi / VideoCore firmware property mailbox); `drivers/mailbox`
  retains compatibility aliases only.
- It defines an `RpiPropertyTransport` trait, a firmware client, and property
  tags, with a deterministic hosted mock transport. The real MMIO transport is
  deferred and the service `run()` emits `RPI_FIRMWARE_SRV_ENTRY` followed by
  `RPI_FIRMWARE_SRV_DEFERRED_NO_MMIO_GRANT`.
- **No `rpi_firmware_srv` bin is added**; the crate manifest declares no such
  binary and the crate root exposes no firmware `run_*` entrypoint. The scaffold
  is **not live-spawned** and **not hardware-proven**.

## GPIO / RP1 GPIO

- Generic contract: `GpioDeviceOps` in `drivers/gpio/mod.rs` (function/mode,
  direction, level read/write, pull). No register layout, address, or discovery
  lives in the generic layer.
- Backend: `drivers/rp1_gpio` provides `Rp1GpioDevice<B: RegisterIo>` that
  implements `GpioDeviceOps` over the RP1 GPIO_CTRL / SYS_RIO / PADS banks
  (BAR-relative offsets, mock-tested). The service `dispatch` is generic over
  `GpioDeviceOps` and does not name `Rp1GpioDevice`.
- A future generic backend home `drivers/gpio/backend/rp1` is **not** created
  yet; RP1 logic stays under `drivers/rp1_gpio` until a second GPIO backend
  justifies the move.
- `rp1_gpio_srv` is **not live-spawned** (`run()` logs
  `RP1_GPIO_SRV_DEFERRED_NO_MMIO_GRANT`). It is mock/protocol-ready and **not
  hardware-proven**. Production use is blocked on RP1 **PCIe discovery**, BAR
  sizing/validation, capability-controlled **MMIO mapping/grant**, interrupt
  routing, and an explicit startup contract carrying that grant.

## Block (virtio) and filesystem gates

- Generic contract: `BlockDeviceOps` (via `yarm_ipc_abi::block_backend_abi`) in
  `drivers/virtio_blk/service.rs`; the service does not name the virtq chain or
  the concrete `VirtioBlkMemoryDevice` backend type.
- Backend: `drivers/virtio_blk/backend/virtio` implements `BlockDeviceOps`; the
  live service emits `VIRTIO_BLK_SRV_READY`. This is a QEMU virtio path, not
  Raspberry Pi 5 hardware.
- Block writes feed the **FAT gates** and the wider filesystem servers
  (`fat_srv`, `ext4_srv`, `ramfs_srv`) above a block-cache layer; on Raspberry
  Pi 5 these remain blocked until a real SD/eMMC block backend exists.

## irqmux

- `irqmux_srv` (`drivers/irqmux`) is a software interrupt route/grant
  authorization model with a real IPC receive loop. It performs no MMIO and is
  safe before hardware interrupts are enabled, but has no GIC or RP1 hardware
  wiring and is **not live-spawned** in the current spawn table. See
  [`IRQMUX_CONTRACT.md`](IRQMUX_CONTRACT.md).

## driver_manager

- `driver_manager` (control-plane) is **live-spawned in the QEMU flow**
  (`image_id=7`) as a driver registry handling `REGISTER` / `GRANT_IRQ` /
  `GRANT_DMA` / `RESTARTED`. It now also has an inert userspace-only
  `PlatformInventory` / `DeviceRecord` model for future RPi5 candidates
  (`Uart`, `Mailbox`, `Gpio`, `IrqMux`, `Block`, `Unknown`) with compatible
  strings, MMIO ranges, IRQs, candidate driver names, and deferred status. The DRS-1B hardening makes privileged requests fail closed: verified
  `sender_tid` metadata is required, payload TIDs are diagnostic only, inventory
  records authorize IRQ/MMIO/DMA requests before any runtime call, and production
  no-op hardware control returns errors instead of dummy `CapId(0)` grants. It
  does not parse the DTB and does not spawn driver binaries. See
  [`DRIVER_PROTOCOL.md`](DRIVER_PROTOCOL.md).

## Summary

The generic/backend split is in place for UART, GPIO, and block; the RPi
firmware mailbox is a transport scaffold. DRS-1 audited every declared
`yarm-driver-servers` binary (`blkcache_srv`, `console_driver`, `input_srv`,
`irqmux_srv`, `uart_srv`, `virtio_blk_srv`, `virtio_gpu_srv`, `virtio_net_srv`,
`rp1_gpio_srv`) and the RPi-facing modules (`uart`, PL011, `mailbox`/firmware,
`gpio`, RP1 GPIO, `irqmux`). Of the Raspberry Pi 5-relevant
servers, none are live-spawned with real hardware integration: they are
mock/protocol-ready and **not hardware-proven**, pending the platform discovery,
MMIO-grant, and interrupt-domain work tracked in
[`profiles/rpi5/DRIVER_ROADMAP.md`](../profiles/rpi5/DRIVER_ROADMAP.md).
