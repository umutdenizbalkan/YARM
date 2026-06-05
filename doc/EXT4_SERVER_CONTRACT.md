// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

# ext4 userspace server contract

## Server binary

`ext4_srv` follows the existing freestanding filesystem-server pattern: `no_std`/`no_main`, the
shared freestanding allocator installer, local panic handler, `yarm_user_entry`, `_start`, resident
loop, and the existing `EXT4_SRV_ENTRY`, `EXT4_BIN_BEFORE_RUN`, `EXT4_MOUNT_READY`, and
`EXT4_MOUNT_FAILED`-style markers. Runtime spawning remains deferred. This work does not alter the
init/PM/VFS service spawn order or the kernel syscall ABI.

## Read-only support matrix

The image reader supports a deliberately bounded read-only profile:

- superblock parsing at byte offset 1024 and ext4 magic validation (`0xef53`);
- 1 KiB through 64 KiB block-size calculation, including `s_first_data_block` validation;
- checked block-group counting from `(s_blocks_count - s_first_data_block)` and inode-capacity
  validation;
- checked descriptor-table arithmetic and sane 32-byte/64bit descriptor sizes;
- 64bit inode-table high fields and complete inode-table span validation;
- flex_bg inode-table placement through absolute descriptor fields, without bitmap-locality
  assumptions;
- power-of-two inode sizes from 128 bytes through one filesystem block;
- depth-0 and bounded indexed extent trees, including external extent blocks;
- initialized extent reads, unwritten-extent zero fill, sparse holes, and overlap/range rejection;
- legacy direct, singly indirect, and doubly indirect block maps with sparse-hole zero fill;
- ordinary directory parsing with block-local, aligned `rec_len` and bounded `name_len` checks;
- indexed-directory lookup through dx roots and up to two `dx_node` levels;
- signed/unsigned legacy-hash routing, collision-adjacent candidate scans, and exact final name
  verification;
- validated exhaustive leaf fallback for half-MD4, TEA, SipHash, and unknown hash versions;
- root-relative nested path lookup, inline/external symlink reads, and bounded symlink traversal.

All reads use checked integer, block, image, inode-table, extent, indirect-pointer, htree logical
block, and directory-record arithmetic. ext4 remains strictly read-only.

## FS-6 fixture strategy and compatibility coverage

Default tests use compact Rust-generated images; they do not require `mkfs.ext4`, host loop devices,
or large binary blobs. The FS-6 mkfs-style fixture uses a 4 KiB block size, two block groups,
256-byte inodes, 64-byte descriptors, `64bit`, `flex_bg`, `extents`, `filetype`, `dir_index`, and
common read-only-compatible flags. Its second inode table is placed through an absolute flex_bg
descriptor field in the first physical group.

The fixture exercises:

- root directory listing and nested path resolution;
- a multi-leaf indexed directory with two indirect `dx_node` levels, lookup hit, and lookup miss;
- an inode from the second inode group;
- a depth-1 external extent block;
- sparse initialized extents and unwritten extents;
- a sparse doubly-indirect file;
- an external-block symlink and path resolution through that symlink;
- malformed first-data-block, inode-size, flex_bg inode-table, extent-overlap, and cross-block
  directory-record rejection;
- stable `metadata_csum`, `bigalloc`, and `inline_data` feature rejection.

A one-off development probe also confirmed that the parser mounts and reads a file from a real
`mke2fs` image created with the otherwise common ext4 defaults but with `metadata_csum` and
`orphan_file` disabled. External tools are intentionally not part of the default test suite.

## Feature and metadata-checksum policy

The accepted feature set includes `filetype`, `extents`, `64bit`, `flex_bg`, `dir_index`,
`sparse_super`, `large_file`, `huge_file`, `dir_nlink`, and `extra_isize`. Unknown incompatible
features and unsupported read-affecting read-only-compatible features are rejected.

A small heap-free/no-`std` CRC32C Castagnoli helper remains covered by empty-input, `123456789`, and
incremental-update vectors. It is checksum groundwork only. **No `metadata_csum` image is partially
validated or accepted.** Mount rejects `metadata_csum` immediately with
`UnsupportedFeature(metadata_csum)`, regardless of checksum-field contents. Full acceptance remains
blocked until checksums are validated together for every metadata type consumed by reads:

- primary superblock;
- group descriptors;
- inodes;
- directory leaves and dx nodes;
- external extent blocks.

This stable rejection avoids presenting a bad checksum as a different policy result and avoids any
claim that validating only one metadata structure makes the image safe. `metadata_csum_seed` also
remains unsupported.

## Unsupported and deferred features

The reader still does not implement:

- native half-MD4, TEA, or SipHash htree hash calculation (validated exhaustive fallback is used);
- htree depths greater than two `dx_node` levels;
- triple-indirect legacy block maps;
- `metadata_csum`, `bigalloc`, `inline_data`, encryption, casefolding, verity, compression-style
  profiles, `meta_bg`, or other unknown required features;
- journal replay or JBD2 transactions;
- block/inode allocation, directory mutation, create, unlink, rename, truncate, or any ext4 write.

## Write and journaling safety

General writable ext4 is **not enabled**. The existing `Ext4Backend` service is a hosted/demo VFS
backend used by service-contract tests; it is not a crash-safe image writer and is not wired to
mutate ext4 media. Journal presence does not enable replay or writes. A future block-backed mount
must remain read-only until JBD2 replay/transaction handling and a complete metadata mutation design
exist.

## Focused test coverage

The ext4 suite covers the mkfs-style profile above plus the smaller parser fixtures for:

- superblock, feature-mask, block-size, descriptor, inode-table, and 64bit high-field handling;
- depth-0/depth-1 extents, sparse holes, unwritten extents, overlap rejection, and bad pointers;
- direct, singly indirect, doubly indirect, sparse indirect, and triple-indirect rejection paths;
- linear and indexed directory lookup, up to two dx-node levels, malformed counts, and bad leaves;
- inline/external symlinks, nested path resolution, and symlink-loop bounds;
- CRC32C helper vectors and conservative metadata-checksum rejection;
- freestanding `ext4_srv` build/check behavior.
