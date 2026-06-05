// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

# VFS shared-region mapper requirements (FS-19 design)

## Status

This document is a design contract. It does not reserve a syscall number, alter a register layout,
enable `VFS_SHARED_IO_ENABLED`, advertise a capability, create a production mapper, or authorize a
filesystem to dereference a transferred mapping.

The FS-19 conclusion is that a userspace-only production mapper is **not** currently safe. A future,
separately reviewed kernel ABI is required to return authoritative object metadata and to resolve the
legacy `ipc_recv` versus recv-v2 map-intent lane conflict. Until that work exists:

- `UnsupportedSharedIoMapper` remains the production default;
- live VFS opcodes `26`, `27`, and `28` remain unsupported;
- RAMFS shared-I/O remains test/helper-only;
- FAT production writes remain unwired; and
- ext4 remains read-only.

## Current interface audit

### Information available today

| Source | Available fields | Authority and limits |
|---|---|---|
| legacy mapped `ipc_recv` / `MappedTransferRecv` | sender TID, receiver-local cap, caller-selected mapping base, page-rounded mapped length, requested map intent | Kernel is authoritative for successful mapping and the local cap. The result does not identify the delivered opcode or exact unrounded region length. |
| `SharedMemoryRegion` payload | sender-provided offset and exact region length | The kernel decodes it while mapping, but FS-18 does not receive it in a portable mapped-receive result. It is not object introspection. |
| recv-v2 out metadata | sender TID, application opcode/flags, payload length, returned cap, reply/transfer classification | It cannot currently carry explicit shared map intent because its metadata-length argument occupies the map-intent lane. |
| `VfsSharedBufferDescriptor` | opaque handle, descriptor generation, buffer offset/length, intended FS access | Helper/service assertion only. It is explicitly not a kernel cap slot and is not bound to the transferred object. |
| `VfsReadSharedRequest` / `VfsWriteSharedRequest` | VFS opcode by envelope, FD, file offset mode, requested length, request ID, flags, descriptor | Service framing exists only as disabled helper ABI. No mapped transfer is atomically bound to it. |
| `VfsSharedIoHandleTable` | local handle allocation and generation reuse protection | Protects helper-local lifecycle state, not kernel object identity or cap generation. |
| `VfsSharedIoLifecycle` | direction, request state, terminal reason, first-winner cleanup, fallback gating | Does not own a real mapping, cap, process-exit hook, timeout source, or revocation notification. |
| `TransferReleaseToken` | local cap plus explicit base/length or active-record release | Metadata-only and explicit. It does not prove object type, rights, request identity, or exact region length. |

### Missing information

A production mapper cannot currently establish:

1. that the transferred capability names a `MemoryObject` or future `SharedBuffer` rather than an
   endpoint, reply cap, notification, address space, or another object;
2. the effective receiver-local rights after attenuation;
3. the total object size and exact unrounded transferred region length;
4. an object/transfer generation that remains stable across cap-slot reuse;
5. the delivered VFS opcode and request ID on the same atomic receive that created the mapping;
6. a cleanup-token identity that cannot be confused with a later mapping of the same local cap slot;
7. requester ownership and requester-exit cleanup authority; or
8. whether revocation occurred while the server was using the range.

No existing userspace wrapper can synthesize these facts. A userspace broker could provide policy and
allocation, but it cannot authoritatively identify kernel objects or rights without a trusted kernel
query/result.

## Required future mapping metadata

A future versioned result should be logically equivalent to `MappedSharedObjectMeta` below. This is a
field contract, not a committed Rust or wire layout.

