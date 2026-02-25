# YARM Linux Compatibility Slice v1

This document defines the Linux-compatibility bridge layer used to ease userland porting.

## Scope (implemented)

- ABI version: `1`
- Linux syscall numbers supported by dispatcher table:
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

## Design note

Compatibility is implemented as a translation layer over microkernel mechanisms (capabilities + VM objects), not by making the kernel internals Linux-specific.
