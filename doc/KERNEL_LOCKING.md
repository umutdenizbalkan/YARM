<!-- SPDX-License-Identifier: Apache-2.0 -->

# Kernel locking model audit (current) and decomposition plan

This document records the current kernel locking shape and a staged plan to
remove implicit global-lock coupling from syscall/trap paths.

## Current status

- `SharedKernel` global lock (`SpinLock<KernelState>`) still exists and remains
  the top-level serialization boundary where that runtime path is used.
- Runtime lock behavior has **not** been decomposed yet; stages so far are
  scaffolding/documentation-only.
- Debug lock-order tracking is hosted-dev + debug-assertions only and is
  non-fatal/report-only (`YARM_LOCK_ORDER_WARN ...`).
- Non-hosted `no_std` lock-rank tracking is currently placeholder-only until a
  safe per-CPU/per-thread debug-local slot is introduced.
- Next future step should be a **single narrow behavior-changing Stage 2 split**
  (not broad decomposition in one pass).

## 1) Current global lock boundary (`SharedKernel`)

`src/runtime.rs` wraps `KernelState` in a single `SpinLock<KernelState>`:

- `SharedKernel::with(...)` acquires the global kernel lock for the full closure.
- `SharedKernel::with_cpu(...)` also acquires the same global lock.

As a result, all operations executed through `SharedKernel` are serialized at
the top-level lock boundary before any subsystem `SpinLockIrq` is reached.

## 2) Current `KernelState` lock-bearing fields

`KernelState` currently owns the following lock fields:

- `scheduler_state: SpinLockIrq<SchedulerState>`
- `ipc_state_lock: SpinLockIrq<()>`
- `driver_state_lock: SpinLockIrq<()>`
- `fault_state_lock: SpinLockIrq<()>`
- `restart_state_lock: SpinLockIrq<()>`
- `capability_state_lock: SpinLockIrq<()>`
- `telemetry_state_lock: SpinLockIrq<()>`
- `boot_config_state_lock: SpinLockIrq<()>`
- `vm_state_lock: SpinLockIrq<()>`
- `task_state_lock: SpinLockIrq<()>`
- `memory_state_lock: SpinLockIrq<()>`

## 3) Current lock-touch map for common paths

### 3.1 Syscall/trap dispatch entry

- Syscalls dispatch through `src/kernel/syscall.rs::dispatch(...)` and then call
  into `KernelState` helpers.
- In runtime-hosted paths, these operations are typically entered through
  `SharedKernel::with(...)` (global lock first), then subsystem locks inside
  `KernelState` helpers.

### 3.2 Subsystem lock families touched by common operations

- **Scheduler/task state**
  - `scheduler_state_lock` via scheduler helpers
  - `task_state_lock` via TCB/task metadata access
- **IPC/endpoints**
  - `ipc_state_lock`
- **Capabilities**
  - `capability_state_lock`
- **VM/address spaces**
  - `vm_state_lock`
- **Memory objects / user-memory helpers**
  - `memory_state_lock`
- **Driver metadata/recovery**
  - `driver_state_lock`
- **Fault/restart/timers/telemetry**
  - `fault_state_lock`, `restart_state_lock`, `telemetry_state_lock`
  - timer access currently hangs off scheduler state in `SchedulerState`

## 4) Proposed mandatory lock ordering (documentation baseline)

The following order should be treated as mandatory for any nested lock
acquisition to prevent future lock-order inversions:

1. **Global runtime lock**: `SharedKernel.state` (if present in that path)
2. `scheduler_state_lock`
3. `task_state_lock`
4. `ipc_state_lock`
5. `capability_state_lock`
6. `vm_state_lock`
7. `memory_state_lock`
8. `driver_state_lock`
9. `fault_state_lock`
10. `restart_state_lock`
11. `telemetry_state_lock`
12. `boot_config_state_lock`

Rationale:

- scheduler/task tend to gate run-state decisions;
- IPC/capability are frequently coupled in syscall paths;
- VM/memory/driver/fault/restart are progressively more specialized.

If a path cannot follow this order, it should be called out explicitly and
handled via a dedicated helper with clear lock-contract comments.

## 5) Staged decomposition plan

### Stage 0 (now): document current behavior and lock order

