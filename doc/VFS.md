<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Umut Deniz Balkan -->

# YARM VFS

> **Ownership rule.** VFS request-loop ABI, shared-I/O contract, shared-I/O
> mapper requirements, and proc/VFS typed codec freeze constants live here.
> Per-server filesystem behavior lives in
> `doc/FILESYSTEM_AND_STORAGE_CONTRACTS.md`. The kernel-side directive
> status lives in `doc/KERNEL_UNLOCKING.md`. New VFS fragment files are
> forbidden; update this doc instead. See `doc/DOCUMENTATION_MAP.md`.

The authoritative implementation lives in
`crates/yarm-control-plane-servers/src/control_plane/vfs/service.rs`.

---

## 1. VFS server position in the capability graph

The VFS server requires three caps before entering its resident loop:

| Slot | Constant | Content |
|------|----------|---------|
| 12 | `process_manager_service_recv_ep` | VFS server's own IPC receive endpoint cap |
| 13 | `service_extra_cap_0` | send cap to `initramfs_srv` (for CPIO lookups) |
| 14 | `service_extra_cap_1` | send cap to `devfs_srv` (for `/dev/` requests) |

These caps are delivered by the PM spawn path: the init server places the
initramfs and devfs send caps into `SpawnV5CapArgs.service_caps[0]` and
`[1]` before calling `PROC_OP_SPAWN_V5_CAP` for `image_id=6`. PM copies
them to slots 13 and 14 of the VFS task's startup args. Slot 12 is the recv
endpoint cap the VFS server blocks on in its resident loop. It is distinct
from PM's recv cap.

---

## 2. Two-phase startup

### Phase 1 — Setup loop (`run_request_loop`)

The VFS server accepts control-plane probe messages on the path prefix
`b"/control-plane/vfs-probe"`. Purpose: allow PM or init to verify the VFS
server is live and has completed mount-table registration before any
client opens files. The setup loop uses
`synthetic_roundtrip_call_reply_with_budget`
(`crates/yarm-control-plane-servers/src/control_plane/ipc_roundtrip.rs`),
which internally calls `ipc_recv_with_deadline` (timed receive) and
`ipc_reply` (reply-cap send). This satisfies the Phase 6 migration
invariant: no legacy blocking `kernel.ipc_recv(` calls.

The setup loop exits when the probe is acknowledged; control transfers to
Phase 2.

### Phase 2 — Resident loop

The resident loop calls `ipc_recv_v2(recv_cap)` on slot 12 indefinitely.
For each delivered message it routes by opcode, performs the operation,
and replies through the reply cap.

---

## 3. Frozen opcodes (v1)

Authoritative values from
`crates/yarm-ipc-abi/src/vfs_abi.rs`. Also pinned in
§ "Proc/VFS typed codec freeze" below.

| Name | Value | Direction | Purpose |
|------|-------|-----------|---------|
| `VFS_OP_OPENAT` | `10` | client → VFS | open a file, returns fd |
| `VFS_OP_CLOSE` | `11` | client → VFS | close fd, release table entry |
| `VFS_OP_READ` | `12` | client → VFS | read from open fd |
| `VFS_OP_WRITE` | `13` | client → VFS | legacy length-only write |
| `VFS_OP_IOCTL` | `14` | client → VFS | (reserved) |
| `VFS_OP_STATX` | `22` | client → VFS | stat a path (no fd required) |
| `VFS_OP_WRITE_INLINE` | `28` | client → VFS | inline write (FAT-only route; 1–96 bytes payload, current-open-file-offset semantics) |
| `VFS_OP_MOUNT_REGISTER` | (kernel-frozen) | service → VFS | dynamic mount registration |

Live shared-I/O opcodes `26`, `27`, and `28` (generic) **remain unsupported** at
the generic `VfsService` level. The exact-inline `VFS_OP_WRITE_INLINE = 28`
is FAT-only (the FAT service implements it directly; the generic router
rejects it). See §5 for the shared-I/O status.

Reply messages use opcode `1` for any locally generated error and `0` for a
successful response (some opcodes use specific reply opcodes; see source).

---

## 4. Fd / mount table semantics

### Fd table

