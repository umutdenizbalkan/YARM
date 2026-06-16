<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Umut Deniz Balkan -->

# YARM libc / Linux / POSIX compatibility

**Canonical: yes.** Successor to the three pre-pass docs
`LIBC_ABI_X86_64_NONE.md`, `LINUX_COMPAT.md`, and
`MUSL_POSIX_IPC_MAPPING.md`. All three were merged here in the global
unlocking readiness audit; the originals are deleted.

Related canonical references:

- `doc/SYSCALL_ABI.md` — native YARM syscall ABI.
- `doc/IPC.md` — IPC send/recv, transfer-cap, shared-region fastpath.
- `doc/PROCESS_AND_SPAWN.md` — PM / supervisor / spawn contract.
- `doc/AI_AGENT_RULES.md` §1 — capability and IPC transfer rules
  agents must follow when extending compat.

---

## 1. Scope and design rule

YARM's Linux/POSIX compatibility is a **userspace personality** layered
on top of the native microkernel mechanisms (capabilities, VM objects,
typed IPC payloads). The kernel ABI stays mechanism-only; POSIX
semantics live in personality servers and a thin compat shim.

Two layers are involved:

1. **`linux-compat` shim** (kernel-side dispatcher + sysdeps hooks).
   Cargo feature `linux-compat`. ABI version `1`. Dispatcher table
   frozen at **20** entries.
2. **musl-on-`x86_64-unknown-none` runner**: the concrete first-runner
   target; sysdeps shim routes through the linux-compat dispatcher to
   `process_manager.srv` / `vfs.srv` over IPC, or to in-kernel VM
   helpers (`linux_*_region`) for memory primitives.

The kernel IPC trap ABI itself is **frozen at `SYSCALL_ABI_VERSION =
3`** (see `src/kernel/syscall.rs`). Transfer-cap semantics:

- Lane: `SYSCALL_ARG_TRANSFER_CAP` (last trapframe arg register).
- No-transfer sentinel: `SYSCALL_NO_TRANSFER_CAP` (`u64::MAX`).
- ABI v3 rule: cap transfer is permitted only when a receiver is
  already waiting on the destination endpoint; otherwise the send
  fails with `WouldBlock`. On success the userspace-visible transfer
  metadata is an opaque transfer-envelope handle, not the raw
  source cap id.

This is intentionally strict to keep capability ownership unambiguous
across queued sends and recv-side materialization deterministic.

---

## 2. Calling convention

- Register lane ABI follows the existing Linux-compat syscall frame
  convention used by the compatibility service.
- `TrapFrame::syscall_num` selects the operation; arguments come from
  lane indices `0..3` (plus additional lanes where applicable).
- Return value uses `ret0`; errors are returned as negative-errno per
  the compatibility personality.

### Important ABI note (March 2026)

Linux `mmap` / `munmap` / `mprotect` now consume **Linux argument
order** directly (`addr, len, prot, …`). Capability-targeted VM
mapping is no longer encoded in Linux `mmap` arg0 — it lives on the
native YARM syscall `sys_vm_map` instead.

---

## 3. Implemented Linux syscall surface

The linux-compat dispatcher accepts exactly these numbers (the
constants live in `crates/yarm-compat-servers/src/posix_compat/mod.rs`):

| Linux name | nr | Routed via |
|------------|---:|------------|
| `exit` | 93 | `process_manager.srv` |
| `getpid` | 172 | `process_manager.srv` |
| `getppid` | 173 | `process_manager.srv` |
| `openat` | 56 | `vfs.srv` |
| `close` | 57 | `vfs.srv` |
| `read` | 63 | `vfs.srv` |
| `write` | 64 | `vfs.srv` |
| `ioctl` | 29 | `vfs.srv` |
| `dup` | 23 | `vfs.srv` |
| `fcntl` | 25 | `vfs.srv` |
| `poll` | 73 | `vfs.srv` |
| `epoll_create1` | 20 | `vfs.srv` |
| `epoll_ctl` | 21 | `vfs.srv` |
| `epoll_pwait` | 22 | `vfs.srv` |
| `sendfile` | 71 | `vfs.srv` |
| `statx` | 291 | `vfs.srv` |
| `brk` | 214 | kernel VM helper |
| `munmap` | 215 | kernel VM helper |
| `mmap` | 222 | kernel VM helper |
| `mprotect` | 226 | kernel VM helper |

Process-manager IPC ABI version `1`; VFS-server IPC ABI version `1`.

### 3.1 errno mapping (`KernelError → errno`)

| Source | Mapped errno |
|--------|--------------|
| `MissingRight` | `EPERM` (1) |
| `VmFull` / `TaskTableFull` / `MemoryObjectFull` | `ENOMEM` (12) |
| invalid object / cap / memory faults, decode failures | `EINVAL` (22) |
| unsupported / unmapped operation | `ENOSYS` (38) |
| unknown Linux syscall number | `ENOSYS` (`LinuxErrno::NoSys`) |
| decode/size mismatch in typed payload | `EINVAL` |