- Keep global `SharedKernel` lock behavior unchanged.
- Treat this document as source of truth for lock order and review checks.

### Stage 1: add lock-contract helpers/assertions

- Introduce small no-nesting or lock-level assertions where possible.
- Ensure multi-lock helpers encode intended order centrally.
- Initial helper scaffolding now exists in `src/kernel/boot/orchestrator_state.rs`:
  - `KernelState::with_scheduler(...)` (alias over scheduler-state access),
  - `KernelState::with_task_state(...)` (alias over task-state/TCB access),
  - `KernelState::debug_lock_order_note(...)` (debug-only, non-enforcing hook),
  - plus lock-order note calls in `with_ipc_state` / `with_ipc_state_mut`.

### Stage 1.5: subsystem hook coverage (current)

- `debug_lock_order_note(...)` hook coverage is now present at all
  `SpinLockIrq` helper entry points for domains:
  - scheduler
  - task
  - ipc
  - capability
  - vm
  - memory
  - driver
  - fault
  - restart
  - telemetry
  - boot_config
- Multi-lock helper acquisition order is explicitly documented inline and kept
  aligned with this document:
  - `with_task_then_capability`: task -> capability
  - `with_scheduler_then_ipc`: scheduler -> ipc
- Hooks remain debug-only/non-enforcing scaffolding; no runtime locking behavior
  change is introduced at this stage.

### Stage 1.6: debug-only rank tracking (current)

- `debug_lock_order_note(domain)` now maps lock domains to rank values based on
  the mandatory order in this document.
- In `debug_assertions + hosted-dev`, rank state is tracked in thread-local
  storage (`LOCK_ORDER_LAST_RANK`) and emits:
  - `YARM_LOCK_ORDER_WARN current=... previous=...`
  when a lower-rank domain is observed after a higher-rank domain.
- This remains non-fatal/report-only instrumentation (no panic/assert).
- On non-hosted `no_std` builds, this stage is currently a documented
  placeholder because a safe generic per-CPU/per-thread debug-local slot is not
  yet wired without behavior-impact risk.

### Stage 2: split high-traffic subsystem lock domains

- Prioritize decomposition across scheduler/task/ipc/vm hot paths.
- Minimize cross-subsystem lock hold durations.
- Suggested narrow candidates (pick one slice first):
  - scheduler/task split
  - IPC endpoint split
  - VM/memory split


### Stage 2A status (helper-only migration started)

- Helper-only migration for scheduler/task access has started.
- Several direct task-table reads were migrated to helper accessors (for example, `with_tcbs(...)`).
- `tcb_mut` remains intentionally direct for now because it returns a mutable reference whose lifetime escapes the helper closure pattern; this is tracked as focused follow-up work.
- `SharedKernel` global lock remains intact in this stage.


### Stage 2B: first partial scheduler read-path split

- Split path: `SharedKernel::scheduler_tick_now_split_read` now reads scheduler ticks by taking `scheduler_state` (`SpinLockIrq`) directly, without going through `SharedKernel::with(...)` for that read-only portion.
- Why safe: this path is strictly read-only (`timer.current_ticks()`), uses the existing scheduler lock domain, and does not mutate scheduler/task state.
- What remains under global lock: all mutation paths and the rest of syscall/dispatch/control-plane state transitions still use `SharedKernel::with(...)` (global lock intact).
- TODO (next call-site migration): switch the runtime trap/dispatch timeout read path to call `SharedKernel::scheduler_tick_now_split_read` instead of reading ticks only through `SharedKernel::with(...)` wrappers.


### Stage 2C status: blocked / no-op under current constraints

- `scheduler_tick_now_split_read` exists as the Stage 2B staged API.
- Stage 2C attempted first caller migration.
- No safe non-IPC/non-VM caller exists yet.
- Current scheduler tick reads that need migration are inside `KernelState` IPC deadline paths.
- Migrating those reads requires explicitly allowing IPC deadline path work in a future Stage 2D slice.
- No behavior change was made for Stage 2C.

### Stage 2D: IPC deadline tick read split

