// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

# ext4 Filesystem Server Contract

`yarm-fs-servers` exports `run_ext4()` and builds an `ext4_srv` binary at
`crates/yarm-fs-servers/src/bin/ext4_srv.rs`.

## Server binary/runtime status

`ext4_srv` follows the same freestanding userspace pattern as the FAT/RAMFS servers:

- `#![no_std]`/`#![no_main]` outside `hosted-dev`;
- `yarm_server_runtime::install_freestanding_allocator!(1024 * 1024, ...)`;
- `yarm_user_entry` routed through `yarm_server_runtime::user_rt::runtime::enter_user_entrypoint`;
- a local `#[panic_handler]` for freestanding builds;
- resident receive/yield loops after `run_ext4()` returns.

The binary emits `EXT4_SRV_ENTRY`, `EXT4_BIN_BEFORE_RUN`, `EXT4_MOUNT_READY`, and
`EXT4_MOUNT_FAILED`-style markers. Runtime spawning remains deferred; this change does not alter
init/PM/VFS service spawn order or kernel syscall ABI.

## Image parser/read-only core

The ext4 image reader supports a deliberately small read-only profile suitable for unit tests and
future block-backed integration:

- superblock parsing at byte offset 1024;
- ext4 magic validation (`0xef53`);
- block size calculation from `s_log_block_size`;
- group descriptor table lookup, including 64-bit inode-table high bits when enabled;
- inode lookup by inode number;
- extent-header validation for depth-0 extent trees;
- regular-file reads through initialized extents;
- linear directory entry parsing with ext4 file-type bytes;
- root-relative path lookup.

## Rejected/unsupported ext4 features

The parser rejects unknown incompatible feature bits and returns an explicit
`UnsupportedFeature(mask)` error. The current read core does not implement:

- extent index/internal nodes (extent depth > 0);
- legacy indirect block maps;
- htree indexed-directory acceleration (linear entries are parsed when present);
- journal replay or JBD2 transactions;
- checksummed metadata verification;
- encrypted, casefolded, inline-data, bigalloc, verity, or compression-style profiles;
- block allocation, inode allocation, directory creation, unlink, rename, or truncation.

## Write and journaling safety

General writable ext4 is **not enabled**. The existing `Ext4Backend` service remains a hosted/demo
VFS backend that tracks demo path lengths and a synthetic journal counter for service-contract tests;
it is not a crash-safe ext4 writer and is not wired to mutate ext4 images.

If journaling/replay is detected on a real image, future block-backed mounting must remain read-only
until JBD2 replay/transaction support exists or an explicit non-journaled test profile is selected.
Do not claim ext4 metadata writes are crash-safe until that work lands.

## Tests

Focused tests cover:

- superblock parsing and block-size calculation;
- required incompatible feature rejection;
- root directory parsing;
- path lookup;
- extent-backed regular-file read;
- existing service write/stat smoke behavior;
- ext4 server binary build/check.
