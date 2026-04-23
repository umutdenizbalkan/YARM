<!-- SPDX-License-Identifier: Apache-2.0 -->

# RamFS Service Contract (`ramfs.srv`)

This document defines behavior for the in-tree `ramfs.srv` implementation.

## Scope

- In-memory writable filesystem backend with deterministic inode/fd allocation.
- Open-by-path allocates inodes lazily for previously unseen path IDs.

## FD and inode allocation semantics

- `openat(path_ptr)` allocates/looks up an inode for `path_ptr`.
- FDs are monotonic and start at `100`.
- Multiple opens for the same path are allowed and return unique fds.
- Capacity exhaustion returns `NoFd`.

## I/O semantics

- `write(fd, len)` grows inode file length by `len` and returns `len`.
- `read(fd, len)` returns `min(len, inode_file_len)`.
- `close(fd)` invalidates the handle.

## `statx` contract

`statx(path)` returns a compact metadata value:

- regular-file marker bit: `0x1000_0000_0000_0000`
- mode bits: owner-read + owner-write (`0o600`)
- encoded file length payload (`file_len << 16`)

## Mount failure/recovery policy for in-flight fds

For current `VfsService` semantics:

- mount failure/recovery transitions mutate mount records.
- existing in-flight ramfs fds remain usable until explicit close.
- unmount deactivates mount record and does not implicitly close backend fds.

## Metrics contract

`RamFsMetrics` tracks:

- open / close / read / write / statx counts
- cumulative bytes written/read
- error count

Runtime loop summary logs include these counters.

## Extensibility rules

1. Keep statx encoding stable unless contract version is bumped.
2. Add deterministic protocol/mount/lifecycle tests for new behavior.
3. Update readiness matrix + roadmap bullets when gate scope changes.
