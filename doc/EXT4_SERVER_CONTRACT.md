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
- indexed-directory lookup and enumeration through dx roots and up to two `dx_node` levels;
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
- a fully checksummed `metadata_csum` variant plus `bigalloc` and `inline_data` rejection.

Development-only probes confirmed that the parser mounts and reads files from real `mke2fs`
images both with the common UUID-seeded `metadata_csum` profile and with `metadata_csum` disabled.
External tools are intentionally not part of the default test suite.

## Feature and metadata-checksum policy

The accepted feature set includes `filetype`, `extents`, `64bit`, `flex_bg`, `dir_index`,
`sparse_super`, `large_file`, `huge_file`, `dir_nlink`, `extra_isize`, UUID-seeded
`metadata_csum`, and `metadata_csum_seed`. Unknown incompatible features and unsupported read-affecting
read-only-compatible features are rejected.

The heap-free/no-`std` CRC32C Castagnoli implementation uses ext4's uncomplemented running CRC
state. Without `metadata_csum_seed`, the checksum seed is `crc32c(~0, filesystem_uuid)`. When
`INCOMPAT_CSUM_SEED` is present together with `metadata_csum`, the reader instead loads the
little-endian `s_checksum_seed` field at superblock offset `0x270`; this value is the checksum state
that ext4 preserved from the original UUID. `metadata_csum_seed` without `metadata_csum` is rejected
as an unsupported feature combination. The primary superblock checksum itself always starts from
`~0` and is not seeded by `s_checksum_seed`. Multi-byte inode numbers, inode generations, and
block-group numbers enter metadata CRCs as their little-endian on-disk byte representation. The
standard complemented empty-input/`123456789` vectors and
incremental update equivalence remain covered by tests.

When `metadata_csum` is present, the reader validates every checksummed metadata structure that it
trusts before parsing it:

- **Primary superblock:** CRC32C from `~0` over bytes before `s_checksum`; the UUID is already inside
  the superblock. Only checksum type `1` (CRC32C) is accepted.
- **Group descriptors:** selected metadata seed, little-endian group number, and the complete descriptor
  with `bg_checksum` treated as zero; the stored lower 16 bits are checked. Every primary-table
  descriptor is validated during mount, including 64-byte descriptors.
- **Inodes:** selected metadata seed, little-endian inode number, inode generation, and the complete inode
  with low/high checksum fields treated as zero. 128-byte inodes use the low 16 bits; sufficiently
  large inodes with `i_extra_isize >= 4` validate all 32 bits.
- **Linear directory leaves:** selected metadata seed, owning directory inode number/generation, and the
  block bytes before the required 12-byte `ext4_dir_entry_tail`.
- **Htree dx roots/nodes:** the same owning-inode prefix, the valid header/entry region, and the
  required zeroed 8-byte dx tail. Validation occurs before routing through each root or node.
- **Htree directory leaves:** the linear directory-tail formula above, before exact-name matching.
- **External extent blocks:** selected metadata seed, owning inode number/generation, and bytes through
  the extent tail position derived from `eh_max`; inode-resident extent roots rely on the inode
  checksum and do not have a separate extent-tail checksum.

Any mismatch returns `ChecksumMismatch`. Malformed or absent checksum tails return `Malformed`.
Checksum validation is performed at the read point, not merely at mount, so later inode, directory,
dx, and external extent reads cannot introduce unchecked metadata.

### Accepted and rejected metadata_csum profiles

`metadata_csum` mounts are accepted for extent-backed regular files, directories, and external
symlinks using the metadata forms listed above. Both UUID-derived and stored-seed profiles are
accepted. Indexed directories support lookup and enumeration through the supported two dx-node
levels on checksummed and non-checksummed images. Enumeration validates the root, every traversed
node, and every leaf before parsing; includes the root `.` and `..` entries; visits each logical leaf
once; and removes repeated `(inode, name)` entries when a leaf is reachable through duplicate dx
paths.

Legacy direct/singly/doubly-indirect files remain available on non-`metadata_csum` images, but are
rejected with `UnsupportedLayout` when encountered on an accepted `metadata_csum` mount: ext4 does
not define metadata checksums for those legacy pointer blocks, so the parser cannot satisfy its
"validate every trusted metadata block" policy. Bitmaps, journal blocks, backup superblocks, and
extended-attribute blocks are not validated because the read-only parser does not consume them.

## Unsupported and deferred features

The reader still does not implement:

- native half-MD4, TEA, or SipHash htree hash calculation (validated exhaustive fallback is used);
- htree depths greater than two `dx_node` levels;
- triple-indirect legacy block maps;
- metadata-checksummed legacy indirect pointer blocks, `bigalloc`, `inline_data`, encryption,
  casefolding, verity, compression-style profiles, `meta_bg`, or other unknown required features;
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
- linear directory enumeration plus indexed lookup/enumeration through two dx-node levels,
  deterministic de-duplication, malformed counts, invalid leaves, and checksum corruption;
- inline/external symlinks, nested path resolution, and symlink-loop bounds;
- CRC32C helper vectors, UUID-derived and stored checksum-seed acceptance, stored-seed mismatch,
  seed-without-`metadata_csum` rejection, unsupported checksum type rejection, and corruption of
  superblocks, descriptors, inodes, directory leaves, dx roots/nodes, and external extent blocks;
- freestanding `ext4_srv` build/check behavior.
