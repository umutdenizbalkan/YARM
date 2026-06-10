// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

# VFS shared-I/O contract (FS-11 through FS-19 scaffold)

## Status and scope

`VFS_SHARED_IO_ENABLED` is the umbrella name for a future userspace VFS/filesystem transfer path.
FS-11 through FS-19 define service-ABI records, exact inline write payloads, typed read/write plans,
a borrowed test-only shared-buffer model, a helper-only lifecycle/cleanup state machine, and the
direction-safe adapter boundary required by a future real mapper. These are
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

## FS-16 userspace mapping adapter boundary

`VfsSharedIoMapper` is the boundary between a validated `VfsSharedIoLifecycle` and any future
userspace object mapper. It has two deliberately asymmetric operations:

- `with_read_reply_buffer` exposes a temporary mutable slice only for `READ_SHARED_REPLY`;
- `with_write_request_buffer` exposes only an immutable slice for `WRITE_SHARED_REQUEST`.

The access wrappers first require the lifecycle to be `InFlight`, validate the active opaque
handle/generation through `VfsSharedIoHandleTable`, and enforce the matching direction. Only then can
the mapper resolve the descriptor range. `cleanup_shared_io` calls the adapter's `release` boundary
before consuming the FS-15 cleanup token; repeated cleanup observes `Cleaned` and does not release the
adapter twice. After cleanup, lifecycle authorization fails before the adapter is invoked.

`UnsupportedSharedIoMapper` is the production-safe default boundary: both operations return
`UnsupportedMapping`. It prevents an opaque descriptor handle from being mistaken for a kernel cap
slot while allowing future service code to depend on a stable interface.

### Mapping audit and decision

The current userspace runtime can receive transferred-cap metadata and has a specialized API to
create a read-only initramfs file-slice MemoryObject for process spawning. It does **not** expose a
general operation that maps an arbitrary transferred MemoryObject into an FS server, requests
FS-write permission for a read reply, queries object size/rights/generation, or revokes/unmaps that
mapping. The opaque shared-I/O handle therefore cannot currently be associated safely with real
bytes.

Classification:

- **A — real mapping adapter today:** no;
- **B — test adapter:** yes;
- **C — external dependency:** a userspace-visible transfer/map/unmap/revoke primitive with object
  type, rights, size, and generation validation is required;
- **D — unsafe shortcut:** interpreting `object_handle` as a capability slot or caller virtual address
  is explicitly forbidden.

Because that dependency is absent, FS-16 does not add the proposed RAMFS live route or a local live
feature flag. Default and test-configured `VfsService` routing remain unchanged and opcodes `26`,
`27`, and `28` remain `Unsupported`.

### Test adapter and RAMFS proofs

`BorrowedSharedIoTestMapper` is compiled only for tests. It checks handle/generation identity,
direction, descriptor arithmetic, and actual borrowed-object bounds. Adapter-level tests prove:

- RAMFS data can be read into a mutable read-reply range before cleanup;
- immutable write-request bytes can be consumed by RAMFS and read back exactly;
- wrong lifecycle direction, stale adapter generation, and an out-of-bounds range are rejected;
- cleanup calls the adapter release boundary exactly once and revokes subsequent access;
- duplicate cleanup remains idempotent without a second adapter release;
- the unsupported production mapper is explicit; and
- timeout cleanup remains idempotent and permits one flagged inline fallback.

This borrowed adapter is not a production mapping implementation and is never installed in live
`VfsService`.

## FS-17 transfer/map/unmap/revoke audit

FS-17 audited `yarm-user-rt`, `yarm-srv-common`, the userspace IPC ABI, existing filesystem services,
and the frozen kernel syscall surface without changing kernel code. A complete production
`VfsSharedIoMapper` cannot be implemented safely from the userspace APIs currently exposed.

### Feasibility classification

- **A — real generic userspace adapter possible now:** no.
- **B — specialized MemoryObject path:** yes. `initramfs_srv` can create a read-only initramfs
  file-slice MemoryObject and transfer it for the specialized spawn/file-grant flow. That path neither
  creates requester-owned writable shared buffers nor maps arbitrary objects into an FS server.
- **C — cap-transfer metadata without a usable generic mapper:** yes. Receive metadata can report a
  materialized transferred cap, and `SharedMemoryRegion` encodes only offset/length. Neither proves
  the object type, rights, size, mapping address, or VFS generation.
- **D — missing userspace wrappers/protocol:** yes. The frozen kernel surface contains receive-time
  shared-region map intent and transfer-release behavior, but `yarm-user-rt` hardcodes its default
  receive intent, does not return mapped base/length, and exposes no transfer-release wrapper.
- **E — potentially missing kernel/capability/VM contract:** yes for the full required profile. There
  is no userspace object-info query that proves MemoryObject/shared-buffer type, rights, and size, and
  no general userspace helper for creating and retaining a transferable requester-owned writable
  object suitable for read replies. These gaps require a separately scoped ABI design unless an
  existing privileged service can provide an equivalent contract.
- **F — unsafe/unclear:** therefore the real adapter remains deferred.

The existing shared-memory receive path is not wire-compatible with simply dispatching reserved VFS
opcode `26` or `27`: shared-region delivery uses its own transport opcode and maps a transferred
region at receive time. A userspace framing convention must bind the mapped region to the VFS
operation, request ID, direction, and local handle generation without treating a local cap number or
virtual address as the ABI's opaque handle.

### Required userspace shared-object adapter ABI

A future implementation should expose the following deliberately typed operations.

#### `yarm-user-rt`

1. A receive API taking an explicit mapping intent: read-only for `WRITE_SHARED_REQUEST`, or
   read/write for `READ_SHARED_REPLY`.
2. A receive result containing the application framing metadata, materialized local transfer cap,
   mapped base, mapped length, and the exact transferred region length.
3. A transfer-release wrapper over the already-frozen release syscall, with stable distinction
   between stale cap, bad mapping, and release failure.
4. A safe borrowed mapping guard whose drop/explicit close path cannot silently double-release and
   which never exposes mutable bytes for a read-only intent.
5. Deadline/cancellation variants with the same rollback guarantees as ordinary receive.

Adding these wrappers does not itself require changing syscall numbers, but it must preserve the
existing register ABI exactly and requires focused userspace tests.

#### Userspace service/IPC contract

1. A framing format that preserves VFS opcode, request ID, direction, descriptor offset/length, and
   generation while the bulk bytes are delivered through the shared-region transport.
2. A service-local registry mapping opaque `(handle, generation)` to the received local cap, mapped
   base/length, rights/direction, owner/request identity, and cleanup state.
3. An object-validation source. If userspace cannot query object kind/rights/size, a dedicated trusted
   shared-buffer service or a new separately reviewed object-info ABI is required.
4. An allocation/ownership path for requester-owned writable buffers used by `READ_SHARED_REPLY`;
   the specialized read-only initramfs file-slice constructor is insufficient.
5. Revocation and process-exit delivery routed to the owning filesystem request, not merely a generic
   supervisor notification.

#### `VfsSharedIoMapper`

The concrete mapper must resolve only through that registry, verify object kind, generation, rights,
size, descriptor range, request identity, and direction, and then expose a scoped borrow. `release`
must unmap/revoke exactly once and invalidate the registry entry even when timeout, cancellation, or
endpoint exit wins. Errors map to `UnsupportedMapping`, `StaleHandle`, `WrongObject`, `MissingRights`,
`BadRange`, `WrongDirection`, `MapFailure`, `ReleaseFailure`, or `AccessAfterCleanup`.

### FS-17 implementation decision

