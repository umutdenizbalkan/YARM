# YARM Linux Compatibility Slice v1

This document defines the Linux-compatibility bridge layer used to ease userland porting.

## Scope (implemented)

- ABI version: `1`
- Process-server IPC ABI version: `1`
- VFS-server IPC ABI version: `1`
- Dispatcher table size frozen at `20` entries
- Linux syscall numbers supported by dispatcher table:
  - `exit` = 93
  - `getpid` = 172
  - `getppid` = 173
  - `openat` = 56
  - `close` = 57
  - `read` = 63
  - `write` = 64
  - `ioctl` = 29
  - `dup` = 23
  - `fcntl` = 25
  - `poll` = 73
  - `epoll_create1` = 20
  - `epoll_ctl` = 21
  - `epoll_pwait` = 22
  - `sendfile` = 71
  - `statx` = 291
  - `brk` = 214
  - `munmap` = 215
  - `mmap` = 222
  - `mprotect` = 226
- Errno mapping support for:
  - `EINVAL` (22)
  - `EPERM` (1)
  - `ENOMEM` (12)
  - `ENOSYS` (38)

## VM wrappers

- `linux_mmap_region(...)` supports multi-page mappings.
- `linux_munmap_region(...)` supports multi-page unmapping.
- `linux_mprotect_region(...)` supports multi-page protection changes.

All range-based wrappers enforce page alignment and round-up page semantics.

## brk manager

A minimal `brk` region manager is implemented:

- tracks per-task `brk_base` / `brk_end`
- grows by mapping pages in the requested range
- shrinks by unmapping pages above the requested end

## Linux-compat dispatcher

`linux_compat::dispatch(kernel, frame)` is separate from kernel-native syscall ABI and routes Linux syscall numbers through the compatibility table.

## Process-manager vertical path

Linux-facing `getpid` / `getppid` / `exit` requests are now mapped onto IPC server requests:

- kernel sends request messages to a registered process-manager request endpoint
- kernel receives replies from a registered process-manager reply endpoint

This is the initial vertical user-space server path over IPC.

## VFS-lite vertical path

Linux-facing `openat` / `close` / `read` / `write` / `ioctl` / `dup` / `fcntl` / `poll` / `epoll_create1` / `epoll_ctl` / `epoll_pwait` / `sendfile` / `statx` requests are mapped onto VFS-manager IPC requests via a dedicated registration path (`register_linux_vfs_manager`).

## Design note

Compatibility is implemented as a translation layer over microkernel mechanisms (capabilities + VM objects), not by making the kernel internals Linux-specific.

## Payload schema stability

For higher-churn VFS calls we now freeze helper packers so request payload layout is explicit and testable:

- `pack_epoll_ctl(epfd, op, fd, event_ptr)`
- `pack_sendfile(out_fd, in_fd, offset_ptr, count)`
- `pack_statx(dirfd, path_ptr, flags, mask)`

Each helper encodes four little-endian u64 words (`[arg0,arg1,arg2,arg3]`) into a 32-byte IPC payload.

## Process-manager protocol v2 slice

In addition to single-u64 process-manager requests, the kernel now provides a dual-u64 request path (`send_linux_process_manager_request2`) and frozen v2 opcodes for future spawn/wait routing:

- `PROC_OP_SPAWN_V2`
- `PROC_OP_WAITPID_V2`

The payload shape is a fixed 16-byte little-endian tuple (`arg0`, `arg1`) and is covered by round-trip unit tests.
