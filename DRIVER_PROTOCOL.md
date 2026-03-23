# Device Server Delegation Protocol (v1)

This file defines the initial IPC contract between user-space **device servers** and kernel-facing registration/grant paths.

## Naming model

YARM kernel does not contain a privileged "driver" type.
A "driver" is a normal IPC server holding hardware capabilities.
Documentation and user-space layout should prefer `.srv` naming (`usb.srv`, `nvme.srv`, etc.).

## ABI

- Driver server ABI version: `1`
- Kernel exports operation constants in `src/kernel/driver_abi.rs`.

## Operations

- `DRIVER_OP_REGISTER` (1): register a server task id as hardware-capable server.
- `DRIVER_OP_GRANT_IRQ` (2): grant an IRQ capability to a server.
- `DRIVER_OP_GRANT_DMA` (3): grant a DMA-window capability to a server.
- `DRIVER_OP_RESTARTED` (4): notify that a restart token was consumed.

## Safety model

- Kernel validates capability object types and rights.
- DMA windows are bounded by offset/length and page alignment.
- Policy and restart decisions remain in user-space supervisor/manager servers.
- Delegation chain is explicit: init/supervisor delegates capabilities to servers.
