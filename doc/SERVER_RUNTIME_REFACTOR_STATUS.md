# Server runtime / POSIX / VFS refactor status

Last updated: 2026-04-29

## Completed milestones

### 1) Server runtime boundary

- `yarm-server-runtime` no longer acts as a root `yarm` re-export bridge.
- Server crates are expected to consume server-facing runtime surfaces from `yarm-server-runtime` and not kernel internals from root `yarm`.
- Boundary enforcement remains in place via crate-graph checks (`scripts/check-crate-graph-boundary.py`).

### 2) Userspace runtime

- `yarm-user-rt` provides userspace IPC entry points (`ipc_send` / `ipc_recv`).
- Architecture-specific asm/runtime glue is split under `crates/yarm-user-rt/src/arch`.
- `IpcTransport` and `SyscallIpcTransport` are available as transport abstractions.
- Startup context and startup-slot accessors are present for userspace bootstrap.

### 3) Startup slot ABI

Current startup slot map:

- slot 0 = task_id
- slot 1 = process-manager request-send cap
- slot 2 = process-manager reply-recv cap
- slot 3 = supervisor fault recv
- slot 4 = supervisor control send
- slot 5 = supervisor control recv
- slot 6 = init alert send
- slot 7 = init alert recv
- slot 8 = init tid
- slot 9 = supervisor tid
- slot 10 = restart window ticks

Delivery convention:

- arg0..arg2 preserve legacy direct slots
- arg3 = pointer to startup slot block
- arg4 = slot count
- arg5 = reserved

### 4) POSIX compat dispatch boundary

- Production dispatch now accepts `&mut impl IpcTransport` (transport boundary), not `&mut KernelState`.
- Kernel-backed `dispatch_with_kernel(...)` exists for kernel-dependent behavior/harness paths.
- `getpid` is IPC-backed in the kernel-backed dispatch path.
- `openat` / `statx` use inline byte-path VFS IPC where kernel-backed user-memory path decoding is available.
- Remaining production `NoSys` branches indicate syscall paths that still require additional runtime abstractions.

### 5) VFS path cleanup state

- OPENAT/STATX pointer-path runtime ABI is removed.
- Removed: `OpenAtArgs`, `StatxArgs`.
- Removed: `openat_message`, `statx_message` helpers.
- Removed: pointer entrypoints from `VfsBackend`.
- OPENAT/STATX decode path is inline byte-path only.
- FS backends are byte-path-primary (`openat_path`/`statx_path`).
- Remaining `path_ptr` naming primarily reflects manifest/wire compatibility and numeric path-id policy semantics, not runtime user-pointer OPENAT/STATX ABI.

### 6) Fault-report and restart pipeline state (current)

- Kernel delivery path:
  - Kernel fault handling emits a supervisor notification on the supervisor fault endpoint using opcode `0` and a compact fixed-size payload.
  - Delivery path remains kernel-owned; this pass does **not** change kernel fault emission or endpoint routing behavior.
- Fault wire format:
  - `SupervisorFaultReportWire` payload length is fixed at 17 bytes:
    - bytes `[0..8)` = `tid` (LE `u64`)
    - bytes `[8..16)` = `fault_addr` (LE `u64`)
    - byte `[16]` = `access` (`0..=2`)
  - The wire format is frozen for compatibility and was not changed in this pass.
- Supervisor production receive/decode status:
  - `supervisor.srv` production runtime loop now receives from control/fault endpoint caps via userspace transport (`SyscallIpcTransport`), decodes opcode `0` fault reports, and logs decode/lookup outcomes.
  - This confirms production fault receive/decode visibility is wired.
- Restart-token IPC ABI:
  - `process_abi` now contains guarded lookup ABI:
    - opcode `PROC_OP_TASK_RESTART_TOKEN = 8`
    - request `TaskRestartTokenRequest { tid }`
    - reply `TaskRestartTokenReply { found, token }`
  - Supervisor runtime-side helper attempts lookup through startup process-manager caps using this ABI.
- Explicit current limitation:
  - Production process-manager currently returns `Unsupported` for `PROC_OP_TASK_RESTART_TOKEN`, because no authoritative runtime restart-token state source is wired yet.
  - Therefore production restart from fault reports is **not** currently enabled; only receive/decode + guarded lookup attempt is active.
- Future work needed for real production restart:
  1. Treat process-manager as authoritative owner of restart-token state (task lifecycle owner).
  2. Populate process-manager restart-token table on authoritative lifecycle points (spawn/register/restart-policy handoff paths).
  3. Implement `PROC_OP_TASK_RESTART_TOKEN` server handling to return real `(found, token)` results from process-manager-owned state.
  4. Use successful lookup to construct `TaskExitedEvent { tid, synthetic_exit_code, restart_token }` in production path and route it through existing supervisor restart policy handling.
  5. Add end-to-end runtime tests validating fault report -> token lookup -> restart scheduling behavior.

## Remaining blockers / future work

1. Replace production `NoSys` POSIX branches with explicit runtime abstractions.
2. Expand POSIX IPC syscall coverage on transport-only production boundary.
3. Optional cleanup: continue narrowing stale `path_ptr` terminology where wire-compatible.
4. Optional cleanup: isolate test-only kernel harness paths into clearer harness modules.
5. Wire authoritative runtime restart-token lookup service and enable production fault-triggered restart path.

## Notes

- This document is status-oriented (what is true now), not a design proposal.
- This pass intentionally avoids code/API redesign and focuses on documenting the current boundary state.
