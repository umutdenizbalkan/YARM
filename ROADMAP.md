# YARM Microkernel Next-Step Checklist (Portable POSIX Direction)

This checklist focuses on turning the current in-memory kernel model into a portable, architecture-neutral microkernel core with a clear machine-adaptation boundary.

## 0) Ground Rules (keep these invariant)

- Keep kernel core logic free of external libraries/crates unless explicitly approved.
- Keep kernel policy and mechanisms architecture-neutral.
- Isolate machine-specific code behind a strict HAL/arch boundary (`arch/*` or equivalent), not mixed into core scheduler/IPC/capability/VM logic.
- Preserve static/bounded data structures where possible for determinism.

## 1) System Call ABI Hardening

- Freeze syscall number table and argument ABI contract.
- Add explicit ABI versioning and compatibility checks.
- Normalize return/error encoding (single convention for all syscalls).
- Add decode/encode tests for every syscall and every error path.

## 2) Task/Thread Context Model

- Introduce explicit kernel/user context structs (register sets independent of ISA details).
- Define lifecycle transitions: Created -> Runnable -> Running -> Blocked -> Faulted -> Dead.
- Add restart/terminate paths for faulted tasks.
- Add deterministic task ID allocator behavior and exhaustion tests.

## 3) Capability Model Maturation

- Add capability types for:
  - Endpoint
  - Address space
  - Memory object/frame
  - IRQ/notification object
  - Scheduler control
- Add delegation/transfer semantics for IPC capability passing.
- Add revocation trees (parent-child cascade revoke), not only flat revoke.
- Add capability audit/introspection hooks for debug builds.

## 4) IPC Protocol Layer

- Define fixed message header ABI (sender, opcode, flags, length, optional cap-transfer metadata).
- Add zero-copy path abstraction for large payload handoff (backed by memory objects).
- Add timeout/notification semantics for blocking receive/send.
- Add priority-aware wakeup policy to avoid starvation.

## 5) VM Subsystem Evolution

- Split virtual address space objects from physical memory objects.
- Add map rights checks from capabilities (map/read/write/execute).
- Add copy-on-write and shared mapping policy scaffolding.
- Add page fault policy API:
  - kill
  - notify+block
  - notify+resume
- Add ASID lifecycle guarantees and recycling strategy tests.

## 6) Interrupt/Trap Routing Architecture

- Define architecture-agnostic interrupt classes:
  - timer
  - external IRQ
  - syscall
  - page fault
- Add IRQ object/capability path so user-mode servers can own device interrupts.
- Add deferred work queue for bottom-half handling.
- Keep trap decoding in arch layer; core sees normalized trap events only.

## 7) Scheduler Core Upgrades

- Keep RR scheduler as baseline; add pluggable policy interface.
- Add priorities and budget accounting (time slice + CPU usage counters).
- Add explicit blocked wait-channel model for IPC, timer, IRQ waits.
- Add starvation and fairness regression tests.

## 8) Timer & Timekeeping

- Separate monotonic time source from scheduler tick source.
- Add kernel timer queue for wakeups/timeouts.
- Define tickless-ready API (even if initial backend remains periodic).

## 9) POSIX Path (User-space servers)

- Add process manager server protocol.
- Add VFS server protocol and file descriptor table model.
- Add signal/event delivery abstraction.
- Map POSIX syscalls to IPC requests against user-space servers.

## 10) Portability Split (recommended repository layout)

- `src/kernel/*` -> machine-neutral core.
- `src/arch/<arch>/*` -> trap entry, context switch glue, IRQ controller, timer driver, MMU backend.
- `src/platform/<board-or-host>/*` -> boot wiring and device discovery.

## 11) Validation Strategy

- Keep exhaustive unit tests for all state machines.
- Add property-style tests for capability and scheduler invariants.
- Add deterministic simulation tests (multi-task IPC + faults + interrupts).
- Add architecture contract tests that verify arch layer emits normalized trap events expected by core.

## 12) Immediate Next 3 Implementable Steps

1. **Fault policy API**: replace hardcoded fault action with configurable per-task/per-system policy.
2. **Syscall ABI table freeze**: centralize syscall IDs/arg contracts + strict decode tests.
3. **Capability types expansion**: introduce `AddressSpace` and `MemoryObject` capability types and enforce map permissions through them.

---

If you want, I can implement Step 1 next (fault policy API) in a minimal, incremental patch.
