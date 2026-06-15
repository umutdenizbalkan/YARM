// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

# FAT Filesystem Server Contract

`yarm-fs-servers` includes a FAT server exported as `run_fat()` and built by
`crates/yarm-fs-servers/src/bin/fat_srv.rs`. The parser and hosted memory-image backend now support
bounded regular-file writes for memory-backed images. FS-20 adds a FAT-only exact-inline service
route and a whole-sector FS-12 write client, but production IPC-backed FAT mutation remains blocked
by the lack of a usable whole-sector read/RMW contract.

## Supported formats

- FAT12, FAT16, and FAT32 are detected from the validated BIOS Parameter Block (BPB)
  cluster count.
- Sector sizes of 512, 1024, 2048, and 4096 bytes are accepted by the parser; the
  runtime IPC block backend currently issues 512-byte-aligned reads as required by
  the existing block ABI.
- FAT12 packed 12-bit entries, FAT16 entries, and FAT32 low-28-bit entries are
  decoded. End-of-chain, free, bad, reserved/out-of-range, and looping cluster
  chains are handled explicitly.

## VFS behavior

The server implements `openat`, `read`, `write`, `close`, and `statx` through the
existing common filesystem service wrapper. Directory traversal is available in the
FAT core for path lookup and hosted tests; there is currently no separate VFS
`readdir` opcode in the shared request contract.

Current write support is intentionally narrow and regular-file-only:

- overwrites within an existing cluster chain;
- appends/growth that allocate free clusters, zero freshly allocated clusters, and link the FAT chain;
- FAT32 FSInfo free-count/next-free maintenance when a valid FSInfo sector is present;
- directory-entry file-size updates after successful growth;
- best-effort rollback of newly allocated clusters when cluster allocation fails before linkage.

The legacy `VFS_OP_WRITE = 13` request remains length-only and retains its historical zero-fill
behavior. The FAT service additionally handles `VFS_OP_WRITE_INLINE = 28` with an exact payload of
1–96 bytes, current-open-file-offset semantics, and a typed completion reply. This narrow route is
FAT-specific: the generic `VfsService` still rejects opcode 28, and shared opcodes 26/27 remain
unsupported. Filesystem-internal callers may also use `write_path`/`write_bytes`.
Unsupported mutating operations such as `mkdir` and `unlink` are still rejected with
`VfsError::Unsupported`.

## Names and directories

- Short 8.3 names are supported case-insensitively.
- VFAT long file name entries are supported for read-only lookup when the checksum
  matches the following short entry. UTF-16 code units are converted to Rust
  `char`s when possible and to U+FFFD for invalid/control values.
- FAT12/FAT16 fixed root directories and FAT32 root directory cluster chains are
  supported. Deleted entries are ignored, `0x00` terminates a directory, and volume
  labels are not exposed as files.

## Production write-path audit

FS-20 connects the two bounded userspace pieces already frozen by earlier stages:

- **Exact VFS payload:** the FAT service, and only the FAT service, decodes
  `VFS_OP_WRITE_INLINE = 28` and passes the 1–96 payload bytes to `FatBackend::write_bytes` at the
  open file description's current offset. Explicit-offset writes are not enabled. The route is
  currently gated to `MemoryImage`; IPC-backed FAT returns `VfsError::Unsupported` before mutation.
- **Sector-write client:** `IpcBlockDevice::write_exact_at` accepts only aligned whole 512-byte
  sectors and sends each sector as the FS-12 ordered `BLK_OP_WRITE = 0x0203` chunk transaction.
  Every chunk reply is checked for request id, LBA, accepted length, status, and final commit marker.
- **Write ordering:** writes are synchronous and write-through. FAT allocation, FAT-chain, file-data,
  directory-entry, and optional FSInfo updates are issued in the existing core order; there is no
  journal or atomic multi-sector transaction.
- **Compatibility:** legacy length-only `VFS_OP_WRITE = 13` is unchanged. The shared-memory umbrella
  remains disabled, and `READ_SHARED_REPLY`/`WRITE_SHARED_REQUEST` opcodes 26/27 remain unsupported.

The practical supported profile is therefore memory-backed overwrite or append/growth of an
already existing regular file, with at most 96 exact bytes per VFS inline request. Larger writes
require caller-side request fragmentation today; no shared mapping is implied.