An fd entry is created by `VFS_OP_OPENAT` on success and removed by
`VFS_OP_CLOSE` from the **owning client**.

- A `VFS_OP_READ` or `VFS_OP_CLOSE` for an fd not owned by the calling
  client returns `VFS_STATUS_ERR_BAD_FD`.
- `VFS_OP_CLOSE` only evicts the calling client's own entry; another
  client's open is unaffected.

### Mount table

- Dynamic mount registration via `VFS_OP_MOUNT_REGISTER` (typically called
  by init for `ramfs`, `ext4`).
- Mount-table changes are committed before the request reply.

### Unknown opcode

The router replies `VFS_STATUS_ERR_UNKNOWN_OP` immediately and continues —
the loop must not drop the recv slot.

---

## 5. Shared-I/O contract (currently unsupported in production)

The VFS shared-region mapper design (FS-19) concluded that a
**userspace-only production mapper is not currently safe**. A future,
separately reviewed kernel ABI is required to return authoritative object
metadata and to resolve the legacy `ipc_recv` versus `recv-v2` map-intent
lane conflict.

### Current state

- `UnsupportedSharedIoMapper` remains the production default.
- Live VFS opcodes `26`, `27`, and `28` (generic) **remain unsupported**.
- RAMFS shared-I/O remains test/helper-only.
- FAT production writes remain unwired.
- ext4 remains read-only.

### Required future mapping metadata

A future versioned mapped-receive result must be logically equivalent to
`MappedSharedObjectMeta` and authoritatively establish:

1. That the transferred capability names a `MemoryObject` or future
   `SharedBuffer` (vs an endpoint / reply / notification / address space /
   other).
2. The effective receiver-local rights after attenuation.
3. Total object size + exact unrounded transferred region length.
4. Object / transfer generation stable across cap-slot reuse.
5. Delivered VFS opcode and request ID on the **same atomic receive** that
   created the mapping.
6. A cleanup-token identity that cannot be confused with a later mapping
   of the same local cap slot.
7. Requester ownership and requester-exit cleanup authority.
8. Whether revocation occurred while the server was using the range.

No existing userspace wrapper can synthesize these facts. A userspace
broker could provide policy and allocation, but it cannot authoritatively
identify kernel objects or rights without a trusted kernel query/result.

### Information available today (audit summary)

| Source | Available fields | Authority |
|--------|------------------|-----------|
| legacy mapped `ipc_recv` / `MappedTransferRecv` | sender TID, receiver-local cap, caller-selected mapping base, page-rounded mapped length, requested map intent | Kernel-authoritative for mapping + local cap; does **not** identify the delivered opcode or exact unrounded length |
| `SharedMemoryRegion` payload | sender offset + exact region length | Decoded by kernel during mapping; not returned in a portable mapped-receive result |
| recv-v2 out metadata | sender TID, application opcode/flags, payload length, returned cap, reply/transfer classification | Cannot currently carry explicit shared map intent (the metadata-length argument occupies the map-intent lane) |
| `VfsSharedBufferDescriptor` | opaque handle, descriptor generation, buffer offset/length, intended access | Helper/service assertion only — **not** a kernel cap slot |
| `VfsReadSharedRequest` / `VfsWriteSharedRequest` | VFS opcode by envelope, FD, file offset mode, requested length, request ID, flags, descriptor | Disabled helper ABI; no mapped transfer is atomically bound to it |
| `VfsSharedIoHandleTable` | local handle allocation + generation reuse protection | Protects helper-local lifecycle state, not kernel identity |
| `VfsSharedIoLifecycle` | direction, request state, terminal reason, first-winner cleanup, fallback gating | Does not own a real mapping / cap / process-exit hook / timeout source / revocation notification |
| `TransferReleaseToken` | local cap + explicit base/length or active-record release | Metadata-only; does not prove object type, rights, request identity, or exact region length |

---

## 6. Proc/VFS typed codec freeze (v2+)

> **CI gate token.** `scripts/check-proc-vfs-codec-freeze.sh` enforces these
> values; `scripts/check-contract-doc-enforcement.sh` greps for the
> `PROC_CODEC_V2_VERSION = 2`, `VFS_CODEC_V1_VERSION = 1`, and
> `scripts/check-proc-vfs-codec-freeze.sh` literals in **this section**. Do
> not rename or reword the constants below without updating both scripts.

