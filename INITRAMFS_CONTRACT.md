<!-- SPDX-License-Identifier: Apache-2.0 -->

# Initramfs Service Contract (`initramfs.srv`)

This document defines behavior for the in-tree read-only `initramfs.srv` implementation.

## Supported nodes (v1)

- boot marker (`INITRAMFS_BOOT_MARKER_PATH_PTR`)
- init image (`INITRAMFS_INIT_PATH_PTR`)
- etc/hosts marker (`INITRAMFS_ETC_HOSTS_PATH_PTR`)

## FD allocation and open semantics

- Open handles are allocated from a monotonic service-local fd allocator.
- Initial fd is `10`; each successful open consumes one fd.
- Multiple opens of the same node are allowed and produce unique fds.
- Exhausted handle table returns `NoFd`.

## I/O semantics

- The filesystem is read-only.
- `read(fd, len)` returns `min(len, file_len)` for the opened inode.
- `write(fd, len)` always returns `Unsupported` for valid initramfs fds.

## `statx` contract

`statx(path)` returns a compact metadata value:

- regular-file marker bit: `0x1000_0000_0000_0000`
- owner-read mode bit: `0o400`
- encoded file length in upper payload bits (`file_len << 16`)

## Mount failure/recovery policy for in-flight fds

For current `VfsService` mount semantics:

- `mark_mount_failed` / `recover_mount` update mount records only.
- Existing in-flight initramfs fds remain usable until explicit close.
- `unmount` deactivates the mount record and does not implicitly close backend fds.

## Metrics contract

`InitramfsMetrics` tracks:

- open / close / read / write / statx counts
- cumulative `bytes_read`
- error count

Runtime loop logs include these counters for observability.

## Extensibility rules

When adding initramfs nodes or behavior:

1. Add a stable path identity constant and inode entry.
2. Define read/write/statx behavior in this contract.
3. Add deterministic tests for protocol vectors, mount policy routing, lifecycle fail/recover, and in-flight fd behavior.
4. Keep initramfs read-only unless contract/ABI is explicitly revised.
