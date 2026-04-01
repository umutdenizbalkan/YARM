# KernelState partition plan (incremental)

This document captures the concrete next steps for splitting `KernelState`
into independently lockable domains while preserving behavior.

## Phase 1 (completed)

- Add explicit lock boundaries for scheduler-facing and IPC-mailbox-facing paths:
  - scheduler domain lock (initially `SpinLockIrq<()>`)
  - `ipc_state_lock: SpinLockIrq<()>`
- Use the scheduler lock in scheduler entry points (`bring_up_cpu`,
  dispatch/preempt/block paths, and enqueue paths) so scheduler mutations are
  serialized through a dedicated lock domain.
- Use `ipc_state_lock` in cross-CPU mailbox submit/drain entry points.

## Phase 2 (completed in this pass)

- Extract `SchedulerState` into a dedicated struct:
  - `scheduler: SmpScheduler`
  - `timer: Timer`
  - `current_cpu: CpuId`
- Store as `SpinLockIrq<SchedulerState>` in `KernelState`.
- Migrate scheduler call sites to lock scheduler state explicitly and remove
  direct `KernelState::{scheduler,timer,current_cpu}` field access.
- Add scheduler/timer test helpers so architecture and boot tests no longer
  rely on direct field access.

## Phase 3 (in progress)

- Extract `IpcState` into a dedicated struct:
  - endpoint tables/waiters/routes/envelopes
  - IPC telemetry
  - cross-CPU mailbox
- Store as `SpinLockIrq<IpcState>` in `KernelState`.
- Migrate IPC call sites to lock IPC state explicitly.

### Phase 3a (completed in this pass)

- Introduce IPC lock-backed accessors on `KernelState`:
  - `with_ipc_state(...)`
  - `with_ipc_state_mut(...)`
- Migrate cross-CPU mailbox submit/drain paths to use IPC accessors.
- Migrate non-IPC-module telemetry touch points (scheduler dispatch/yield/context
  counters and driver telemetry snapshot) to use IPC accessors.

### Phase 3b (completed in this pass)

- Migrate endpoint/notification validation and lifecycle entry points to the IPC
  accessor layer:
  - `resolve_endpoint_index`
  - `destroy_endpoint`
  - `resolve_notification_index`
  - `bind_irq_notification`
  - `route_external_irq`

## Phase 4

- Extract VM/memory/task domains into additional lockable partitions:
  - `VmState` (`kernel_aspace`, `user_spaces`)
  - `TaskState` (`tcbs`, tls restore queue, robust futex state)
  - `MemoryState` (allocator, memory objects, brk regions)
- Introduce lock ordering rules and a small helper API to avoid deadlocks.

## Lock ordering (proposed)

To avoid deadlocks as partitioning progresses, acquire in this order:

1. scheduler
2. task
3. ipc
4. vm
5. memory
6. driver/fault/restart (if split further)

No function should acquire a lock earlier in the order after acquiring a later
one.
