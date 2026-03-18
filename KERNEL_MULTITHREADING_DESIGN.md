# Kernel Multithreading Design

## Goal

Define the minimum kernel-resident threading mechanism required to support real userspace TLS, futex-backed synchronization, and libc runtimes such as musl without collapsing the microkernel boundary.

## Design principles

- Keep **mechanism** in kernel: schedulable threads, context/TLS state, blocking, wakeup, same-address-space thread creation.
- Keep **policy** in userspace/libc: POSIX `pthread_*` API, mutex/condvar semantics, cancellation, higher-level scheduling policy.
- Support multiple ISAs by storing architecture-neutral thread metadata in the generic TCB and leaving ISA-specific TLS register restore to arch trap/context-switch paths.

## Current kernel additions in this change

### Thread model

`ThreadControlBlock` is extended with:

- `thread_group_id`: identifies threads belonging to the same userspace process / address space group.
- `tls_base`: per-thread TLS base requested by userspace/libc.
- `user_entry`: initial user entry PC for spawned userspace threads.
- `user_stack_top`: initial userspace stack pointer for spawned threads.

This is enough to distinguish "process leader" from "additional threads in same process" while keeping existing task/scheduler semantics intact.

### Kernel APIs

The kernel now exposes mechanism-oriented helpers:

- `allocate_thread_id()`
- `thread_group_id(tid)`
- `thread_tls_base(tid)`
- `set_thread_tls_base(tid, tls_base)`
- `spawn_user_thread(parent_tid, tls_base, user_stack_top, user_entry)`
- `futex_wait_current(addr, expected, observed)`
- `futex_wake(addr, max_wake)`

### Scheduler / blocking semantics

A futex wait blocks the current thread by:

1. validating the futex word/address contract,
2. marking the current TCB as `Blocked(WaitReason::Futex(addr))`,
3. removing it from the running slot,
4. dispatching the next runnable thread.

A futex wake scans blocked TCBs for matching futex addresses, marks them runnable, and re-enqueues them.

This is intentionally simple (`O(n)` wake scan over the fixed TCB table) but it is **real scheduler-integrated blocking**, unlike the previous compatibility-only bookkeeping shim.

## libc / musl mapping

The linux-compat sysdeps thread/TLS/futex hooks now route into kernel mechanisms rather than stand-alone globals:

- `clone_thread_hook` -> `spawn_user_thread`
- `set_tls_hook` -> `set_thread_tls_base`
- `get_tls_hook` -> `thread_tls_base`
- `futex_wait_hook` -> `futex_wait_current`
- `futex_wake_hook` -> `futex_wake`

This does **not** yet provide full POSIX pthread semantics. It provides the kernel substrate required for them.

## What is still missing

To complete production-quality pthread/libc support, follow-up work should add:

1. saved userspace register context per thread,
2. arch-specific TLS register restore (`fsbase` / `tpidr_el0` / `tp`),
3. explicit `pid` vs `tid` / thread-group semantics in process service contracts,
4. join / detach / thread-exit cleanup,
5. robust futex lists, timeouts, and priority inheritance if needed,
6. signal delivery semantics (or explicit non-support policy).

## Why this boundary is correct

This design keeps the kernel responsible for only the mechanism that user runtimes cannot implement safely on their own:

- scheduling,
- wait/wake,
- thread identity,
- same-address-space execution contexts,
- per-thread TLS state.

Everything POSIX-specific remains outside the kernel.