### Process Manager (`src/kernel/process_abi.rs`)

- Server ABI version: `PROC_SERVER_ABI_VERSION = 1`
- Typed codec version: `PROC_CODEC_V2_VERSION = 2`
- Extended typed codec versions:
  - `PROC_CODEC_V3_VERSION = 3` (`ProcV3Args`)
  - `PROC_CODEC_V4_VERSION = 4` (`ProcV4Args`)
- Typed request args: `ProcV2Args`
  - Encoding: little-endian `[arg0:u64, arg1:u64]`
  - Exact payload length: `16` bytes
  - Decode policy: **exact-length only** (reject truncated or oversized)
  - Golden vector (stable test fixture):
    - args = `(0x1122334455667788, 0x99aabbccddeeff00)`
    - bytes = `88 77 66 55 44 33 22 11 00 ff ee dd cc bb aa 99`

Additional frozen process payloads:

- `ProcV3Args(parent_pid, image_id, requested_cnode_slots)`
  - Encoding: little-endian `[arg0:u64, arg1:u64, arg2:u64]`
  - Exact payload length: `24` bytes
  - Decode policy: **exact-length only**
- `ProcV4Args(parent_pid, image_id, requested_cnode_slots, task_class_hint)`
  - Encoding: little-endian `[arg0:u64, arg1:u64, arg2:u64, arg3:u64]`
  - Exact payload length: `32` bytes
  - Decode policy: **exact-length only**
  - `task_class_hint` values: `0 = TaskClass::App`, `1 = TaskClass::Driver`,
    `2 = TaskClass::SystemServer`.

Opcodes frozen in this phase:

- `PROC_OP_GETPID = 1`
- `PROC_OP_EXIT = 2`
- `PROC_OP_GETPPID = 3`
- `PROC_OP_SPAWN_V2 = 4`
- `PROC_OP_WAITPID_V2 = 5`
- `PROC_OP_SPAWN_V3 = 6`
- `PROC_OP_SPAWN_V4 = 7`

### VFS (`src/kernel/vfs_abi.rs`)

- Server ABI version: `VFS_SERVER_ABI_VERSION = 1`
- Typed codec version: `VFS_CODEC_V1_VERSION = 1`
- Typed request args: `VfsV1Args`
  - Encoding: little-endian `[arg0:u64, arg1:u64, arg2:u64, arg3:u64]`
  - Exact payload length: `32` bytes
  - Decode policy: **exact-length only**
  - Golden vector:
    - args = `(0x0102030405060708, 0x1112131415161718,
       0x2122232425262728, 0x3132333435363738)`
    - bytes = `08 07 06 05 04 03 02 01 18 17 16 15 14 13 12 11
       28 27 26 25 24 23 22 21 38 37 36 35 34 33 32 31`

### Compatibility policy

Any version or payload-width change must:

1. Introduce a new codec version constant and typed struct.
2. Keep old decode paths intact until all call-sites migrate.
3. Add explicit round-trip and malformed-vector tests for both old and
   new versions.

CI gate: `scripts/check-proc-vfs-codec-freeze.sh` enforces version
constants and runs golden-vector tests.

---

## 7. Smoke / observability markers

The VFS server resident loop emits:

- `VFS_SRV_RECV_MSG op=N` — message received with opcode N.
- `VFS_ROUTE_UNKNOWN_OP op=N` — opcode not handled by router.

Optional-FS strict smoke markers (do not rename or remove — see
`doc/KERNEL_UNLOCKING.md` §3):

- `VFS_MOUNT_REGISTER_RAMFS_OK`
- `VFS_MOUNT_REGISTER_EXT4_OK`

---

## 8. Authoring rule

Future VFS changes update **this file** and (where applicable)
`crates/yarm-ipc-abi/src/vfs_abi.rs`. Do **not** create new `VFS_*` /
`PROC_VFS_*` fragment files. Closed phase / milestone outcomes belong in
`doc/PROJECT_HISTORY.md`.