A production IPC FAT mount cannot safely use this route yet. FAT metadata and short file writes are
sub-sector updates and require read-modify-write. The current `BlkReadRequest` requires a
sector-multiple `byte_len`, while the 128-byte inline `BlkReadReply` can carry only 120 data bytes;
there is no read chunk offset/assembler corresponding to FS-12 writes. Consequently no valid
512-byte sector read can be requested through this ABI, and inventing a partial read contract here
would exceed FS-20's scope. The whole-sector write client is retained and tested, but FAT IPC inline
writes remain explicitly rejected until the read-side sector transfer contract is repaired.

## Production backend selection

The FAT core is backend-agnostic through a small `BlockDevice` trait. Hosted tests
use an in-memory block image. In production, `run_fat()` reads the userspace startup
context and expects:

- `service_extra_cap_0` to contain the filesystem-facing blkcache/block service send
  capability. This is the only block service cap source currently supported by the
  FAT config (`ServiceExtraCap0`).
- `process_manager_reply_recv_cap` to contain the reply receive endpoint used for
  synchronous block IPC replies.
- startup slot 14 (`service_extra_cap_1` raw value) to contain up to eight bytes of
  mount prefix, little-endian byte packed. Examples supported by this compact
  userspace-only format are `/fat` and `/mnt/fat`.
- startup slot 15 (`initrd_ptr` raw value for this service) to contain FAT mount
  metadata: low 32 bits = block device id, bits 32..47 = flags, bits 48..55 =
  prefix length, bits 56..63 = block cap source (`1` = `service_extra_cap_0`).
  Flag bit 0 means read-only and is set by default.

When config words are present, the service logs
`FAT_CONFIG_FOUND prefix=... device_id=...`, uses the configured prefix and block
device id, logs `FAT_BLOCK_BACKEND_STARTUP_CAP cap=...`, constructs an IPC block
backend, and logs `FAT_MOUNT_READY prefix=... device_id=...` after the read-only
mount smoke succeeds. If IPC probing or BPB parsing fails, the service logs
`FAT_MOUNT_FAILED reason=...`.

When production has caps but no config words, the temporary compatibility fallback
uses device id `1` and prefix `/fat`, logs
`FAT_CONFIG_DEFAULT_DEVICE_ID device_id=1 reason=missing-config`, and still requires
real block IPC to mount.

When either cap is missing in the production/no-default-features path, the service
logs `FAT_NO_BLOCK_BACKEND` and `FAT_MOUNT_FAILED reason=no-block-backend`. It does
not silently mount the sample image and does not fake filesystem availability.

Hosted-dev and unit tests may explicitly select the sample image path. That path logs
`FAT_BLOCK_BACKEND_SAMPLE_IMAGE reason=no-startup-block-cap-hosted-dev` and remains
for synthetic image tests and local development only.

## Init/VFS wiring

`init_server` now has userspace-only wiring to spawn `fat_srv` (image id 10) once a
blkcache send cap is available. It passes the blkcache send cap in
`service_extra_cap_0`, the packed FAT prefix word in startup slot 14, and FAT mount
metadata in startup slot 15. If spawning succeeds, init sends an existing
`VFS_OP_MOUNT_REGISTER` request to `vfs_server` for the configured FAT prefix and
logs `PM_IMAGE_ID_10_FAT_SRV` when PM resolves image id 10 and
`VFS_MOUNT_REGISTER_FAT_OK prefix=...` when VFS accepts the route. This does not
change kernel ABI or SpawnV5 semantics; it only uses existing userspace startup
words and existing VFS mount registration.

Smoke scripts treat FAT markers as optional unless `FAT_SMOKE_EXPECTED=1` is set,
because the current core smoke profiles may run without a real FAT block image.
When enabled, the smoke marker block counts `INIT_FAT_SPAWN_BEGIN`,
`INIT_FAT_SPAWN_OK`, `FAT_CONFIG_FOUND`, `FAT_BLOCK_BACKEND_STARTUP_CAP`,
`FAT_MOUNT_READY`, `FAT_MOUNT_FAILED`, and `VFS_MOUNT_REGISTER_FAT_OK`.

## Stage 93: Production readiness checklist

Stage 93 added FAT block-device profile groundwork. The following items are **done**:

- `IpcBlockDevice::read_exact_at` and `write_sector` now use `ipc_recv_v2` (blocking)
  instead of `ipc_recv_with_deadline(_, 0)` — same deadline=0 race fixed in Stage 92
  for `vfs_client.rs`. Without this fix, block reads would fail immediately on any
  scheduler where blkcache_srv has not yet replied within 0 ticks.