### 3.2 Edge-case requirements covered by tests

- Unsupported syscall numbers return `ENOSYS`.
- Truncated codec payloads are rejected.
- VM helpers enforce page alignment and overflow checks.
- `brk` growth / shrink / query semantics remain deterministic.

---

## 4. POSIX → IPC mapping matrix (musl-on-x86_64-unknown-none v1)

Mapping pipeline for musl sysdeps:

1. musl `__syscall*` stubs decode syscall number + args.
2. Sysdeps shim maps Linux nr to YARM compat operation class.
3. Compat layer emits typed IPC payload (`ProcV2Args` / `VfsV1Args` or
   fixed u64 reply lanes).
4. Service reply is converted to return value + `errno`.

| POSIX / musl entry | Linux nr | IPC target | IPC opcode / codec | Return mapping |
|---|---:|---|---|---|
| `getpid()` | 172 | `process_manager.srv` | `PROC_OP_GETPID`, u64 reply | `pid_t` from u64 |
| `getppid()` | 173 | `process_manager.srv` | `PROC_OP_GETPPID`, u64 reply | `pid_t` from u64 |
| `_Exit()/exit()` | 93 | `process_manager.srv` | `PROC_OP_EXIT`, arg0=`status` | no return |
| `openat()` | 56 | `vfs.srv` | `VFS_OP_OPENAT`, `VfsV1Args` | `fd` from u64 |
| `close()` | 57 | `vfs.srv` | `VFS_OP_CLOSE`, `VfsV1Args` | `0/-1` |
| `read()` | 63 | `vfs.srv` | `VFS_OP_READ`, `VfsV1Args` | bytes read from u64 |
| `write()` | 64 | `vfs.srv` | `VFS_OP_WRITE`, `VfsV1Args` | bytes written from u64 |
| `ioctl()` | 29 | `vfs.srv` | `VFS_OP_IOCTL`, `VfsV1Args` | ioctl result as u64 |
| `dup()` | 23 | `vfs.srv` | `VFS_OP_DUP`, `VfsV1Args` | new fd as u64 |
| `fcntl()` | 25 | `vfs.srv` | `VFS_OP_FCNTL`, `VfsV1Args` | fcntl result as u64 |
| `poll()` | 73 | `vfs.srv` | `VFS_OP_POLL`, `VfsV1Args` | ready count as u64 |
| `epoll_create1()` | 20 | `vfs.srv` | `VFS_OP_EPOLL_CREATE1`, `VfsV1Args` | epfd as u64 |
| `epoll_ctl()` | 21 | `vfs.srv` | `VFS_OP_EPOLL_CTL`, `VfsV1Args` | `0/-1` |
| `epoll_pwait()` | 22 | `vfs.srv` | `VFS_OP_EPOLL_PWAIT`, `VfsV1Args` | ready count as u64 |
| `sendfile()` | 71 | `vfs.srv` | `VFS_OP_SENDFILE`, `VfsV1Args` | copied bytes as u64 |
| `statx()` | 291 | `vfs.srv` | `VFS_OP_STATX`, `VfsV1Args` | `0/-1` |
| `mmap()` | 222 | kernel VM helper | `linux_mmap_region` | mapped address |
| `munmap()` | 215 | kernel VM helper | `linux_munmap_region` | `0/-1` |
| `mprotect()` | 226 | kernel VM helper | `linux_mprotect_region` | `0/-1` |
| `brk()` | 214 | kernel VM helper | `linux_brk` | program break |

---

## 5. VM wrappers

- `linux_mmap_region(...)` — multi-page mappings.
- `linux_munmap_region(...)` — multi-page unmapping.
- `linux_mprotect_region(...)` — multi-page protection changes.
- `linux_*_current_task(...)` variants route Linux ABI VM calls against
  the current task's ASID so Linux argument order remains ABI-compatible.

All range-based wrappers enforce page alignment and round-up page
semantics.

### 5.1 `brk` manager

Minimal `brk` region manager:

- tracks per-task `brk_base` / `brk_end`;
- grows by mapping pages in the requested range;
- shrinks by unmapping pages above the requested end.

---

## 6. Personality server boundary

Linux compatibility is a **standalone personality server boundary**.
Core `process_manager` and `vfs` servers depend only on the protocol
modules (`process_abi`, `vfs_abi`) and can be shipped without Linux
personality support. The Linux compatibility layer consumes those
protocols over IPC rather than owning their contracts.

### 6.1 Process-manager protocol v2 slice

In addition to single-u64 process-manager requests, the kernel
provides a dual-u64 request path (`send_linux_process_manager_request2`)
and frozen v2 opcodes for spawn/wait routing:

- `PROC_OP_SPAWN_V2`
- `PROC_OP_WAITPID_V2`

Payload shape is a fixed 16-byte little-endian tuple (`arg0`, `arg1`),
covered by round-trip unit tests. The v2 path defines a typed
`ProcV2Args` codec with explicit version constant
`PROC_CODEC_V2_VERSION` to freeze payload semantics before broader
spawn/wait APIs land.

