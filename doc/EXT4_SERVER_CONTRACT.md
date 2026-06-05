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

## Image parser/read-only support matrix

The ext4 image reader supports a deliberately small read-only profile suitable for unit tests and
future block-backed integration:

- superblock parsing at byte offset 1024 and ext4 magic validation (`0xef53`);
- checked block size calculation from `s_log_block_size`;
- group descriptor table bounds validation from computed group count;
- 64bit descriptor sizes when the descriptor size is sane, including high inode-table block fields;
- flex_bg-compatible inode-table lookup by absolute descriptor fields (no bitmap-dependent layout assumptions);
- inode lookup with checked inode-table offsets and inode-size validation;
- extent-header validation for depth-0 leaves and bounded depth-1+ extent-index traversal
  (`EXT4_MAX_EXTENT_DEPTH` guard);
- regular-file reads through initialized extents, with sparse holes left as zero-filled bytes;
- legacy non-extent regular-file reads through direct and singly indirect block maps;
- zero-filled holes for missing extent or legacy block pointers;
- htree/indexed-directory awareness with validated dx root entries and indexed leaf-block scanning;
- linear directory entry parsing with ext4 file-type bytes;
- root-relative path lookup with bounded final/intermediate symlink resolution;
- inline and external-block symlink target reads.

## Rejected/unsupported ext4 features

The parser rejects unknown incompatible feature bits and returns an explicit
`UnsupportedFeature(mask)` error. The current read core does not implement:

- double- and triple-indirect legacy block maps;
- htree hash-version-specific hashing/collision acceleration beyond validated indexed leaf scanning;
- htree indirect levels (`dx_node`) above the root leaf list;
- journal replay or JBD2 transactions;
- metadata checksum validation; `metadata_csum` and `bigalloc` are rejected at mount;
- encrypted, casefolded, inline-data, verity, or compression-style profiles;
- block allocation, inode allocation, directory creation, unlink, rename, or truncation.

## Feature flag and checksum policy

The parser accepts the small feature set needed by the current read-only tests: `filetype`,
`extents`, `64bit`, `flex_bg`, and common read-only-compatible flags such as `sparse_super`,
`large_file`, `huge_file`, `dir_nlink`, and `extra_isize`. Unknown incompatible features are
rejected. Read-affecting read-only-compatible features outside the supported mask are rejected.
`metadata_csum` is rejected rather than silently ignored because CRC32C checksum coverage for
superblocks, group descriptors, inodes, directories, and extent blocks is not yet complete.

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
- 64bit descriptor sizing and high inode-table field rejection when out of image;
- descriptor table bounds rejection;
- root directory parsing;
- path lookup;
- depth-0 and depth-1 extent-backed regular-file reads;
- direct and singly indirect legacy block-map reads;
- sparse extent and legacy block-map hole zero-fill behavior;
- invalid extent depth and invalid extent/block pointer rejection;
- metadata checksum and bigalloc rejection;
- htree indexed-directory leaf scanning and malformed dx root/leaf pointer rejection;
- inline and external symlink reads plus bounded symlink path resolution;
- existing service write/stat smoke behavior;
- ext4 server binary build/check.
