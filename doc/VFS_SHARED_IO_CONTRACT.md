// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

# VFS shared-I/O contract (FS-11 scaffold)

## Status and scope

`VFS_SHARED_IO_ENABLED` is the umbrella name for a future userspace VFS/filesystem transfer path.
FS-11 defines service-ABI records, validation helpers, ownership rules, and cleanup requirements only.
It does **not** define or enable a runtime feature switch, advertise a capability, dispatch the new
opcodes, transfer/map a MemoryObject, or change any live filesystem behavior.

The umbrella is split into independently staged capabilities:

| Capability | Data producer | Shared-buffer permission held by FS | Status |
|---|---|---|---|
| `READ_SHARED_REPLY` | filesystem server | write access to the requested range | ABI/helper scaffold only |
| `WRITE_SHARED_REQUEST` | requester/VFS | read-only for the requested range | ABI/helper scaffold only |

The stages must remain separate. A read reply requires the filesystem server to write caller-owned
memory, while a write request requires the filesystem server to consume caller-owned memory without
receiving permission to modify it.

## Current-contract audit

The existing live path is classified as follows:

- **A — inline read path:** `VFS_OP_READ` carries the frozen 32-byte `ReadWriteArgs`. Filesystem
  services may return an eight-byte read length or an extended inline reply containing
  `bytes_read`, status, and bytes within the IPC payload limit.
- **B — inline/write-length path:** `VFS_OP_WRITE` carries only `fd`, an untrusted caller virtual
  address, and length. The live filesystem backend interface receives only `fd` and `len`; it does
  not receive write payload bytes. Existing `WriteLen` replies remain unchanged.
- **C — missing `READ_SHARED_REPLY`:** no live shared object transfer/mapping contract currently
  lets an FS server write a read result into requester-owned memory.
- **D — missing `WRITE_SHARED_REQUEST`:** no live shared object transfer/mapping contract currently
  lets an FS server read requester-owned write bytes with read-only permission.
- **E — block write path:** filesystem-facing block/blkcache/virtio sector writes remain deferred to
  FS-12 and are not specified here.
- **F — runtime wiring:** VFS routing, capability negotiation, mapping, cancellation, and service
  activation remain deferred.

The existing `VFS_OP_READ` (`12`) and `VFS_OP_WRITE` (`13`) numbers and their live behavior are
unchanged. Service opcodes `26` and `27` are reserved for the helper-only shared protocols, but the
current `VfsService` intentionally returns `Unsupported` for them.

## ABI scaffold

`yarm-ipc-abi::vfs_abi` defines:

- capability bits `VFS_SHARED_IO_CAP_READ_SHARED_REPLY` and
  `VFS_SHARED_IO_CAP_WRITE_SHARED_REQUEST`;
- request flags for current-offset semantics and permitted inline fallback;
- exact FS-side access values `VFS_SHARED_BUFFER_FS_WRITE` and
  `VFS_SHARED_BUFFER_FS_READ`;
- `VfsSharedBufferDescriptor`;
- `VfsReadSharedRequest` / `VfsReadSharedReply`;
- `VfsWriteSharedRequest` / `VfsWriteSharedReply`;
- shared-I/O status constants and codec validation errors;
- helper-only message constructors in filesystem common code.

The fixed 80-byte request contains `fd`, file offset, requested length, request ID, flags, and a
40-byte buffer descriptor. The fixed 24-byte reply contains request ID, bytes completed, status, and
reserved flags. All multi-byte fields are little-endian. Payload lengths are exact; reserved bytes
must be zero; unknown flags are rejected; `buffer_offset + buffer_len` is checked for overflow; and
`requested_len` must fit in `buffer_len`.

`object_handle` and `object_generation` are placeholders, not kernel capability slots. They allow
codec and stale-generation tests without inventing a kernel mechanism. A future live layer must
replace/resolve them using an actual userspace-visible transfer primitive and then validate object
type, rights, generation, and object bounds before mapping. The ABI scaffold alone is never proof
that a mapping exists.

### Offset and fallback rules

