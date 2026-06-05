// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

# Userspace block-sector write contract (FS-12)

## Scope and current stack

FS-12 adds a bounded userspace sector-write path below filesystems:

```text
BLKCACHE_OP_WRITE_BLOCK
    -> blkcache write-through validation/cache
    -> BLK_OP_WRITE
    -> virtio_blk service assembler
    -> VIRTIO_BLK_OP_WRITE request chain/device model
```

This is not VFS or filesystem wiring. FAT production writes remain disconnected, ext4 remains
read-only, and `VFS_SHARED_IO_ENABLED` remains an unadvertised helper-only design. Kernel syscall
ABI, `SYSCALL_COUNT`, IPC/VM/capability internals, and runtime spawn policy are unchanged.

## Audit classification

- **A — public block read path:** `BLK_OP_GET_INFO` and `BLK_OP_READ` already exist in
  `block_abi.rs`. The current inline read structure is preserved unchanged.
- **B — blkcache internal read path:** `BLKCACHE_OP_READ_BLOCK` and the registered-buffer
  `BlockIoRequest` exist, but live buffer registration/mapping remains unsupported; FS-12 does not
  change that read behavior.
- **C — existing unsupported write opcode:** `BLKCACHE_OP_WRITE_BLOCK` existed but always returned
  `BLKCACHE_STATUS_ERR_UNSUPPORTED`.
- **D — virtio primitive:** `VIRTIO_BLK_OP_WRITE = 2` and the queue/request model already existed.
- **E — missing codecs:** there was no public filesystem-facing `BLK_OP_WRITE` request/reply carrying
  bytes.
- **F — missing forwarding:** blkcache did not forward writes and virtio_blk did not expose an inline
  block-write service operation.
- **G — missing tests:** no end-to-end userspace service-model write/read/overwrite test existed.

## Initial write ABI

`BLK_OP_WRITE` is `0x0203`. The same `BlkWriteRequest`/`BlkWriteReply` codec is used by
`BLKCACHE_OP_WRITE_BLOCK` and the lower block service.

A 512-byte sector cannot fit in the existing 128-byte IPC payload. FS-12 therefore defines an
ordered chunk transaction rather than pretending that one IPC message can carry a sector:

- request header: 32 bytes;
- inline data capacity: 96 bytes;
- request payload: exactly 128 bytes;
- reply payload: exactly 24 bytes;
- one logical sector per request ID/LBA transaction;
- chunks must be contiguous and ordered from offset zero;
- `BLK_WRITE_F_FIRST` is required on the offset-zero chunk;
- `BLK_WRITE_F_LAST` is valid only when the chunk ends exactly at byte 512;
- unknown flags, zero/oversized chunks, offset overflow, cross-sector chunks, stale/mismatched
  transaction IDs, gaps, and out-of-order chunks are rejected;
- multi-sector requests are not supported; callers start a new request ID for each sector.

The reply reports request ID, status, bytes accepted from the current chunk, whether the complete
sector was committed, and the LBA. Intermediate successful replies report `sector_committed = 0`;
only the final successfully forwarded chunk reports `1`.

## Blkcache behavior

`BLKCACHE_OP_WRITE_BLOCK` now decodes `BlkWriteRequest`, verifies that the selected registered
backend uses 512-byte blocks, validates the LBA against its registered block count, and assembles one
sector. Every chunk is forwarded synchronously to the lower backend using `BLK_OP_WRITE`.

The policy is **write-through**, not write-back:

1. validate and stage the chunk;
2. synchronously forward it;
3. require matching request ID/LBA/accepted length from the lower reply;
4. cache a sector only after the lower service confirms the final sector commit.

A failed lower reply clears the in-progress assembly and does not install or overwrite a cache
entry. No dirty list, delayed flush, write coalescing, eviction writeback, or crash-consistency
protocol exists yet. The small cache is currently used for service-model validation; the existing
registered-buffer read opcode remains unchanged and unsupported until its mapping contract is
implemented.

## virtio_blk behavior

The virtio service accepts `BLK_OP_WRITE` chunks, validates/assembles one complete sector, builds a
virtqueue chain whose request opcode is `VIRTIO_BLK_OP_WRITE`, and submits the sector to its bounded
memory-backed device model. The model supports exact write/read/overwrite tests and validates device
bounds and whole-sector length.

This pass does not claim real hardware durability. The existing repository service has no completed
MMIO/DMA transport attachment for these inline bytes, so a QEMU persistent-media write smoke remains
deferred. The request builder, queue opcode, service transaction behavior, and lower-service
forwarding are unit tested; connecting the same completed sector to a real DMA descriptor chain is a
future driver integration step and must preserve the codec and commit semantics.

## Status and errors

The existing `BlkStatus` values are reused:

- `Success`: chunk accepted; final reply may additionally mark sector committed;
- `InvalidRequest`: malformed flags, request ID, transaction identity/order, or out-of-range LBA;
- `InvalidAlignment`: chunk crosses a sector or is not at the expected next offset;
- `OversizedRequest`: zero-length or larger-than-96-byte inline chunk;
- `DeviceUnavailable` / `NotReady`: no registered backend or no reply endpoint;
- `IOError`: lower reply/transport mismatch or device submission failure.

`BLK_OP_GET_INFO`, the existing public read ABI, blkcache registration, and block-backend SG codecs
are unchanged.

## Supported and deferred profiles

Supported now:

- one 512-byte sector per ordered inline chunk transaction;
- synchronous blkcache write-through to a registered lower block service;
- exact cache/device-model read-after-write and overwrite behavior;
- checked LBA, chunk, transaction, reply, and device bounds;
- virtio write request-chain construction with operation `2`.

Deferred:

- multi-sector writes in one request;
- shared-memory/SG payload mapping and zero-copy block I/O;
- live registered-buffer reads/writes;
- write-back caching, flush barriers, FUA/discard, and crash-safe ordering;
- real MMIO/DMA virtio transport persistence and QEMU storage smoke;
- FAT production write wiring;
- VFS `WRITE_SHARED_REQUEST` routing and all live shared-I/O enablement;
- ext4 writes.

## Next pass

FS-13 should implement the VFS write-payload/`WRITE_SHARED_REQUEST` helper path and its ownership and
cleanup state machine. It should not connect FAT production writes until both that payload contract
and the lower persistent-device path have completed lifecycle and failure testing.