- Exact bridge added: `SharedKernel::ipc_recv_with_deadline_split_bridge`.
- Exact IPC function touched: `KernelState::ipc_recv_until_deadline` (new narrow helper) and existing `ipc_recv_with_deadline` remains unchanged in behavior.
- Why safe: only the timeout deadline tick read is split (`scheduler_tick_now_split_read`); all IPC capability checks, waiter updates, queue mutation, blocking, and dispatch still happen under `SharedKernel::with(...)`.
- What remains under global lock: all IPC mutation/state transitions and the existing syscall path logic.
- Current status: bridge added; syscall recv-timeout path is not yet migrated in this slice because it currently operates on `&mut KernelState` directly.
- TODO: migrate recv-timeout syscall dispatch to call `SharedKernel::ipc_recv_with_deadline_split_bridge` when syscall/trap plumbing is explicitly moved to a `SharedKernel` entry path.

### Stage 2E status: recv-timeout syscall bridge migration blocked (current call graph)

- Target path inspected: `SYSCALL_IPC_RECV_TIMEOUT_NR` / `handle_ipc_recv_timeout`.
- Blocker: syscall dispatch currently enters `handle_ipc_recv_timeout` with `&mut KernelState` from `KernelState::handle_trap(...)`, not through a `SharedKernel` entry that can call `ipc_recv_with_deadline_split_bridge`.
- Minimal migration is therefore not possible in this slice without widening trap/syscall signatures beyond the allowed narrow scope.
- `ipc_recv_with_deadline_split_bridge` remains bridge-only staged for this caller.
- Additional Stage 2E attempt outcome: a narrow `SharedKernel` recv-timeout syscall wrapper cannot be wired into the real trap/syscall dispatch without introducing a new top-level SharedKernel-owned syscall entry callsite (or widening current `KernelState::handle_trap` signatures).
- Under current constraints (no broad trap/syscall refactor), Stage 2E remains blocked in production dispatch shape.
- No behavior change in Stage 2E.

### Stage 2F design: SharedKernel-aware syscall entry (recv-timeout only)

- Audited flow:
  - `src/runtime.rs`: `SharedKernel::with(...)` / `with_cpu(...)` are the global-lock entry points.
  - `src/kernel/boot/fault_state.rs`: `KernelState::handle_trap(...)` dispatches syscall on `&mut KernelState`.
  - `src/kernel/syscall.rs`: `dispatch(...)` routes `SYSCALL_IPC_RECV_TIMEOUT_NR` to `handle_ipc_recv_timeout(...)`, both on `&mut KernelState`.
  - `src/kernel/syscall.rs`: `handle_ipc_recv_timeout(...)` currently reads/uses timeout only via `KernelState` APIs.

- Proposed minimal API (design only, not implemented):
  - Add a narrow optional entry wrapper on `SharedKernel` for syscall dispatch:
    - `SharedKernel::dispatch_syscall_with_split_reads(cpu, frame)`
  - Keep existing `KernelState::handle_trap(...)` + `syscall::dispatch(...)` unchanged for all existing paths.
  - Inside the wrapper, special-case recv-timeout only:
    1. decode syscall number from frame;
    2. if `SYSCALL_IPC_RECV_TIMEOUT_NR` and `timeout_ticks != 0`, call `ipc_recv_with_deadline_split_bridge(...)`;
    3. otherwise fall back to existing `with_cpu(... -> KernelState::handle_trap(...))`.

- Why this avoids re-entrant global locking:
  - Do the split read (`scheduler_tick_now_split_read`) before entering `with(...)`.
  - Ensure the bridge mutation call is the single `with(...)` boundary (no nested `with` while already holding global lock).
  - Keep current `KernelState`-based dispatch path unchanged for non-target syscalls.

- Risks to manage in implementation:
  - Re-entrant global locking if wrapper is called from a path already holding `SharedKernel::state`.
  - Lifetime/borrow pitfalls when combining mutable frame handling with wrapper closures.
  - Lock-order visibility (scheduler split-read then global lock) must remain documented and consistent.
  - CPU-local context correctness (`set_current_cpu`) must be preserved exactly as today.

- Future implementation steps (recv-timeout only):
  1. Add the wrapper method and route exactly one selected entry call site to it.
  2. Emit Stage 2E marker from wrapper path only: `YARM_LOCK_SPLIT_STAGE2E path=recv_timeout_syscall_bridge`.
  3. Keep `ipc_recv` and send-timeout paths unchanged.
  4. Add focused regression tests for timeout outcomes (`TimedOut` vs `WouldBlock`) to prove behavioral parity.

