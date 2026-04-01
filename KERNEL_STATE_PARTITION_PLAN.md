# KernelState partition plan (incremental)

This document captures the concrete next steps for splitting `KernelState`
into independently lockable domains while preserving behavior.

## Current status snapshot (after Phase 6)

The original issue (“KernelState is a single monolithic struct”) is **partially
fixed, but not fully eliminated**:

- ✅ **Fixed in practice for major hot domains**: scheduler, IPC, driver, fault,
  restart, VM/user-spaces, task/TCBs, and memory now have explicit lock-backed
  accessor APIs and dedicated lock domains.
- ⚠️ **Still structurally monolithic at the type level**: these domains still
  live as fields inside `KernelState`, so compile-time coupling and some
  cross-domain ergonomics remain.
- ⚠️ **Residual non-partitioned areas remain**: capability/cnode bookkeeping and
  some bootstrap/global counters are still managed as top-level `KernelState`
  concerns rather than extracted domain structs.

### What has been partitioned so far

- Scheduler domain:
  - `SchedulerState` + `with_scheduler_state(...)` /
    `with_scheduler_state_mut(...)` / ordered `with_scheduler_then_ipc(...)`
- IPC domain:
  - `IpcState` (subsystem) + `with_ipc_state(...)` / `with_ipc_state_mut(...)`
- Driver domain:
  - `with_driver_state(...)` / `with_driver_state_mut(...)`
- Fault domain:
  - `with_fault_state(...)` / `with_fault_state_mut(...)`
- Restart domain:
  - `with_restart_state(...)` / `with_restart_state_mut(...)`
- VM domain:
  - `with_user_spaces(...)` / `with_user_spaces_mut(...)`
- Task domain:
  - `with_tcbs(...)` / `with_tcbs_mut(...)`
- Memory domain:
  - `with_memory_state(...)` / `with_memory_state_mut(...)`

### Net effect vs original concern

- The runtime locking model is no longer “single global state lock” for the
  partitioned paths; major subsystem operations are now lock-scoped by domain.
- The codebase still benefits from a future Phase 7+ effort to extract
  capability/cnode and other residual global concerns into dedicated domains to
  reduce remaining monolithic coupling.

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

## Phase 3 (completed)

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

## Phase 4 (completed)

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

## Phase 5 (completed in this pass)

- Complete IPC struct extraction closure work:
  - mark IPC partitioning as complete (Phase 3 status update) now that IPC
    endpoint/notification/waiter/telemetry/mailbox ownership is consistently in
    the `IpcState` domain with accessor-mediated entry points
  - migrate remaining non-IPC-module IPC capacity reads in `boot/mod.rs`
    (`capacity_telemetry`) to `with_ipc_state(...)` accessors
- This closes the extraction loop needed to start the next partitioning phase.

## Phase 6 (completed)

Split the remaining multi-concern boot state into explicit lockable domains for
driver, fault, and restart flows. This phase focuses on reducing lock coupling
with task/ipc/vm/memory paths while preserving behavior.

### Phase 6a (completed in this pass): Driver domain extraction

- Extract `DriverState` from `KernelState` fields currently used by
  `boot/driver_state.rs` and related helpers.
- Add lock/accessor API:
  - `driver_state_lock: SpinLockIrq<()>` (or `SpinLockIrq<DriverState>`)
  - `with_driver_state(...)`
  - `with_driver_state_mut(...)`
- Migrate driver registration/delegation/runtime-capability bookkeeping call
  paths to the new accessor layer.
- Migrate non-driver-module driver telemetry/capacity reads in `boot/mod.rs`
  (`capacity_telemetry`) to `with_driver_state(...)`.
- Focused driver-registration/delegation tests continue to validate behavior
  under the new lock domain.

### Phase 6b (completed in this pass): Fault domain extraction

- Extract `FaultState` from `KernelState` fields currently used by
  `boot/fault_state.rs` and supervisor fault-reporting paths.
- Add lock/accessor API:
  - `fault_state_lock: SpinLockIrq<()>` (or `SpinLockIrq<FaultState>`)
  - `with_fault_state(...)`
  - `with_fault_state_mut(...)`
- Migrate fault recording/reporting/policy call paths to the fault accessor
  layer and keep trap hot paths minimal.
- Migrate adjacent supervisor-endpoint and endpoint-destroy interactions in
  `boot/scheduler_state.rs`, `boot/restart_state.rs`, and `boot/ipc_state.rs`
  to fault accessors.
- Fault-policy and supervisor notification tests continue to validate behavior
  under extracted fault state.

### Phase 6c (completed in this pass): Restart domain extraction + lock-order hardening

- Extract `RestartState` from `KernelState` fields currently used by
  `boot/restart_state.rs` (exit/restart tokens, lifecycle transitions).
- Add lock/accessor API:
  - `restart_state_lock: SpinLockIrq<()>` (or `SpinLockIrq<RestartState>`)
  - `with_restart_state(...)`
  - `with_restart_state_mut(...)`
