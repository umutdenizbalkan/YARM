<!-- SPDX-License-Identifier: Apache-2.0 -->

# Signal Policy

**Canonical: yes.** Owns YARM's stance on POSIX/Linux signal support
(currently: not implemented in the kernel; explicit non-goal).
Threading prerequisites live in `doc/KERNEL_MULTITHREADING_DESIGN.md`;
Linux/musl personality scope lives in `doc/LIBC_AND_LINUX_COMPAT.md`.

## Current policy

YARM does **not** implement classic POSIX/Linux asynchronous signal delivery in the kernel at this stage.

The current freestanding and `linux-compat` milestones focus on kernel mechanisms required for musl/runtime bring-up:

- schedulable threads
- per-thread TLS state
- futex-backed synchronization
- explicit process/thread lifecycle
- IPC/notification delivery

## Why

Classic signals combine mechanism and policy in ways that are a poor fit for the current microkernel boundary:

- per-thread masks
- process-directed vs thread-directed routing
- async handler delivery
- `EINTR` / restart behavior
- alternate signal stacks
- compatibility subtleties (`sigaction`, `rt_sig*`, `signalfd`, etc.)

For now, these semantics are intentionally deferred rather than partially emulated.

## What to use instead

Use deterministic, explicit primitives instead of async signals:

- thread exit + join
- procman/supervisor lifecycle operations
- futex wait/wake
- timer service events
- notification objects and typed IPC control messages
- explicit terminate/stop/cancel requests as control-plane operations

## Compatibility stance

Until a later compatibility milestone says otherwise, Linux signal syscalls should be treated as either:

1. unsupported (`ENOSYS`), or
2. mapped to a narrow deterministic control-plane behavior that is explicitly documented per syscall.

## Follow-up gates before revisiting signals

Do not implement full signals until the threading substrate is complete:

1. saved user register context
2. arch-specific TLS restore on context switch
3. clearer pid/tid/thread-group semantics
4. join/detach and thread-exit cleanup
5. robust futex metadata and recovery rules

Only after those land should YARM consider a minimal signal subset, likely starting with process-control semantics rather than arbitrary async handlers.
