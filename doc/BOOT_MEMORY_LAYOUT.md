<!-- SPDX-License-Identifier: Apache-2.0 -->

# Boot memory layout and initrd handoff invariants

This note records the runtime contract for boot initrd handoff and reservation.

## Initrd data source by ISA

- **x86_64**: initrd bytes come from the PVH module list (`start_info` module
  window) discovered during early boot.
- **aarch64**: initrd bytes come from DTB `/chosen` properties
  `linux,initrd-start` and `linux,initrd-end`.
- **riscv64**: initrd bytes come from parsed DTB initrd range
  (`initrd_start` / `initrd_end`).

`install_boot_initrd_bytes()` stores a pointer/length pair for that boot-memory
window, and `boot_initrd_bytes()` later exposes it as `&'static [u8]`.

## CRITICAL invariant: reserve initrd before allocator reuse

The initrd physical region **MUST** be reserved before:

1. frame allocator initialization, and
2. any physical-memory reuse by allocators.

This invariant is required because `boot_initrd_bytes()` returns a borrowed
slice into boot memory, not copied data. The pointer must never refer to
temporary buffers or memory that may be reclaimed.

## x86_64 PVH note (past bug and fix)

- **Past bug**: the PVH module handoff path installed initrd bytes without
  reserving the module window, allowing potential allocator reuse overlap.
- **Current fix**: x86_64 PVH handoff now explicitly reserves the page-aligned
  initrd window through:
  `Bootstrap::install_boot_reserved_range(...)`
  before `install_boot_initrd_bytes(...)`.

## Failure mode if invariant is violated

If initrd memory is not reserved, allocator reuse can overwrite initrd bytes.
That can corrupt CPIO/ELF parsing and cause non-deterministic early-boot
failures that are hard to reproduce.