No real mapper or userspace syscall wrapper is added in FS-17 because the full object creation,
introspection, framing, and lifecycle binding contract is incomplete. `UnsupportedSharedIoMapper`
remains the production default. Its regression test verifies that mutable read-reply access,
immutable write-request access, and release all return `UnsupportedMapping`. Live `VfsService`
continues to reject opcodes `26`, `27`, and `28`.

## FS-18 frozen receive/release wrapper audit

FS-18 adds only a typed `yarm-user-rt` wrapper layer around the existing register ABI. It does not
change syscall numbers, register meanings, kernel code, or live VFS routing.

### ABI classification

- **A — receive-map-intent ABI:** supported through the legacy `IpcRecv` layout. Argument 4 accepts
  historical default read/write (`0`), explicit read-only (`READ=0x1`), or explicit read/write
  (`READ|WRITE=0x3`).
- **B — transfer-release ABI:** supported. Syscall `4` takes local transfer cap, mapped base, and
  mapped length; zero base and length select the kernel's active-mapping-record fast path.
- **C — complete mapped receive metadata:** no. The caller knows the requested mapping base, result
  lane 1 returns the page-rounded mapped length, and result lane 2 returns the receiver-local cap.
- **D — partial metadata only:** yes. The portable register result does not return the exact unrounded
  transferred region length or an independently verifiable delivered opcode.
- **E — direction-safe borrowed slice guard:** not yet safe. Object type, rights, exact region length,
  and protocol identity remain unverified.
- **F — metadata-only release guard:** supported. `TransferReleaseToken` is non-copyable, performs no
  syscall in `Drop`, retries after a failed release, and rejects a second successful release.
- **G — missing generic mapper ABI:** object introspection, generic writable shared-buffer creation,
  framing, generation binding, cancellation, and process-exit integration remain absent.

### Wrappers added

`yarm-user-rt::syscall::shared_transfer` exposes:

- `IpcRecvMapIntent::{DefaultReadWrite, ReadOnly, ReadWrite}` with the frozen bit values;
- unsafe `ipc_recv_transfer_with_map_intent`, which uses the legacy receive registers and requires the
  caller's endpoint protocol to guarantee the next message is `OPCODE_SHARED_MEM`;
- `MappedTransferRecv`, containing sender TID, receiver-local transfer cap, caller-provided mapped
  base, page-rounded mapped length, and requested direction;
- `TransferReleaseRequest::{explicit, active}` and unsafe `transfer_release`; and
- a metadata-only `TransferReleaseToken` for explicit, retryable, at-most-once release through the
  wrapper. It deliberately exposes no byte slice.

The mapped receive wrapper is unsafe because the legacy result does not identify the delivered
opcode. A plain cap-transfer message could otherwise be misclassified as an auto-mapped shared
transfer. The wrapper also requires a page-aligned, reserved target range and a capacity large enough
for the sender's region. Kernel validation remains authoritative.

### recv-v2 limitation

The current frozen syscall layout cannot combine recv-v2 metadata with explicit map intent: recv-v2
uses argument 4 for metadata-buffer length, while shared-memory receive interprets argument 4 as map
intent. FS-18 therefore leaves `ipc_recv`, `ipc_recv_v2`, and timed receive behavior unchanged and
uses the legacy receive layout only in the new protocol-constrained wrapper. No mapped-base or exact
region-length fields are invented.

These wrappers are necessary but insufficient for `VfsSharedIoMapper`: they do not establish object
kind, rights, object size, VFS handle generation, request identity, or a generic requester-owned
writable buffer. `UnsupportedSharedIoMapper` remains the production default, RAMFS live shared I/O
remains disabled, FAT production writes remain unwired, and ext4 remains read-only.

## FS-19 mapper ABI design decision

The detailed missing-metadata table, receive option analysis, validation ownership, generic writable
buffer design, VFS/shared-region frame, and cancellation/exit/revocation matrix are frozen in
[`VFS_SHARED_IO_MAPPER_REQUIREMENTS.md`](VFS_SHARED_IO_MAPPER_REQUIREMENTS.md).

FS-19 recommends a separately reviewed, versioned mapped-receive ABI (recv-v3 or equivalent) that
returns authoritative object kind, effective rights, exact object/region sizes, object generation,
actual mapping permissions, and an unforgeable cleanup identity without changing legacy recv-v2. A
userspace broker may provide allocation and policy, but cannot be the security authority without
kernel-backed object metadata. No syscall number or wire layout is selected and no live support is
enabled in this pass.

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
6. **FS-16:** define the direction-safe mapper boundary and borrowed test adapter; defer the
   RAMFS-only live experiment because no general userspace mapping primitive exists.
7. **FS-17:** audit the dormant transfer-map/release surface and document the missing userspace
   wrappers, framing, registry, object introspection, and writable-object creation contract; keep the
   production mapper unsupported.
8. **FS-18:** add typed wrappers for the frozen legacy receive-map-intent and transfer-release
   register layouts, plus a metadata-only at-most-once successful release token; keep live routing disabled.
9. **FS-19:** freeze the missing metadata, validation, writable-buffer, framing, and lifecycle
   requirements; recommend a separate versioned mapped-receive kernel ABI design task.
10. **FS-20:** stop live shared-I/O work until that separately scoped ABI is reviewed; userspace-only
   work may prototype non-authoritative broker policy but must not enable a mapper.
11. **Read enablement:** implement transfer/mapping for `READ_SHARED_REPLY`, retain inline fallback,
   and advertise only the read capability after lifecycle tests pass.
12. **Write enablement:** independently implement read-only request-buffer mapping for
   `WRITE_SHARED_REQUEST`; only then connect writable filesystems to the FS-12 block path.
13. **Umbrella enablement:** consider `VFS_SHARED_IO_ENABLED` true only when the selected sub-capability
   has routing, permissions, cancellation, cleanup, and process-exit tests. Supporting one stage does
   not imply support for the other.

## Requirements before production enablement

Neither capability may be advertised until a real implementation of `VfsSharedIoMapper` is backed
by a userspace transfer primitive that supplies object type,
rights, size, generation, mapping, revocation, cancellation, and process-exit notifications. The live
implementation must prove concurrent first-winner cleanup, stale-handle rejection, no access after
cleanup, partial completion accounting, and fallback only after unmap/release. RAMFS must be the first
gated backend; FAT production writes and ext4 writes remain out of scope until that experiment passes.

## FS-20 bounded FAT inline-write route

FS-20 does not resume live shared-memory work. It adds a separate, bounded FAT-only route for
`VFS_OP_WRITE_INLINE = 28`: one request carries 1–96 exact bytes and uses current-offset semantics.
The FAT IPC block adapter now has a validated whole-sector chunked `BLK_OP_WRITE` client, but the
FAT-only inline route remains gated to memory images because the current inline block-read ABI cannot
return a complete sector for required read-modify-write. The generic `VfsService` still rejects opcode 28, while opcodes
26 (`READ_SHARED_REPLY`) and 27 (`WRITE_SHARED_REQUEST`) remain unsupported everywhere in live
service dispatch. Larger writes still need explicit inline fragmentation or a future, separately
reviewed shared-region ABI.

This route does not make `VFS_SHARED_IO_ENABLED` true, does not advertise shared-I/O capabilities,
and does not change RAMFS or read-only ext4 behavior.

## Explicit non-changes

FS-20 does not change kernel syscall ABI or `SYSCALL_COUNT`, IPC internals, VM/capability internals,
init/PM/supervisor/driver-manager policy, runtime service spawn order, ext4 writes, or the ext4 FS-10
read-side matrix. It does not enable IPC-backed FAT file mutation: only the FAT memory-image inline
route and a standalone whole-sector FS-12 client are added. Kernel/global-lock work is untouched. No
QEMU smoke is required because default production/shared-I/O behavior is unchanged.

