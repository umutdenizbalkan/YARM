# libc ABI contract for freestanding musl shim (ISA-agnostic core, x86_64 first runner)

This document freezes the minimum libc-facing ABI expected by the musl sysdeps shim across ISAs; x86_64 is the first concrete boot runner.

## Calling convention

- Register lane ABI follows the existing Linux-compat syscall frame convention used by the compatibility service.
- `TrapFrame::syscall_num` selects operation; arguments are read from lane indices 0..3 (and additional lanes where applicable).
- Return value uses `ret0`; errors are returned as negative errno conventions in the compatibility personality.

## Frozen syscall numbers for minimal bootstrap

These numbers are already defined and tested in `src/services/compatibility/linux_compat/mod.rs`:

- `brk`: 214
- `mmap`: 222
- `munmap`: 215
- `mprotect`: 226
- `getpid`: 172
- `getppid`: 173
- `exit`: 93
- VFS-oriented calls (`openat`, `close`, `read`, `write`, `ioctl`, `dup`, `fcntl`, `poll`, `epoll_*`, `sendfile`, `statx`) remain routed through service bindings.

## Error mapping policy (`KernelError` -> `errno`)

- `MissingRight` -> `EPERM`
- memory/resource fullness (`VmFull`, `TaskTableFull`, `MemoryObjectFull`) -> `ENOMEM`
- invalid object/capability/memory faults and decode failures -> `EINVAL`
- unsupported or unmapped operations -> `ENOSYS`

## Edge-case behavior required by tests

- Unsupported syscall numbers must return `ENOSYS`.
- Truncated codec payloads must be rejected.
- VM helpers must enforce page alignment and overflow checks.
- `brk` growth/shrink/query semantics must remain deterministic.

## Relationship to milestone tracking

- Milestone 2 checklist items in `X86_64_NONE_MUSL_PORT_TODO.md` are grounded in the constants, mapping logic, and tests already present in `linux_compat/mod.rs`.
- Milestone 3 begins in `src/services/compatibility/linux_compat/sysdeps.rs` with startup/memory hooks and a temporary clock stub (`ENOSYS`) until timer service plumbing lands.

## Bootstrap sysdeps coverage now implemented

- startup hook validation (`stack_top` sanity)
- memory hooks (`mmap`, `munmap`, `mprotect`, `brk`) routed to kernel VM helpers
- clock hooks (`clock_gettime`, `nanosleep`) for early userspace timing
- thread/TLS hooks (`clone` id allocator + TLS slot set/get)
- futex-like wait/wake bookkeeping hooks for deterministic synchronization tests

These are intentionally bootstrap-oriented and will be replaced/refined as timer/process/sync services are fully integrated for production semantics.
