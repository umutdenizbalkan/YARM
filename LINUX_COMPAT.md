# YARM Linux Compatibility Slice v1

This document defines the Linux-compatibility bridge layer used to ease userland porting.

## Scope (implemented)

- ABI version: `1`
- Process-server IPC ABI version: `1`
- VFS-server IPC ABI version: `1`
- Dispatcher table size frozen at `12` entries
- Linux syscall numbers supported by dispatcher table:
  - `exit` = 93
  - `getpid` = 172
  - `getppid` = 173
  - `openat` = 56
  - `close` = 57
  - `read` = 63
  - `write` = 64
  - `ioctl` = 29
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

Linux-facing `openat` / `close` / `read` / `write` / `ioctl` requests are mapped onto VFS-manager IPC requests via a dedicated registration path (`register_linux_vfs_manager`).

## Design note

Compatibility is implemented as a translation layer over microkernel mechanisms (capabilities + VM objects), not by making the kernel internals Linux-specific.