---

## Stage 64 readiness audit (June 2026)

### Scope

This audit evaluates whether `recv_shared_v3` (NR 30, Stage 61+62) and the existing
VFS shared-IO scaffold (FS-11 through FS-20) are ready for production use.
No new Rust code is added; this section documents blockers only.

### recv_shared_v3 map_intent=0 (plain receive)

Stage 63 proved the plain-receive kernel dispatch path end-to-end:
- `result_status = RECV_V3_STATUS_OK`
- `transferred_cap` is materialized (DmaRegion cap)
- `mapped_base = 0`, `cleanup_token = 0` (no mapping established)
- No active transfer mapping registered
- `RecvSharedV3Output` ABI struct decodes correctly
- `RecvSharedV3Delivery::from_output()` produces `has_mapping() = false`

### Readiness table

| Capability | Status | Reason |
|---|---|---|
| `recv_shared_v3` plain receive (map_intent=0) | **Ready** | Proved in Stage 63; kernel dispatch + user-rt encode/decode both verified |
| `recv_shared_v3` MAP_READ (map_intent=1) | **Ready** | Proved in Stage 61+62; mapping fields + cleanup_token populated correctly |
| MAP_WRITE (map_intent=3) | **Blocked (intentional)** | Stage 60 RW gate hard-rejects; no plan to enable |
| `VfsSharedIoLifecycle` state machine | **Helper-only** | Fully implemented; not connected to real kernel mapper |
| `VfsSharedIoHandleTable<N>` | **Helper-only** | Correctly models exactly-once generation invalidation; userspace only |
| `VfsSharedIoMapper` trait | **Helper-only** | `UnsupportedSharedIoMapper` for production; `BorrowedSharedIoTestMapper` for tests |
| `WRITE_SHARED_REQUEST` (FS needs MAP_READ of caller buffer) | **Helper-only** | MAP_READ sufficient; but `VfsSharedBufferDescriptor.object_handle` is opaque and has no defined relationship to `recv_shared_v3`'s `cleanup_token` |
| `READ_SHARED_REPLY` (FS needs MAP_WRITE to write into requester buffer) | **Blocked** | FS server would need MAP_WRITE of requester-owned memory; Stage 60 RW gate rejects `map_intent & WRITE != 0`; requires new kernel primitive |
| Object type validation (MemoryObject vs DmaRegion vs other) | **Blocked** | `recv_shared_v3` writes `object_kind` to output but `VfsSharedBufferDescriptor` does not cross-reference it |
| Effective rights validation | **Blocked** | `recv_shared_v3` writes `effective_rights` but no VFS code validates them |
| Exact region length validation | **Blocked** | `exact_region_len` available from `recv_shared_v3` but not plumbed to VFS scaffold |
| Requester-exit cleanup authority | **Blocked** | `VfsSharedIoTerminalReason::RequesterExit` exists in lifecycle; no kernel death notification |
| Server-exit cleanup | **Blocked** | `VfsSharedIoTerminalReason::ServerExit` exists; no kernel signal |
| Timeout/cancel | **Helper-only** | Terminal reason vocabulary complete; no kernel cancellation signal path |
| Duplicate cleanup | **Ready** | `AlreadyCleaned` result; `recv_shared_v3` duplicate-release rejection also proved |
| Inline fallback | **Helper-only** | Gated in lifecycle; no live dispatch path |
| DmaRegion as buffer object type | **Helper-only** | `recv_shared_v3` fully supports DmaRegion; correct object for `WRITE_SHARED_REQUEST`; not wired to VFS scaffold |

### Key blockers before Stage 65+66

1. **`READ_SHARED_REPLY` MAP_WRITE**: requires a new kernel ABI that allows the FS receiver to map
   requester-owned memory with write permission. The Stage 60 RW gate explicitly forbids this until
   such an ABI is designed and reviewed. `WRITE_SHARED_REQUEST` (read-only for FS) does not require
   this and is closer to ready.

2. **`VfsSharedBufferDescriptor` ↔ `cleanup_token` binding**: `object_handle` in the descriptor is
   opaque; it has no defined relationship to `recv_shared_v3`'s `cleanup_token`. A production wiring
   must define how the receiver maps the descriptor handle to an active transfer record so that
   `release_v3_cleanup_token` can be called at the right time.

3. **Object introspection**: `recv_shared_v3` supplies `object_kind`, `effective_rights`, and
   `exact_region_len`. The VFS scaffold does not validate these against the VFS request. A production
   adapter must reject non-DmaRegion / non-MemoryObject caps and verify rights before granting access.

4. **Process-exit and timeout signals**: the lifecycle state machine has terminal reasons for requester
   exit, server exit, and timeout, but no kernel path notifies the FS server when the requester
   terminates or a deadline expires. These are required for safe live operation.

### Why VFS_SHARED_IO_ENABLED remains disabled

Until blockers 1-4 are resolved (new kernel primitive for MAP_WRITE, defined descriptor-to-token
binding, object introspection wiring, and process-exit/timeout signals), enabling
`VFS_SHARED_IO_ENABLED` would expose:
- no authoritative object type or rights check;
- no safe memory ownership transfer for read replies;
- no reliable cleanup on requester exit; and
- potential double-free or stale mapping if the server continues using a buffer after the requester
  has exited.

`WRITE_SHARED_REQUEST` is closer to production-ready than `READ_SHARED_REPLY` because it requires
only MAP_READ (already working), but the descriptor-to-token binding gap still applies.

---

## Stage 65 — WRITE_SHARED_REQUEST binding to recv_shared_v3 MAP_READ

### Scope

Stage 65 defines the **binding contract** between a `recv_shared_v3` MAP_READ delivery and a VFS
`WRITE_SHARED_REQUEST` descriptor, and implements a **helper-only** `VfsWriteSharedBinding` type
that validates the cross-reference. No live VFS dispatch is changed. `VFS_SHARED_IO_ENABLED` remains
disabled. `READ_SHARED_REPLY` remains blocked (requires MAP_WRITE). MAP_WRITE is not enabled.

### Binding contract

When a VFS `WRITE_SHARED_REQUEST` arrives, the requester must have previously called
`recv_shared_v3` with `map_intent=1 (MAP_READ)`. The kernel populates `cleanup_token` as a full
`u64` CapId value (`(generation << 16) | slot_index`).

The binding contract is:

```
descriptor.object_handle     = cleanup_token          (full u64 CapId)
descriptor.object_generation = cleanup_token >> 16    (generation field only)
```

`VfsWriteSharedBinding::validate()` enforces this by recomputing `expected_gen = cleanup_token >> 16`
and comparing it to `descriptor.object_generation`. This provides a two-field cross-reference so
that a stale or wrong-generation descriptor fails independently of a handle collision.

### Constraints enforced by `VfsWriteSharedBinding::validate()`

| Constraint | Error variant |
|---|---|
| `cleanup_token != 0` (token present) | `MissingCleanupToken` |
| `transferred_cap != 0` (cap materialised) | `NoTransferCap` |
| `actual_mapping_perm == MAP_PERM_READ_ONLY (1)` | `MappingNotReadOnly` |
| `mapped_base != 0` (mapping established) | `MappingNotEstablished` |
| `object_kind == OBJECT_KIND_DMA_REGION` | `UnsupportedObjectKind` |
| `descriptor.access_flags & VFS_SHARED_BUFFER_FS_READ != 0` | `WrongDescriptorAccess` |
| `descriptor.object_handle == cleanup_token` | `DescriptorHandleMismatch` |
| `descriptor.object_generation == cleanup_token >> 16` | `DescriptorGenerationMismatch` |
| `page_rounded_mapped_len >= exact_region_len` | `MappingRangeTooShort` |
| `exact_region_len > 0` | `ExactRegionLenInsufficient` |
| `request_id != 0` | `ZeroRequestId` |

