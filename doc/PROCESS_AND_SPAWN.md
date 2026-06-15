<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Umut Deniz Balkan -->

# YARM Process, Spawn, and Control-Plane Contracts

> **Ownership rule.** PM SpawnV5 contract, startup-slot layout, image-ID
> → binary path table, TID allocation policy, init boot contract, and
> control-plane boundary live here. New process/spawn fragment files are
> forbidden; update this doc instead. See `doc/DOCUMENTATION_MAP.md`.
>
> The kernel-mechanism-only invariant lives in `doc/KERNEL_UNLOCKING.md`
> §3. The capability transfer rules live in `doc/CAPABILITY_MODEL.md`
> §11. The frozen Proc codec versions / opcodes live in `doc/VFS.md` §6.

Authoritative implementation:
`crates/yarm-control-plane-servers/src/control_plane/process_manager/service.rs`.

---

## 1. Bootstrap boundary

The spawn path is divided at compile-time constants:

```rust
const BOOTSTRAP_IMAGE_ID_MIN: u64 = 1;
const BOOTSTRAP_IMAGE_ID_MAX: u64 = 3;
```

- **`image_id 0`** — kernel-internal; PM returns `Err(Unsupported)`
  immediately.
- **`image_ids 1..=3`** — bootstrap-critical. These must be available
  before VFS exists; spawned via
  `KernelProcessSpawnBackend::spawn_with_caps()`.
- **`image_id >= 4`** — all non-bootstrap images. PM unconditionally routes
  through `pm_vfs_spawn_inline()`. Unknown id in this range returns
  `Err(Unsupported)`; kernel syscall failure returns `Err(TableFull)`.

**New image IDs should be added to `pm_vfs_spawn_inline`'s match table and
go through the VFS-backed path by default. The bootstrap range `1..=3`
must remain frozen.**

---

## 2. Image ID → binary path table

| image_id | CPIO path | Spawn backend |
|----------|-----------|----------------|
| 0 | (init, kernel-internal) | rejected (`Unsupported`) |
| 1 | `sbin/supervisor` | `KernelProcessSpawnBackend` |
| 2 | `sbin/process_manager` | `KernelProcessSpawnBackend` |
| 3 | `sbin/init_server` | `KernelProcessSpawnBackend` |
| 4 | `sbin/initramfs_srv` | `pm_vfs_spawn_inline` |
| 5 | `sbin/devfs_srv` | `pm_vfs_spawn_inline` |
| 6 | `sbin/vfs_server` | `pm_vfs_spawn_inline` |
| 7 | `sbin/driver_manager` | `pm_vfs_spawn_inline` |
| 8 | `sbin/blkcache_srv` | `pm_vfs_spawn_inline` |
| 9 | `sbin/virtio_blk_srv` | `pm_vfs_spawn_inline` |
| 10 | `sbin/fat_srv` | `pm_vfs_spawn_inline` (disabled-by-default) |
| 11 | `sbin/ramfs_srv` | `pm_vfs_spawn_inline` |
| 12 | `sbin/ext4_srv` | `pm_vfs_spawn_inline` |
| ≥13 | (future) | `pm_vfs_spawn_inline` |

`image_ids >= 4` use `SpawnFromInitramfsFile` (syscall `nr=26`) via
`pm_vfs_spawn_inline`. `image_ids 1..=3` use the direct kernel spawn
backend. `image_id = 0` is rejected by PM — it is never spawned from
userspace.

`SpawnFromInitramfsFile` is a **privileged kernel-extension slot**, not
part of the public user syscall count/range. See `doc/SYSCALL_ABI.md` for
the public ABI vs. kernel dispatch-table split.

Image IDs 7–12 are **frozen** (see `doc/KERNEL_UNLOCKING.md` §3).

---

## 3. Startup slot layout (0–17, `STARTUP_SLOT_COUNT = 18`)

The kernel populates a `startup_args: [u64; 18]` array passed to every
spawned task. Slot assignments are defined in
`crates/yarm-user-rt/src/lib.rs` (`StartupContext`).

