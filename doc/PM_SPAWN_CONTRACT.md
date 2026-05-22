<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Umut Deniz Balkan -->

# PM Spawn Contract

This document defines the authoritative contract for process-manager service
spawning, capability delivery, and lifecycle tracking as implemented in
`crates/yarm-control-plane-servers/src/control_plane/process_manager/service.rs`.

---

## Bootstrap Boundary

The spawn path is divided at the compile-time constants:

```rust
const BOOTSTRAP_IMAGE_ID_MIN: u64 = 1;
const BOOTSTRAP_IMAGE_ID_MAX: u64 = 3;
```

- **image_id 0** — kernel-internal; PM returns `Err(Unsupported)` immediately.
- **image_ids 1–3** (`BOOTSTRAP_IMAGE_ID_MIN..=BOOTSTRAP_IMAGE_ID_MAX`) — bootstrap-critical. These must be available before VFS exists and are spawned via `KernelProcessSpawnBackend::spawn_with_caps()`.
- **image_id ≥ 4** — all non-bootstrap images. PM unconditionally routes through `pm_vfs_spawn_inline()`. An unknown `image_id` in this range returns `Err(Unsupported)`; a kernel syscall failure returns `Err(TableFull)`.

**New image IDs should be added to `pm_vfs_spawn_inline`'s match table and go through the VFS-backed path by default. The bootstrap range (`1..=3`) must remain frozen.**

---

## Image ID → Binary Path Table

| image_id | CPIO path               | Spawn backend            |
|----------|-------------------------|--------------------------|
| 0        | (init, kernel-internal) | rejected (`Unsupported`) |
| 1        | `sbin/supervisor`       | `KernelProcessSpawnBackend` |
| 2        | `sbin/process_manager`  | `KernelProcessSpawnBackend` |
| 3        | `sbin/init_server`      | `KernelProcessSpawnBackend` |
| 4        | `sbin/initramfs_srv`    | `pm_vfs_spawn_inline`    |
| 5        | `sbin/devfs_srv`        | `pm_vfs_spawn_inline`    |
| 6        | `sbin/vfs_server`       | `pm_vfs_spawn_inline`    |
| 7        | `sbin/driver_manager`   | `pm_vfs_spawn_inline`    |
| 8        | `sbin/blkcache_srv`     | `pm_vfs_spawn_inline`    |
| 9        | `sbin/virtio_blk_srv`   | `pm_vfs_spawn_inline`    |
| ≥ 8      | (future)                | `pm_vfs_spawn_inline`    |

image_ids ≥ 4 use `SpawnFromInitramfsFile` (syscall nr=26) via
`pm_vfs_spawn_inline`. image_ids 1–3 use the direct kernel spawn backend.
image_id=0 is rejected by PM — it is never spawned from userspace.

---

## Startup Slot Layout (0–17)

The kernel populates a `startup_args: [u64; 18]` array passed to every spawned
task. Slot assignments are defined in
`crates/yarm-user-rt/src/lib.rs` (`StartupContext`).

