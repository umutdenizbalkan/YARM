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
| 17 | `service_admin_recv_ep` | receive side of the mount-admin endpoint (§3.2; VFS-CORRECT1B) |

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
| `VFS_OP_MOUNT_REGISTER` | `23` | — | retired by VFS-CORRECT1B: always refused, status 5 (§3.2) |
| `VFS_OP_MOUNT_STAGE` | `29` | administrator → VFS admin endpoint | stage a mount, backend endpoint transferred (§3.2) |
| `VFS_OP_MOUNT_COMMIT` | `30` | administrator → VFS | commit a staged mount (§3.2) |

Live shared-I/O opcodes `26`, `27`, and `28` (generic) **remain unsupported** at
the generic `VfsService` level. The exact-inline `VFS_OP_WRITE_INLINE = 28`
is FAT-only (the FAT service implements it directly; the generic router
rejects it). See §5 for the shared-I/O status.

Replies to the operations the router forwards are payload completions (§3.1); the reply
opcode is not part of the contract (NR 7 does not carry it). Mount registration replies with a
4-byte status (§3.2).

### 3.1 Request/reply transaction and completions (VFS-CORRECT1A)

One VFS operation is one NR 6 (`IpcCall`) and, **only when NR 6 succeeded**, one blocking
`ipc_recv_v2` on the caller's reply endpoint. Any NR 6 failure — `WouldBlock` included — has
delivered nothing (the kernel's send path returns the error and removes the transfer envelope), so
it is returned at once and no receive is attempted. This holds for the client wrappers
(`yarm_user_rt::vfs_client`), for the router's forward to a backend, and for PM's VFS calls.
Nothing is retried; in particular a READ is never re-sent, because a second READ after an
ambiguous completion could advance the file cursor twice.

NR 7 (`IpcReply`) delivers the reply's payload bytes and at most one transferred capability; the
kernel rebuilds the message from the payload, so the replier's `Message.opcode` does not reach the
caller. Every reply to an operation the router forwards (STATX, OPENAT, READ, CLOSE,
FILE_GRANT_RO), and every router-local error for one, is therefore a **completion carried in the
payload** (`yarm_ipc_abi::vfs_abi::VfsCompletion`):

| offset | size | field | |
|---|---|---|---|
| 0 | 2 | magic | `VFS_COMPLETION_MAGIC` (`b"VC"`) |
| 2 | 2 | op | the `VFS_OP_*` this completes |
| 4 | 4 | status | `VFS_STATUS_OK` or a `VFS_STATUS_ERR_*` |
| 8 | 8 | value | OPENAT: fd; READ: byte count; STATX: the statx value; CLOSE: 0; FILE_GRANT_RO: file length; 0 on error |
| 16 | 8 | fd | the request's fd (READ, CLOSE, FILE_GRANT_RO); 0 for path operations |
| 24 | n | data | READ success only: exactly `value` bytes |

The reply carries a transferred capability exactly when it is a successful FILE_GRANT_RO, and never
a reply capability. All fields are little-endian. Kernel syscall ABI v13 is unchanged; this is a
service wire format.

**Limits.** An NR 7 reply carries at most `Message::MAX_PAYLOAD` = 128 payload bytes, so one READ
completion carries at most `VFS_INLINE_READ_MAX` = 128 − 24 = **104** bytes. NR 6 places the
2-byte opcode in front of the request payload inside the same 128-byte limit, so a request payload
is at most `VFS_REQUEST_PAYLOAD_MAX` = 126 bytes (a READ request is 32).

**Validation** (`validate_vfs_completion`, shared by the client, the router and PM) happens before
a byte is copied, an fd is published or a grant is consumed: magic and length, operation, fd echo,
status (an error carries value 0, no data and no capability), the READ count against the request
(≤ requested, ≤ 104) and against the bytes actually carried (equal), and the capability shape. A
caller can tell apart: a transport failure (`VfsClientError::Send` / `Recv` / `NoReply`), a
malformed completion (`Malformed(fault)`), a router or backend error (`Status(code)`), and end of
file (`Ok(0)` for a nonempty request).

**READ semantics.** `vfs_read(fd, buf)` asks for `min(buf.len(), 104)` bytes. The resident server
asks its backend for at most that many **before** the backend advances its cursor, so the next READ
starts at the first undelivered byte, and the completion carries exactly the bytes delivered; a
backend that reports a count without bytes (no byte source) answers `VFS_STATUS_ERR_UNSUPPORTED`,
never a success. On success the client copies exactly the validated count into `buf[..n]` and
leaves `buf[n..]` untouched; a short read is valid. On any error `buf` is untouched. A zero-length
request is valid and returns 0; EOF repeats.

**Status codes** (`VFS_STATUS_ERR_*`): 1 unknown op, 2 invalid path (including a path the backend
does not have), 3 no mount, 4 bad fd (not in the router's table for this client, or unknown to the
backend), 5 backend transport (forward send or backend receive failed), 6 codec, 7 unsupported, 8
permission (backend path policy), 9 no free fd, 10 backend completion malformed (the router
validated the backend's reply and did not relay it).

**Router.** The router validates each backend completion for the forwarded (op, fd, length) before
it touches its fd table or relays anything: an OPENAT fd is tracked only from a valid success, a
CLOSE removes the entry only on a valid success, and a malformed backend completion is answered with
status 10 instead of being relayed. `VFS_OP_MOUNT_REGISTER` keeps its own 4-byte status reply;
`VFS_OP_READ_BULK` (retired since Stage 197A) is relayed unvalidated.

**PM.** PM publishes an OPENAT fd and consumes a FILE_GRANT_RO MemoryObject only from a validated
completion (status, op, fd echo, a transferred cap present exactly on success); any other outcome
is a hard failure for that image, as before (no fallback exists).

**Fixture.** Every QEMU initramfs carries `etc/vfs-fixture`, served as `/initramfs/etc/vfs-fixture`:
300 bytes, `byte[i] = (i * 131 + 43) mod 256` (`scripts/fixtures/README.md`). Later VFS passes
compare against it.

**Not in this contract** (later passes): fd namespaces, grant reclamation, shared buffers and rings, a per-request nonce (association is the
operation plus the fd; the one-shot reply capability and the client's sequential calls cover the
rest), and any other service's send/reply path.

### 3.2 Mount authority and capability identities (VFS-CORRECT1B)

**Capability identities are full width.** A `CapId` is `generation << 16 | slot index`, a `u64`;
a slot reused more than 65,535 times has an ID above `u32::MAX`. Every user-runtime path carries
the whole value: startup slots (`cap_from_slot`), NR 2 receive metadata (`decode_recv_meta_v2`:
reply capability, transferred capability and the `NO_TRANSFER_CAP` sentinel stay distinct), the
call/reply/send wrappers, the VFS client, VFS's mount and fd routing records, the backends' own
endpoints and PM's FILE_GRANT_RO relay into NR 29. NR 23 and NR 29 return three unpacked lanes
(`doc/SYSCALL_ABI.md`, "Within v13 (VFS-CORRECT1B)"). A truncated ID names another generation of
the same slot, which the kernel refuses as `InvalidCapability`.

**Mount administration is a capability.** At spawn PM asks the kernel for a third endpoint for
`image_id=6` (NR 29 `arg5`, or NR 23 `arg4`): VFS gets its RECEIVE side in startup slot 17, PM gets
the SEND side and returns it to the spawn requester (init) as a transferred capability on the
`PROC_OP_SPAWN_V5_CAP` reply. An ordinary client holds only VFS's public endpoint (slot 12's
send side) and cannot reach the admin endpoint. The authority check is the kernel's: a message on
the admin endpoint was sent by a holder of its SEND capability.

A registration is two steps (`yarm_fs_servers::fs::common::mount_admin`):

1. **Stage.** NR 1 (`IpcSend`) to the **admin** endpoint, opcode `VFS_OP_MOUNT_STAGE`, payload
   `MountStageArgs` (`flags: u64`, `sender_local_cap: u64` — informational, never used for
   routing — `prefix_len: u8`, prefix), and the backend's service endpoint as the message's
   **transferred capability**. The kernel checks the administrator's capability (a stale or
   wrong one refuses the send at the sender; nothing is queued) and installs a copy of the
   backend capability in VFS's CNode under a **VFS-local** ID. The sender keeps its own copy.
