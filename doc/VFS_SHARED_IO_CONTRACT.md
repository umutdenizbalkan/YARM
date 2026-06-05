// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

# VFS shared-I/O contract (FS-11 through FS-15 scaffold)

## Status and scope

`VFS_SHARED_IO_ENABLED` is the umbrella name for a future userspace VFS/filesystem transfer path.
FS-11 through FS-15 define service-ABI records, exact inline write payloads, typed read/write plans,
a borrowed test-only shared-buffer model, and a helper-only lifecycle/cleanup state machine. These are
design and test scaffolding only. They do **not** define or enable a runtime feature
switch, advertise a capability, transfer/map a MemoryObject, or change live filesystem dispatch.

The umbrella is split into independently staged capabilities:

| Capability | Data producer | Shared-buffer permission held by FS | Status |
|---|---|---|---|
| `READ_SHARED_REPLY` | filesystem server | write access to the requested range | helper plan and RAMFS proof only |
| `WRITE_SHARED_REQUEST` | requester/VFS | read-only for the requested range | helper plan and RAMFS proof only |

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
- **C — helper read plan:** FS-14 adds decoded `VfsReadSharedPlan`, checked completion replies, and a
  RAMFS test proof, but no live shared object transfer/mapping contract currently lets an FS server
  write a read result into requester-owned memory.
- **D — helper write payload path:** FS-13 adds a bounded exact-byte inline representation and a
  typed shared-write plan, but neither is accepted by live `VfsService` dispatch.
- **E — missing shared mapping:** no live transfer/mapping contract lets an FS server read the
  requester-owned shared write buffer yet.
- **F — lower block write path:** FS-12 supplies chunked `BLK_OP_WRITE` and blkcache write-through
  below filesystems, but no VFS or FAT code invokes it.
- **G — runtime wiring:** VFS routing, capability negotiation, mapping, cancellation, and service
  activation remain deferred.

The existing `VFS_OP_READ` (`12`) and `VFS_OP_WRITE` (`13`) numbers and their live behavior are
unchanged. Service opcodes `26` and `27` remain reserved for helper-only shared protocols. Opcode
`28` is reserved for the FS-13 bounded inline write helper. FS-14 does not add opcodes. The current `VfsService` intentionally
returns `Unsupported` for all three.

## FS-13 exact write-payload representation

`VfsWritePayload` is the filesystem-common typed plan:

- `Inline(VfsWriteInlineRequest)`: carries actual bytes and may expose them to a helper/test backend;
- `Shared(VfsWriteSharedRequest)`: carries only the validated descriptor and returns `Unsupported`
  when code asks for bytes, because no mapping exists.

The helper-only inline service opcode is `VFS_OP_WRITE_INLINE = 28`. Its variable-length payload has
a 32-byte header (`fd`, file offset, request ID, flags, byte length) and at most 96 exact data bytes,
which keeps the complete message within the existing 128-byte IPC payload. Empty writes, zero request
IDs, unknown flags, inconsistent encoded lengths, and payloads above 96 bytes are rejected. The fixed
24-byte inline reply reports request ID, bytes completed, status, and reserved flags.

This is intentionally not a live conversion of `VFS_OP_WRITE = 13`: legacy requests still decode to
`ReadWriteArgs` and call `VfsBackend::write(fd, len)`. RAMFS unit tests decode the helper plan and feed
its exact bytes directly to `RamFsBackend::write_bytes`, proving read-after-write without modifying
the global service dispatcher. Oversized inline data can be represented by a shared descriptor, but
attempting to obtain bytes from that plan remains `Unsupported`.

`WRITE_SHARED_REQUEST` validation now requires a nonzero opaque handle, nonzero generation,
nonzero requested length, checked `buffer_offset + buffer_len`, sufficient descriptor length, known
flags, and exactly `VFS_SHARED_BUFFER_FS_READ`. Supplying FS write access is rejected. The generation
is still only a correlation value; it is not a capability slot and cannot prove object freshness
until a real transfer layer validates it.

