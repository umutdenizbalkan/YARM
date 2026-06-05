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