All 11 checks must pass. The first failing check returns immediately.

### What remains blocked

- `READ_SHARED_REPLY` is still blocked (requires MAP_WRITE, which the Stage 60 RW gate forbids).
- MAP_WRITE is not enabled.
- Live VFS dispatch still uses `UnsupportedSharedIoMapper`.
- Process-exit and timeout signals are still unimplemented.

### Test coverage

21 `stage65_*` tests in `crates/yarm-fs-servers/src/fs/common/shared_io_adapter.rs`:

- 11 rejection tests (one per error variant)
- 1 acceptance test (`stage65_valid_write_shared_binding_accepted`)
- 1 RAMFS roundtrip (`stage65_ramfs_consumes_immutable_bytes_via_binding_and_mapper`)
- 1 direction-safety proof (`stage65_mapper_rejects_write_access_to_write_request_buffer`)
- 1 lifecycle idempotency (`stage65_cleanup_idempotent_after_success`)
- 1 fallback-gate (`stage65_cleanup_before_fallback_required_for_write_request`)
- 1 production-mapper rejection (`stage65_production_mapper_rejects_write_shared_request`)
- 1 READ_SHARED_REPLY still-blocked (`stage65_read_shared_reply_still_unsupported_by_production_mapper`)
- 1 VFS_SHARED_IO disabled invariant (`stage65_vfs_shared_io_enabled_remains_disabled`)
- 1 `cleanup_token_parts()` correctness (`stage65_cleanup_token_parts_decompose_correctly`)

Total `yarm-fs-servers` tests after Stage 65: **228** (up from 207).

---

## Stage 66+67+68 — Gated WRITE_SHARED_REQUEST live route in VfsService

### Scope

Stages 66+67+68 move VFS shared I/O from helper-only to a controlled, disabled-by-default live
route for WRITE_SHARED_REQUEST. No global enable occurs. READ_SHARED_REPLY remains blocked.

### Feature flag constants (in `shared_io_adapter.rs`)

| Constant | Value | Meaning |
|---|---|---|
| `VFS_WRITE_SHARED_REQUEST_ENABLED` | `true` (Stage 78) | WRITE_SHARED_REQUEST helper proven; prerequisites met |
| `VFS_READ_SHARED_REPLY_ENABLED` | `true` (Stage 73) | READ_SHARED_REPLY gate — enabled; live notification via PM path |
| `VFS_SHARED_IO_ENABLED` | `true` (Stage 78) | Aggregate umbrella — `WRITE && READ && PM` = `true` |
| `VFS_SUPERVISOR_TASK_EXIT_NOTIFICATION_ENABLED` | `false` (Stage 75) | Supervisor→VFS task-exit channel — PM model replaces |
| `VFS_PM_TASK_EXIT_NOTIFICATION_ENABLED` | `true` (Stage 77+78) | PM→VFS task-exit push notification channel — both blockers resolved |

`VFS_SHARED_IO_ENABLED` is `true` when all three gates are `true`. Stage 78 enables
`VFS_WRITE_SHARED_REQUEST_ENABLED` after auditing all prerequisites. `handle_request` still
rejects shared opcodes — `UnsupportedSharedIoMapper` is the production default. Gate `true`
means helper-level prerequisites are proven, not that live production routing is active.

### Live route: `VfsService::dispatch_write_shared_request`

A new method `dispatch_write_shared_request<M: VfsSharedIoMapper>` is added to `VfsService<B>`.
It is independent of `handle_request` — `handle_request` still rejects `VFS_OP_WRITE_SHARED_REQUEST`
with `VfsError::Unsupported`.

The method performs:
1. `VfsWriteSharedBinding::validate()` — all 11 Stage 65 constraints enforced.
2. `mapper.with_write_request_buffer(descriptor, len, |bytes| backend.write_shared_bytes(fd, bytes))`.
3. `mapper.release(descriptor)` — cleanup unconditionally after access attempt.
4. Returns `VfsWriteSharedReply { request_id, bytes_completed, status=OK, flags=0 }`.

`backend.write_shared_bytes` is a new default method on `VfsBackend` (default: `Err(Unsupported)`).
`RamFsBackend` overrides it to delegate to `write_bytes`, updating write metrics.

### Production confirmations

- `handle_request` rejects `VFS_OP_WRITE_SHARED_REQUEST` with `Unsupported` (unchanged).
- `handle_request` rejects `VFS_OP_READ_SHARED_REPLY` with `Unsupported` (unchanged).
- FAT/ext4/blkcache production write behavior unchanged.
- No production service loops changed.
- No runtime spawn/policy changes.

### Test coverage

17 `stage66_*` / `stage67_*` / `stage68_*` tests in `vfs_service.rs` (`mod stage66_68_tests`):

- `stage66_default_dispatch_still_rejects_write_shared_opcode` — `handle_request` gate check
- `stage66_gated_dispatch_ramfs_write_shared_succeeds` — RAMFS roundtrip
- `stage66_gated_dispatch_bytes_written_match_file_contents` — file content verified
- `stage66_gated_dispatch_cleanup_performed_exactly_once` — `release_count == 1`
- `stage66_gated_dispatch_op_sequence_advances_on_success` — op_sequence tracking
- `stage66_gated_dispatch_missing_cleanup_token_rejected` — `VfsError::Malformed`
- `stage66_gated_dispatch_stale_generation_rejected` — `VfsError::PermissionDenied`
- `stage66_gated_dispatch_wrong_object_handle_rejected` — `VfsError::PermissionDenied`
- `stage66_gated_dispatch_non_readonly_mapping_rejected` — `VfsError::Malformed`
- `stage66_gated_dispatch_range_too_short_rejected` — `VfsError::Malformed`
- `stage66_gated_dispatch_unsupported_production_mapper_rejected` — `VfsError::Malformed`
- `stage66_gated_dispatch_cleanup_called_even_on_failed_write` — no panic on failure
- `stage67_read_shared_reply_still_unsupported_by_parse_request` — READ blocked
- `stage68_write_shared_request_gate_disabled_by_default` — const assertion
- `stage68_read_shared_reply_gate_disabled_by_default` — const assertion
- `stage68_global_vfs_shared_io_disabled_by_default` — const assertion
- `stage68_global_gate_false_unless_both_direction_gates_true` — aggregate logic

Total `yarm-fs-servers` tests after Stage 66+67+68: **245** (up from 228).

### Remaining blockers before global `VFS_SHARED_IO_ENABLED`

1. **`READ_SHARED_REPLY` MAP_WRITE** — requires new kernel ABI; Stage 60 RW gate blocks.
2. **Process-exit and timeout signals** — `VfsSharedIoTerminalReason::RequesterExit/ServerExit` exist in lifecycle; no kernel signal path.
3. **Object introspection in production** — `write_shared_bytes` must validate `effective_rights` in production before granting access.
4. **Cap revocation** — the current helper uses opaque `object_handle`; a production path must hold and eventually revoke the kernel capability after use.

---

## Stage 69+70 — MAP_WRITE audit + READ_SHARED_REPLY helper/gated path

### Scope

Stage 69 audits whether MAP_WRITE in recv_shared_v3 is safe to enable. Stage 70 implements a
helper-only READ_SHARED_REPLY path (VfsReadSharedBinding + dispatch_read_shared_reply) using the
existing BorrowedSharedIoTestMapper without touching the kernel MAP_WRITE gate.

