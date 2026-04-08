<!-- SPDX-License-Identifier: Apache-2.0 -->

# Initramfs Executable Manifest Contract (Phase 2)

This contract defines the binary manifest used by the freestanding launch path to discover
core service images in initramfs.

## Scope

- Core services only (v1):
  - `init.srv`
  - `process_manager.srv`
  - `vfs.srv`
  - `supervisor.srv`
- Parser: `src/services/fs/initramfs/manifest.rs`

## Wire format (little-endian)

### Header (8 bytes)

- `magic: u32` = `0x5941_524D` (`"YARM"`)
- `version: u16` = `1`
- `entry_count: u16` = `4` (exactly)

### Entry (28 bytes, repeated `entry_count` times)

- `path_ptr: u64` (stable initramfs path identity)
- `file_len: u64` (must be non-zero)
- `entry_addr: u64` (must be non-zero)
- `abi: u16`
- `flags: u16`

## Required path identities

- `INITRAMFS_INIT_PATH_PTR`
- `INITRAMFS_PROC_MGR_PATH_PTR`
- `INITRAMFS_VFS_PATH_PTR`
- `INITRAMFS_SUPERVISOR_PATH_PTR`

All four must appear exactly once in the manifest.

## Validation and failure policy

`parse_core_service_manifest(...)` rejects the manifest when:

- header is truncated,
- magic/version mismatch,
- entry count is not exactly four,
- payload is truncated,
- an entry has `file_len == 0`,
- an entry has `entry_addr == 0`,
- required paths are duplicated or missing.

## Test vectors

Phase 2 includes deterministic tests for:

- valid manifest decode with all required entries,
- duplicate/missing path rejection,
- corrupt zero `entry_addr` / zero `file_len` rejection.
