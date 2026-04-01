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

### Phase 3c (completed in this pass)

- Migrate transfer-envelope and shared-memory bookkeeping paths in
  `boot/mod.rs` to the IPC accessor layer:
  - envelope stash/materialize/revoke helpers
  - active transfer mapping register/query/remove/revoke flows
  - transfer telemetry counters (`*_records_*`, shared-memory byte counters)
- Migrate endpoint waiter and capability liveness checks in `boot/mod.rs` to
  IPC accessor-backed reads.

### Phase 3d (completed in this pass)

- Add explicit lock-order helper API on `KernelState`:
  - `with_scheduler_state(...)`
  - `with_scheduler_state_mut(...)`
  - `with_scheduler_then_ipc(...)` (ordered acquisition: scheduler -> IPC)
- Migrate scheduler entry points to the new scheduler helper API.
- Add a lock-order snapshot test that reads scheduler+IPC state through the
  ordered helper.

## Phase 4 (in progress)

- Extract VM/memory/task domains into additional lockable partitions:
  - `VmState` (`kernel_aspace`, `user_spaces`)
  - `TaskState` (`tcbs`, tls restore queue, robust futex state)
  - `MemoryState` (allocator, memory objects, brk regions)
- Introduce lock ordering rules and a small helper API to avoid deadlocks.

### Phase 4a (completed in this pass)

- Add new lock domains and helper accessors on `KernelState`:
  - `vm_state_lock` + `with_user_spaces(...)` / `with_user_spaces_mut(...)`
  - `task_state_lock` + `with_tcbs(...)` / `with_tcbs_mut(...)`
  - `memory_state_lock` + `with_memory_state(...)`
- Migrate representative scheduler and VM-touching call sites to use new
  accessors (`task_priority`, affinity resolution/update, retired ASID checks,
  shootdown ack path, and retired shootdown ticking).

### Phase 4b (completed in this pass)

- Migrate core memory/address-space flows in `boot/memory_state.rs` to phase-4
  accessors:
  - user-space create/destroy/map paths via `with_user_spaces_mut(...)`
  - memory-object allocation/lookup and frame allocation paths via
    `with_memory_state(...)` / `with_memory_state_mut(...)`
  - task brk bounds read/write paths via memory accessors

### Phase 4c (completed in this pass)

- Continue task/VM callsite migration:
  - task registration capacity/slot assignment in `task_policy_state.rs` now
    uses `with_tcbs(...)` / `with_tcbs_mut(...)`
  - fault-policy and ASID lookup paths in `boot/mod.rs` now use `with_tcbs(...)`
  - `bind_task_asid` ASID-existence check now uses `with_user_spaces(...)`

### Phase 4d (completed in this pass)

- Migrate thread/task query paths in `boot/thread_state.rs` to task accessors:
  - thread identity/context accessors (`thread_group_id`, `thread_tls_base`,
    `thread_user_context`, `thread_kernel_context`, `thread_detach_state`)
  - default kernel context provisioning index lookup and TLS-restore pending
    query
  - joiner wake scan and parent TCB lookup for thread spawn

### Phase 4e (completed in this pass)

- Migrate remaining thread/exec TCB mutation paths in this partitioning sweep to
  task accessors:
  - `boot/thread_state.rs`: kernel/user context mutation helpers, join state
    transitions, TLS base update, spawn initialization, and frame-sync path now
    use `with_tcbs_mut(...)` / `with_tcbs(...)`
  - `boot/exec_state.rs`: futex wait status transitions, spawn image TCB
    initialization, scheduler running/runnable state transitions, and
    kernel-context switch frame selection now use `with_tcbs(...)` /
    `with_tcbs_mut(...)`

### Phase 4f (completed in this pass)

- Migrate remaining single-TCB state transitions in adjacent boot domains away
  from `tcb_mut(...)` to task accessor APIs:
  - `boot/ipc_state.rs`: endpoint/notification block + wake transitions now set
    waiter/sender task status via `with_tcbs_mut(...)`
  - `boot/restart_state.rs`: exit/restart/dead transitions and restart-token
    checks now use `with_tcbs(...)` / `with_tcbs_mut(...)`
  - `boot/fault_state.rs`, `boot/scheduler_state.rs`, `boot/driver_state.rs`,
    and `boot/memory_state.rs`: migrated remaining task-existence/status updates
    to task accessors

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