2. **Commit.** NR 6 to the **public** endpoint, opcode `VFS_OP_MOUNT_COMMIT`, payload the prefix;
   the reply is a 4-byte status and carries no capability. VFS first drains its admin endpoint
   (non-blocking), then matches the commit to a staged mount by the kernel-authenticated sender
   TID and the prefix. In order: no match → 6 `NOT_STAGED`; flags outside
   `VFS_MOUNT_FLAGS_SUPPORTED` (only `VFS_MOUNT_FLAG_READONLY = 1`) → 8 `UNSUPPORTED_FLAGS`; an
   invalid prefix → 4, a duplicate → 1, a full table → 2; then the **handshake**, one
   `VFS_OP_STATX` of the prefix through the staged VFS-local capability, where the kernel enforces
   object kind, SEND right and live generation (`WrongObject`, `MissingRight`,
   `InvalidCapability`) and a backend must answer with a valid completion → otherwise 7
   `BACKEND`; only then is the mount inserted (0), routing through the VFS-local ID.

`VFS_OP_MOUNT_REGISTER` (23), which named the backend by the **sender's** numeric slot, is refused
with 5 `PERMISSION` whatever it names, as is a commit when VFS has no admin endpoint.

**Failure cleanup.** A refused commit leaves the routing table exactly as it was; existing routes
keep working. No system call deletes a capability, so VFS cannot hand back the copy the stage
brought in: it moves it to a bounded **retained** record (`MAX_RETAINED_CAPS = 8`), logged with
`VFS_MOUNT_ADMIN_REFUSED … retained_cap=` and never routed. A stage that cannot be recorded
(malformed payload, a second stage of the same sender and prefix) is retained the same way. At most
`MAX_STAGED_MOUNTS = 4` stages are held; VFS drains its admin endpoint only while every staged
mount could still be retained, so each acquired capability is always either published or
retained. Past that bound further admin messages stay queued in the kernel and commits answer 6.
Statuses 5 to 8 are new; 0 to 4 keep their meaning.