- With `VFS_SHARED_IO_F_CURRENT_OFFSET`, `file_offset` must be zero and the open file description's
  current offset is used and advanced by `bytes_completed` according to ordinary read/write rules.
- Without that flag, `file_offset` is an absolute offset and does not alter the shared open-file
  offset unless a later live protocol explicitly says otherwise.
- `VFS_SHARED_IO_F_ALLOW_INLINE_FALLBACK` permits, but does not require, retry through the existing
  inline opcode. Fallback must be a new attempt with its own cleanup state; it may not reuse a
  mapping after cleanup.

## Ownership and permissions

| Stage | Object owner | FS permission | FS obligation | Retention after reply |
|---|---|---|---|---|
| `READ_SHARED_REPLY` | requester/VFS | write requested range only | write at most `requested_len`; report actual bytes | forbidden |
| `WRITE_SHARED_REQUEST` | requester/VFS | read requested range only | consume/copy at most `requested_len`; never mutate buffer | forbidden |

The requester remains the logical owner in both stages. Mapping authority is temporary and scoped to
one request ID/generation. The filesystem server must not cache the object handle, mapping, or raw
pointer after completion. In particular, `WRITE_SHARED_REQUEST` must never grant the FS server write
permission merely because both stages use the `VFS_SHARED_IO_ENABLED` umbrella.

## Exactly-once cleanup and failure matrix

The live implementation must give each accepted request one cleanup token. Whichever terminal path
wins atomically consumes that token, unmaps/releases once, and makes all later cleanup attempts
no-ops. Sending a reply is not itself cleanup; cleanup must complete before the server exposes a
terminal reply.

| Failure/event | Required result | Cleanup owner |
|---|---|---|
| bad handle/cap or wrong object type | invalid-descriptor/permission error; backend not called | receiver that accepted transfer |
| stale generation | invalid-descriptor error | receiver |
| offset/length outside object or arithmetic overflow | invalid-descriptor error | receiver |
| backend error before progress | backend status, zero bytes | server |
| backend error after progress | backend/partial status with exact completed count | server |
| requester exits | cancel request; revoke/unmap once | VFS/runtime transfer owner |
| server exits | runtime revokes temporary mapping and wakes requester with backend failure | VFS/runtime transfer owner |
| timeout/cancel races with reply | one terminal state wins; loser observes cleanup already consumed | shared request state machine |
| duplicate reply or double cleanup | reject/ignore duplicate; never unmap or release twice | shared request state machine |
| unsupported backend/capability | unsupported status; optional inline fallback only when requested | VFS/requester |
| inline fallback | complete shared cleanup first, then issue an independent inline operation | VFS/requester |

A partial completion reports the exact number of initialized/read or durably consumed bytes. The
server must never expose uninitialized read-buffer bytes and must never report more than the
requested length.

## Staged implementation plan

1. **FS-11 (this pass):** frozen codecs, permissions, cleanup contract, and message constructors;
   capability bits remain unadvertised and dispatch remains unsupported.
2. **FS-12:** userspace block/blkcache/virtio sector-write contract below filesystems; no VFS shared
   mapping is implied by that work.
3. **Read stage:** implement transfer/mapping and exactly-once cleanup for `READ_SHARED_REPLY`, retain
   inline fallback, then advertise only the read capability after focused lifecycle tests.
4. **Write stage:** independently implement read-only request-buffer mapping for
   `WRITE_SHARED_REQUEST`; only then connect writable filesystems to the FS-12 block path.
5. **Umbrella enablement:** consider `VFS_SHARED_IO_ENABLED` true only when the selected sub-capability
   has routing, permissions, cancellation, cleanup, and process-exit tests. Supporting one stage does
   not imply support for the other.

## Explicit non-changes

FS-11 does not change kernel syscall ABI or `SYSCALL_COUNT`, IPC internals, VM/capability internals,
init/PM/supervisor/driver-manager policy, runtime service spawn order, FAT production writes, ext4
writes, block writes, or the ext4 FS-10 read-side matrix. Kernel/global-lock work is untouched. No
QEMU smoke is required because no runtime behavior is enabled.