### MAP_WRITE audit verdict (Stage 69)

| Item | Finding | Safe to enable? |
|---|---|---|
| Stage 60 gate location | `syscall.rs` ~4266: `if req.map_intent & WRITE != 0 { return Err(InvalidArgs) }` | Gate is intact |
| MAP_PERM_READ_WRITE | Defined in `recv_core.rs` as `3`; currently unreachable | No change needed |
| Rollback on writeback failure | Exists: unmap → remove registry → revoke cap | ✓ |
| TransferRelease removes mapping | Yes: two-phase unmap + cap revocation | ✓ |
| Process-exit cleanup | NOT confirmed — no kernel signal path identified | **Blocker** |
| execute bit enforcement | Hardcoded `execute: false` for all shared mappings | ✓ |
| Rights enforcement | `cap_rights & CAP_RIGHT_WRITE` check exists in planning | ✓ |

**Verdict:** Do not remove Stage 60 MAP_WRITE gate. Process-exit cleanup gap means a writable
mapping could outlive its owner process. Implement helper-only path via test mapper.

### READ_SHARED_REPLY binding contract (VfsReadSharedBinding, 12 constraints)

Defined in `crates/yarm-fs-servers/src/fs/common/shared_io_adapter.rs`.

| Constraint | Field checked | Error variant |
|---|---|---|
| 1 | cleanup_token != 0 | `MissingCleanupToken` |
| 2 | transferred_cap != u64::MAX | `NoTransferCap` |
| 3 | actual_mapping_perm & 0x2 != 0 | `MappingNotWritable` |
| 4 | actual_mapping_perm & 0x4 == 0 | `ExecutableMapping` |
| 5 | mapped_base != 0 | `MappingNotEstablished` |
| 6 | object_kind ∈ {1, 5} | `UnsupportedObjectKind` |
| 7 | descriptor.access == VFS_SHARED_BUFFER_FS_WRITE | `WrongDescriptorAccess` |
| 8 | descriptor.object_handle == cleanup_token | `DescriptorHandleMismatch` |
| 9 | descriptor.object_generation == cleanup_token >> 16 | `DescriptorGenerationMismatch` |
| 10 | page_rounded_mapped_len >= buffer_offset + buffer_len | `MappingRangeTooShort` |
| 11 | exact_region_len == 0 or >= buffer_offset + buffer_len | `ExactRegionLenInsufficient` |
| 12 | request_id != 0 | `ZeroRequestId` |

Constraint 3 (`MappingNotWritable`) acts as the kernel gate mirror: the Stage 60 gate prevents
`actual_mapping_perm = 3` from ever arriving via a live recv_shared_v3 delivery, so this
binding is only reachable today via the test mapper.

### Live route: `VfsService::dispatch_read_shared_reply`

New method `dispatch_read_shared_reply<M: VfsSharedIoMapper>` added to `VfsService<B>`.
`handle_request` still rejects `VFS_OP_READ_SHARED_REPLY` with `VfsError::Unsupported`.

The method performs:
1. `VfsReadSharedBinding::validate()` — all 12 constraints enforced.
2. `mapper.with_read_reply_buffer(descriptor, len, |buf| backend.read_shared_bytes(fd, buf))`.
3. `mapper.release(descriptor)` — cleanup unconditionally after access attempt.
4. Returns `VfsReadSharedReply { request_id, bytes_completed, status=OK, flags=0 }`.

`backend.read_shared_bytes` is a new default method on `VfsBackend` (default: `Err(Unsupported)`).
`RamFsBackend` overrides it to delegate to `read_bytes`, updating read metrics.

### Error mapping (dispatch_read_shared_reply)

| Binding error | VfsError |
|---|---|
| `WrongDescriptorAccess` | `PermissionDenied` |
| `DescriptorHandleMismatch` | `PermissionDenied` |
| `DescriptorGenerationMismatch` | `PermissionDenied` |
| All others | `Malformed` |

### Production confirmations

- `handle_request` still rejects `VFS_OP_READ_SHARED_REPLY` with `Unsupported`.
- `handle_request` still rejects `VFS_OP_WRITE_SHARED_REQUEST` with `Unsupported`.
- Stage 60 kernel MAP_WRITE gate removed by Stage 72; `compute_recv_v3_mapping_plan` now
  enforces rights (MAP+READ+WRITE required for RW mapping; InsufficientRights → InvalidArgs).
- `VFS_READ_SHARED_REPLY_ENABLED = true` (enabled Stage 73; prerequisites: Stage 72 MAP_WRITE
  delivery + `deliver_requester_exit` helper model proven with 7 lifecycle tests).
- `VFS_SHARED_IO_ENABLED = false` (unchanged — WRITE direction still disabled).
- FAT/ext4/blkcache production read behavior unchanged.
- No runtime spawn/policy changes.

### Test coverage

16 `stage69_*` / `stage70_*` tests in `vfs_service.rs` (`mod stage69_70_tests`):

- `stage69_audit_map_write_gate_remains_blocking` — proves perm=1 → MappingNotWritable
- `stage69_write_shared_request_still_works_after_read_shared_added` — regression
- `stage69_read_shared_reply_default_dispatch_still_unsupported` — handle_request gate
- `stage69_gate_values_all_false` — all three feature gates are false
- `stage70_read_shared_reply_ramfs_writes_bytes_into_buffer` — RAMFS roundtrip proof
- `stage70_read_shared_reply_short_eof_bytes_read_le_requested` — EOF / short-read case
- `stage70_read_shared_reply_wrong_direction_rejected` — `PermissionDenied` for FS_READ descriptor
- `stage70_read_shared_reply_readonly_mapping_rejected` — `Malformed` for perm=1
- `stage70_read_shared_reply_stale_generation_rejected` — `PermissionDenied` for stale gen
- `stage70_read_shared_reply_range_too_short_rejected` — `Malformed` for range overflow
- `stage70_read_shared_reply_cleanup_called_on_backend_error` — release_count=1 on error
- `stage70_read_shared_reply_unsupported_production_mapper_rejects_safely` — `Malformed`
- `stage70_read_shared_reply_op_sequence_advances_on_success` — op_sequence tracking
- `stage70_read_shared_reply_cleanup_exactly_once` — release called exactly once
- `stage70_global_vfs_shared_io_still_false` — VFS_SHARED_IO_ENABLED == false
- `stage70_write_shared_request_still_unsupported_in_handle_request` — no regression

Total `yarm-fs-servers` tests after Stage 69+70: **261** (up from 245).

Total `yarm-fs-servers` tests after Stage 76: **313** (up from 295; +18 Stage 76 tests).

Total `yarm-fs-servers` tests after Stage 77+78: **325** (up from 313; +12 Stage 77+78 tests).

Total `yarm-fs-servers` tests after Stage 78: **340** (up from 325; +15 Stage 78 tests).

## Stage 78 — Final VFS shared-I/O readiness audit + global enable

### Scope

Stage 78 performs the final gate matrix audit and conditionally enables the global flag.
No new kernel code is added. No live `handle_request` routing is changed.

### Gate matrix audit (all pass)

| Gate | Value | Resolved | Notes |
|---|---|---|---|
| `VFS_WRITE_SHARED_REQUEST_ENABLED` | **`true`** | Stage 78 | MAP_READ + binding + RAMFS + RequesterExit + PM notification |
| `VFS_READ_SHARED_REPLY_ENABLED` | **`true`** | Stage 73 | MAP_WRITE + binding + RAMFS + RequesterExit |
| `VFS_PM_TASK_EXIT_NOTIFICATION_ENABLED` | **`true`** | Stage 77+78 | kernel→PM→VFS death path wired |
| `VFS_SHARED_IO_ENABLED` | **`true`** | Stage 78 | `WRITE && READ && PM = true` |
| `VFS_SUPERVISOR_TASK_EXIT_NOTIFICATION_ENABLED` | `false` | Stage 75 | PM model replaces; unwired |

