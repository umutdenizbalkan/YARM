// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

# Driver ABI and service layering audit

This audit records ownership boundaries only. It does not enable drivers, add
startup grants, or claim hardware support.

| Area | Generic ABI | Platform ABI | Generic service | Backend | Bin | Spawn status | Hardware-proven? |
|---|---|---|---|---|---|---|---|
| UART | `uart_abi` | none | `drivers/uart/service.rs`, through `UartDeviceOps` | `drivers/uart/backend/pl011` | `uart_srv` | present for build parity; deferred and not live-spawned | No |
| Raspberry Pi firmware properties | none | `platform/rpi/property_mailbox_abi`; legacy `mailbox_abi` re-export retained | none; dispatch is platform firmware-specific | `drivers/firmware/rpi` mock transport plus deferred mapping holder | none, intentionally | not spawned | No |
| GPIO | `gpio_abi` | none | `drivers/gpio` defines `GpioDeviceOps`; existing pure ABI dispatch is generic over it | RP1 remains at `drivers/rp1_gpio` | `rp1_gpio_srv` | deferred and not live-spawned | No |
| Virtio block | `block_abi` and `blkcache_abi` are generic; `block_backend_abi` is the generic backend contract | none | `drivers/virtio_blk/service.rs`, through `BlockDeviceOps` | `drivers/virtio_blk/backend/virtio`; deterministic in-memory device preserves existing behavior | `virtio_blk_srv` | existing entrypoint/spawn policy unchanged | No production virtio hardware proof |
| Virtio net/GPU | netdev or existing service contracts | none | implementation-specific service modules | `drivers/virtio_net`, `drivers/virtio_gpu` | existing bins | existing project policy unchanged | Not audited here |
| IRQ mux/input/block cache | existing generic ABIs where present | none identified | existing service modules | mock/service-local implementations | existing bins | existing project policy unchanged | Not audited here |

## Audit findings

- The UART ABI is generic and remains top-level. PL011 registers, mock/volatile
  register transports, configuration, and device implementation now live under
  `uart/backend/pl011`. Generic dispatch sees only `UartDeviceOps`.
- The property mailbox protocol is Raspberry Pi / VideoCore firmware-specific.
  Its canonical ABI path is `platform::rpi::property_mailbox_abi`, and its
  client/transport/dispatch path is `drivers::firmware::rpi`. Compatibility
  re-exports preserve the prior `mailbox_abi` and `drivers::mailbox` paths.
- Generic `gpio_abi` remains unchanged. `drivers/gpio` now owns the generic
  `GpioDeviceOps`, error, direction, pin-mode, and pull contract; RP1 implements
  that contract and pure ABI dispatch depends on it rather than an RP1 type.
  The existing `drivers/rp1_gpio` path and public aliases are retained. A future
  move to `drivers/gpio/backend/rp1` is deferred until compatibility review.
- RP1 production remains blocked on PCIe discovery, BAR sizing/validation,
  capability-granted MMIO, interrupt routing, and an explicit startup-grant
  contract. The current backend remains mock/protocol-ready, not hardware-proven.
- The block wire ABIs, status values, request/reply sizes, and blkcache handoff
  remain unchanged. Generic GET_INFO and sector read/write service logic now
  sees only `BlockDeviceOps`; virtio frame, queue, request builder, and the
  existing deterministic in-memory device live under `backend/virtio`.
- `virtio_blk_srv` and its `VIRTIO_BLK_SRV_READY`, GET_INFO, and inline-write
  markers are preserved. This cleanup does not change its existing entrypoint or
  spawn policy and does not make a production virtio transport hardware-proven.
- No `rpi_firmware_srv` bin is added: the scaffold has no live IPC contract or
  startup grant, so a runnable entry would imply integration that does not exist.

## Block-driver test and deferred-work notes

- Trait-backed tests cover GET_INFO geometry and malformed requests, exact sector
  write/read/overwrite behavior, and out-of-range rejection. Existing virtio
  frame golden vectors, three-descriptor queue behavior, request construction,
  memory-device behavior, and blkcache integration tests remain in place.
- The deterministic `VirtioBlkMemoryDevice` remains the backend selected by the
  existing service entrypoint to preserve behavior. A future production backend
  should add actual virtio transport discovery, capability-granted queue memory,
  DMA/coherency policy, feature negotiation, and interrupt/polling policy without
  changing the generic service contract.
- Blkcache behavior, block ABI constants, optional filesystem/FAT gates, and VFS
  mount policy are outside this refactor and remain unchanged.