### 6.2 Frozen helper packers (typed payload codec)

For higher-churn VFS calls, helper packers + typed codec are frozen so
request payload layout is explicit and testable:

- `pack_epoll_ctl(epfd, op, fd, event_ptr)`
- `pack_sendfile(out_fd, in_fd, offset_ptr, count)`
- `pack_statx(dirfd, path_ptr, flags, mask)`

All route through `VfsV1Args` (version `VFS_CODEC_V1_VERSION`) which
encodes four little-endian u64 words (`[arg0, arg1, arg2, arg3]`) into
a 32-byte IPC payload.

### 6.3 Compatibility test surface

- end-to-end personality shim flow (`getpid` + `openat` + `exit`)
  verifies process-manager and VFS routing in one sequence;
- deterministic mixed syscall sequence (`getpid` / `openat`) verifies
  stable cross-server routing over repeated dispatch cycles;
- deterministic mixed server flow with IRQ notification routing
  (`getpid` + IRQ notification + `openat`) verifies that server IPC
  and notification delivery remain stable under interleaving;
- golden fixture vectors + truncated-payload rejection checks for
  `ProcV2Args` and `VfsV1Args` fail tests immediately on any wire-format
  drift.

A minimal VFS service scaffold lives in the control-plane service
layer (`crates/yarm-control-plane-servers/src/control_plane/vfs/service.rs`)
and is exercised by `crates/yarm-control-plane-servers/src/bin/vfs_server.rs`.

---

## 7. Sysdeps coverage status

Currently implemented in
`crates/yarm-compat-servers/src/posix_compat/sysdeps.rs`:

- startup hook validation (`stack_top` sanity);
- memory hooks (`mmap`, `munmap`, `mprotect`, `brk`) routed to kernel
  VM helpers;
- clock hooks (`clock_gettime`, `nanosleep`) for early userspace
  timing;
- thread / TLS hooks (`clone` id allocator + TLS slot set/get);
- futex-like wait / wake bookkeeping hooks for deterministic
  synchronization tests.

These are intentionally bootstrap-oriented and will be replaced /
refined as timer / process / sync services are fully integrated for
production semantics.

### Sysdeps implementation order (canonical)

1. **Process + VFS core**: `getpid` / `getppid` / `exit` / `openat` /
   `close` / `read` / `write`.
2. **Descriptor control**: `ioctl` / `dup` / `fcntl` / `poll` /
   `epoll_*`.
3. **Metadata / transfer**: `statx` / `sendfile`.
4. **Memory primitives**: `mmap` / `munmap` / `mprotect` / `brk` via
   the current bridge helpers.
5. **Thread / time sync**: `clone` / TLS / futex / `clock_gettime` /
   `nanosleep` over existing hooks.

Milestone 3 in `doc/X86_64_NONE_MUSL_PORT_TODO.md` is partially
implemented: startup / memory / thread / futex hooks and timer-backed
clock hooks exist; full musl crt / `__libc_start_main` integration is
still pending.

---

## 8. Non-goals (current pass)

- Classic POSIX/Linux **asynchronous signal delivery** is not
  implemented in the kernel. See `doc/SIGNAL_POLICY.md` for the policy
  and the prerequisites before signals can be revisited.
- **Full pthread semantics**. The current pass provides the kernel
  substrate (TLS, futex, thread spawn) per
  `doc/KERNEL_MULTITHREADING_DESIGN.md` §7. POSIX-specific policy
  (mutex/condvar/cancellation/scheduling policy) lives outside the
  kernel.
- Untyped or open-ended Linux syscall acceptance. Linux syscall
  numbers outside the table in §3 return `ENOSYS`.

---

## 9. Exit criteria — "minimal musl runtime usable"

- Busybox-class process can perform open / read / write / close
  through VFS IPC.
- `errno` values are stable for invalid fd / path / timeout / no-sys
  cases.
- `poll` / `epoll` paths are deterministic under replay tests.
- Memory hooks pass deterministic `mmap` / `brk` tests.

---

## 10. Build / profile boundary

Linux compatibility is **optional** and built behind Cargo feature
`linux-compat`.

- Core microkernel + protocol servers only: `cargo test`
- Linux personality enabled: `cargo test --features linux-compat`

This keeps `process_manager` / `vfs` deliverables independent from
Linux personality policy code.

---

## 11. Architecture-specific status

| Arch | libc compat status |
|------|--------------------|
| x86_64 | first concrete runner; native and `linux-compat` paths both validated by the test suite. |
| AArch64 | linux-compat dispatcher compiles and routes correctly; no `unknown-none` libc runner staged yet. |
| RISC-V64 | linux-compat dispatcher compiles for `riscv64gc-unknown-none-elf`; first runner staging gated on RISC-V SMP scheduling and timer-IRQ enablement (see `doc/ARCH_RISCV64.md` §13). |

---

## 12. Authoring rule

Future libc / Linux / musl / POSIX compatibility documentation MUST
update **this file**. Do not create new `LIBC_*.md`, `LINUX_*.md`, or
`MUSL_*.md` fragments.
