<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Umut Deniz Balkan -->

# PM Spawn Contract

This document defines the authoritative contract for process-manager service
spawning, capability delivery, and lifecycle tracking as implemented in
`crates/yarm-control-plane-servers/src/control_plane/process_manager/service.rs`.

---

## Image ID → Binary Path Table

| image_id | CPIO path               | Spawn backend            |
|----------|-------------------------|--------------------------|
| 0        | (init, kernel-internal) | kernel direct            |
| 1        | `sbin/supervisor`       | `KernelProcessSpawnBackend` |
| 2        | `sbin/process_manager`  | `KernelProcessSpawnBackend` |
| 3        | `sbin/init_server`      | `KernelProcessSpawnBackend` |
| 4        | `sbin/initramfs_srv`    | `pm_vfs_spawn_inline`    |
| 5        | `sbin/devfs_srv`        | `pm_vfs_spawn_inline`    |
| 6        | `sbin/vfs_server`       | `pm_vfs_spawn_inline`    |
| 7        | `sbin/driver_manager`   | `pm_vfs_spawn_inline`    |

image_ids 4–7 use `SpawnFromInitramfsFile` (syscall nr=26) via
`pm_vfs_spawn_inline`. image_ids 1–3 use the direct kernel spawn backend.
image_id=0 is never spawned by PM.

---

## Startup Slot Layout (0–17)

The kernel populates a `startup_args: [u64; 18]` array passed to every spawned
task. Slot assignments are defined in
`crates/yarm-user-rt/src/lib.rs` (`StartupContext`).

| Slot | Name                              | Set by     | Content                                         |
|------|-----------------------------------|------------|-------------------------------------------------|
| 0    | `task_id`                         | kernel     | spawned task's own TID                          |
| 1    | `proc_mgr_request_send_cap`       | kernel     | send cap to PM request endpoint                 |
| 2    | `proc_mgr_reply_recv_cap`         | kernel     | recv cap for PM reply channel (task-local)      |
| 3    | `supervisor_send_cap`             | kernel     | send cap to supervisor endpoint                 |
| 4    | `init_server_send_cap`            | kernel     | send cap to init_server endpoint                |
| 5    | `vfs_send_cap`                    | kernel     | send cap to VFS endpoint (may be 0 pre-VFS)     |
| 6    | `reserved_6`                      | —          | reserved, 0                                     |
| 7    | `reserved_7`                      | —          | reserved, 0                                     |
| 8    | `reserved_8`                      | —          | reserved, 0                                     |
| 9    | `reserved_9`                      | —          | reserved, 0                                     |
| 10   | `reserved_10`                     | —          | reserved, 0                                     |
| 11   | `reserved_11`                     | —          | reserved, 0                                     |
| 12   | `process_manager_service_recv_ep` | PM/kernel  | service recv cap for the spawned task           |
| 13   | `service_extra_cap_0`             | PM caller  | extra service cap (e.g. initramfs send cap)     |
| 14   | `service_extra_cap_1`             | PM caller  | extra service cap (e.g. devfs send cap)         |
| 15   | `service_extra_cap_2`             | PM caller  | extra service cap                               |
| 16   | `service_extra_cap_3`             | PM caller  | extra service cap                               |
| 17   | `reserved_17`                     | —          | reserved, 0                                     |

Slot 12 carries the service's own receive endpoint capability. This is the cap
the service blocks on in its resident IPC loop. It is distinct from the PM's own
recv cap (slot 2 of the PM's own startup context).

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

---

## Sequential Spawn Ordering

The init server spawns services in strict order:

1. `image_id=4` — `initramfs_srv`
2. `image_id=5` — `devfs_srv`
3. `image_id=6` — `vfs_server` (receives initramfs and devfs send caps in slots 13/14)
4. `image_id=7` — `driver_manager`

Each spawn is a synchronous `PROC_OP_SPAWN_V5_CAP` call-reply. The init server
does not proceed to the next spawn until it receives the reply from PM. This
guarantees that service endpoints exist in the order they are needed (e.g. VFS
can be given valid initramfs and devfs caps at spawn time).

---

## Log Markers

| Marker                          | Emitted by       | Description                                     |
|---------------------------------|------------------|-------------------------------------------------|
| `PM_VFS_SPAWN_RESULT`           | PM               | raw kernel spawn result (tid, caller_cap, spawner_cap) |
| `PM_VFS_SPAWN_IMAGE_SELECTED`   | PM               | confirms VFS-backed path selected for image_id  |
| `PM_VFS_SPAWN_ERROR`            | PM               | spawn syscall returned an error                 |
| `PM_LIFECYCLE_RECORD`           | PM               | per-spawn record: image_id, tid, pm_service_send_cap, parent_tid, state, recorded flag |
| `INIT_*_SPAWN_V5_CALL_BEGIN`    | init_server      | before each PROC_OP_SPAWN_V5_CAP call           |
| `INIT_*_SPAWN_V5_CALL_RETURN`   | init_server      | after reply: ok=1 child_tid=N or ok=0           |

---

## Forbidden Patterns

- **Duplicate spawns**: `pm_vfs_spawn_inline` must be called exactly once per
  `SpawnV5Cap` request for image_ids 4–7. The old two-phase pattern
  (`spawn_with_caps` + post-reply `vfs_probe_pending` probe) is retired and
  must not be re-introduced.
- **`vfs_probe_pending` field**: removed; any re-introduction breaks the
  single-spawn invariant.
- **Discarding `spawner_cap`**: the third return value of `pm_vfs_spawn_inline`
  must not be ignored. Discarding it loses PM's own cap to the service when
  parent delegation is in play.
- **Fake success**: a spawn returning `Err` must propagate
  `ProcessManagerError::TableFull` (or appropriate variant) to the caller
  rather than replying with a zero TID.
- **Spawning image_ids 4–7 via `KernelProcessSpawnBackend`**: these must always
  go through `pm_vfs_spawn_inline` (ELF loaded from CPIO via
  `SpawnFromInitramfsFile`). The legacy direct-spawn backend does not load from
  the initramfs image.