**Live witness** (`vfs-mount-witness`; optionally with the kernel's `cap-gen-witness`, which
starts every CNode slot at generation 0x1_0001 so every ID exceeds 2^32): init's
`init/vfs_mount_witness.rs` drives the production client against the resident VFS and ramfs,
which seeds `/ram/vc1b-witness` (192 bytes, `byte[i] = (i * 37 + 11) mod 256`). Graded by
`scripts/grade-vfs-mount-witness.py`; `scripts/qemu-vfs-mount-witness-smoke.sh ARCH SMP wide|narrow`
also runs the VFS-CORRECT1A grader on the same boot.

**Limits.** The retained record is never drained (eight refusals exhaust mount administration
until VFS restarts); a stage that is never committed holds its slot; association is sender TID
plus prefix, with no nonce; one administrator capability, no delegation or revocation of mount
authority; unmounting does not exist.

### 3.3 Client fd namespace, sessions and canonical paths (VFS-CORRECT1C)

The resident router is `crates/yarm-control-plane-servers/src/control_plane/vfs/router.rs`; the
descriptor model is `yarm_fs_servers::fs::common::fd_space`. Every request except mount
administration is handled in this order.

**1. Who is asking: the session.** A session is a **process**, named by the kernel as
`(thread_group_id, process_incarnation)` through NR 21 `IpcCallerSession` op 0 on the request's
reply capability (`doc/SYSCALL_ABI.md`, ABI v14). Every thread of a process shares it; a process
incarnation is never reused, so a recycled TID or thread-group id names a different session. A
request whose reply capability names no live caller is refused with 8 `PERMISSION` and nothing is
forwarded. The NR 6 sender TID is no longer an identity for anything but mount administration
(§3.2).

**2. Housekeeping.** Before routing, VFS retries at most one undelivered backend close (below) and
asks the kernel, for every other session holding descriptors, whether it still has a live thread
(NR 21 op 1; VFS is a `SystemServer`). A session with none is **reclaimed**: its descriptors are
removed and every backend handle only they referred to is closed at the instance that opened it.
Reclamation is therefore driven by the next request from any client, not by an exit notification.

**3. Descriptors and open file descriptions.** A client fd is chosen by VFS: the lowest number
`>= 3` the session does not hold. It names a descriptor (session, fd), which refers to an open
file description (OFD): the exact backend instance — the mount registration's instance id, unique
for the life of VFS, plus VFS's own full-width endpoint capability — and the backend's handle. A
backend handle never reaches a client and a client fd never reaches a backend. Capacity:
`MAX_DESCRIPTORS = 32` in total, `MAX_FDS_PER_SESSION = 16`.