- No behavior change in this Stage 2F design note.

### Stage 2G: SharedKernel syscall-entry seam

- Added seam: `SharedKernel::handle_trap_with_cpu(cpu, trap, frame)`.
- Current behavior: wrapper-only; it forwards to existing behavior via `with_cpu(... -> KernelState::handle_trap(...))`.
- No syscall behavior change in Stage 2G.
- Future Stage 2H intent: special-case recv-timeout syscall at this seam before entering the global-lock mutation path.
- Intended first caller: selected arch trap-entry/runtime dispatch site that currently calls `KernelState::handle_trap(...)` after CPU selection.

### Stage 2G.1 status: callsite migration blocked by current trap callsite ownership

- Searched for clean callsites using `SharedKernel::with_cpu(... -> kernel.handle_trap(...))` and found none outside the new seam itself.
- Existing real trap/syscall callsites currently invoke `KernelState::handle_trap(...)` on `&mut KernelState` directly (for example in `kernel/boot/fault_state.rs`, kernel tests, and `main.rs`).
- Migrating one of those sites to `SharedKernel::handle_trap_with_cpu(...)` would require changing ownership/type at the callsite from `KernelState` to `SharedKernel`, which exceeds this narrow Stage 2G.1 scope.
- No behavior change in Stage 2G.1.

### Stage 2G.2 boundary audit: trap entry ownership handoff

- Audited files/functions (current ownership handoff):
  - `src/runtime.rs`: `SharedKernel::with_cpu(...)` and new seam `SharedKernel::handle_trap_with_cpu(...)`.
  - `src/kernel/boot/fault_state.rs`: `KernelState::handle_selected_arch_trap_entry(...)` forwards into arch trap entry with `&mut KernelState`.
  - `src/arch/trap_entry.rs`: target-ISA shims all accept `&mut KernelState` and call ISA-specific `handle_trap_entry`.
  - `src/arch/{x86_64,aarch64,riscv64}/trap.rs`: `handle_trap_entry(kernel: &mut KernelState, ...)` invokes `kernel.handle_trap_event(...)`.

- Earliest viable `SharedKernel` boundary:
  - The earliest stable boundary is immediately before `KernelState::handle_selected_arch_trap_entry(...)` is invoked by higher-level runtime/arch dispatch code, i.e. where CPU id + trap frame are known but `&mut KernelState` has not yet been borrowed.

- Minimal future signature change for Stage 2H (single trap path first):
  1. Add one new alternate entry function (do not remove existing one), for example:
     - `handle_selected_arch_trap_entry_shared(shared: &SharedKernel, cpu, context, frame)`
  2. In that alternate path, decode trap event as today, then route one selected trap/syscall path through `shared.handle_trap_with_cpu(...)`.
  3. Keep existing `&mut KernelState` entry functions and all non-selected paths unchanged.

- Risks to manage:
  - Re-entrant global lock risk if any caller already holds `SharedKernel::with(...)` before calling the new alternate entry.
  - Lifetime/borrow coupling for mutable trap-frame references when passed through shared wrapper boundaries.
  - Lock-order visibility: split-read stages must remain read-only before entering global-lock mutation path.
  - CPU-local context correctness: `set_current_cpu` behavior must remain exactly equivalent to current `with_cpu` entry semantics.

- No behavior change in this Stage 2G.2 audit note.

### Stage 2H: shared trap-entry seam (alternate entry only)

- Added alternate entry seam: `arch::trap_entry::handle_trap_entry_shared(shared, cpu, context, frame)`.
- Current behavior: forwarding-only; it enters via `SharedKernel::with_cpu(...)` and calls the existing `handle_trap_entry(...)` path unchanged.
- No production caller migration in Stage 2H (staged only).
- Intended future caller: selected runtime/arch trap dispatch boundary identified in Stage 2G.2 before borrowing `&mut KernelState`.
- Future Stage 2I intent: special-case recv-timeout syscall at this shared seam before entering global-lock mutation path.
- No behavior change in Stage 2H.

### Stage 2I seam-callsite status

