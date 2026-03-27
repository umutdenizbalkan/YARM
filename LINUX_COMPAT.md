# YARM Linux Compatibility Slice v1

This document defines the Linux-compatibility bridge layer used to ease userland porting.

> **Important ABI note (March 2026 update):**
> Linux `mmap`/`munmap`/`mprotect` now consume Linux argument order directly
> (`addr`, `len`, `prot`, ...). Capability-targeted VM mapping is no longer
> encoded in Linux `mmap` arg0 and is instead exposed via the native YARM
> syscall `sys_vm_map` in the kernel syscall ABI.

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
- `linux_*_current_task(...)` variants route Linux ABI VM calls against the
  current task ASID so Linux argument order remains ABI-compatible.

All range-based wrappers enforce page alignment and round-up page semantics.

## brk manager

A minimal `brk` region manager is implemented:

- tracks per-task `brk_base` / `brk_end`
- grows by mapping pages in the requested range
- shrinks by unmapping pages above the requested end

## Linux-compat dispatcher

`linux_compat::dispatch(kernel, frame)` is separate from kernel-native syscall ABI and routes Linux syscall numbers through the compatibility table.

- `mmap(addr,len,prot,flags,fd,off)` uses Linux argument positions.
- `munmap(addr,len)` uses Linux argument positions.
- `mprotect(addr,len,prot)` uses Linux argument positions.
- `brk` currently remains capability-routed in arg1 for explicit heap mapping policy.

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

For higher-churn VFS calls we now freeze helper packers and a typed codec so request payload layout is explicit and testable:

- `pack_epoll_ctl(epfd, op, fd, event_ptr)`
- `pack_sendfile(out_fd, in_fd, offset_ptr, count)`
- `pack_statx(dirfd, path_ptr, flags, mask)`

Each helper now routes through `VfsV1Args` (version `VFS_CODEC_V1_VERSION`) which encodes four little-endian u64 words (`[arg0,arg1,arg2,arg3]`) into a 32-byte IPC payload.

## Process-manager protocol v2 slice

In addition to single-u64 process-manager requests, the kernel now provides a dual-u64 request path (`send_linux_process_manager_request2`) and frozen v2 opcodes for future spawn/wait routing:

- `PROC_OP_SPAWN_V2`
- `PROC_OP_WAITPID_V2`

The payload shape is a fixed 16-byte little-endian tuple (`arg0`, `arg1`) and is covered by round-trip unit tests.

The process-manager v2 path also defines a typed `ProcV2Args` 16-byte codec (`arg0`, `arg1`) with explicit codec version constant (`PROC_CODEC_V2_VERSION`) to freeze payload semantics before adding broader spawn/wait APIs.

A minimal VFS service scaffold now exists in the control-plane service layer (`src/services/control_plane/vfs/service.rs`) and is exercised by `src/bin/vfs_server.rs`.

The compatibility test suite now includes an end-to-end personality shim flow (`getpid` + `openat` + `exit`) to verify process-manager and VFS routing in one sequence.
The compatibility test suite also includes a deterministic mixed syscall sequence (`getpid`/`openat`) to ensure stable cross-server routing behavior over repeated dispatch cycles.

The compatibility suite further validates deterministic mixed server flow with IRQ notification routing (`getpid` + IRQ notification + `openat`) so server IPC and notification delivery remain stable under interleaving.


The compatibility-gate suite now includes golden fixture vectors and truncated-payload rejection checks for `ProcV2Args` and `VfsV1Args`, so any wire-format drift fails tests immediately.


## Standalone server boundary

Linux compatibility is now treated as a standalone personality server boundary. Core process-manager and VFS servers depend only on protocol modules (`process_abi`, `vfs_abi`) and can be shipped without Linux personality support. The Linux compatibility layer consumes those protocols over IPC rather than owning their contracts.


## Build/profile boundary

Linux compatibility is optional and built behind Cargo feature `linux-compat`.

- Core microkernel + protocol servers only: `cargo test`
- Linux personality enabled: `cargo test --features linux-compat`

This keeps `process_manager`/`vfs` deliverables independent from Linux personality policy code.
