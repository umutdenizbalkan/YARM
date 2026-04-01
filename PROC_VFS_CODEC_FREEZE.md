<!-- SPDX-License-Identifier: Apache-2.0 -->

# Process/VFS Codec Freeze (v1)

This document freezes the typed wire payload contracts used by the process-manager and VFS service paths.

## Process manager (`src/kernel/process_abi.rs`)

- Server ABI version: `PROC_SERVER_ABI_VERSION = 1`
- Typed codec version: `PROC_CODEC_V2_VERSION = 2`
- Typed request args: `ProcV2Args`
  - Encoding: little-endian `[arg0:u64, arg1:u64]`
  - Exact payload length: `16` bytes
  - Decode policy: **exact-length only** (reject truncated or oversized payloads)
  - Golden vector (stable test fixture):
    - args = `(0x1122334455667788, 0x99aabbccddeeff00)`
    - bytes = `88 77 66 55 44 33 22 11 00 ff ee dd cc bb aa 99`

Opcodes frozen in this phase:

- `PROC_OP_GETPID = 1`
- `PROC_OP_EXIT = 2`
- `PROC_OP_GETPPID = 3`
- `PROC_OP_SPAWN_V2 = 4`
- `PROC_OP_WAITPID_V2 = 5`

## VFS (`src/kernel/vfs_abi.rs`)

- Server ABI version: `VFS_SERVER_ABI_VERSION = 1`
- Typed codec version: `VFS_CODEC_V1_VERSION = 1`
- Typed request args: `VfsV1Args`
  - Encoding: little-endian `[arg0:u64, arg1:u64, arg2:u64, arg3:u64]`
  - Exact payload length: `32` bytes
  - Decode policy: **exact-length only** (reject truncated or oversized payloads)
  - Golden vector (stable test fixture):
    - args = `(0x0102030405060708, 0x1112131415161718, 0x2122232425262728, 0x3132333435363738)`
    - bytes = `08 07 06 05 04 03 02 01 18 17 16 15 14 13 12 11 28 27 26 25 24 23 22 21 38 37 36 35 34 33 32 31`

Commonly used opcodes frozen in this phase:

- `VFS_OP_OPENAT = 10`
- `VFS_OP_CLOSE = 11`
- `VFS_OP_READ = 12`
- `VFS_OP_WRITE = 13`
- `VFS_OP_IOCTL = 14`

## Compatibility policy

Any version or payload-width change must:

1. Introduce a new codec version constant and typed struct.
2. Keep old decode paths intact until all call-sites migrate.
3. Add explicit round-trip and malformed-vector tests for both old and new versions.

CI gate:
- `scripts/check-proc-vfs-codec-freeze.sh` enforces version constants and runs golden-vector tests.