- Production caller audited: `KernelState::handle_selected_arch_trap_entry(...)` in `src/kernel/boot/fault_state.rs` currently calls `arch::trap_entry::handle_trap_entry(self, ...)`.
- Blocker: this caller only has `&mut KernelState`; it does not have a `&SharedKernel` handle to call `arch::trap_entry::handle_trap_entry_shared(...)`.
- Under current constraints (no trap ownership refactor in this slice), no production callsite migration was performed.
- Stage 2H shared seam remains staged for the future boundary where `SharedKernel` is available before borrowing `&mut KernelState`.
- No behavior change in Stage 2I.

### Stage 2J design audit: caller ownership for `handle_selected_arch_trap_entry`

- Exact callers found for `KernelState::handle_selected_arch_trap_entry(...)`:
  - `src/kernel/boot/tests.rs` test-only callsites (multiple arch trap-entry smoke tests).
  - No production runtime/arch callsite currently invokes this wrapper directly.

- Current production routing shape:
  - ISA trap entry functions in `src/arch/{x86_64,aarch64,riscv64}/trap.rs` accept `&mut KernelState` and call `kernel.handle_trap_event(...)` directly.
  - Therefore, production flow bypasses `KernelState::handle_selected_arch_trap_entry(...)` and reaches kernel trap handling with `&mut KernelState` already borrowed.

- Earliest `SharedKernel`-owned routing point for future Stage 2K:
  - The runtime/arch boundary immediately before ISA trap handlers are invoked, where CPU id + frame/context are known but mutable kernel borrow has not yet been materialized.

- Minimal future patch plan (Stage 2K, design only):
  1. Add one alternate top-level runtime/arch dispatch entry that accepts `&SharedKernel` instead of `&mut KernelState` for one selected trap path.
  2. Route that selected path through `arch::trap_entry::handle_trap_entry_shared(shared, cpu, context, frame)`.
  3. Keep existing `&mut KernelState` trap entry path intact for all other callers until parity is proven.
  4. After routing seam is in place, keep behavior identical and defer recv-timeout special-casing to later stage.

- No behavior change in Stage 2J.

### Stage 2K: top-level SharedKernel trap dispatch seam

- Added top-level seam: `arch::trap_entry::dispatch_trap_entry_with_shared_kernel(shared, cpu, context, frame)`.
- The new seam accepts `&SharedKernel` at the boundary and forwards to `handle_trap_entry_shared(...)`, which preserves existing forwarding behavior.
- Existing `&mut KernelState` trap-entry path remains unchanged.
- Stage 2K status: staged only (no production caller migrated safely in this slice).
- Future Stage 2L intent: special-case recv-timeout syscall at this shared seam before entering global-lock mutation path.
- No behavior change in Stage 2K.

### Stage 2L seam-callsite migration status

- Callsites audited (production + seam-related):
  - `src/arch/aarch64/boot.rs`: vector handoff path calls `aarch64::trap::handle_trap_entry(kernel, ...)` after obtaining `kernel: &mut KernelState` from `trap_kernel_state_mut()`.
  - `src/arch/x86_64/descriptor_tables.rs`: trap stub dispatch paths call `x86_64::trap::handle_trap_entry(kernel, ...)` on `&mut KernelState`.
  - `src/arch/*/trap.rs`: ISA trap handlers accept `&mut KernelState` and call `kernel.handle_trap_event(...)`.
  - `src/kernel/boot/fault_state.rs`: `handle_selected_arch_trap_entry(...)` forwards `&mut KernelState` into `arch::trap_entry::handle_trap_entry(...)`.

- Exact blocker for Stage 2L migration:
  - First production trap-entry callers currently materialize `&mut KernelState` before dispatch; they do not retain `&SharedKernel`.
  - Specifically, `aarch64` vector handoff uses `trap_kernel_state_mut() -> Option<&mut KernelState>` and therefore cannot call `dispatch_trap_entry_with_shared_kernel(...)` without changing the kernel-state handoff type.

- Next type/function that must change for future migration:
  - Introduce a shared-kernel accessor at trap entry (for example `trap_shared_kernel()`), or equivalent ownership change where `trap_kernel_state_mut` callsites can obtain `&SharedKernel` first.

