# init.srv Boot Contract (Scaffold v1)

This document defines the initial boot orchestration contract for `init.srv` in the core profile.

## Scope

- Core profile boot only (no Linux personality assumptions).
- Service graph registration for:
  - `procman.srv`
  - `vfs.srv`
  - `supervisor.srv`
- Delegation validation for expected `init -> service` policy edges.

## Required startup identity

- `init_tid`
- `process_manager_tid`
- `vfs_tid`
- `supervisor_tid`

These IDs are registered by `InitServerLite::register_core_graph` and assigned service roles through kernel policy APIs.

## Phase machine

`InitServerLite` phase transitions are:

1. `Uninitialized`
2. `CoreServicesRegistered`
3. `Running`

`begin_running()` is valid only after successful graph registration.

## Required checks before running

- Tasks for all core services are registered.
- Service roles are assigned:
  - `Init`
  - `ProcessManager`
  - `Vfs`
  - `Supervisor`
- Delegation validation succeeds for:
  - `init -> procman`
  - `init -> vfs`
  - `init -> supervisor`

## Notes

- This is a mechanism-level scaffold in `src/kernel/init_server.rs` and `src/bin/init_server.rs`.
- Future revisions will add explicit launch ordering, capability intake maps, and restart/fault policy handoff.
