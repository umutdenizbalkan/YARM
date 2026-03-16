# Storage Service Contract (blkcache / fat / ext4 / virtio_blk)

This document defines the shared wire and behavior contract between block transport services and filesystem services.

## Goals

- Allow `fat.srv` and `ext4.srv` to swap block backends (`virtio_blk.srv`, future NVMe, loopback) without protocol drift.
- Keep request/response framing stable with explicit little-endian layout and golden vectors.
- Define minimum behavior for caching (`blkcache.srv`) and error propagation.

## Layered model

1. **`virtio_blk.srv` (transport)**
   - Owns queue/ring transport semantics.
   - Accepts framed block requests.
   - Returns framed completion responses.
2. **`blkcache.srv` (cache policy)**
   - Optional write-back/read cache in front of transport.
   - Must not alter request framing when forwarding I/O.
3. **Filesystem services (`fat.srv`, `ext4.srv`)**
   - Own on-disk metadata parsing and inode/dir/file policy.
   - Operate only on logical block read/write contract.

## Request/response framing (v1)

### Request frame (`VirtioBlkReqFrame`, 20 bytes)

- `op: u16` (LE)
  - `1` = read
  - `2` = write
- `reserved: u16` (LE, must be 0)
- `sector: u64` (LE)
- `len: u32` (LE)
- `tag: u32` (LE; echoed in response)

### Response frame (`VirtioBlkRespFrame`, 12 bytes)

- `status: u8`
  - `0` success
  - non-zero failure
- `pad: [u8; 3]` (reserved)
- `done_len: u32` (LE)
- `tag: u32` (LE)

## Error contract

- Malformed frame length -> `Malformed`.
- Out-of-range sector or unsupported operation -> backend error mapped to service error (`BadFd` currently in scaffold path).
- Queue saturation -> allocation/queue error (`NoFd` currently in scaffold path).

## Cache contract (`blkcache.srv`)

- Cache configuration controls line count, writeback batch size, and eviction policy.
- Dirty lines may be flushed incrementally (`writeback_tick`) or completely (full flush path).
- Cache must be transparent to upper layers: a successful read/write from the cache layer must preserve block semantics and frame ordering.

## FAT/ext4 backend swap invariants

- Both filesystems consume a block device contract, not concrete transport internals.
- Metadata ownership stays in filesystem services:
  - FAT: BPB/FAT chain parsing.
  - EXT4: inode/block metadata parsing.
- Shared transport frame vectors must remain stable across services and releases.

## Compatibility test requirements

- Golden vector tests for request/response framing are required.
- Deterministic mount-failure matrix simulation must pass in CI.
- Service boundary scripts must pass in CI to prevent kernel/service layering regressions.