## FS-14 helper-only shared-buffer proof

`VfsReadSharedPlan` decodes only opcode `26`, retains the validated `VfsReadSharedRequest`, and builds
an OK `VfsReadSharedReply` only when `bytes_read <= requested_len`. The filesystem-common
`read_shared_reply_message` helper encodes the completion without adding live dispatch.

`VfsSharedIoTestBuffer` is compiled only for tests and borrows a byte slice. It models enough of a
future mapping to check the opaque handle and generation, descriptor access direction, checked
object range, and the actual backing-object bounds:

- `write_read_reply` accepts only `VFS_SHARED_BUFFER_FS_WRITE` and copies at most the requested range;
- `read_write_request` accepts only `VFS_SHARED_BUFFER_FS_READ` and returns an immutable slice;
- stale identity, wrong direction, overflow, or a descriptor extending beyond the borrowed object is
  rejected before RAMFS is called.

This test double is not a capability, mapping, ownership-transfer, revocation, cancellation, or
exactly-once cleanup implementation. It must not be reused as live mapping code.

RAMFS tests prove exact shared reads, short EOF reads without exposing untouched tail bytes, actual
object-bound rejection, and exact shared writes from an immutable caller view. The earlier bounded
inline-write proof remains in place. These tests call `RamFsBackend::read_bytes`/`write_bytes`
directly after helper decoding; opcodes `26`, `27`, and `28` remain unsupported by live
`VfsService`.

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

## FS-15 helper-only lifecycle state machine

`VfsSharedIoLifecycle` models one future shared request. It is not connected to `VfsService`, process
lifecycle notifications, a capability table, or a mapper. The state transitions are:

| State | Meaning | Permitted next step |
|---|---|---|
| `Reserved` | wire descriptor passed format/access validation | validate live handle generation and map |
| `MappedForReadReply` | helper granted temporary FS-write direction | begin request |
| `MappedForWriteRequest` | helper granted temporary FS-read-only direction | begin request |
| `InFlight` | backend may access only the validated direction/range | complete or terminal cleanup |
| `Completed` | byte count is recorded but cleanup token is still live | success cleanup or competing exit cleanup |
| `Cleaned` | first terminal path released the handle and consumed cleanup | optional independent inline fallback |

Cancellation, timeout, requester exit, and server exit are terminal reasons rather than extra states:
they all converge on `Cleaned`. The terminal-reason vocabulary is `Success`, `BackendError`,
`Unsupported`, `Cancelled`, `Timeout`, `RequesterExit`, `ServerExit`, `StaleHandle`, `BadDescriptor`,
`DuplicateReply`, and `FallbackInline`. Validation errors that occur before accepting a mapping are
returned without pretending that a live mapping existed.

### Handle and generation model

`VfsSharedIoHandleTable<N>` is a fixed-capacity userspace helper table. Handles are one-based opaque
slot numbers. Allocation activates a slot; exactly-once cleanup releases it and increments a nonzero
generation. A descriptor is accepted at the mapping transition only when both handle and generation
match an active slot. Old descriptors therefore become stale immediately after cleanup, and reuse of
the same handle requires the new generation. Zero handle/generation, wrong access direction, an
out-of-range handle, or an inactive generation is rejected.

This table models correlation and invalidation only. It is not a kernel cap table and does not grant
mapping authority.

### Exactly-once cleanup token

Each `VfsSharedIoLifecycle` contains one logical cleanup token. `cleanup(reason)` behaves as follows:

1. The first terminal path validates and releases the active handle, records the reason, transitions
   to `Cleaned`, and returns `Won(reason)`.
2. Later cleanup attempts do not release again or change the winner; they return
   `AlreadyCleaned(original_reason)`.
3. Access authorization after `Cleaned` returns `AccessAfterCleanup`; a completion after `Completed`
   or `Cleaned` returns `DuplicateReply`.