| op | client → VFS | VFS → backend | backend completion validated against | client completion |
|---|---|---|---|---|
| `STATX` | path | the canonical path, `dirfd` 0, flags and mask as sent | op, fd 0 | value as returned, fd 0 |
| `OPENAT` | path | the canonical path, `dirfd` 0, flags and mode as sent | op, fd 0 | value = the new **client fd**, fd 0 |
| `READ` | client fd, len | backend handle, same len | op, **the forwarded handle**, len | the same bytes and count, fd = client fd |
| `FILE_GRANT_RO` | client fd | backend handle, other fields as sent | op, the forwarded handle, cap present | value as returned, fd = client fd, the capability transferred on |
| `CLOSE` | client fd | backend handle, only when no other descriptor refers to the OFD | op, the forwarded handle | value as returned (or 0), fd = client fd |

A completion that names any other fd — a backend echoing a client fd, say — is malformed (10).
An fd the session does not hold, or one whose instance is no longer the registration in the mount
table, answers 4 `BAD_FD` and is not forwarded.

**4. Open is recoverable.** OPENAT reserves the descriptor and OFD **before** the backend is asked:
a full session or table answers 9 `NO_FD` and nothing is opened anywhere. Then:

- the backend refuses (a valid error completion) → the reservation is released, the status
  relayed;
- NR 6 fails (nothing delivered) → released, 5 `BACKEND`;
- the backend opened handle H → the descriptor is committed and the client told its fd; if that
  NR 7 is refused (the client cannot be told), the open is **rolled back**: the descriptor goes
  and H is closed at the same instance (`VFS_OPEN_ROLLBACK`);
- no reply, or a malformed one → released, 5 or 10, and the open is counted as
  `unknown_backend_opens`: the backend may hold a handle nobody can name. It is never closed by
  guess, because the number may already belong to someone else.

