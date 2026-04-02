<!-- SPDX-License-Identifier: Apache-2.0 -->

# Phase 6 Service Migration Matrix

This matrix is the implementation tracker for **Phase 6 — Service migration and deprecation**.

Status values:

- ✅ migrated
- 🟡 partial / in progress
- ⏳ pending

## Core control-plane service matrix (pass 1 / PR-6.1)

| Service | Current receive path | Current request/reply model | Notification usage | Target primitive | Owner | Risk | Status | Planned PR |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| VFS (`src/services/control_plane/vfs/service.rs`) | budgeted timed receive (`ipc_recv_with_deadline`) in kernel roundtrip helper | reply-cap call/reply helper (`create_reply_cap_for_caller` + `ipc_reply`) for kernel-IPC loop | none | migrate to direct `IpcCall/IpcReply` syscall choreography for user-mode service boundary where applicable | control-plane | medium | 🟡 | PR-6.3, PR-6.5 |
| Supervisor (`src/services/control_plane/supervisor/service.rs`) | budgeted helper (`recv_with_budget`) for control/fault queues | event-driven control handling (no dedicated call/reply wrapper) | receives fault/control events; no blocking legacy recv in loop | keep budgeted receive and migrate request/reply-like ops to reply-cap path where safe | control-plane | medium | 🟡 | PR-6.2, PR-6.3 |
| Init (`src/services/control_plane/init/service.rs`) | orchestrates services; no dedicated kernel receive loop in this module | direct service orchestration calls | alert/status interactions via supervisor handoff | keep orchestration, document compatibility window and sunset waivers if any legacy choreography remains downstream | init/runtime | low | 🟡 | PR-6.2, PR-6.4, PR-6.5 |
| Process Manager (`src/services/control_plane/process_manager/service.rs`) | budgeted timed receive (`ipc_recv_with_deadline`) in kernel roundtrip helper | reply-cap call/reply helper (`create_reply_cap_for_caller` + `ipc_reply`) for kernel-IPC loop | none | migrate to direct `IpcCall/IpcReply` syscall choreography for user-mode service boundary where applicable | process | medium | 🟡 | PR-6.3, PR-6.5 |

## Cross-cutting gates tracked from this matrix

1. No regressions to legacy blocking `kernel.ipc_recv` for migrated services.
2. Remaining ad-hoc two-endpoint request/reply flows are enumerated and retired or granted dated sunset waivers.
3. Phase 6 completion requires this matrix to show all core services as either:
   - ✅ migrated, or
   - explicitly covered by dated deprecation/sunset waiver.
4. Exit-gate bundle canary in `src/services/control_plane/mod.rs` must remain green for migrated invariants (VFS/process-manager reply-cap + timed receive, supervisor budgeted receive).
