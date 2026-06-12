// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

# Driver ABI and service layering audit

This audit records ownership boundaries only. It does not enable drivers, add
startup grants, or claim hardware support.

| Area | Generic ABI | Platform ABI | Generic service | Backend | Bin | Spawn status | Hardware-proven? |
|---|---|---|---|---|---|---|---|
| UART | `uart_abi` | none | `drivers/uart/service.rs`, through `UartDeviceOps` | `drivers/uart/backend/pl011` | `uart_srv` | present for build parity; deferred and not live-spawned | No |
| Raspberry Pi firmware properties | none | `platform/rpi/property_mailbox_abi`; legacy `mailbox_abi` re-export retained | none; dispatch is platform firmware-specific | `drivers/firmware/rpi` mock transport plus deferred mapping holder | none, intentionally | not spawned | No |
| GPIO | `gpio_abi` | none | RP1 service currently lives with the implementation | `drivers/rp1_gpio` | `rp1_gpio_srv` | deferred and not live-spawned | No |
| Virtio block | block/backend ABIs | none | currently coupled to its virtio memory device | `drivers/virtio_blk` | `virtio_blk_srv` | existing project policy unchanged | Not audited here |
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
- Generic `gpio_abi` contains GPIO operations and may remain generic. The RP1
  implementation is already explicitly named `drivers/rp1_gpio`; moving it to
  `drivers/gpio/backend/rp1` is deferred because it would add churn without
  improving the current public boundary. Its service still imports the RP1
  device trait, so a future low-risk cleanup should introduce a generic GPIO
  device-ops trait before moving directories.
- The virtio block service also imports its concrete backend. Separating that
  existing implementation is outside this Raspberry Pi/PL011 cleanup and is a
  deferred generic-driver audit item.
- No `rpi_firmware_srv` bin is added: the scaffold has no live IPC contract or
  startup grant, so a runnable entry would imply integration that does not exist.