### Policy decision

`VFS_SHARED_IO_ENABLED = VFS_WRITE_SHARED_REQUEST_ENABLED && VFS_READ_SHARED_REPLY_ENABLED && VFS_PM_TASK_EXIT_NOTIFICATION_ENABLED`

All three are `true` in Stage 78. `VFS_SHARED_IO_ENABLED = true` means both direction helpers
and the PM notification path are proven correct at the helper level. It does NOT mean
`handle_request` routes shared opcodes in production. Both `VFS_OP_WRITE_SHARED_REQUEST` and
`VFS_OP_READ_SHARED_REPLY` remain `VfsError::Unsupported` until a real `VfsSharedIoMapper` is
available (see FS-17/FS-19 for mapper ABI design). `UnsupportedSharedIoMapper` remains the
production default.

### New Stage 78 tests (15 total, mod stage78_tests)

**Gate constants (6):** both direction gates true, PM notification true, global gate true,
conjunction invariant (WRITE && READ && PM), supervisor path still disabled.

**handle_request routing (2):** still rejects WRITE_SHARED_REQUEST and READ_SHARED_REPLY
even with `VFS_SHARED_IO_ENABLED = true`.

**RequesterExit for WRITE direction (3):** `dispatch_pm_task_exited_push` cleans a WRITE
lifecycle; duplicate exit is idempotent (AlreadyCleaned); unmatched TID is safe noop.

**Legacy behavior (2):** VfsService construction unchanged; RAMFS read/write unchanged.

**Production mapper (2):** `UnsupportedSharedIoMapper` still rejects both directions.

### Production confirmations

- `handle_request` still rejects `VFS_OP_WRITE_SHARED_REQUEST` with `Unsupported`.
- `handle_request` still rejects `VFS_OP_READ_SHARED_REPLY` with `Unsupported`.
- FAT/ext4/blkcache production behavior unchanged.
- No runtime spawn/policy changes.
- SYSCALL_COUNT = 31; STARTUP_SLOT_COUNT = 18; both unchanged.

### Remaining work before live production routing

1. **Real `VfsSharedIoMapper` implementation** — requires a separately reviewed userspace
   transfer/map/unmap/revoke primitive with object type, rights, size, generation, and
   process-exit integration (FS-17 through FS-19 requirements).
2. **`VfsService` service loop integration** — wiring `dispatch_write_shared_request` and
   `dispatch_read_shared_reply` into `handle_request` routing once a real mapper exists.
3. **FAT/ext4 server migration** — RAMFS is the proof backend; FAT production writes and
   ext4 writes remain out of scope until the mapper is proven in production.

### Remaining blockers before global `VFS_SHARED_IO_ENABLED` (all resolved at Stage 78)

1. **Process-exit cleanup** — ~~kernel must signal VFS server when a process holding a mapped~~
   ~~receive exits~~ **IDENTITY PROVEN (Stage 75)**:
   - Kernel cleanup path: `mark_task_dead` → `purge_active_transfer_mappings_for_pid` (Stage 71).
   - VFS lifecycle model: `deliver_requester_exit` (Stage 73), `deliver_requester_exit_if_tid_matches`
     (Stage 75); lifecycle proven with 7+10 tests.
   - `VfsSharedIoLifecycle::requester_tid` field (Stage 75): stores the TID so exit events can
     be correlated to active lifecycles by `deliver_requester_exit_if_tid_matches(tid, handles)`.
   - `VFS_SUPERVISOR_TASK_EXIT_NOTIFICATION_ENABLED = false` (Stage 75): two remaining pieces
     before this can be enabled:
     a. **Supervisor→VFS notification cap**: `InitFaultHandoff` needs a `vfs_task_exit_send_cap`
        so the supervisor's `handle_task_exit` can forward `SUPERVISOR_OP_TASK_EXITED(tid)` to VFS.
     b. **VFS-side lifecycle store**: `VfsService` needs a bounded table keyed by `requester_tid`
        so it can look up and call `deliver_requester_exit_if_tid_matches` on notification.
   - **PM-owned model defined (Stage 76)**: architectural decision that PM should own lifecycle
     notifications; supervisor should own fault/restart policy only.
     - `PROC_OP_TASK_EXITED = 13`: new PM→VFS push opcode with [`PmTaskExitedEvent`] 16-byte payload.
     - `PROC_OP_PROCESS_EXITED = 14`: new PM→VFS push opcode with [`PmProcessExitedEvent`] 16-byte payload.
     - `handle_pm_task_exited(tid, lifecycle, handles)`: VFS entry point for PM push events.
     - `VFS_PM_TASK_EXIT_NOTIFICATION_ENABLED = true` (Stage 77+78): both blockers resolved:
       a. **PM→VFS send cap RESOLVED**: PM already has `vfs_send_cap` via
          `lifecycle_table.get_by_image_id(6).pm_service_send_cap` (image_id=6 = VFS).
       b. **Kernel→PM task-exit delivery RESOLVED (Stage 77+78)**:
          `FaultSubsystem::pm_task_exit_endpoint: Option<usize>` added.
          `exit_task()` calls `report_task_exit_to_pm(tid, code)` (new) after
          `report_task_exit_to_supervisor()`.  Kernel sends `KERNEL_OP_PM_TASK_EXITED = 0xDC`
          with 16-byte LE payload (tid + exit_code) to PM's registered endpoint.
          `set_pm_task_exit_endpoint_for_task(tid, recv_cap)` registers PM's recv endpoint.
     - VFS dispatch added: `dispatch_pm_task_exited_push(opcode, payload, lifecycle, handles)` decodes
       `PROC_OP_TASK_EXITED` and calls `handle_pm_task_exited`. PM decode helper:
       `decode_kernel_pm_task_exited(opcode, payload)` for kernel→PM direction.
     - 18 tests in `mod stage76_tests` + 12 tests in `mod stage77_vfs_tests` prove ABI codec,
       handler dispatch, idempotency, pipeline, and gate status.
2. **Live MAP_WRITE delivery** — ~~the Stage 60 gate must be removed~~ **ENABLED (Stage 72)**:
   the Stage 60 blanket WRITE gate has been removed from `syscall.rs`.  `recv_shared_v3` with
   `map_intent=0x3` now maps memory writably when the transferred cap carries `CAP_RIGHT_WRITE`.
   Rights enforcement lives in `compute_recv_v3_mapping_plan`; WRITE-only (0x2) is rejected by
   `validate_v3_request`.  Cleanup/rollback paths are identical to MAP_READ.  9 kernel tests in
   `mod stage72` prove the new behaviour.
3. **READ_SHARED_REPLY gate** — ~~blocked pending MAP_WRITE~~ **ENABLED (Stage 73)**:
   `VFS_READ_SHARED_REPLY_ENABLED = true`.  `dispatch_read_shared_reply` is available for direct
   calls.  `handle_request` still returns `VfsError::Unsupported` for the opcode (by design).
   Production blocker: live `RequesterExit` notification (see blocker 1 above).
4. **Object introspection in production** — `read_shared_bytes` must validate `effective_rights`
   in a production mapper before writing into the caller's buffer.
5. **Cap revocation** — the helper uses an opaque `object_handle`; a production path must hold
   and eventually revoke the kernel capability after use (via `TransferRelease` syscall).