4. `Success` cleanup is legal only after `Completed`. Error, cancel, timeout, and exit cleanup may win
   before completion.
5. Inline fallback is a separate one-shot attempt. It requires the fallback request flag and completed
   cleanup first. It is rejected before cleanup and after success, cancellation, or either endpoint
   exit. Timeout, backend-error, unsupported, and descriptor-failure winners may permit it.

### Read and write lifecycle

For `READ_SHARED_REPLY`, descriptor validation requires FS-write access. The backend may write only
while `InFlight`, may report a short EOF completion, and must then clean up before a reply/fallback
path can cease using the shared object. Cleanup invalidates the generation and all later FS access.

For `WRITE_SHARED_REQUEST`, descriptor validation requires exactly FS-read-only access. The backend
may consume an immutable range while `InFlight` and may complete fewer bytes than requested for a
partial write. FS-write direction is rejected. Success/error cleanup invalidates access in the same
way as the read lifecycle.

### Race model

| Race | Helper result |
|---|---|
| timeout then late completion | timeout wins cleanup; completion is `DuplicateReply` |
| cancel then late completion | cancellation wins cleanup; completion is `DuplicateReply` |
| duplicate cancel/cleanup | original reason is returned; no second release |
| requester exit before backend access | requester-exit cleanup releases and invalidates the handle |
| requester exit after completion but before reply consumption | requester exit may win the still-live cleanup token |
| server exit before completion | server-exit cleanup releases and invalidates the handle |
| timeout then fallback | timeout cleanup must complete first; fallback is then one-shot |
| reuse after exit/cleanup | same handle may be allocated only with its incremented generation |

These are deterministic helper tests, not hooks into actual timeout clocks or process-exit delivery.
A live implementation must preserve the same first-winner semantics under concurrency.

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
3. **FS-13:** bounded inline write bytes, typed inline/shared plans, stricter shared descriptor
   validation, and RAMFS inline helper proof; live dispatch and mapping remain disabled.
4. **FS-14:** typed shared-read plan, checked read completion, borrowed test buffer, and RAMFS shared
   read/write proofs; still no live mapping, routing, cancellation, or cleanup state machine.
5. **FS-15:** helper lifecycle states, generation invalidation, first-winner cleanup, fallback gating,
   and deterministic cancel/timeout/exit race tests; no live mapper or process hooks.
6. **FS-16 gated experiment:** add the first RAMFS-only live experiment behind a disabled-by-default
   local service capability after defining the real transfer/mapping adapter and concurrency model.
7. **Read enablement:** implement transfer/mapping for `READ_SHARED_REPLY`, retain inline fallback,
   and advertise only the read capability after lifecycle tests pass.
8. **Write enablement:** independently implement read-only request-buffer mapping for
   `WRITE_SHARED_REQUEST`; only then connect writable filesystems to the FS-12 block path.
9. **Umbrella enablement:** consider `VFS_SHARED_IO_ENABLED` true only when the selected sub-capability
   has routing, permissions, cancellation, cleanup, and process-exit tests. Supporting one stage does
   not imply support for the other.

## Requirements before production enablement

Neither capability may be advertised until a real userspace transfer primitive supplies object type,
rights, size, generation, mapping, revocation, cancellation, and process-exit notifications. The live
implementation must prove concurrent first-winner cleanup, stale-handle rejection, no access after
cleanup, partial completion accounting, and fallback only after unmap/release. RAMFS must be the first
gated backend; FAT production writes and ext4 writes remain out of scope until that experiment passes.

## Explicit non-changes

FS-15 does not change kernel syscall ABI or `SYSCALL_COUNT`, IPC internals, VM/capability internals,
init/PM/supervisor/driver-manager policy, runtime service spawn order, FAT production writes, ext4
writes, the FS-12 block stack, or the ext4 FS-10 read-side matrix. Kernel/global-lock work is
untouched. No QEMU smoke is required because no runtime behavior is enabled.
