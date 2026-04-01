<!-- SPDX-License-Identifier: Apache-2.0 -->

# DevFS Service Contract (`devfs.srv`)

This document defines the behavior contract for the in-tree `devfs.srv` implementation.

## Supported nodes (v1)

- `/dev/console`
- `/dev/null`

Path identity currently uses frozen pointer IDs:

- `DEV_CONSOLE_PATH_PTR = 0x434F_4E53_4F4C_4500`
- `DEV_NULL_PATH_PTR = 0x4445_564E_554C_4C00`

## FD allocation and open semantics

- Open handles are allocated from a monotonic service-local fd allocator.
- Initial fd is `3`; each successful open consumes one new fd.
- Multiple opens of the same node are allowed and produce distinct fds.
- If the open-handle table is full, open fails with `NoFd`.

## Node-specific I/O semantics

### `/dev/null`
- `read(fd, len)` returns `0` (EOF-like behavior).
- `write(fd, len)` returns `len`.

### `/dev/console`
- `write(fd, len)` returns `len` and increments console-write metrics.
- `read(fd, len)` returns `Unsupported`.

## `statx` contract

`statx(path)` returns a compact metadata value:

- high type bit for character-device class: `0x2000_0000_0000_0000`
- mode bits:
  - console: owner-write (`0o200`)
  - null: owner-read|owner-write (`0o600`)

## Mount failure/recovery policy for in-flight fds

For current `VfsService` semantics:

- `mark_mount_failed` / `recover_mount` mutate mount records.
- Existing in-flight devfs fds remain valid until explicitly closed.
- `unmount` marks mount inactive and does not implicitly close backend fds.

This policy is deterministic and tested.

## Metrics contract

`DevFsMetrics` tracks:

- open / close / read / write / statx counts
- console bytes written
- null bytes written
- error count

The runtime loop summary logs these counters for boot-path observability.

## Extensibility rules

When adding new nodes:

1. Add path identity constant and node-kind mapping.
2. Define read/write/statx behavior explicitly in this contract.
3. Add deterministic tests for open/read/write/close/statx and error paths.
4. Update Phase readiness docs when gate scope changes.
