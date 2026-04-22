<!-- SPDX-License-Identifier: Apache-2.0 -->

# Userspace Server Maturity (Current)

This document tracks maturity by extracted workspace service domain.

## Domain map

- Control plane: `crates/yarm-control-plane-servers`
- Drivers: `crates/yarm-driver-servers`
- Filesystems: `crates/yarm-fs-servers`
- Networking: `crates/yarm-network-servers`
- UI: `crates/yarm-ui-servers`
- Compatibility: `crates/yarm-compat-servers`

## Current maturity signals

### Structural

- Dedicated workspace crates own service code and bins.
- Root crate is no longer the monolithic service owner.
- Boundary checks enforce crate-graph and source-shape constraints.

### Behavioral

- Domain-specific deterministic tests exist in service crates.
- Runtime-entrypoint parity checks exist for FS/driver/network/UI domains.
- Shared ABI contracts are centralized in `crates/yarm-ipc-abi`.

### Compatibility

- POSIX compatibility is crate-owned (`yarm-compat-servers`) and binding-backed.
- Socket syscall routing uses shared socket ABI contracts and IPC dispatch bindings.

## Main maturity gates contributors should run

```bash
scripts/phase5-boundary-gates.sh
scripts/phase5-boundary-gates.sh --fs-runtime-entrypoint
scripts/phase5-boundary-gates.sh --driver-runtime-entrypoint
scripts/phase5-boundary-gates.sh --network-runtime-entrypoint
scripts/phase5-boundary-gates.sh --ui-runtime-entrypoint
```
