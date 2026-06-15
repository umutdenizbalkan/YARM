<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Umut Deniz Balkan -->

# YARM Filesystem and Storage Contracts

> **Ownership rule.** Per-filesystem (initramfs / devfs / ramfs / ext4 / FAT)
> behavior, block-backend ABI, and blkcache ABI live here. Generic VFS routing
> lives in `doc/VFS.md`. Per-server runtime status lives in `doc/STATUS.md`
> §2.3. New FS / storage fragment files are forbidden; update this doc
> instead. See `doc/DOCUMENTATION_MAP.md`.

For the optional-FS smoke marker invariants see `doc/KERNEL_UNLOCKING.md`
§3 ("Optional-FS smoke markers — do not rename or remove").

---

## 1. initramfs.srv (read-only)

**Image ID 4.** Authoritative implementation lives under
`crates/yarm-fs-servers`.

### Supported nodes (v1)

- boot marker (`INITRAMFS_BOOT_MARKER_PATH_PTR`)
- init image (`INITRAMFS_INIT_PATH_PTR`)
- `etc/hosts` marker (`INITRAMFS_ETC_HOSTS_PATH_PTR`)

### fd / open semantics

- Service-local monotonic fd allocator.
- **Initial fd = `10`**; each successful open consumes one fd.
- Multiple opens of the same node produce unique fds.
- Exhaustion returns `NoFd`.

### I/O semantics

- Read-only.
- `read(fd, len)` returns `min(len, file_len)`.
- `write(fd, len)` always returns `Unsupported`.

### `statx` contract

`statx(path)` returns a compact metadata value:

- Regular-file marker bit: `0x1000_0000_0000_0000`.

### Executable manifest lookup

