# Process/VFS Codec Freeze (v1)

This document freezes the typed wire payload contracts used by the process-manager and VFS service paths.

## Process manager (`src/kernel/process_abi.rs`)

- Server ABI version: `PROC_SERVER_ABI_VERSION = 1`
- Typed codec version: `PROC_CODEC_V2_VERSION = 2`
- Typed request args: `ProcV2Args`
  - Encoding: little-endian `[arg0:u64, arg1:u64]`
  - Exact payload length: `16` bytes
  - Decode policy: **exact-length only** (reject truncated or oversized payloads)

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