- Migrate task-exit and restart paths to the restart accessor layer, minimizing
  cross-domain critical sections.
- Add lock-order validation tests to cover any new multi-domain acquisition
  paths introduced by driver/fault/restart extraction.
- Update lock-order guidance (below) to concrete domain order now that
  driver/fault/restart extraction has landed.

## Lock ordering (proposed)

To avoid deadlocks as partitioning progresses, acquire in this order:

1. scheduler
2. task
3. ipc
4. vm
5. memory
6. driver
7. fault
8. restart

No function should acquire a lock earlier in the order after acquiring a later
one.

## Phase 7+ roadmap (planned): residual global-concern extraction

Goal: reduce remaining top-level `KernelState` coupling by extracting
capability/cnode and related global bookkeeping into explicit domains with
lock-backed accessors.

### Proposed Phase 7 domain targets

1. **Capability/CNode domain**
   - Extract:
     - `cnode_spaces`
     - `process_cnodes`
     - delegated capability link table
   - New APIs:
     - `with_capability_state(...)`
     - `with_capability_state_mut(...)`
   - Scope:
     - cspace lookup/mint/revoke/delegate paths
     - process cnode binding/cleanup paths

2. **Telemetry/Global counters domain (optional split)**
   - Extract:
     - cross-cutting counters that are still top-level and not owned by an
       existing domain
   - New APIs:
     - `with_telemetry_state(...)`
     - `with_telemetry_state_mut(...)`
   - Scope:
     - capacity snapshots and global accounting that currently touches mixed
       domains

3. **Bootstrap/Config domain (optional split)**
   - Extract long-lived static/profile/config knobs that are read broadly but
     mutated rarely.
   - New APIs:
     - `with_boot_config(...)`
     - `with_boot_config_mut(...)` (if needed)

### PR execution list (recommended)

The following PR sequence is intentionally narrow to keep reviewable diffs and
safe behavior preservation.

- **PR 7.1: CapabilityState scaffold**
  - Add `CapabilityState` struct + lock field + accessor helpers.
  - Move `cnode_spaces`, `process_cnodes`, delegated capability links into the
    new state, without broad callsite migration yet.
  - Add compile-only refactors and targeted constructor/init updates.

#### PR 7.1 status (completed in this pass)

- Added `CapabilityState` scaffold on `KernelState` with:
  - `capability_state_lock`
  - `with_capability_state(...)`
  - `with_capability_state_mut(...)`
- Moved `cnode_spaces`, `process_cnodes`, and delegated capability link storage
  under the new capability subsystem.
- Per PR 7.1 scope, broad capability callsite migration is deferred to PR 7.2.

- **PR 7.2: Capability callsite migration (core)**
  - Migrate mint/revoke/resolve/grant/delegate/retype paths to
    `with_capability_state(...)` / `with_capability_state_mut(...)`.
  - Keep behavior unchanged; add focused unit tests for cap lifecycle and stale
    capability checks.

#### PR 7.2 status (completed in this pass)

- Migrated core capability-state call paths to the capability accessor layer:
  - process-cnode lookup/set helpers
  - capability capacity-slot accounting
  - delegated-capability link record/cleanup/descendant traversal paths
- Kept behavior-preserving semantics while moving mutation/read ownership to the
  capability domain scaffold introduced in PR 7.1.

- **PR 7.3: Process-CNode and task lifecycle migration**
  - Migrate process-cnode bind/lookup/cleanup and task registration/teardown
    call paths that touch capability mappings.
  - Add tests for process cleanup and cross-task capability delegation.

#### PR 7.3 status (completed in this pass)

- Migrated process-cnode and capability-lifecycle integrations to capability
  accessors in core `boot/mod.rs` paths:
  - process-cnode cleanup bookkeeping
  - cspace ensure/mint/revoke/revoke-direct paths
  - capability lookup helpers used by delegation/lifecycle paths
- Kept task lifecycle semantics unchanged while reducing direct capability-state
  field access in lifecycle-sensitive paths.

- **PR 7.4: Lock-order hardening + helper consolidation**
  - Introduce/adjust ordered acquisition helpers for any new multi-domain
    pairs involving capability state.
  - Add lock-order snapshot tests to prove safe acquisition ordering.

- **PR 7.5: Optional telemetry/global split**
  - If profiling shows meaningful contention or coupling remains, split global
    counters into a dedicated telemetry state.

- **PR 7.6: Optional bootstrap/config split**
  - Extract rarely-mutated configuration/profile state if it still causes broad
    compile/runtime coupling.

### Definition of done for Phase 7

- Capability/cnode/global-link concerns are no longer directly mutated as
  top-level `KernelState` fields.
- Critical cap lifecycle paths run through accessor-backed domain APIs.
- Lock-order guidance and tests are updated to include any new domain edges.