- Stage 2L status: staged-only; no production callsite migrated in this slice.
- No behavior change in Stage 2L.

### Stage 2M: shared-kernel trap accessor seam

- Added sibling accessor at trap-entry state storage boundary (aarch64): `trap_shared_kernel() -> Option<&'static SharedKernel>`.
- Companion installer helper added: `install_trap_shared_kernel(...)` (staged seam only; existing trap path remains unchanged).
- Existing production trap handling still uses `trap_kernel_state_mut() -> &mut KernelState` and then ISA `handle_trap_entry(...)`.
- Future Stage 2N intent: migrate one trap-entry callsite to `dispatch_trap_entry_with_shared_kernel(...)` using this accessor seam.
- No behavior change in Stage 2M.


#### Stage 2M per-ISA parity status

- `aarch64`: parity available (staged accessors exist):
  - `trap_kernel_state_mut() -> Option<&'static mut KernelState>`
  - `trap_shared_kernel() -> Option<&'static SharedKernel>`
- `x86_64`: parity available (staged accessors added):
  - `trap_kernel_state_mut() -> Option<&'static mut KernelState>`
  - `trap_shared_kernel() -> Option<&'static SharedKernel>`
- `riscv64`: parity not applicable yet. Current trap entry takes `&mut KernelState` from caller and does not maintain a local trap-global kernel-state accessor slot in the same style as `aarch64`/`x86_64`.

### Stage 2N: aarch64 trap entry migrated to shared seam

- Migrated aarch64 vector handoff trap path to prefer `trap_shared_kernel()` when available.
- Shared path now routes through `arch::trap_entry::dispatch_trap_entry_with_shared_kernel(...)`.
- Added marker: `YARM_LOCK_SPLIT_STAGE2N path=aarch64_shared_trap_entry`.
- Fallback behavior preserved: if `trap_shared_kernel()` is `None`, code falls back to existing `trap_kernel_state_mut()` + ISA `handle_trap_entry(...)` path.
- Behavior remains unchanged because shared seam forwards through existing `SharedKernel::with_cpu(...)` / `KernelState` trap handling flow.
- Per-ISA status: aarch64 migrated; x86_64 remains staged; riscv64 not applicable yet under current ownership shape.


#### Stage 2N AArch64 activation status

- `trap_shared_kernel()` returned `None` in validation runs because the aarch64 bootstrap path installs only `trap_kernel_state_mut` (`&mut KernelState`) from `Bootstrap::init_static()` and does not currently materialize a long-lived `SharedKernel` instance at that same point.
- `install_trap_shared_kernel(...)` is present as a seam, but there is no safe same-point `SharedKernel` object to install yet without changing ownership/lifetime of the boot kernel object.
- Therefore Stage 2N remains fallback-active on aarch64 for now: shared-dispatch branch is attempted first, then fallback `trap_kernel_state_mut()` path is used when shared pointer is absent.
- Marker behavior remains correct:
  - shared marker on shared-path use
  - fallback marker only when fallback is actually used.


#### Stage 2N final status (current)

- Stage 2N seam callsite migration is in place, but aarch64 remains **fallback-active** in normal boot flow (not fully active shared-path).
- `trap_shared_kernel()` is `None` because bootstrap currently owns/exposes `&mut KernelState` from `Bootstrap::init_static()` rather than a canonical long-lived `SharedKernel` at trap entry installation time.
- Recv-timeout special-casing remains blocked until `SharedKernel` becomes canonical at trap entry boundary.

#### Future Stage 3 design note

- Introduce a canonical boot-owned `SharedKernel` instance and derive `KernelState` access through it.
- Once canonical, route trap/syscall entry through shared seams first, then apply targeted recv-timeout split-read special-casing without altering global-lock mutation semantics.

### Stage 3: remove global lock from syscall fast path

- Route trap/syscall dispatch directly to subsystem locks where safe.
- Keep global lock only for coarse-grain control-plane operations, if needed.

### Stage 4: per-CPU scheduler/runqueue locking

- Move scheduler queues and CPU-local runnable state to per-CPU lock domains.
- Retain cross-CPU coordination only for explicit migration/work-queue paths.

## 6) Scope note

This document is audit/design-only and does not change runtime lock behavior.
