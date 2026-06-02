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
  capability.
- `process_manager_reply_recv_cap` to contain the reply receive endpoint used for
  synchronous block IPC replies.
- device id `1` (`FAT_DEFAULT_BLOCK_DEVICE_ID`) until a userspace mount/config ABI
  exists for per-mount device selection.

When both caps are present, the service logs `FAT_BLOCK_BACKEND_STARTUP_CAP cap=...`,
constructs an IPC block backend, mounts FAT from device id 1, and logs
`FAT_MOUNT_READY` after the read-only mount smoke succeeds. If IPC probing or BPB
parsing fails, the service logs `FAT_MOUNT_FAILED reason=...`.

When either cap is missing in the production/no-default-features path, the service
logs `FAT_NO_BLOCK_BACKEND` and `FAT_MOUNT_FAILED reason=no-block-backend`. It does
not silently mount the sample image and does not fake filesystem availability.

Hosted-dev and unit tests may explicitly select the sample image path. That path logs
`FAT_BLOCK_BACKEND_SAMPLE_IMAGE reason=no-startup-block-cap-hosted-dev` and remains
for synthetic image tests and local development only.

## Known limitations

- The VFS reply ABI currently returns only the historical scalar `statx` value, so
  file type metadata is exposed by the FAT core but not serialized in a richer stat
  structure.
- Current production startup has a fixed device id 1 expectation; a future
  userspace-only mount/config payload should carry the device id and mount prefix
  once that control-plane path exists.
- The existing blkcache/block stack still exposes truthful stub behavior in some
  driver paths; FAT mount fails clearly when the backend cannot return real sector
  data.
- FAT writes, allocation, truncation, mkdir, rename, and unlink are intentionally
  unsupported and return `VfsError::Unsupported` where the current VFS/backend
  surface exposes them.