| Slot | Name | Set by | Content for PM-spawned task |
|------|------|--------|------------------------------|
| 0 | `task_id` | kernel | spawned task's own TID |
| 1 | `proc_mgr_request_send_cap` | kernel | send cap to PM request endpoint |
| 2 | `proc_mgr_reply_recv_cap` | kernel | recv cap for PM reply channel (task-local) |
| 3 | `supervisor_send_cap` | kernel | send cap to supervisor endpoint |
| 4 | `init_server_send_cap` | kernel | send cap to init_server endpoint |
| 5 | `vfs_send_cap` | kernel | send cap to VFS endpoint (may be `0` pre-VFS) |
| 6 | `reserved_6` | — | reserved, `0` |
| 7 | `reserved_7` | — | reserved, `0` (PM) / init_alert_recv_ep (init) |
| 8 | `STARTUP_SLOT_OPTIONAL_INIT_TID` | kernel | init_server TID if available; `0` for PM |
| 9 | `STARTUP_SLOT_OPTIONAL_SUPERVISOR_TID` | kernel | supervisor TID if available; `0` for PM |
| 10 | `reserved_10` | — | reserved, `0` |
| 11 | `reserved_11` | — | reserved, `0` |
| **12** | `process_manager_service_recv_ep` | PM / kernel | **PM-private**: service recv cap for the spawned task |
| 13 | `service_extra_cap_0` | PM caller | extra service cap (e.g. initramfs send cap) |
| 14 | `service_extra_cap_1` | PM caller | extra service cap (e.g. devfs send cap) |
| 15 | `service_extra_cap_2` | PM caller | extra service cap |
| 16 | `service_extra_cap_3` | PM caller | extra service cap |
| 17 | `reserved_17` | — | reserved, `0` |

