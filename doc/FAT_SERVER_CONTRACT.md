// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

# FAT Filesystem Server Contract

`yarm-fs-servers` includes a read-only FAT server exported as `run_fat()` and built by
`crates/yarm-fs-servers/src/bin/fat_srv.rs`.

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

The server implements read-only `openat`, `read`, `close`, and `statx` through the
existing common filesystem service wrapper. Directory traversal is available in the
FAT core for path lookup and hosted tests; there is currently no separate VFS
`readdir` opcode in the shared request contract.

Unsupported mutating operations such as write, mkdir, and unlink are rejected with
`VfsError::Unsupported`. The server must not fake successful writes.

## Names and directories

- Short 8.3 names are supported case-insensitively.
- VFAT long file name entries are supported for read-only lookup when the checksum
  matches the following short entry. UTF-16 code units are converted to Rust
  `char`s when possible and to U+FFFD for invalid/control values.
- FAT12/FAT16 fixed root directories and FAT32 root directory cluster chains are
  supported. Deleted entries are ignored, `0x00` terminates a directory, and volume
  labels are not exposed as files.

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

## Known limitations

- The VFS reply ABI currently returns only the historical scalar `statx` value, so
  file type metadata is exposed by the FAT core but not serialized in a richer stat
  structure.
- The compact startup config supports prefixes up to eight bytes. Longer mount
  prefixes need a future userspace config transport, not a kernel ABI change.
- The existing blkcache/block stack still exposes truthful stub behavior in some
  driver paths; FAT mount fails clearly when the backend cannot return real sector
  data.
- FAT writes, allocation, truncation, mkdir, rename, and unlink are intentionally
  unsupported and return `VfsError::Unsupported` where the current VFS/backend
  surface exposes them.
