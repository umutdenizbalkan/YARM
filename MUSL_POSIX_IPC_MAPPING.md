# musl POSIX syscall → YARM IPC mapping plan (v1)

This document defines the concrete mapping layer for `x86_64-unknown-none` musl sysdeps on YARM.

Design rule: keep kernel ABI mechanism-only and route POSIX semantics through service IPC codecs.

## 1) Dispatch pipeline

1. musl syscall stubs (`__syscall*`) decode syscall number + args.
2. Sysdeps shim maps Linux syscall number to YARM compat operation class.
3. Compat layer emits typed IPC payload (`ProcV2Args` / `VfsV1Args` or fixed u64 reply lanes).
4. Service reply is converted to return value + `errno`.

## 2) Mapping matrix (minimum viable)

| POSIX / musl entry | Linux nr (compat) | IPC target | IPC opcode / codec | Return mapping |
|---|---:|---|---|---|
| `getpid()` | `LINUX_NR_GETPID` (172) | `process_manager.srv` | `PROC_OP_GETPID`, u64 reply | `pid_t` from u64 |
| `getppid()` | `LINUX_NR_GETPPID` (173) | `process_manager.srv` | `PROC_OP_GETPPID`, u64 reply | `pid_t` from u64 |
| `_Exit()/exit()` | `LINUX_NR_EXIT` (93) | `process_manager.srv` | `PROC_OP_EXIT`, arg0=`status` | no return |
| `openat()` | `LINUX_NR_OPENAT` (56) | `vfs.srv` | `VFS_OP_OPENAT`, `VfsV1Args` | `fd` from u64 |
| `close()` | `LINUX_NR_CLOSE` (57) | `vfs.srv` | `VFS_OP_CLOSE`, `VfsV1Args` | `0/-1` |
| `read()` | `LINUX_NR_READ` (63) | `vfs.srv` | `VFS_OP_READ`, `VfsV1Args` | bytes read from u64 |
| `write()` | `LINUX_NR_WRITE` (64) | `vfs.srv` | `VFS_OP_WRITE`, `VfsV1Args` | bytes written from u64 |
| `ioctl()` | `LINUX_NR_IOCTL` (29) | `vfs.srv` | `VFS_OP_IOCTL`, `VfsV1Args` | ioctl result as u64 |
| `dup()` | `LINUX_NR_DUP` (23) | `vfs.srv` | `VFS_OP_DUP`, `VfsV1Args` | new fd as u64 |
| `fcntl()` | `LINUX_NR_FCNTL` (25) | `vfs.srv` | `VFS_OP_FCNTL`, `VfsV1Args` | fcntl result as u64 |
| `poll()` | `LINUX_NR_POLL` (73) | `vfs.srv` | `VFS_OP_POLL`, `VfsV1Args` | ready count as u64 |
| `epoll_create1()` | `LINUX_NR_EPOLL_CREATE1` (20) | `vfs.srv` | `VFS_OP_EPOLL_CREATE1`, `VfsV1Args` | epfd as u64 |
| `epoll_ctl()` | `LINUX_NR_EPOLL_CTL` (21) | `vfs.srv` | `VFS_OP_EPOLL_CTL`, `VfsV1Args` | `0/-1` |
| `epoll_pwait()` | `LINUX_NR_EPOLL_PWAIT` (22) | `vfs.srv` | `VFS_OP_EPOLL_PWAIT`, `VfsV1Args` | ready count as u64 |
| `sendfile()` | `LINUX_NR_SENDFILE` (71) | `vfs.srv` | `VFS_OP_SENDFILE`, `VfsV1Args` | copied bytes as u64 |
| `statx()` | `LINUX_NR_STATX` (291) | `vfs.srv` | `VFS_OP_STATX`, `VfsV1Args` | `0/-1` |
| `mmap()` | `LINUX_NR_MMAP` (222) | kernel vm helper (bridge) | `linux_mmap_region` | mapped address |
| `munmap()` | `LINUX_NR_MUNMAP` (215) | kernel vm helper (bridge) | `linux_munmap_region` | `0/-1` |
| `mprotect()` | `LINUX_NR_MPROTECT` (226) | kernel vm helper (bridge) | `linux_mprotect_region` | `0/-1` |
| `brk()` | `LINUX_NR_BRK` (214) | kernel vm helper (bridge) | `linux_brk` | program break |

## 3) errno policy

- IPC/kernel errors convert through compat `LinuxErrno` (`EINVAL`, `EPERM`, `ENOMEM`, `ENOSYS` etc.).
- Unknown syscall numbers return `ENOSYS` (`LinuxErrno::NoSys`).
- Decode/size mismatches in typed payloads map to `EINVAL`.

## 4) Sysdeps implementation order

1. **Process + VFS core**: `getpid/getppid/exit/openat/close/read/write`.
2. **Descriptor control**: `ioctl/dup/fcntl/poll/epoll*`.
3. **Metadata/transfer**: `statx/sendfile`.
4. **Memory primitives**: `mmap/munmap/mprotect/brk` using current bridge helpers.
5. **Thread/time sync**: `clone/TLS/futex/clock_gettime/nanosleep` over existing hooks.

## 5) Exit criteria for “minimal musl runtime usable”

- Busybox-class process can perform open/read/write/close through VFS IPC.
- `errno` values are stable for invalid fd/path/timeout/no-sys cases.
- `poll/epoll` paths are deterministic under replay tests.
- Memory hooks pass deterministic mmap/brk tests.