| Field | Meaning | Source required |
|---|---|---|
| `abi_version` | Version and encoded length of the metadata record | future kernel receive ABI |
| `sender_tid` | Sender responsible for the transfer envelope | available today/kernel |
| `receiver_local_cap` | Cap materialized in the receiver | available today/kernel |
| `object_kind` | `MemoryObject` or future `SharedBuffer`; all other kinds rejected | **new kernel metadata/query** |
| `effective_rights` | Rights of the receiver-local cap after attenuation | **new kernel metadata/query** |
| `object_id` | Non-slot kernel object identity suitable for correlation, not authorization | **new kernel metadata** |
| `object_generation` | Generation changed when the object identity is destroyed/reused | **new kernel metadata** |
| `object_size` | Exact total byte size of the backing object | **new kernel metadata/query** |
| `region_offset` | Exact unrounded transfer offset within the object | kernel already decodes; future result must return it |
| `region_len` | Exact unrounded transferred byte length | kernel already decodes; future result must return it |
| `mapped_base` | Receiver virtual base chosen/confirmed for this mapping | partially available today; future explicit result |
| `mapped_len` | Page-rounded mapped length | available today/kernel |
| `mapping_permissions` | Actual mapped read/write permissions, not merely requested intent | **new explicit kernel result** |
| `transport_opcode` | Must identify shared-region transport | **new versioned receive result** |
| `application_opcode` | `READ_SHARED_REPLY` or `WRITE_SHARED_REQUEST` | future VFS/shared transport frame |
| `request_id` | Nonzero VFS request correlation ID | future VFS/shared transport frame |
| `direction` | Read-reply or write-request | future VFS/shared transport frame |
| `descriptor_generation` | Generation of the service-local opaque descriptor | future VFS frame/registry |
| `cleanup_token_id` | Unique identity for release/revoke and late-event correlation | **new kernel transfer identity or trusted registry binding** |
| `owner_process_id` | Requester whose exit cancels the transfer | runtime/process lifecycle integration |

The metadata must be returned atomically with mapping success or be queried using an unforgeable
transfer identity before any byte access. A raw cap-slot number, virtual address, request ID, or
userspace generation alone is not an object identity.

## recv-v2 and map-intent conflict

### Existing conflict

Legacy shared receive interprets argument lane 4 as map-intent bits. recv-v2 uses that same lane for
the metadata-buffer length. Therefore a call cannot safely request both recv-v2 metadata and explicit
mapping permissions under the frozen layout. The FS-18 wrapper deliberately uses legacy receive and
requires an endpoint protocol guarantee, leaving it unable to verify the delivered opcode.

### Options

| Option | Kernel impact | Userspace impact | Metadata completeness and safety | Read reply | Write request |
|---|---|---|---|---|---|
| **A. Legacy mapped receive, then query** | New object/transfer introspection call | Keep FS-18 receive, then query before access | Two-step race unless query uses an unforgeable active-transfer ID; still lacks atomic application framing | Possible only after query and writable-buffer creation | Possible only after query and rights attenuation proof |
| **B. Versioned recv-v3 / mapped-recv-v3** | New versioned syscall or unambiguous argument block; no mutation of recv-v2 | New wrapper and metadata record; old recv paths unchanged | Best atomic binding of opcode, transfer identity, object metadata, exact range, mapping permissions, and cleanup token | Yes | Yes |
| **C. Trusted userspace broker** | Still needs kernel object-info/map authority unless broker receives a privileged existing interface | Broker allocates and signs service framing | Policy can be userspace-only, but object kind/rights/size claims are not authoritative today | Broker could allocate, but writable object creation primitive is missing | Broker can attenuate only if kernel exposes authoritative rights operations |
| **D. Receive cap, then explicit map** | New object-info and map syscalls/service ABI | Separates receive, validate, reserve VA, and map | Clear phases and easier retry, but more calls and a cap-revoke race that needs transfer/object generation | Yes | Yes |

### Recommendation

Use **Option B**, a new versioned mapped-receive ABI, as the primary design. It must use a pointer to a
versioned request/result record (or otherwise separate mapping intent from metadata length), return an
unforgeable transfer/cleanup identity, and leave legacy receive, timed receive, and recv-v2 unchanged.

Option D is an acceptable alternative if the project prefers explicit receive/inspect/map phases, but
it still requires new kernel object-info and map authority plus generation-safe revocation handling.
Option A should not be used without an unforgeable transfer ID, and Option C must not be treated as an
authoritative security boundary without kernel-backed object metadata.

This recommendation means FS-19 stops live shared-I/O work and requests a separate, explicitly scoped
kernel ABI design task. No syscall number or layout is selected here.

## Validation responsibility

Validation is layered; later layers must not silently substitute assertions for earlier authority.

### Kernel

Before reporting mapping success, the future kernel path must:

- accept only `MemoryObject` or a deliberately introduced `SharedBuffer` object kind;
- reject endpoints, reply caps, notifications, cnodes, address spaces, and unknown kinds;
- validate transfer-envelope ownership and materialize a receiver-local cap exactly once;
- attenuate and report effective rights;
- validate `region_offset + region_len <= object_size` with checked arithmetic;
- validate mapping base, page-rounded length, address-space bounds, and overlap;
- enforce no executable mapping and no write mapping without effective write rights;
- create an unforgeable active-transfer/cleanup identity; and
- make release/revoke idempotence observable without releasing a later reused object.

### Userspace wrapper

The future `yarm-user-rt` wrapper must:

- validate metadata version, encoded length, known object kind, and known permission bits;
- reject inconsistent exact versus rounded lengths and arithmetic overflow;
- expose mutable bytes only when actual mapping permissions include write;
- expose immutable bytes for read-only mappings;
- retain the cleanup identity in a non-copyable guard;
- perform explicit close with a visible error path; and
- never infer object identity from a cap slot or mapping address.

### VFS service

Before forwarding to an FS server, VFS must:

- allocate a globally unique/nonzero request ID for the connection epoch;
- bind request ID, VFS opcode, direction, FD, offset mode, length, descriptor generation, object
  identity/generation, and cleanup token in one registry entry;
- verify descriptor offset/length fits both exact region length and object size;
- require FS-write-only-for-purpose access for `READ_SHARED_REPLY`;
- require FS-read-only access for `WRITE_SHARED_REQUEST` and reject any write permission;
- reject stale generations, duplicate active request IDs, and direction/opcode mismatch; and
- retain requester ownership information for exit cleanup.

### Filesystem server

The FS server must:

- resolve mappings only through `VfsSharedIoMapper` and the validated registry;
- compare received metadata with lifecycle request ID, direction, descriptor generation, and length;
- never retain a slice, cap, or address after completion;
- report exact partial completion without touching bytes beyond that count;
- call cleanup before terminal reply or inline fallback; and
- treat any mismatch, revocation, or access-after-cleanup as a terminal error.

## Generic requester-owned writable buffer design

`READ_SHARED_REPLY` requires a requester-owned buffer into which the FS temporarily writes.

1. **Allocation:** VFS/requester asks a future shared-buffer allocator (kernel primitive or trusted
   service backed by one) for an exact logical size, rounded to pages for mapping. Zero-sized objects
   are rejected and per-request/accounting limits apply.
2. **Ownership:** requester retains the owning capability. VFS holds lifecycle authority but does not
   transfer ownership to the FS.
3. **Initialization:** allocation must be zero-filled so short reads or backend failures cannot expose
   stale memory. VFS records requested length separately from object capacity.
4. **Transfer:** VFS sends a versioned shared frame plus a delegated cap limited to `MAP|READ|WRITE`
   as required for the read-reply mapping. Delegation must identify the requester and request ID.
5. **FS mapping:** the kernel maps only the validated region writable and non-executable. The FS may
   write at most the requested range and may not retain the mapping.
6. **Completion:** FS reports `bytes_read <= requested_len`. Bytes beyond `bytes_read` remain ignored;
   VFS must not expose them as file data.
7. **Cleanup:** FS mapping/cap is released exactly once. Requester ownership remains unless the
   requester exited or explicitly dropped it.
8. **Fallback:** inline fallback is a new operation allowed only after shared cleanup succeeds. The
   shared request ID cannot be reused.
9. **Exit:** requester exit cancels allocation ownership and triggers transfer cleanup. Server exit
   revokes only the delegated FS mapping and completes the requester with a backend failure.

`WRITE_SHARED_REQUEST` uses the same ownership model but delegates only `MAP|READ`; the FS mapping
must be read-only even if the requester owns a writable object.

## VFS/shared-region framing

A future frame must bind the service operation to the transferred object. The logical fields are:

| Field | Rule |
|---|---|
| `frame_version` / `frame_len` | Required for compatible extension; unknown mandatory versions rejected |
| `vfs_opcode` | Exactly `READ_SHARED_REPLY` or `WRITE_SHARED_REQUEST` |
| `request_id` | Nonzero and unique for the connection epoch |
| `fd` | Open file description selected by VFS |
| `file_offset` / `offset_mode` | Current-offset mode requires encoded offset zero |
| `requested_len` | Nonzero, within policy limit, and no greater than descriptor region |
| `direction` | Must agree with opcode and mapping permissions |
| `request_flags` | Known bits only; includes inline-fallback eligibility |
| `descriptor_handle` / `descriptor_generation` | Opaque service-local registry key; never a raw cap or VA |
| `buffer_offset` / `buffer_len` | Checked against exact transferred region and object size |
| `object_id` / `object_generation` | Must match authoritative mapped metadata |
| `cleanup_token_id` | Must match the active transfer identity returned by the mapping ABI |
| `owner_process_id` | Used only for lifecycle routing, not authorization by itself |

The frame and cap transfer must be delivered atomically or joined through the unforgeable cleanup
identity. A request is rejected before backend access if any opcode, direction, generation, object,
range, owner, or cleanup-token field differs from the registry entry.

## Cancellation, timeout, revocation, and exit matrix

| Event | Lifecycle terminal reason | Release action | Reply/result | Inline fallback |
|---|---|---|---|---|
| requester exits before map | `RequesterExit` | revoke pending transfer/cap; no mapping access | no requester reply; backend not called | no |
| requester exits while in flight | `RequesterExit` wins if completion not committed | cancel backend if supported; unmap/revoke once | discard late completion | no |
| FS server exits while mapped | `ServerExit` | runtime/kernel revokes delegated cap and mapping once | requester receives backend/server failure | only as a new request after cleanup |
| timeout before backend starts | `Timeout` | release mapping/cap once | timeout with zero bytes | allowed only after confirmed cleanup and policy flag |
| timeout after partial completion | `Timeout` or committed completion, first winner | release once; freeze exact completed count | report policy-defined partial timeout; never over-report | no automatic replay of completed bytes |
| cancellation races completion | `Cancelled` or `Success`/`BackendError`, first winner | one cleanup token consumed | loser is ignored as late/duplicate | only if cancellation won and cleanup completed with zero committed bytes |
| duplicate reply | `DuplicateReply` observation; terminal state unchanged | no second release | reject/ignore duplicate | no |
| stale handle reused | `StaleHandle` | release only the independently identified stale transfer, never the new generation | invalid-descriptor failure | optional after stale cleanup, with new request ID |
| cap revoked in flight | `StaleHandle` or dedicated `Revoked` future reason | kernel/runtime invalidates mapping and marks cleanup consumed | permission/stale failure with exact prior progress | only after revocation cleanup; normally no |
| transfer release fails | `BackendError`/dedicated cleanup failure; request remains quarantined | retry or escalate to runtime; never reuse identity | do not send success or fallback while mapping may remain | no |
| timeout cleanup then fallback | `Timeout`, then `Cleaned`, then one-shot fallback gate | shared release must succeed first | independent inline request/result | yes, once, only if policy flag set |

Late replies, cancels, timeout callbacks, and exit notifications must carry request ID plus cleanup
identity and generation. Matching only a cap slot or descriptor handle is insufficient.

## Gates before any live mapper

A production implementation must not start until all of the following are separately reviewed and
tested:

1. versioned authoritative object/mapping metadata;
2. a resolved recv-v2/map-intent ABI with no overloaded lane;
3. generic requester-owned writable shared-buffer allocation;
4. object kind, effective rights, exact size, and generation validation;
5. atomic VFS frame/transfer binding;
6. unforgeable cleanup identity and release/revoke semantics;
7. process-exit, timeout, cancellation, and server-exit delivery;
8. first-winner cleanup and late-event tests across handle/cap reuse; and
9. a disabled-by-default RAMFS-only experiment before FAT or any block-backed filesystem.

## FS-19 non-changes

FS-19 changes documentation only. It does not change kernel or architecture code, syscall numbers,
`SYSCALL_COUNT`, control-plane policy, runtime spawn order, IPC/VM/capability internals, live VFS
dispatch, the production mapper, FAT production writes, ext4 writes, or the FS-12 block path. QEMU
smoke is not required because runtime behavior is unchanged.