**5. Close and its failures.** CLOSE removes the descriptor first; a failed backend close still
releases the client's fd (as POSIX `close`). The backend close itself settles as: answered →
done (a refusal's status is relayed); NR 6 failed → **pending**, retried later against the same
instance and handle (at most one per request; `MAX_PENDING_CLOSES = 8`, overflow counted); sent but
no valid answer → **quarantined**, never retried, counted. The same settlement covers the rollback
close and reclamation closes. A session that exits mid-operation cannot: VFS is single-threaded,
so a reservation lives only inside one request.

**6. Canonical paths.** For STATX and OPENAT the path is normalized once (`vfs::path::normalize`:
repeated `/` collapse, `.` is dropped, `..` pops a component and stays at `/`, a trailing `/` is
dropped, at most 96 bytes, absolute only); that one result selects the mount (longest prefix, on
whole components: `/ram/bx` is not under `/ram/b/`, `/ram/b` is its root) and is the path the
backend receives, re-encoded with the other fields preserved. A canonical path is never longer
than the one decoded, so the wire limit holds. Symlinks do not exist here.

**7. `dirfd`, `dup`, `fork`, `exec`.** Paths must be absolute, so `dirfd` takes no part; a
nonzero one is logged (`VFS_DIRFD_IGNORED`) and not forwarded. `dup` is not implemented
(`VFS_OP_DUP` answers 1 `UNKNOWN_OP`; the OFD keeps a reference count for it). A fork child is a
new process and so a **new session with no descriptors**: it does not inherit its parent's fds,
and the parent's numbers resolve nothing in it. An `exec` gives the process a new incarnation, so
its old session is reclaimed. `VFS_OP_READ_BULK` (24) is retired: 7 `UNSUPPORTED`. Other fd-bearing
opcodes (`WRITE`, `IOCTL`, `FCNTL`, `POLL`, `EPOLL_*`, `SENDFILE`, shared I/O) answer 1.

**Backend restart.** No path removes or replaces a mount yet. If one did, the new registration gets
a new instance id; an old descriptor's instance is no longer in the table, so it answers `BAD_FD`
and is never forwarded — even to a restarted backend that hands out the same handle numbers.
`Router::instance_gone` then marks its OFDs stale and drops its pending closes (hosted tests only).

**Live witness** (`vfs-fd-witness`; `scripts/qemu-vfs-fd-witness-smoke.sh ARCH SMP wide|narrow`,
graded by `scripts/grade-vfs-fd-witness.py` and, on the same boot, the VFS-CORRECT1A grader):
init's `init/vfs_fd_witness.rs` spawns a second ramfs, mounts it at `/ram/b`, and drives the
two-backend handle collision, canonical spellings and mount boundaries, the full session, a failed
client reply (VFS's one injection under this feature: an OPENAT of `/ram/vc1c-failreply` is
answered first through a stale reply capability), a second thread, and a fork child that exits
holding two descriptors. Each ramfs seeds its files with bytes of its own TID and answers a STATX of
`…/vc1c-stat` with its handle counts.

The witness passes at SMP1 under the unmodified core smokes. At SMP2 its thread and fork child
can run on another CPU while VFS blocks in its receive. They then reach the split receive route's
queue-publication race (`doc/IPC.md` §8.7). The route unwinds exactly and the message is still
received. But the `D2_PUBLISH_RACE_UNWIND` it logs is a stop-ship marker
(`doc/AI_AGENT_RULES.md` §14.3), so every core smoke rejects that boot, and so does the
grader's RACE rule. No exclusion admits it.

**Limits.** Reclamation waits for the next request; a quarantined or unknown backend open stays
open at the backend for its life; descriptors are not shared across processes (no fd passing, no
`dup`); a late reply to a request VFS gave up on could still arrive on its backend reply endpoint
(completion validation rejects one for another operation or fd); grant capabilities a client never
releases are VFS-LIFETIME1's.

---

## 4. Fd / mount table semantics

### Fd table

The client fd namespace of §3.3: descriptors are per **session** (process), numbered by VFS,
and refer to open file descriptions bound to one backend instance and handle.

- A `VFS_OP_READ`, `VFS_OP_CLOSE` or `VFS_OP_FILE_GRANT_RO` for an fd the calling session does not
  hold returns `VFS_STATUS_ERR_BAD_FD`, and nothing is forwarded.
- `VFS_OP_CLOSE` removes only the calling session's descriptor; another session's file with the
  same number is unaffected.

### Mount table

- Dynamic mount registration is the two-step stage/commit protocol of §3.2, by the holder of the
  mount-admin capability (init, for `ramfs`, `ext4` and optionally FAT).
- Mount-table changes are committed before the commit's reply; a refused commit changes nothing.

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
- `VFS_ROUTE_ERR status=N` — a router-local error completion (§3.1).
- `VFS_FORWARD_CALL_FAIL op=N err=E` — the forward NR 6 failed; the client got status 5 and the
  backend reply endpoint was not received on.
- `VFS_BACKEND_STATUS op=N fd=F status=S` — a valid backend error completion, relayed.
- `VFS_BACKEND_COMPLETION_MALFORMED op=N fd=F fault=… has_cap=…` — a backend completion failed
  validation; the client got status 10.
- Live witness (init built with `vfs-correct-witness`, `scripts/qemu-vfs-correct-witness-smoke.sh`):
  `VC1A_*`, graded by `scripts/grade-vfs-correct-witness.py`.
- VFS-CORRECT1C (§3.3): `VFS_FD_SPACE_READY`, `VFS_FD_OPEN tgid= incarnation= tid= client_fd=
  instance= target= backend_handle=`, `VFS_FD_CLOSE … outcome=`, `VFS_FD_BAD op= fd= reason=`,
  `VFS_FD_NO_FREE reason=session_full|table_full`, `VFS_OPEN_ROLLBACK`,
  `VFS_OPEN_BACKEND_STATE_UNKNOWN`, `VFS_CLOSE_RETRY`, `VFS_SESSION_RECLAIM`,
  `VFS_SESSION_UNKNOWN`, `VFS_CALLER_SESSION_FAIL`, `VFS_SESSION_ALIVE_FAIL`,
  `VFS_CLIENT_REPLY_REFUSED`, `VFS_DIRFD_IGNORED`, `VFS_ROUTE_LOOKUP path=<canonical> target=
  instance= canonical=0|1`, and `VFS_FD_STATE why= descriptors= ofds= sessions= pending_retry=
  quarantined= pending_overflow= unknown_backend_opens=` after each change. The live witness
  (`vfs-fd-witness`) logs `VC1C_*`, graded by `scripts/grade-vfs-fd-witness.py`.

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
