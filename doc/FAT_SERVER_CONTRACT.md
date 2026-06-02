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

## Block backend assumptions

The FAT core is backend-agnostic through a small `BlockDevice` trait. Hosted tests
use an in-memory block image. Freestanding/server code has an `IpcBlockDevice`
implementation for the existing inline block ABI, but production mounting still
requires startup wiring that provides the block service send capability, reply
receive capability, and device id to the FAT server.

## Known limitations

- The VFS reply ABI currently returns only the historical scalar `statx` value, so
  file type metadata is exposed by the FAT core but not serialized in a richer stat
  structure.
- The shipped `run_fat()` bootstrap mounts a built-in sample image until process
  manager/VFS mount plumbing provides a real block device capability.
- FAT writes, allocation, truncation, mkdir, rename, and unlink are intentionally
  unsupported.