The initramfs manifest defines stable paths for core services (`init.srv`,
`process_manager.srv`, `vfs.srv`, `supervisor.srv`) with a typed
loader-manifest format. Contract tests cover missing / corrupt manifest
entries. Adding a new sbin server requires bumping `MAX_INITRAMFS_INODES`,
adding the inode entry, adding the `from_cpio_newc` match arm, and adding
a path test (also pinned in `doc/KERNEL_UNLOCKING.md` §3 "Initramfs path
table completeness").

---

## 2. devfs.srv (in-tree device backend)

**Image ID 5.** Authoritative implementation lives under
`crates/yarm-fs-servers`.

### Supported nodes (v1)

- `/dev/console`
- `/dev/null`

Path identity uses frozen pointer IDs:

- `DEV_CONSOLE_PATH_PTR = 0x434F_4E53_4F4C_4500`
- `DEV_NULL_PATH_PTR = 0x4445_564E_554C_4C00`

### fd / open semantics

- Service-local monotonic fd allocator.
- **Initial fd = `3`**; each successful open consumes one new fd.
- Multiple opens of the same node produce distinct fds.
- Full open-handle table returns `NoFd`.

### Per-node I/O

| Node | `read(fd, len)` | `write(fd, len)` |
|------|-----------------|------------------|
| `/dev/null` | `0` (EOF-like) | `len` (drop) |
| `/dev/console` | (uart-bound; see source) | (uart-bound) |

---

## 3. ramfs.srv (writable, in-memory)

**Image ID 11.** Default mount prefix `/ram`. Authoritative implementation
lives under `crates/yarm-fs-servers`.

### Startup config (via SpawnV5 startup slots)

Init may override defaults through the existing slots used by FS services:

- **Slot 14 (`service_extra_cap_1`)** stores packed prefix bytes (up to 8).
- **Slot 15 (`initrd_ptr` raw startup word)** stores RAMFS metadata:
  - bits `0..31`: `max_bytes`
  - bits `32..47`: flags (bit 0 = readonly)
  - bits `48..55`: prefix length
  - bits `56..63`: userspace-only RAMFS config source tag

No kernel ABI or SpawnV5 semantic change. Missing config → log
`RAMFS_CONFIG_DEFAULT prefix=/ram reason=missing-config`; use writable
`/ram` default. Config present → `RAMFS_CONFIG_FOUND prefix=... max_bytes=...`.

### fd / inode semantics

- `openat(path_ptr)` allocates / looks up an inode for `path_ptr`.
- fds are monotonic and **start at `100`**.
- Multiple opens for the same path return unique fds.
- Capacity exhaustion → `NoFd`.

### I/O semantics

- `write(fd, len)` grows inode file length by `len` and returns `len`.
- `read(fd, len)` returns `min(len, inode_file_len)`.
- `close(fd)` invalidates the handle.

### `statx` contract

`statx(path)` returns:

- Regular-file marker bit: `0x1000_0000_0000_0000`.
- Mode bits: owner-read + owner-write (`0o600`).

---

## 4. ext4_srv (read-only)

**Image ID 12.** Authoritative implementation lives under
`crates/yarm-fs-servers`.

### Spawn + mount

Runtime spawning is live: init spawns `ext4_srv` via
`spawn_v5_cap(..., 12, [0,0,0,0], 1)` and calls
`register_ext4_mount_with_vfs()` on success, registering `/ext4`
**read-only (flags=1)** in the VFS mount table via `VFS_OP_MOUNT_REGISTER`.
This does not alter the kernel syscall ABI, SpawnV5 ABI, or
`STARTUP_SLOT_COUNT`.

Markers: `EXT4_SRV_ENTRY`, `EXT4_BIN_BEFORE_RUN`, `EXT4_MOUNT_READY`,
`EXT4_MOUNT_FAILED`-style. Optional-FS strict smoke pins
`EXT4_SRV_READY` + `VFS_MOUNT_REGISTER_EXT4_OK`.

### FS-10 read-side freeze

The ext4 read side is frozen at this contract before shared-I/O and
write-pipeline design:

- **Supported mount profiles:** strictly read-only ext4 with checked
  1–64 KiB blocks, 32/64-byte descriptors, `64bit`, `flex_bg`, extents,
  file types, indexed directories, and the documented read-only-compatible
  feature set. `flex_bg` is supported through absolute inode-table
  locations.
- **Supported checksum profiles:** UUID-derived `metadata_csum` and stored
  `metadata_csum_seed`, with validation of the primary superblock, every
  consumed group descriptor and inode, linear / indexed directory blocks,
  dx roots / nodes, and external extent blocks before use.
- **Supported file block maps:** initialized / unwritten extents, bounded
  external extent trees, sparse holes, and direct / singly / doubly
  indirect legacy maps on non-checksummed images.
- **Supported directory behavior:** linear lookup / enumeration and htree
  lookup / enumeration through at most two dx-node levels. Native routing
  covers signed / unsigned legacy, half-MD4, and TEA hashes;
  SipHash / unknown versions use validated exhaustive indexed-leaf
  traversal. Exact names are always required.

### Writes

`Unsupported` on every mutating VFS opcode.

---

## 5. fat_srv (profile-ready, disabled by default)

**Image ID 10.** Authoritative implementation lives under
`crates/yarm-fs-servers`. `yarm-fs-servers` exports `run_fat()`;
binary at `crates/yarm-fs-servers/src/bin/fat_srv.rs`.

### Status

**Disabled by default** (`INIT_FAT_SPAWN_SKIPPED reason=server_disabled`).
The parser and hosted memory-image backend support bounded regular-file
writes for memory-backed images. FS-20 adds a FAT-only exact-inline
service route and a whole-sector FS-12 write client, but **production
IPC-backed FAT mutation remains blocked** by the lack of a usable
whole-sector read/RMW contract (see also `doc/PROJECT_HISTORY.md`
"Optional FS Milestone 1" → activation blockers).

### Supported formats

- FAT12, FAT16, FAT32 detected from the validated BIOS Parameter Block (BPB)
  cluster count.
- Sector sizes 512 / 1024 / 2048 / 4096 bytes accepted by the parser; the
  runtime IPC block backend issues 512-byte-aligned reads per the existing
  block ABI.
- FAT12 packed 12-bit entries, FAT16, FAT32 low-28-bit entries decoded.
  End-of-chain, free, bad, reserved / out-of-range, and looping cluster
  chains handled explicitly.

### VFS behavior

`openat`, `read`, `write`, `close`, `statx` through the common FS-service
wrapper. Directory traversal in the FAT core for path lookup and hosted
tests; **no separate VFS `readdir` opcode** in the shared request
contract.

Current write support is intentionally narrow and regular-file-only:

- Overwrites within an existing cluster chain.
- Appends / growth that allocate free clusters, zero freshly allocated
  clusters, and link the FAT chain.
- FAT32 FSInfo free-count / next-free maintenance when a valid FSInfo
  sector is present.
- Directory-entry file-size updates after successful growth.
- Best-effort rollback of newly allocated clusters when cluster
  allocation fails before linkage.

`VFS_OP_WRITE = 13` remains length-only and retains its historical
zero-fill behavior. **FAT-specific:** `VFS_OP_WRITE_INLINE = 28` is handled
with an exact payload of 1–96 bytes, current-open-file-offset semantics,
and a typed completion reply. The generic `VfsService` still rejects
opcode 28; shared opcodes 26 / 27 remain unsupported.
`VFS_SUPERVISOR_TASK_EXIT_NOTIFICATION_ENABLED` remains `false`.

Filesystem-internal callers may use `write_path` / `write_bytes`.
Unsupported mutating operations (`mkdir`, `unlink`) are rejected with
`VfsError::Unsupported`.

### Names and directories

- Short 8.3 names supported case-insensitively.
- VFAT long file name entries supported for read-only lookup when the
  checksum matches the following short entry. UTF-16 code units are
  converted via the standard mapping.

---

## 6. Storage service layered model

```text
Filesystem services (fat_srv, ext4_srv)
        │
        ▼
blkcache.srv (cache policy, optional)
        │
        ▼
virtio_blk.srv (transport)
```

### Layer ownership

1. **`virtio_blk.srv` (transport)** — owns queue / ring transport
   semantics; accepts framed block requests; returns framed completion
   responses.
2. **`blkcache.srv` (cache policy)** — optional write-back / read cache in
   front of transport; must not alter request framing when forwarding I/O.
3. **Filesystem services (`fat.srv`, `ext4.srv`)** — own on-disk metadata
   parsing and inode / dir / file policy; operate only on the logical
   block read / write contract.

### Request frame `VirtioBlkReqFrame` (20 bytes, little-endian)

| Field | Type | Note |
|-------|------|------|
| `op` | `u16` | `1` = read, `2` = write |
| `reserved` | `u16` | must be `0` |
| `sector` | `u64` | |
| `len` | `u32` | |
| `tag` | `u32` | echoed in response |

### Response frame `VirtioBlkRespFrame` (12 bytes, little-endian)

`status: u8` first; remaining layout in the source. Goals:

- Allow `fat.srv` / `ext4.srv` to swap block backends (`virtio_blk.srv`,
  future NVMe, loopback) without protocol drift.
- Keep request / response framing stable with explicit little-endian
  layout and golden vectors.
- Define minimum behavior for caching (`blkcache.srv`) and error
  propagation.

---

## 7. Blkcache ABI (Stage 1)

`blkcache_srv` is **storage middleware** and is owned by
`crates/yarm-driver-servers`. `driver_manager` does not route normal
filesystem I/O. The future direction is:

```text
filesystem  →  blkcache  →  block-driver service
```

IPC carries control metadata only. **Block data must not be carried in IPC
payloads.** Shared buffers / zero-copy transport are future work.

Current stage behavior:

- Decode known blkcache opcodes.
- Validate fixed-size payloads.
- Reply with `BlkCacheResponse`.
- Return `Unsupported` for real operations.
- Return `BadRequest` for malformed payloads.

See `crates/yarm-ipc-abi/src/blkcache_abi.rs` for the frozen opcode /
status and wire struct definitions.

---

## 8. Block backend ABI

`BLOCK_BACKEND_ABI` is for **blkcache → block-driver backend**
communication only. It is **separate** from the frontend blkcache ABI used
by FS-facing clients.

- `Message.opcode` carries operation (`QUERY_STATE`, `READ`, `WRITE`,
  `FLUSH`, `GET_GEOM`).
- Payload carries only metadata / descriptor fields (no bulk data
  transfer).
- SG entries are `(mem_cap, offset, length, flags)` and **never raw
  physical addresses**.

Shared-memory mapping, DMA / IOMMU mapping, and zero-copy transport are
future work. **Current `virtio_blk_srv` is truthful stub behavior:**

- `QUERY_STATE` → `EAGAIN` with logical / physical block size 512.
- `GET_GEOM` → `EAGAIN` while hardware remains not ready.
- `READ` / `WRITE` / `FLUSH` → `ENOSYS`.

**No fake I/O success is allowed.**

---

## 9. Block-write contract (FS-12)

FS-12 adds a bounded userspace sector-write path below filesystems:

```text
BLKCACHE_OP_WRITE_BLOCK
    → blkcache write-through validation/cache
    → BLK_OP_WRITE
    → virtio_blk service assembler
    → VIRTIO_BLK_OP_WRITE request chain/device model
```

**This is not VFS or filesystem wiring.** FAT production writes remain
disconnected, ext4 remains read-only, and `VFS_SHARED_IO_ENABLED` remains
an unadvertised helper-only design. Kernel syscall ABI, `SYSCALL_COUNT`,
IPC / VM / capability internals, and runtime spawn policy are unchanged.

### Audit classification

- **A — public block read path:** `BLK_OP_GET_INFO` and `BLK_OP_READ`
  already exist in `block_abi.rs`. The current inline read structure is
  preserved unchanged.
- **B — blkcache internal read path:** `BLKCACHE_OP_READ_BLOCK` and the
  registered-buffer `BlockIoRequest` exist; live buffer registration /
  mapping remains unsupported.
- **C — existing unsupported write opcode:** `BLKCACHE_OP_WRITE_BLOCK`
  existed but always returned `BLKCACHE_STATUS_ERR_UNSUPPORTED`.
- **D — virtio primitive:** `VIRTIO_BLK_OP_WRITE = 2` and the
  queue / request model already existed.
- **E — missing codecs:** there was no public filesystem-facing
  `BLK_OP_WRITE` request / reply carrying bytes.
- **F — missing forwarding:** blkcache did not forward writes and
  virtio_blk did not expose an inline block-write service operation.
- **G — missing tests:** no end-to-end userspace service-model
  write / read / overwrite test existed.

### Initial write ABI

- **`BLK_OP_WRITE` = `0x0203`.**
- The same `BlkWriteRequest` / `BlkWriteReply` codec is used by FS-facing
  callers and forwarded internally.

---

## 10. Optional-FS smoke markers (do not rename / remove)

Pinned in `doc/KERNEL_UNLOCKING.md` §3:

- `INIT_RAMFS_SPAWN_OK`, `RAMFS_SRV_ENTRY`, `RAMFS_MOUNT_READY`,
  `VFS_MOUNT_REGISTER_RAMFS_OK`
- `INIT_EXT4_SPAWN_OK`, `EXT4_SRV_ENTRY`, `EXT4_SRV_READY`,
  `VFS_MOUNT_REGISTER_EXT4_OK`
- `INIT_FAT_SPAWN_SKIPPED reason=server_disabled`
- `INIT_PM_RECV_DRAIN_DONE count=N`

---

## 11. Smoke-test pinned tokens (do not reword)

The two `include_str!` source-grep tests in
`crates/yarm-fs-servers/src/fs/ramfs/service.rs` and
`crates/yarm-fs-servers/src/fs/fat/service.rs` assert these tokens
verbatim. They are preserved here as the canonical location.

### RAMFS server (memory-only)

The `ramfs_srv` is memory-only. Markers used by smoke / source tests:

- `INIT_RAMFS_SPAWN_BEGIN` — init begins spawning ramfs.
- `INIT_RAMFS_SPAWN_OK` — ramfs spawn succeeded.
- `PM_IMAGE_ID_11_RAMFS_SRV` — PM image-ID 11 dispatch marker.
- `RAMFS_BIN_ENTRY_START` — ramfs binary entered.
- `RAMFS_BEFORE_RUN` — about to enter resident loop.
- `RAMFS_CONFIG_FOUND prefix=...` — startup config detected.
- `RAMFS_CONFIG_DEFAULT prefix=/ram reason=missing-config` — fallback to
  default.
- `RAMFS_MOUNT_READY prefix=...` — mount is live.
- `RAMFS_MOUNT_FAILED reason=...` — mount failure path.
- `VFS_MOUNT_REGISTER_RAMFS_OK prefix=...` — VFS registered ramfs mount.

`VFS_OP_WRITE` mutating opcode is supported.

### FAT server (production write-path audit)

`fat_srv` Stage-9x activation checklist (preserved from the former
`doc/FAT_SERVER_CONTRACT.md` test fixtures):

- Mount prefix defaults to `/mnt/fat`.
- Boot config carries `device id` and `service_extra_cap_0` slot usage.
- `FAT_NO_BLOCK_BACKEND` is logged when the block backend is absent.
- `PM_IMAGE_ID_10_FAT_SRV` — PM image-ID 10 dispatch marker.
- `VFS_MOUNT_REGISTER_FAT_OK prefix=...` is the success marker.
- A hosted sample image proves the end-to-end read path.
- FAT-12/16/32 detection works on a sample image, including read-only
  mode.
- Mutating ops not in the inline-write whitelist return
  `VfsError::Unsupported`.

The Production write-path audit:

- The FAT-only inline write opcode is `VFS_OP_WRITE_INLINE = 28` with
  exact 1–96 payload bytes.
- The block write primitive is `BLK_OP_WRITE = 0x0203`.
- Generic VFS-level shared opcodes 26/27 remain unsupported.

---

## 12. Authoring rule

Future FS / storage changes update **this file**. Per-syscall ABI updates
go in `doc/SYSCALL_ABI.md`; IPC framing updates go in `doc/IPC.md`. Do
**not** create new per-server fragment files (no `RAMFS_*.md`,
`EXT4_*.md`, etc.).