## Stage 79 — RecvV3SharedIoMapper production bridge (RAMFS-only metadata proof)

### What was added

`RecvV3SharedIoMapper` in `shared_io_adapter.rs` is the first production-style `VfsSharedIoMapper`
implementation. It wraps a `RecvSharedV3Delivery` (the userspace-decoded output of `recv_shared_v3`)
and enforces the full direction/permission/range/liveness contract before any byte access.

**Fields captured from delivery:** `cleanup_token`, `mapped_base`, `page_rounded_mapped_len`,
`actual_mapping_perm`, plus a `released: bool` flag for at-most-once cleanup semantics.

**Two constructors:**
- `from_delivery(delivery: &RecvSharedV3Delivery)` — production path: takes all fields from delivery.
- `from_fields(cleanup_token, mapped_base, page_rounded_mapped_len, actual_mapping_perm)` — test path.

**Validation layers (ordered, all must pass before `from_raw_parts`):**
1. `released` check → `AccessAfterCleanup`
2. Direction check: `with_read_reply_buffer` requires `VFS_SHARED_BUFFER_FS_WRITE`; `with_write_request_buffer` requires `VFS_SHARED_BUFFER_FS_READ` → `WrongDirection`
3. Descriptor cross-reference: `object_handle == cleanup_token` AND `object_generation == cleanup_token >> 16` → `StaleHandle`
4. Permission check: write-request path requires `MAP_PERM_READ_ONLY (1)` exactly; read-reply path requires `MAP_PERM_WRITE_BIT (0x2)` set → `MissingRights`
5. Range bounds: `buffer_offset + requested_len <= page_rounded_mapped_len`, overflow-safe → `BadRange`
6. `from_raw_parts` / `from_raw_parts_mut` — only reached in production (real kernel-mapped VA).

**Release contract (`release` method):**
- Descriptor cross-reference checked first → `StaleHandle` on mismatch.
- If `released` is already `true`: return `Ok(())` immediately (at-most-once, second call is free).
- Set `released = true` **before** calling `release_v3_cleanup_token` (guards against panic/unwind).
- `release_v3_cleanup_token` → `Ok(())` in production; in hosted-dev tests returns `SyscallError::Internal`
  (Linux maps syscall NR=4 to `write(fd=token, ...)` → EBADF; RCX = return address → decode_release → Err).
- Failure mapped to `VfsSharedIoAdapterError::ReleaseFailure`.

### Byte-access blocker (hosted-dev)

In `hosted-dev` (unit-test) builds, `mapped_base` is a synthetic value — it is NOT a valid virtual
address. Calling `core::slice::from_raw_parts` on it would be undefined behaviour and would crash
the test process. Therefore:

- **All `RecvV3SharedIoMapper` tests exercise only error paths that return before the unsafe block.**
- The validation layers (direction, stale handle, permission, range) all return errors before
  `from_raw_parts` is reached; tests are structured to trigger one of these exits.
- No test calls `with_read_reply_buffer` or `with_write_request_buffer` with a valid permission
  path on a `RecvV3SharedIoMapper` — that would require a real kernel-mapped VA.
- RAMFS byte-content proof in `stage79_tests` uses `BorrowedSharedIoTestMapper` (in-process backing
  store), not `RecvV3SharedIoMapper`. This is the designated first proof backend for Stage 79.
- Real end-to-end byte transfer requires a live `recv_shared_v3` delivery from the kernel.
  That path is out of scope for hosted-dev unit tests.

### Tests added (Stage 79)

**`shared_io_adapter.rs` — `mod tests` (12 new tests):**

| Test | What it proves |
|---|---|
| `stage79_recv_v3_mapper_from_delivery_constructs_with_all_fields` | `from_delivery` copies all fields; `!is_released()` |
| `stage79_recv_v3_mapper_from_fields_is_not_released` | `from_fields` starts unreleased |
| `stage79_write_request_wrong_direction_rejected` | FS_WRITE descriptor → `WrongDirection` (before stale check) |
| `stage79_write_request_stale_handle_rejected` | wrong `object_handle` → `StaleHandle` |
| `stage79_write_request_stale_generation_rejected` | wrong `object_generation` → `StaleHandle` |
| `stage79_write_request_rw_perm_rejected` | `PERM_RW` on write-request path → `MissingRights` |
| `stage79_write_request_bad_range_rejected` | `offset + len > page_rounded_len` → `BadRange` |
| `stage79_read_reply_wrong_direction_rejected` | FS_READ descriptor → `WrongDirection` |
| `stage79_read_reply_readonly_perm_rejected` | `PERM_RO` on read-reply path → `MissingRights` |
| `stage79_release_stale_handle_rejected` | wrong handle → `StaleHandle`; `released` stays `false` |
| `stage79_release_marks_released_and_blocks_subsequent_access` | release sets `released`; subsequent buffer call → `AccessAfterCleanup` |
| `stage79_release_idempotent_second_call_returns_ok` | second `release` → `Ok(())` regardless of first outcome |

**`vfs_service.rs` — `mod stage79_tests` (8 new tests):**

| Test | What it proves |
|---|---|
| `stage79_recv_v3_mapper_implements_vfs_shared_io_mapper_trait` | compile-time trait bound satisfied |
| `stage79_dispatch_write_shared_request_with_recv_v3_mapper_rw_perm_rejected` | wrong-perm mapper → `VfsError::Malformed`; no `from_raw_parts` |
| `stage79_dispatch_read_shared_reply_with_recv_v3_mapper_ro_perm_rejected` | wrong-perm mapper → `VfsError::Malformed`; no `from_raw_parts` |
| `stage79_byte_access_blocker_documented_and_gates_unchanged` | documents blocker; all three gate flags `true` |
| `stage79_dispatch_write_shared_request_ramfs_regression` | RAMFS write via `BorrowedSharedIoTestMapper`; byte content verified |
| `stage79_dispatch_read_shared_reply_ramfs_regression` | RAMFS read via `BorrowedSharedIoTestMapper`; byte content verified |
| `stage79_handle_request_still_rejects_shared_opcodes` | shared opcodes → `VfsError::Unsupported` from `parse_request` |
| `stage79_gate_values_unchanged_from_stage78` | all three direction/PM gate flags `true` |

### Invariants frozen by Stage 79

- `RecvV3SharedIoMapper::from_delivery` must copy all four delivery fields without modification.
- `released` flag must be set BEFORE calling `release_v3_cleanup_token` (at-most-once guarantee).
- All six validation layers must remain in order; none may be skipped or reordered.
- `handle_request` must NOT route shared opcodes (unchanged from Stage 78).
- `VFS_WRITE_SHARED_REQUEST_ENABLED`, `VFS_READ_SHARED_REPLY_ENABLED`, `VFS_PM_TASK_EXIT_NOTIFICATION_ENABLED` all remain `true`.
- `VFS_SUPERVISOR_TASK_EXIT_NOTIFICATION_ENABLED` remains `false`.
- SYSCALL_COUNT, recv_shared_v3 ABI offsets, SpawnV5/Phase2B/Phase3B unchanged.
- FAT/ext4/blkcache production behavior unchanged; shared I/O not enabled for those backends.

**Run commands:**
- `cargo test -p yarm-fs-servers --features hosted-dev -- stage79` (20 Stage 79 tests)
- `cargo test -p yarm-fs-servers --features hosted-dev` (full 360-test regression)

## Stage 80: ramfs_srv/fat_srv/ext4_srv CPIO staging, spawn wiring, and conservative VFS mount policy