| Slot | Name                              | Set by     | Content for PM                                      |
|------|-----------------------------------|------------|-----------------------------------------------------|
| 0    | `task_id`                         | kernel     | spawned task's own TID                              |
| 1    | `proc_mgr_request_send_cap`       | kernel     | send cap to PM request endpoint                     |
| 2    | `proc_mgr_reply_recv_cap`         | kernel     | recv cap for PM reply channel (task-local)          |
| 3    | `supervisor_send_cap`             | kernel     | send cap to supervisor endpoint                     |
| 4    | `init_server_send_cap`            | kernel     | send cap to init_server endpoint                    |
| 5    | `vfs_send_cap`                    | kernel     | send cap to VFS endpoint (may be 0 pre-VFS)         |
| 6    | `reserved_6`                      | —          | reserved, 0 for PM                                  |
| 7    | `reserved_7`                      | —          | reserved, 0 for PM                                  |
| 8    | `STARTUP_SLOT_OPTIONAL_INIT_TID`  | kernel     | init_server TID if kernel can provide it; 0 for PM  |
| 9    | `STARTUP_SLOT_OPTIONAL_SUPERVISOR_TID` | kernel | supervisor TID if kernel can provide it; 0 for PM  |
| 10   | `reserved_10`                     | —          | reserved, 0 for PM                                  |
| 11   | `reserved_11`                     | —          | reserved, 0 for PM                                  |
| 12   | `process_manager_service_recv_ep` | PM/kernel  | service recv cap for the spawned task               |
| 13   | `service_extra_cap_0`             | PM caller  | extra service cap (e.g. initramfs send cap)         |
| 14   | `service_extra_cap_1`             | PM caller  | extra service cap (e.g. devfs send cap)             |
| 15   | `service_extra_cap_2`             | PM caller  | extra service cap                                   |
| 16   | `service_extra_cap_3`             | PM caller  | extra service cap                                   |
| 17   | `reserved_17`                     | —          | reserved, 0                                         |

