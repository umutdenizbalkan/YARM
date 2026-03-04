# Driver Manager Protocol (v1)

This file defines the initial IPC contract between user-space driver manager services and kernel-facing registration/grant paths.

## ABI

- Driver server ABI version: `1`
- Kernel exports operation constants in `src/kernel/driver_proto.rs`.

## Operations

- `DRIVER_OP_REGISTER` (1): register a driver task id.
- `DRIVER_OP_GRANT_IRQ` (2): grant an IRQ capability to a driver.
- `DRIVER_OP_GRANT_DMA` (3): grant a DMA-window capability to a driver.
- `DRIVER_OP_RESTARTED` (4): notify that a restart token was consumed.

## Safety model

- Kernel validates capability object types and rights.
- DMA windows are bounded by offset/length and page alignment.
- Driver policy and restart policy remain in user-space supervisor/manager services.
