# Control-plane boundary summary

## Purpose

This document records the current stopping point of `yarm` dependency reduction for
`crates/yarm-control-plane-servers`.

Primary goal:
- move userspace-facing and policy-facing surfaces out of direct control-plane dependency on
  kernel internals where practical,
- while keeping kernel mechanism boundaries explicit and avoiding abstraction drift.

## Extracted userspace/runtime surfaces

The following surfaces were moved to userspace/runtime-facing crates and are no longer expected
to be sourced from kernel internals in control-plane servers:

- Logging -> `yarm-user-rt`
- IPC value types -> `yarm-user-rt`
  - `Message`, `ThreadId`
- Time value types -> `yarm-user-rt`
  - `TickInstant`, `TickDuration`
- Syscall userspace error surface -> `yarm-user-rt`
  - `SyscallError`
- Capability value/rights surface -> `yarm-user-rt`
  - `CapId`, `CapRights`
- Driver shared ABI subset -> `yarm-ipc-abi`
- Task userspace value surface -> `yarm-user-rt`
  - `TaskStatus`, `TaskClass`
- VM userspace value surface -> `yarm-user-rt`
  - `Asid`, `PAGE_SIZE`
- Process userspace/runtime surface -> `yarm-user-rt`
  - `ProcessId`, `ProcessError`, `WaitResult`, `ProcessManagerOps`

## KernelState-boundary redesign pattern used

Where incremental redesign was coherent, control-plane server code adopted narrow local
trait/facade + adapter patterns around `KernelState`-backed operations. This was used to isolate
small helper families without pretending kernel mechanism moved out of `yarm`.

Redesigned families include:
- driver-manager control family
- process-manager helper/request-loop family
- process-manager IPC seam stabilization
- supervisor query-status helper family
- supervisor outbound message helper family
- supervisor control-request handling family
- supervisor restart/redelegation family
- supervisor task-exit family
- VFS kernel-IPC roundtrip request-loop helper family

## Intentional remaining boundaries (irreducible for now)

The following remaining boundaries are intentional and should be treated as current design
constraints rather than unfinished cleanup:

1. Supervisor recv/run-loop residue
   - Intentional irreducible supervisor runtime boundary for now.

2. VFS remaining raw `KernelState` residue
   - Intentional local test-harness residue for now.

3. Init `core` + `service` residue
   - Intentional irreducible launch/bootstrap boundary for now.

## Contributor guardrails

Do **not** continue extracting tiny facades in the above remaining areas unless doing a deliberate,
scoped redesign.

Do **not**:
- move `KernelState` out of `yarm`,
- create fake boundaries that hide kernel mechanism coupling,
- broaden scope beyond the target boundary when touching control-plane code.

## Possible future redesign targets (only with explicit design approval)

- Supervisor runtime loop architecture (if run-loop ownership model is intentionally redesigned).
- Init launch/bootstrap orchestration (if task/image/bootstrap ownership boundaries are
  intentionally redesigned end-to-end).

Absent such redesign decisions, this document is the authoritative stopping-point boundary summary.
