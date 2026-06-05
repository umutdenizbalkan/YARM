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
- legacy non-extent regular-file reads through direct, singly indirect, and doubly indirect block maps;
- zero-filled holes for missing extent or legacy block pointers;
- htree/indexed-directory lookup with corrected dx count/limit parsing, signed and unsigned legacy-hash routing, collision-adjacent leaf scanning, and bounded one-level `dx_node` traversal;
- safe exhaustive validated-leaf fallback for half-MD4, TEA, SipHash, and unknown hash versions, so exact-name verification is preserved without claiming unsupported hash compatibility;
- linear directory entry parsing with ext4 file-type bytes;
- root-relative path lookup with bounded final/intermediate symlink resolution;
- inline and external-block symlink target reads.

Read-path arithmetic uses checked offsets and block ranges. Mount and inode lookup validate the
complete inode-table span, descriptor bounds, declared filesystem block count, file-size conversion,
extent physical ranges, indirect pointers, htree logical blocks, and directory `rec_len`/`name_len`
bounds. The synthetic robustness profile combines `64bit`, `flex_bg`, extents, sparse data, indexed
directories, and external symlinks without enabling metadata checksums.

## Rejected/unsupported ext4 features

The parser rejects unknown incompatible feature bits and returns an explicit
`UnsupportedFeature(mask)` error. The current read core does not implement:

- triple-indirect legacy block maps;
- native half-MD4, TEA, and SipHash htree hash calculation (these versions use validated exhaustive leaf fallback);
- htree depths greater than one `dx_node` level;
- journal replay or JBD2 transactions;
- complete metadata checksum validation; `metadata_csum` and `bigalloc` remain rejected at mount;
- encrypted, casefolded, inline-data, verity, or compression-style profiles;
- block allocation, inode allocation, directory creation, unlink, rename, or truncation.

## Feature flag and checksum policy

The parser accepts the small feature set needed by the current read-only tests: `filetype`,
`extents`, `64bit`, `flex_bg`, and common read-only-compatible flags such as `sparse_super`,
`large_file`, `huge_file`, `dir_nlink`, and `extra_isize`. Unknown incompatible features are
rejected. Read-affecting read-only-compatible features outside the supported mask are rejected.
A local no-heap/no-`std` CRC32C Castagnoli implementation is present and tested against the
standard empty and `123456789` vectors, including incremental update equivalence. When
`metadata_csum` is set, the primary superblock checksum type and checksum are validated first;
a mismatch returns `ChecksumMismatch`. The mount is then still rejected with
`UnsupportedFeature(metadata_csum)` even when the superblock checksum is valid.

This conservative policy is intentional: group descriptor, inode, directory leaf, htree node, and
external extent-block checksums are not all validated yet, and those metadata types are trusted by
current reads. Therefore **no metadata_csum image is accepted**, and the implementation does not
claim partial-checksum mounts are safe. `metadata_csum_seed` is likewise not accepted while
`metadata_csum` remains gated.

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
- direct, singly indirect, and doubly indirect legacy block-map reads, including invalid inner-pointer rejection and triple-indirect range rejection;
- sparse extent and legacy block-map hole zero-fill behavior;
- invalid extent depth and invalid extent/block pointer rejection;
- CRC32C vectors, valid superblock checksum validation followed by conservative `metadata_csum` rejection, checksum mismatch rejection, and `bigalloc` rejection;
- htree legacy-hash routing, unsupported-hash exhaustive fallback, one-level `dx_node` traversal, and malformed dx root/node/leaf pointer rejection;
- combined 64bit + flex_bg reads covering sparse extents and external symlinks;
- inline and external symlink reads plus bounded symlink path resolution;
- existing service write/stat smoke behavior;
- ext4 server binary build/check.
