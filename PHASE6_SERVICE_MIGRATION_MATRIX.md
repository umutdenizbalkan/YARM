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
| VFS (`src/services/control_plane/vfs/service.rs`) | budgeted timed receive (`ipc_recv_with_deadline`) in kernel roundtrip helper | reply-cap call/reply helper (`create_reply_cap_for_caller` + `ipc_reply`) for kernel-IPC loop | none | direct syscall choreography optional follow-up for user-mode boundary ergonomics | control-plane | medium | ✅ migrated | PR-6.3 complete |
| Supervisor (`src/services/control_plane/supervisor/service.rs`) | budgeted helper (`recv_with_budget`) for control/fault queues | event-driven control handling; status-query reply supports transferred reply-cap fallback path, with dedicated call/reply query helper entrypoint | receives fault/control events; no blocking legacy recv in loop | remaining non-RPC/event-driven flows are dated-waiver scoped in exit-gate report | control-plane | medium | ✅ waived (dated) | PR-6.5 complete |
| Init (`src/services/control_plane/init/service.rs`) | orchestrates services; no dedicated kernel receive loop in this module | direct service orchestration calls | alert/status interactions via supervisor handoff | non-applicable to dedicated kernel request/reply loop migration; tracked as dated waiver | init/runtime | low | ✅ waived (dated) | PR-6.5 complete |
| Process Manager (`src/services/control_plane/process_manager/service.rs`) | budgeted timed receive (`ipc_recv_with_deadline`) in kernel roundtrip helper | reply-cap call/reply helper (`create_reply_cap_for_caller` + `ipc_reply`) for kernel-IPC loop | none | direct syscall choreography optional follow-up for user-mode boundary ergonomics | process | medium | ✅ migrated | PR-6.3 complete |

## Cross-cutting gates tracked from this matrix

1. No regressions to legacy blocking `kernel.ipc_recv` for migrated services.
2. Remaining ad-hoc two-endpoint request/reply flows are enumerated and retired or granted dated sunset waivers.
3. Phase 6 completion requires this matrix to show all core services as either:
   - ✅ migrated, or
   - explicitly covered by dated deprecation/sunset waiver.
4. Exit-gate bundle canary in `src/services/control_plane/mod.rs` must remain green for migrated invariants (VFS/process-manager reply-cap + timed receive, supervisor budgeted receive).
5. `PHASE6_EXIT_GATE_REPORT.md` must be kept current with dated waivers and closure evidence before Phase 6 completion sign-off.
