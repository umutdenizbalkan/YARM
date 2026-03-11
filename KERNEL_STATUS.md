# Kernel Status (Current Branch Snapshot)

This note summarizes where the in-kernel mechanisms currently stand, what is complete enough to rely on, and what remains before declaring kernel-mechanism completeness and shifting primary effort to user-space components.

## What is in place

- Architecture boundary scaffolding exists:
  - HAL trait boundary (`src/arch/hal.rs`).
  - RISC-V trap decoding adapter (`src/arch/riscv.rs`).
- Core mechanism modules are present and test-covered:
  - Capability model (`src/kernel/capabilities.rs`).
  - IPC endpoints/messages (`src/kernel/ipc.rs`).
  - Scheduler/SMP work queue (`src/kernel/scheduler.rs`, `src/kernel/smp.rs`).
  - Virtual memory primitives and address spaces (`src/kernel/vm.rs`).
  - Trap/trapframe/timer/task bookkeeping (`src/kernel/trap.rs`, `src/kernel/trapframe.rs`, `src/kernel/timer.rs`, `src/kernel/task.rs`).
  - Bootstrap integration state (`src/kernel/bootstrap.rs`).
- Early compatibility/service scaffolds are present:
  - Linux-compat syscall translation (`src/kernel/linux_compat.rs`).
  - Process-manager/VFS-lite/driver-manager scaffolds.

## What is still not “done” for in-kernel mechanisms

The kernel has broad feature coverage, but core mechanism hardening is still pending:

1. **Type consistency hardening in integration layer (`bootstrap.rs`)**
   - There is still drift between strongly-typed IDs/addresses used in some modules and raw `u64`/`usize` plumbing in integration paths.
   - Finish migration to typed IDs/addresses where intended (thread IDs, cap IDs, virtual/physical addresses).

2. **Kernel state decomposition**
   - `KernelState` remains a large integration struct.
   - Split into explicit mechanism sub-states (IPC/VM/scheduling/driver delegation/fault handling) to reduce coupling and improve invariants.

3. **Trap/IPC/scheduler fastpath invariants**
   - Fastpath behavior should be explicitly documented and tested for single-resume guarantees and no duplicate scheduling side effects.
   - Keep preemption/yield semantics deterministic under repeated calls.

4. **Mechanism-policy separation final pass**
   - Ensure service/personality specifics remain outside mechanism core where possible.
   - Keep kernel-level code generic to hosted services.

5. **Validation depth and conformance tests**
   - Expand adversarial and boundary tests (cap revocation races, endpoint saturation, fault policy edges, mapping/protection transitions, restart policy interactions).
   - Add focused tests for integration invariants at subsystem boundaries.

## Completion criteria for “in-kernel mechanisms complete”

Treat kernel mechanisms as complete when all of the following are true:

- **Mechanism API stability:** typed interfaces are coherent across modules and integration points.
- **Invariants encoded:** key illegal states are unrepresentable or guarded by explicit checks.
- **Policy separation:** service-specific logic is out of core mechanism paths.
- **Test confidence:** unit + integration tests cover success paths, error paths, and race-adjacent boundary conditions.
- **No known correctness blockers:** open “must-fix” issues in scheduler/IPC/trap/VM/restart paths are closed.

Once those criteria are met, primary development can shift to user-space servers/components with the kernel treated as a stable substrate.

## Recommended next execution order

1. Finish `bootstrap.rs` type-alignment and subsystem boundary cleanup.
2. Lock down trap/IPC/scheduler invariants with dedicated tests.
3. Finalize VM/fault/restart contract checks and failure semantics.
4. Freeze mechanism-facing ABI docs and internal contracts.
5. Move main focus to user-space components (process manager, VFS, driver servers, Linux personality expansion).
