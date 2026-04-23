<!-- SPDX-License-Identifier: Apache-2.0 -->

# Phase 6 Service Migration Matrix (Current Ownership)

This matrix is synchronized to extracted workspace paths.

| Service | Owning crate path | Current receive/reply model | Status |
| --- | --- | --- | --- |
| VFS control-plane service | `crates/yarm-control-plane-servers/src/control_plane/vfs/service.rs` | typed/budgeted control-plane request/reply helper model | ✅ migrated |
| Supervisor service | `crates/yarm-control-plane-servers/src/control_plane/supervisor/service.rs` | fault/control queue handling + reply paths | ✅ migrated |
| Init service | `crates/yarm-control-plane-servers/src/control_plane/init/service.rs` | orchestration-focused (not a dedicated request loop service) | ✅ migrated |
| Process manager service | `crates/yarm-control-plane-servers/src/control_plane/process_manager/service.rs` | typed/budgeted control-plane request/reply helper model | ✅ migrated |

## Exit criteria (current)

- No reintroduction of legacy monolithic service paths under `src/services/*`.
- Boundary gates remain green (`phase5-boundary-gates`).
- Service ownership remains crate-local in extracted server crates.