Slot 12 carries the service's own **receive** endpoint capability. This
is the cap the service blocks on in its resident IPC loop. **Slot 12 is
PM-private** for PM↔VFS subcalls; distinct from the PM's own recv cap
(slot 2 of the PM's own startup context).

`init_server` is intentionally different from long-lived request
services: it is a one-shot boot orchestrator. After spawning core
services and running VFS smokes, it idles on `init_alert_recv_ep` (slot
7) when present; if absent, an explicit
`INIT_NO_RECV_CAP_EXPECTED_ONE_SHOT_IDLE` log marker is expected and is
not treated as a service failure.

Slots 13–16 carry optional extra capabilities passed by the spawn
initiator via `SpawnV5CapArgs.service_caps[0..4]`. For `image_id=6`
(`vfs_server`), the init server places the initramfs send cap in slot 13
and the devfs send cap in slot 14 before calling `PROC_OP_SPAWN_V5_CAP`.

### Stack semantics

Startup args + cap slots are placed on the initial stack **below** the
nominal `stack_top` address. At first entry, SP points below `stack_top`,
**not at it**. Do **not** add an assertion `SP == stack_top` in entry
stubs.

`startup_args[0]` must be the final task TID before first entry; the
kernel sets it before the task's first instruction executes. Userspace
must not overwrite `startup_args[0]` after spawn.

On x86_64, at the first user instruction, the stack pointer must be
16-byte aligned minus 8 bytes (as if a `call` instruction just pushed a
return address). Violating this causes SSE / AVX faults.

### Slot count / image-id discipline

- **`STARTUP_SLOT_COUNT = 18`** is frozen (see `doc/KERNEL_UNLOCKING.md`
  §3).
- **image_id 7/8/9 late services use zeroed extra caps.** Do not
  reintroduce a `vfs_recv_cap` in slot 13 or any other slot unless a real
  consumer is implemented and the slot layout is re-validated end-to-end.

---

## 4. SpawnV5Cap wire protocol

**Opcode:** `PROC_OP_SPAWN_V5_CAP = 11`
**Source:** `crates/yarm-ipc-abi/src/process_abi.rs`

### Request encoding `SpawnV5CapArgs` — 48 bytes LE

| Offset | Size | Field | Description |
|--------|------|-------|-------------|
| 0 | 8 | `parent_pid` | parent TID; 0 means PM is own parent |
| 8 | 8 | `image_id` | binary selector (§2) |
| 16 | 8 | `service_caps[0]` | extra cap → slot 13 |
| 24 | 8 | `service_caps[1]` | extra cap → slot 14 |
| 32 | 8 | `service_caps[2]` | extra cap → slot 15 |
| 40 | 8 | `service_caps[3]` | extra cap → slot 16 |

### Reply encoding `SpawnV5CapResult` — 16 bytes LE

| Offset | Size | Field | Description |
|--------|------|-------|-------------|
| 0 | 8 | `pid` | spawned task's TID |
| 8 | 8 | `service_send_cap` | caller's send cap to the spawned service |

`service_send_cap` in the reply is `caller_cap` from the kernel spawn
syscall result — the cap through which the spawn requester (init_server)
can send to the newly spawned service. It is **not** the PM's own cap to
that service.

`SpawnV5CapResult::ENCODED_LEN = 16` is **frozen** (see
`doc/KERNEL_UNLOCKING.md` §3).

---

## 5. Capability flow

### Case A — `parent_pid = 0` (PM is sponsor)

```
init_server ──PROC_OP_SPAWN_V5_CAP──► PM
    │
    ├─ pm_vfs_spawn_inline(image_id, parent_pid=0, startup_args)
    │   └─ SpawnFromInitramfsFile syscall → (tid, caller_cap, spawner_cap)
    │       spawner_cap = PM's send cap to new service
    │       caller_cap  = init's send cap to new service
    │
    ├─ pm_send_cap = spawner_cap  (non-zero in this case)
    ├─ records ServiceLifecycleRecord {pm_service_send_cap: spawner_cap}
    └─ replies SpawnV5CapResult {pid: tid, service_send_cap: caller_cap}
```

### Case B — `parent_pid != 0` (delegation from parent task)

```
init_server ──PROC_OP_SPAWN_V5_CAP──► PM
    │
    ├─ pm_vfs_spawn_inline(image_id, parent_pid, startup_args)
    │   └─ SpawnFromInitramfsFile syscall
    │       spawner_cap may be 0 (kernel delegates to parent)
    │       caller_cap = send cap to new service
    │
    ├─ pm_send_cap = if spawner_cap != 0 {spawner_cap} else {caller_cap as u32}
    ├─ records ServiceLifecycleRecord {pm_service_send_cap: pm_send_cap}
    └─ replies SpawnV5CapResult {pid: tid, service_send_cap: caller_cap}
```

The `pm_send_cap` selection ensures PM always retains a usable send cap
to every service it spawns, regardless of whether the kernel granted a
separate sponsorship cap or collapsed them.

---

## 6. Lifecycle table contract

- **Type:** `LifecycleTable` in `process_manager/service.rs`.
- **Capacity:** `MAX_LIFECYCLE_ENTRIES = 32`.

### `ServiceLifecycleRecord` fields

| Field | Type | Description |
|-------|------|-------------|
| `tid` | `u64` | TID of the spawned service task |
| `image_id` | `u64` | binary selector used at spawn time |
| `parent_tid` | `u64` | parent TID passed in `SpawnV5CapArgs.parent_pid` |
| `pm_service_send_cap` | `u32` | PM's own send cap to this service |
| `state` | `ServiceState` | `Spawned` (only state currently defined) |

`LifecycleTable::record()` stores entries in insertion order. When the
table is full (len == 32) it returns `false` and the spawn still succeeds
— the record is simply not tracked. This is logged via
`PM_LIFECYCLE_RECORD ... recorded=0`.

### Lifecycle query opcode (`PROC_OP_LIFECYCLE_QUERY = 12`)

- **Request** `LifecycleQueryRequest` — 8 bytes (`tid: u64 LE`).
- **Reply** `LifecycleQueryReply` — 19 bytes:

| Offset | Size | Field | Description |
|--------|------|-------|-------------|
| 0 | 1 | `found` | 1 = record present, 0 = TID unknown to PM |
| 1 | 8 | `tid` | u64 LE; echoes queried TID when `found=1` |
| 9 | 8 | `image_id` | u64 LE; image_id at spawn time |
| 17 | 1 | `state` | `LIFECYCLE_STATE_SPAWNED` (0) — only state now |
| 18 | 1 | `restart_supported` | always 0; restart not yet wired |

---

## 7. TID allocation contract

### Allocation domains

- **Static / bootstrap TIDs:** `0..=static_tid_upper_bound`.
- **Dynamic TIDs:** `dynamic_tid_floor..=u64::MAX`.

Current kernel policy exports these boundaries through:

- `KernelState::dynamic_tid_floor()`
- `KernelState::static_tid_upper_bound()`
- `KernelState::is_dynamic_tid(tid)`

### Caller expectations

- Dynamic TIDs are **unique while live**, but are **not globally
  monotonic forever** because the cursor can wrap from `u64::MAX` back
  to `dynamic_tid_floor`.
- Callers must not infer ordering semantics from numeric TID magnitude
  across long runtimes.
- Logic that classifies dynamic vs. static IDs must use policy helpers,
  not hard-coded literals.

### Telemetry

- `dynamic_tid_allocations` — successful dynamic allocations.
- `dynamic_tid_wraps` — wrap events when cursor rolls to floor.
- `gap_floor_repairs` — times a stale cursor below floor was normalized.

Boot logs emit `YARM_TID_POLICY ...`; wrap events emit
`YARM_TID_ALLOC_WRAP ...`. CI gate:
`scripts/check-tid-allocation-policy.sh`.

---

## 8. Init server boot contract (core profile)

### Scope

- Core-profile boot only (no Linux personality assumptions).
- Service-graph registration for: `process_manager.srv`, `vfs.srv`,
  `supervisor.srv`.
- Delegation validation for expected `init → service` policy edges.

### Required startup identity

`init_tid`, `process_manager_tid`, `vfs_tid`, `supervisor_tid` are
registered by `InitService::register_core_graph` and assigned service
roles through kernel policy APIs.

### Phase machine

`InitService` phase transitions:

1. `Uninitialized`
2. `CoreServicesRegistered`
3. `LaunchingCore`
4. `Running`
5. `Failed`

`begin_running()` is valid only after successful core launch
(`LaunchingCore`) and explicit fault-policy handoff installation.

### Checked contract requirements

- `InitService::validate_boot_contract()` must succeed before entering
  `Running`.
- The minimum core graph (`process_manager`, `vfs`, `supervisor`) must
  have registered task identities.
- Fault handoff and delegation edges (`init → process_manager`,
  `init → vfs`, `init → supervisor`) must be installed and validated.
- The configured mount plan must complete through service-backed mount
  activity before `Running`.
- Supervisor replay state must be populated from seeded control-plane
  registrations.

### Notes

- The runtime init entrypoint in
  `crates/yarm-control-plane-servers/src/control_plane/init/service.rs`
  accepts an externally prepared `KernelState` plus
  `InitRuntimeBootConfig`, so boot/runtime wiring no longer has to be
  hardcoded to `Bootstrap::init()`.
- Launch ordering routes through `launch_core_services` with explicit
  core image plan and failure transition support (`mark_failed`).
- Restart / fault policy handoff is represented by `InitFaultHandoff` and
  must be installed before `Running`.
- Mount orchestration executes real service-backed mount activity for the
  configured mount plan instead of only counting deterministic
  placeholders.
- Supervisor recovery includes replaying core-service registration
  requests so a fresh `supervisor.srv` instance can rebuild its
  managed-service table.

---

## 9. Control-plane boundary

Primary goal: move userspace-facing and policy-facing surfaces **out of
direct control-plane dependency on kernel internals** while keeping
kernel mechanism boundaries explicit.

### Extracted userspace / runtime surfaces (no longer kernel-sourced)

| Surface | Moved to |
|---------|----------|
| Logging | `yarm-user-rt` |
| IPC value types (`Message`, `ThreadId`) | `yarm-user-rt` |
| Time value types (`TickInstant`, `TickDuration`) | `yarm-user-rt` |
| Syscall userspace error surface (`SyscallError`) | `yarm-user-rt` |
| Capability value/rights surface (`CapId`, `CapRights`) | `yarm-user-rt` |
| Driver shared ABI subset | `yarm-ipc-abi` |
| Task userspace value surface (`TaskStatus`, `TaskClass`) | `yarm-user-rt` |
| VM userspace value surface (`Asid`, `PAGE_SIZE`) | `yarm-user-rt` |
| Process userspace/runtime (`ProcessId`, `ProcessError`, `WaitResult`, `ProcessManagerOps`) | `yarm-user-rt` |

### KernelState-boundary redesign pattern

Where incremental redesign was coherent, control-plane server code
adopted narrow local trait/facade + adapter patterns around
`KernelState`-backed operations. Redesigned families include:

- driver-manager control family
- process-manager helper / request-loop family
- process-manager IPC seam stabilization
- supervisor query-status helper family

Live history of the server-runtime / POSIX / VFS refactor lives in
`doc/PROJECT_HISTORY.md`; live boundary status lives in
`doc/STATUS.md` §3.

### Domain ownership rules (gated by `scripts/check-service-domain-ownership.sh`)

- `network` must not depend on `fs` internals.
- `ui` must not depend on `fs` / `network` internals.
- `control_plane` must not depend on `fs` / `network` / `ui` /
  `compatibility` internals (with documented exceptions for
  `vfs/service.rs` and `init/service.rs`).
- `compatibility` must not depend on `fs` / `network` / `ui` internals
  (with documented exception for `posix_compat/sysdeps/service_hooks.rs`).
- `fs` must not depend on `network` / `ui` internals.
- `network` must not depend on `ui` internals.

---

## 10. Authoring rule

Future process / spawn / control-plane changes update **this file**.
Per-syscall ABI updates go in `doc/SYSCALL_ABI.md`; capability rules in
`doc/CAPABILITY_MODEL.md`; per-server filesystem behavior in
`doc/FILESYSTEM_AND_STORAGE_CONTRACTS.md`; per-arch userspace status in
`doc/ARCH_*.md`. Do **not** create new `PM_*` / `INIT_*` / `TID_*` /
`CONTROL_PLANE_*` fragment files.