Stage 80 does not change any VFS shared-I/O ABI, opcode routing, or gate flags. The `VFS_WRITE_SHARED_REQUEST_ENABLED`,
`VFS_READ_SHARED_REPLY_ENABLED`, and `VFS_SHARED_IO_ENABLED` constants established in Stage 78 remain
`true` and unchanged.

### What Stage 80 adds

1. **CPIO archive staging**: `sbin/ramfs_srv`, `sbin/fat_srv`, and `sbin/ext4_srv` ELFs are added to
   the initramfs CPIO archive. All three ELFs must be packed at 4096-byte-aligned data offsets, with
   `ALIGN_PROOF path=/<name> data_offset=<N> alignment_mod=0 aligned=true` emitted for each.

2. **PM image-ID table extension**: `VFS_SERVICE_IMAGE_ID_MAX` extended from 9 to 12.
   Image IDs 10 (fat_srv), 11 (ramfs_srv), 12 (ext4_srv) are now within the VFS spawn range.
   `pm_vfs_spawn_inline` maps `12 => b"/initramfs/sbin/ext4_srv"` and `pm_image_cpio_name` maps
   `12 => Some(b"sbin/ext4_srv")`.

3. **init spawn wiring**: `init/service.rs::run()` spawns ext4_srv via `spawn_v5_cap(pm_send, pm_recv, 12, ...)`.
   Log markers: `INIT_EXT4_SPAWN_BEGIN`, `INIT_EXT4_SPAWN_OK child_tid=<N> mount_deferred=true reason=no-ipc-loop`,
   `EXT4_SRV_READY`.

4. **Conservative VFS mount policy**:
   - ramfs: writable at `/ram` — live via `run_with_config(RamFsStartupConfig::default_compat())`.
   - fat: read-only at `/fat` if block backend available; guarded by `FatStartupConfig::production(None, ...)`.
   - ext4: spawned but VFS registration **deferred**. Blocker: `ext4/service.rs::run()` is a demo
     smoke that returns without entering a kernel `ipc_recv` loop. VFS cannot route requests to a
     non-listening service. Registration is wired only after a real recv loop is added.

### What Stage 80 does NOT change

- Shared-I/O opcodes 26, 27, 28 remain `Unsupported` in live `VfsService`.
- `VFS_SHARED_IO_ENABLED`, `VFS_WRITE_SHARED_REQUEST_ENABLED`, `VFS_READ_SHARED_REPLY_ENABLED` unchanged.
- `SYSCALL_COUNT` remains 31.
- ext4 writes remain `Err(VfsError::Unsupported)` for all lengths.
- FAT production writes remain guarded by `NoBlockBackend` error when no block backend is present.
- No kernel syscall/IPC/VM/cap internals modified.

**Run commands:**
- `python3 scripts/test_pack_initramfs_aligned.py` (4 CPIO alignment tests)
- `cargo test -p yarm-fs-servers --features hosted-dev -- stage80` (8 FS backend tests)
- `cargo test -p yarm-control-plane-servers --features hosted-dev -- stage80` (5 gate tests)

## Stage 80R/81: Profile-gate optional FS live spawns and document kernel spawn-path blocker

Stage 80R/81 addresses a QEMU smoke regression introduced by Stage 80 where attempting to
live-spawn ramfs_srv/fat_srv/ext4_srv caused core services (blkcache, virtio_blk, driver_manager)
to never appear. On AArch64, the kernel halted.

### Root cause

`spawn_image_path_for_image_id()` in `src/kernel/syscall.rs` covers image IDs 0–9 only. Attempting
to spawn IDs 10/11/12 returns `SyscallError::InvalidArgs`. On AArch64 this halts the kernel via the
trap handler. On x86_64, PM falls through a long Phase-2B VFS bulk-read failure path, corrupting the
PM reply state needed by subsequent core spawns.

### What Stage 80R/81 changes

1. **Profile gate**: `const INIT_SPAWN_OPTIONAL_FS_SERVERS: bool = false;` added to `init/service.rs`.
   All ramfs/fat/ext4 live spawn code is placed inside `if INIT_SPAWN_OPTIONAL_FS_SERVERS { ... }`.

2. **SKIPPED markers**: When the gate is false, init emits:
   - `INIT_RAMFS_SPAWN_SKIPPED reason=profile_disabled`
   - `INIT_FAT_SPAWN_SKIPPED reason=profile_disabled`
   - `INIT_EXT4_SPAWN_SKIPPED reason=profile_disabled`

3. **Spawn order**: The optional FS section now appears **after** driver_manager, blkcache, and
   virtio_blk spawns and their smoke checks. Core-service startup is never gated on optional FS.

4. **Blocker documented**: `init/service.rs` comments cite `spawn_image_path_for_image_id` and
   `SyscallError::InvalidArgs` as the kernel blocker preventing live optional-FS spawns.

5. **Smoke scripts**: `INIT_FAT_SPAWN_SKIPPED`, `INIT_RAMFS_SPAWN_SKIPPED`, `INIT_EXT4_SPAWN_SKIPPED`
   added to informational marker lists. New EXT4 info section added. Core service checks unchanged.

### What Stage 80R/81 does NOT change

- Stage 80 CPIO staging (ramfs_srv/fat_srv/ext4_srv remain in initramfs).
- ALIGN_PROOF coverage (`test_stage80_ramfs_fat_ext4_elfs_are_aligned_and_emit_proof` still passes).
- PM image-ID table (`VFS_SERVICE_IMAGE_ID_MAX = 12`, all three CPIO path/name mappings intact).
- All Stage 80 gate tests continue to pass.
- Shared-I/O opcodes, `VFS_SHARED_IO_ENABLED`, `VFS_WRITE_SHARED_REQUEST_ENABLED`,
  `VFS_READ_SHARED_REPLY_ENABLED` unchanged.
- `SYSCALL_COUNT` remains 31; SpawnV5 ABI, Phase2B/Phase3B unchanged.
- No kernel syscall/IPC/VM/cap internals modified.

**Run commands:**
- `cargo test -p yarm-control-plane-servers --features hosted-dev -- stage81` (5 gate tests)
- `cargo test -p yarm-control-plane-servers --features hosted-dev -- stage80` (5 regression checks)
- `bash -n scripts/qemu-x86_64-core-smoke.sh && bash -n scripts/qemu-aarch64-core-smoke.sh`
- `cargo test -p yarm-fs-servers --features hosted-dev` (full 368-test regression)

---

## Stage 81A+B: Kernel trap-error parity and optional-FS spawn path table

**Stage 81A — Syscall error parity:**
`handle_trap`'s `Trap::Syscall` arm now encodes errors into the trap frame
(`trapframe.set_err(e.code())`) rather than propagating them as `TrapHandleError`. This prevents
normal user-space errors (`InvalidArgs`, `MissingRight`, etc.) from triggering the fatal kernel halt
paths in AArch64 (`WFE` spin), x86_64 (`halt_forever()`), and RISC-V (`?` propagation).

Kernel-internal wrappers that synthesize a syscall trap and need to observe denial results
(e.g. `control_plane_set_process_cnode_slots_via_syscall`) now read `frame.error_code()` after
`handle_trap` returns and re-raise the error via `SyscallError::from_code`.

**Stage 81B — Optional-FS spawn path table:**
`spawn_image_path_for_image_id()` extended with `fat_srv` (10), `ramfs_srv` (11), `ext4_srv` (12).
`INIT_SPAWN_OPTIONAL_FS_SERVERS` remains `false`; no live spawning in core profile.

**Invariants preserved:** syscall numbers, `SYSCALL_COUNT`, `SpawnV5` ABI, Phase2B/Phase3B
semantics, CPIO/ALIGN_PROOF artifacts, VFS shared-I/O opcodes — all unchanged.