- `scripts/create-fat-image.sh` creates a 1 MiB FAT image with `hello.txt` and
  `dir/nested.txt` using `mtools` (rootless) or `mkfs.fat + mount` (fallback).
- `scripts/qemu-aarch64-fat-block-smoke.sh` and `scripts/qemu-x86_64-fat-block-smoke.sh`:
  attach `virtio-blk-pci` device backed by the FAT image; check all FAT markers.
- Official FS profile matrix documented in `VFS_SHARED_IO_CONTRACT.md`.

The following items are **blocked** pending:

1. **INIT_SPAWN_FAT_SRV must be changed to `true`** in the fat-block profile.
   Currently `false` to prevent spurious spawns in the optional-fs profile.
   Change only via a compile-time feature or config, not in the default binary.

2. **Blkcache whole-sector read round-trip contract** must be proven on QEMU:
   `IpcBlockDevice::read_exact_at` issues `BLK_OP_READ` in 120-byte chunks; sector
   assembly logic in blkcache_srv must correctly re-assemble multi-chunk replies.

3. **virtio_blk_srv device-presence handshake**: `virtio_blk_srv` currently has no
   probe/ready handshake. `fat_srv` must not attempt `BLK_OP_READ` before `virtio_blk_srv`
   has signaled device readiness.

4. **FAT shared-I/O** (`VFS_FAT_SHARED_IO_ENABLED`): remains `false` until the normal
   IPC read path is proven on real block hardware with at least one full file round-trip.

5. **FAT writes**: production IPC-backed writes remain unsupported until the
   whole-sector read/RMW contract is established and tested.

### Exact activation sequence for fat-block profile

```
INIT_SPAWN_FAT_SRV = true        (fat-block profile compile flag)
VFS_FAT_LIVE_MOUNT_ENABLED = true (follows INIT_SPAWN_FAT_SRV)
VFS_FAT_SHARED_IO_ENABLED = false (prove read path first)

QEMU args: -drive file=fat.img,if=none,id=blk0,format=raw
           -device virtio-blk-pci,drive=blk0

Expected markers (FAT_SMOKE_EXPECTED=1):
  INIT_FAT_SPAWN_BEGIN
  INIT_FAT_SPAWN_OK
  FAT_BIN_ENTRY_START
  FAT_CONFIG_FOUND
  FAT_BLOCK_BACKEND_STARTUP_CAP
  FAT_MOUNT_READY
  FAT_SRV_READY
  VFS_MOUNT_REGISTER_FAT_OK prefix=/fat

Forbidden:
  INIT_FAT_SPAWN_FAIL
  FAT_MOUNT_FAILED
  PM_ELF_ZC_FAIL image_id=10
  KSPAWN_EXTRA_CAP_DELEGATE_FAIL
  PM_VFS_SPAWN_FAIL
```

## Known limitations

- The VFS reply ABI currently returns only the historical scalar `statx` value, so
  file type metadata is exposed by the FAT core but not serialized in a richer stat
  structure.
- The compact startup config supports prefixes up to eight bytes. Longer mount
  prefixes need a future userspace config transport, not a kernel ABI change.
- The existing blkcache/block stack still exposes truthful stub behavior in some
  driver paths; FAT mount fails clearly when the backend cannot return real sector
  data.
- Exact inline writes are limited to 96 bytes per request and memory-backed images. IPC-backed FAT
  mutation remains blocked by the contradictory inline sector-read limits described above.
- There is no multi-request atomicity, crash-safe ordering, or shared-buffer large-write path.
- Truncation/shrinking, create, mkdir, rename, and unlink remain unsupported.
- FAT32 allocation still scans the FAT for a free cluster; FSInfo is maintained opportunistically
  when present but is not trusted as the allocator source of truth.
- FAT writes are not journaled and are not crash-safe across power loss or mid-write failure.

## Milestone 1 closure (Stage 94/100)

FAT reached **Optional FS Milestone 1** as "profile-ready, disabled" status.
All scripts, tests, and checklist items are complete. `INIT_SPAWN_FAT_SRV` remains
`false`. See `doc/PROJECT_HISTORY.md` for the exact activation blockers.

Filesystem work is paused after Milestone 1. The FAT production activation sequence
(§ Stage 93 checklist) remains valid and will be executed in a future FS milestone
once the virtio-blk round-trip is proven.