Slot 12 carries the service's own receive endpoint capability. This is the cap
the service blocks on in its resident IPC loop. It is distinct from the PM's own
recv cap (slot 2 of the PM's own startup context).

`init_server` is intentionally different from long-lived request services:
it is currently a one-shot boot orchestrator. After spawning core services and
running VFS smokes, it idles on `init_alert_recv_ep` (slot 7) when present; if
that alert recv cap is absent, an explicit `INIT_NO_RECV_CAP_EXPECTED_ONE_SHOT_IDLE`
log marker is expected and is not treated as a service failure.

Slots 13–16 carry optional extra capabilities passed by the spawn initiator via
`SpawnV5CapArgs.service_caps[0..4]`. For image_id=6 (vfs_server), the init
server places the initramfs send cap in slot 13 and the devfs send cap in slot
14 before calling `PROC_OP_SPAWN_V5_CAP`.

---

## SpawnV5Cap Wire Protocol

**Opcode**: `PROC_OP_SPAWN_V5_CAP = 11`  
**Source**: `crates/yarm-ipc-abi/src/process_abi.rs`

### Request encoding (`SpawnV5CapArgs`) — 48 bytes LE

| Offset | Size | Field          | Description                            |
|--------|------|----------------|----------------------------------------|
| 0      | 8    | `parent_pid`   | parent TID; 0 means PM is own parent   |
| 8      | 8    | `image_id`     | binary selector (see image ID table)   |
| 16     | 8    | `service_caps[0]` | extra cap forwarded to slot 13      |
| 24     | 8    | `service_caps[1]` | extra cap forwarded to slot 14      |
| 32     | 8    | `service_caps[2]` | extra cap forwarded to slot 15      |
| 40     | 8    | `service_caps[3]` | extra cap forwarded to slot 16      |

### Reply encoding (`SpawnV5CapResult`) — 16 bytes LE

| Offset | Size | Field              | Description                              |
|--------|------|--------------------|------------------------------------------|
| 0      | 8    | `pid`              | spawned task's TID                       |
| 8      | 8    | `service_send_cap` | caller's send cap to the spawned service |

`service_send_cap` in the reply is `caller_cap` from the kernel spawn syscall
result — the cap through which the spawn requester (init_server) can send to
the newly spawned service. It is not the PM's own cap to that service.

---

## Capability Flow

### Case A: `parent_pid = 0` (PM is sponsor)

```
init_server ──PROC_OP_SPAWN_V5_CAP──► PM
                                       │
                                       ├─ calls pm_vfs_spawn_inline(image_id, parent_pid=0, startup_args)
                                       │     └─ SpawnFromInitramfsFile syscall
                                       │         returns (tid, caller_cap, spawner_cap)
                                       │         spawner_cap = PM's send cap to new service
                                       │         caller_cap  = init's send cap to new service
                                       │
                                       ├─ pm_send_cap = spawner_cap  (non-zero in this case)
                                       ├─ records ServiceLifecycleRecord { pm_service_send_cap: spawner_cap }
                                       └─ replies SpawnV5CapResult { pid: tid, service_send_cap: caller_cap }
```

### Case B: `parent_pid ≠ 0` (delegation from parent task)

```
init_server ──PROC_OP_SPAWN_V5_CAP──► PM
                                       │
                                       ├─ calls pm_vfs_spawn_inline(image_id, parent_pid, startup_args)
                                       │     └─ SpawnFromInitramfsFile syscall
                                       │         spawner_cap may be 0 (kernel delegates to parent)
                                       │         caller_cap = send cap to new service
                                       │
                                       ├─ pm_send_cap = if spawner_cap != 0 { spawner_cap }
                                       │                else { caller_cap as u32 }
                                       ├─ records ServiceLifecycleRecord { pm_service_send_cap: pm_send_cap }
                                       └─ replies SpawnV5CapResult { pid: tid, service_send_cap: caller_cap }
```

The `pm_send_cap` selection ensures PM always retains a usable send cap to
every service it spawns, regardless of whether the kernel granted a separate
sponsorship cap or collapsed them.

---

## Lifecycle Table Contract

**Type**: `LifecycleTable` in `process_manager/service.rs`  
**Capacity**: `MAX_LIFECYCLE_ENTRIES = 32`

### `ServiceLifecycleRecord` fields

| Field                 | Type          | Description                                        |
|-----------------------|---------------|----------------------------------------------------|
| `tid`                 | `u64`         | TID of the spawned service task                    |
| `image_id`            | `u64`         | binary selector used at spawn time                 |
| `parent_tid`          | `u64`         | parent TID passed in `SpawnV5CapArgs.parent_pid`   |
| `pm_service_send_cap` | `u32`         | PM's own send cap to this service (see cap flow)   |
| `state`               | `ServiceState`| `Spawned` (only state currently defined)           |

`LifecycleTable::record()` stores entries in insertion order. When the table is
full (len == 32) it returns `false` and the spawn still succeeds — the record is
simply not tracked. This is logged via `PM_LIFECYCLE_RECORD ... recorded=0`.

### Lifecycle Query Opcode (`PROC_OP_LIFECYCLE_QUERY = 12`)

Any task holding PM send/recv caps may query the lifecycle table by TID:

- **Request**: `LifecycleQueryRequest` — 8 bytes (tid: u64 LE)
- **Reply**: `LifecycleQueryReply` — 19 bytes

| Offset | Size | Field              | Description                                    |
|--------|------|--------------------|------------------------------------------------|
| 0      | 1    | `found`            | 1 = record present, 0 = TID unknown to PM      |
| 1      | 8    | `tid`              | u64 LE; echoes queried TID when found=1        |
| 9      | 8    | `image_id`         | u64 LE; the image_id at spawn time             |
| 17     | 1    | `state`            | `LIFECYCLE_STATE_SPAWNED` (0) — only state now |
| 18     | 1    | `restart_supported`| always 0; restart not yet wired                |

PM logs `PM_LIFECYCLE_QUERY_REPLY tid=… found=… image_id=…` on every query.

**PM lifecycle table is the authoritative source for supervision metadata.**
Callers must not fabricate lifecycle records or treat `found=0` as an error —
it simply means PM was not the spawner for that TID.

---

## Bootstrap Lifecycle Seeding

Bootstrap services (image_ids 1–3) are spawned by the kernel before PM's
request loop is running. They never pass through `PROC_OP_SPAWN_V5_CAP`, so PM
has no opportunity to call `lifecycle_table.record()` at spawn time. To make
these services visible to lifecycle queries (e.g. the supervisor querying its
own TID), PM seeds three records unconditionally at the top of `run()`, before
the first `ipc_recv_v2` call.

### Seeded records

| TID               | image_id | Binary             | pm_service_send_cap |
|-------------------|----------|--------------------|---------------------|
| `pm_tid`          | 2        | `sbin/process_manager` | 0               |
| `pm_tid - 1`      | 1        | `sbin/supervisor`  | 0                   |
| `pm_tid - 2`      | 3        | `sbin/init_server` | 0                   |

`pm_service_send_cap` is 0 for all bootstrap records because PM did not mint a
service send cap for them at spawn time (the kernel spawned them directly before
PM existed).

### Slot-preferred, deterministic fallback

The preferred source for supervisor and init TIDs is startup slots 8 and 9:

- **Slot 8** (`STARTUP_SLOT_OPTIONAL_INIT_TID`): init_server TID, if populated
- **Slot 9** (`STARTUP_SLOT_OPTIONAL_SUPERVISOR_TID`): supervisor TID, if populated

In the current kernel, both slots arrive as 0 for PM (the kernel has not yet
assigned those TIDs when PM is first launched). When a slot is zero PM:

1. Logs `PM_LIFECYCLE_BOOTSTRAP_SKIP image_id=N reason=missing_slot`
2. Derives the TID from the deterministic boot-sequence order:
   - `supervisor_tid = pm_tid - 1` — supervisor is spawned one slot before PM
   - `init_tid = pm_tid - 2` — init is spawned two slots before PM

These are real kernel-assigned TIDs derived from known boot ordering, not
fabricated values. If a future kernel populates the slots, PM will use the
slot value and skip the fallback entirely — the branch is:

```rust
if raw_sup_tid != 0 {
    service.seed_bootstrap_lifecycle_record(raw_sup_tid, 1);
} else {
    // PM_LIFECYCLE_BOOTSTRAP_SKIP image_id=1 reason=missing_slot
    service.seed_bootstrap_lifecycle_record(ctx.task_id - 1, 1);
}
```

### Expected log sequence at PM startup

```
PM_LIFECYCLE_BOOTSTRAP tid=3 image_id=2 recorded=1
PM_STARTUP_SLOT_8_INIT_TID raw=0
PM_STARTUP_SLOT_9_SUPERVISOR_TID raw=0
PM_LIFECYCLE_BOOTSTRAP_SKIP image_id=1 reason=missing_slot
PM_LIFECYCLE_BOOTSTRAP tid=2 image_id=1 recorded=1
PM_LIFECYCLE_BOOTSTRAP_SKIP image_id=3 reason=missing_slot
PM_LIFECYCLE_BOOTSTRAP tid=1 image_id=3 recorded=1
```

If a future kernel populates the slots (`raw=N` where N≠0), the
`PM_LIFECYCLE_BOOTSTRAP_SKIP` line for that image_id will not appear and the
slot value is used directly.

### Supervisor lifecycle registration contract

The supervisor calls `PROC_OP_LIFECYCLE_QUERY` for its own TID during startup
handoff. Because bootstrap lifecycle records are now seeded before PM's request
loop, the query returns `found=1` for the supervisor:

- `found=1`: logs `SUPERVISOR_LIFECYCLE_FOUND tid=… image_id=… restart_supported=0` then logs `"restart unsupported: PM lifecycle record found but no restart token source wired"`
- `found=0`: logs `SUPERVISOR_LIFECYCLE_MISSING tid=…` (should not occur in a correctly booted system)

Expected supervisor log after bootstrap seeding is in place:

```
SUPERVISOR_LIFECYCLE_QUERY tid=2
PM_LIFECYCLE_QUERY_REPLY tid=2 found=1 image_id=1
SUPERVISOR_LIFECYCLE_FOUND tid=2 image_id=1 restart_supported=0
restart unsupported: PM lifecycle record found but no restart token source wired
```

**Restart is intentionally unsupported** until real restart-token population
(via `PROC_OP_REGISTER_SUPERVISED_TASK`) is wired to an authoritative source.
The supervisor must not fabricate tokens or silently treat missing records as
success.

---

## Sequential Spawn Ordering

The init server spawns services in strict order:

1. `image_id=4` — `initramfs_srv`
2. `image_id=5` — `devfs_srv`
3. `image_id=6` — `vfs_server` (receives initramfs and devfs send caps in slots 13/14)
4. `image_id=7` — `driver_manager`
5. `image_id=8` — `blkcache_srv`

Each spawn is a synchronous `PROC_OP_SPAWN_V5_CAP` call-reply. The init server
does not proceed to the next spawn until it receives the reply from PM. This
guarantees that service endpoints exist in the order they are needed (e.g. VFS
can be given valid initramfs and devfs caps at spawn time).

`blkcache_srv` is spawned by init through PM's VFS-backed spawn path and is
not owned/spawned by `driver_manager`. `driver_manager` remains responsible
for hardware-driver lifecycle and resource grants only. `blkcache_srv` is
storage middleware that will consume block-driver services later; real caching,
shared-memory, and zero-copy integration are future work.

---

## Log Markers

| Marker                                | Emitted by  | Description                                                    |
|---------------------------------------|-------------|----------------------------------------------------------------|
| `PM_VFS_SPAWN_IMAGE_BEGIN`            | PM          | before `SpawnFromInitramfsFile` syscall                        |
| `PM_VFS_SPAWN_RESULT`                 | PM          | raw kernel spawn result (tid, caller_cap, spawner_cap)         |
| `PM_VFS_SPAWN_IMAGE_SELECTED`         | PM          | confirms VFS-backed path selected for image_id                 |
| `PM_VFS_SPAWN_IMAGE_UNKNOWN`          | PM          | image_id has no CPIO mapping; returns Unsupported              |
| `PM_VFS_SPAWN_FAIL`                   | PM          | spawn syscall returned an error; returns TableFull             |
| `PM_LIFECYCLE_RECORD`                 | PM          | per-spawn record: image_id, tid, pm_service_send_cap, parent_tid, state, recorded flag |
| `PM_LIFECYCLE_BOOTSTRAP`              | PM          | bootstrap seed: `tid=N image_id=M recorded=1`                  |
| `PM_LIFECYCLE_BOOTSTRAP_SKIP`         | PM          | slot was zero; deterministic fallback used: `image_id=N reason=missing_slot` |
| `PM_STARTUP_SLOT_8_INIT_TID`          | PM          | raw value of startup slot 8 at boot: `raw=N`                   |
| `PM_STARTUP_SLOT_9_SUPERVISOR_TID`    | PM          | raw value of startup slot 9 at boot: `raw=N`                   |
| `PM_LIFECYCLE_QUERY_RECV`             | PM          | opcode 12 received: `tid=N`                                    |
| `PM_LIFECYCLE_QUERY_REPLY`            | PM          | response to PROC_OP_LIFECYCLE_QUERY: `tid=N found=F image_id=M`|
| `SUPERVISOR_LIFECYCLE_QUERY`          | supervisor  | before querying PM lifecycle table during handoff              |
| `SUPERVISOR_LIFECYCLE_FOUND`          | supervisor  | PM replied found=1; `tid=N image_id=M restart_supported=0`     |
| `SUPERVISOR_LIFECYCLE_MISSING`        | supervisor  | PM replied found=0; TID not in lifecycle table                 |
| `SUPERVISOR_LIFECYCLE_QUERY_ERR`      | supervisor  | IPC error during PM lifecycle query                            |
| `INIT_*_SPAWN_V5_CALL_BEGIN`          | init_server | before each PROC_OP_SPAWN_V5_CAP call                          |
| `INIT_*_SPAWN_V5_CALL_RETURN`         | init_server | after reply: `ok=1 child_tid=N` or `ok=0`                     |

---

## Lifecycle Table in Tests

In `#[cfg(test)]` builds, the non-test spawn syscall is unavailable. The test
path uses `synthetic_elf_image()` and `manager.allocate_process()`. It **must**
call `lifecycle_table.record()` with a `ServiceLifecycleRecord` so that test
assertions on lifecycle state are valid. `pm_service_send_cap` is 0 in tests
because no real capability is minted.

`image_id=0` is rejected in the test path just as in non-test: returns
`Err(Unsupported)` before any allocation.

---

## Forbidden Patterns

- **Routing non-bootstrap images through `KernelProcessSpawnBackend`**: image_ids ≥ 4 must always use `pm_vfs_spawn_inline`. Never widen the bootstrap range without also ensuring VFS is available at that point in the boot sequence.
- **Adding to the bootstrap range**: `BOOTSTRAP_IMAGE_ID_MIN..=BOOTSTRAP_IMAGE_ID_MAX` (`1..=3`) is frozen. New services are VFS-backed by default.
- **Duplicate spawns**: `pm_vfs_spawn_inline` must be called exactly once per
  `SpawnV5Cap` request for image_ids ≥ 4. The old two-phase pattern
  (`spawn_with_caps` + post-reply `vfs_probe_pending` probe) is retired and
  must not be re-introduced.
- **`vfs_probe_pending` field**: removed; any re-introduction breaks the
  single-spawn invariant.
- **Discarding `spawner_cap`**: the third return value of `pm_vfs_spawn_inline`
  must not be ignored. Discarding it loses PM's own cap to the service when
  parent delegation is in play.
- **Fake success**: a spawn returning `Err` must propagate the error to the
  caller rather than replying with a zero TID. `pm_vfs_spawn_inline` returns
  `Err(Unsupported)` for unknown image_ids and `Err(TableFull)` on syscall
  failure — both propagate via `?` in the dispatch block.
- **Skipping lifecycle recording**: every `SpawnV5Cap` that reaches the
  `lifecycle_table.record()` call — both in non-test and test paths — must
  record the spawned service. Gaps in the lifecycle table are a bug.
- **Fabricating lifecycle data in supervisor**: supervisor must not generate
  synthetic lifecycle records. `PROC_OP_LIFECYCLE_QUERY` is the only path to
  obtain supervision metadata. `found=0` is a truthful answer, not an error.
- **Treating `found=0` as restart success**: a missing PM lifecycle record means
  restart is unsupported for that TID. The supervisor must log
  `SUPERVISOR_LIFECYCLE_MISSING` and proceed without fake token registration.
- **Fabricating restart tokens**: restart tokens must come from kernel or PM,
  never from arbitrary values. Until `PROC_OP_REGISTER_SUPERVISED_TASK` is
  wired to an authoritative source, `restart_supported=0` is the contract.
- **Fabricating arbitrary bootstrap TIDs**: the bootstrap fallback (`pm_tid - 1`
  for supervisor, `pm_tid - 2` for init) is valid only because the boot
  sequence is deterministic and these TIDs are real kernel-assigned values. Do
  not extend this arithmetic to non-bootstrap image_ids (≥ 4) or invent TIDs
  that are not derivable from `ctx.task_id` and the known spawn order.
- **Applying the bootstrap fallback to non-bootstrap image_ids**: the
  `pm_tid - N` derivation is exclusively for image_ids 1–3. Non-bootstrap
  services are always spawned via `PROC_OP_SPAWN_V5_CAP` and their TIDs are
  returned by the kernel — never derived by arithmetic.
- **Claiming restart support without a token source**: `restart_supported` must
  remain 0 in all `LifecycleQueryReply` messages until
  `PROC_OP_REGISTER_SUPERVISED_TASK` is wired to a real restart-token
  authority. Setting it to 1 without that wiring would cause the supervisor to
  attempt a restart it cannot complete.
