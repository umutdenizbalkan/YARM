# init.srv Boot Contract (Scaffold v1)

This document defines the initial boot orchestration contract for `init.srv` in the core profile.

## Scope

- Core profile boot only (no Linux personality assumptions).
- Service graph registration for:
  - `process_manager.srv`
  - `vfs.srv`
  - `supervisor.srv`
- Delegation validation for expected `init -> service` policy edges.

## Required startup identity

- `init_tid`
- `process_manager_tid`
- `vfs_tid`
- `supervisor_tid`

These IDs are registered by `InitService::register_core_graph` and assigned service roles through kernel policy APIs.

## Phase machine

`InitService` phase transitions are:

1. `Uninitialized`
2. `CoreServicesRegistered`
3. `LaunchingCore`
4. `Running`
5. `Failed`

`begin_running()` is valid only after successful core launch (`LaunchingCore`) and explicit fault-policy handoff installation.

## Required checks before running

- Tasks for all core services are registered.
- Fault/restart handoff is installed and bound to `supervisor_tid`.
- Supervisor control-plane registration requests are seeded before `Running` and replayed if `supervisor.srv` is restarted by `init.srv`.
- Service roles are assigned:
  - `Init`
  - `ProcessManager`
  - `Vfs`
  - `Supervisor`
- Delegation validation succeeds for:
  - `init -> process_manager`
  - `init -> vfs`
  - `init -> supervisor`

## Notes

- This is a mechanism-level scaffold in `src/services/init/mod.rs` and `src/bin/init_server.rs`.
- Launch ordering now routes through `launch_core_services` with explicit core image plan and failure transition support (`mark_failed`).
- Restart/fault policy handoff is now represented by `InitFaultHandoff` and must be installed before `Running`.
- Supervisor recovery includes replaying core-service registration requests so a fresh `supervisor.srv` instance can rebuild its managed-service table.
