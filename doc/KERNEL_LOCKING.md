<!-- SPDX-License-Identifier: Apache-2.0 -->

# Kernel locking model audit (current) and decomposition plan

This document records the current kernel locking shape and a staged plan to
remove implicit global-lock coupling from syscall/trap paths.

## Current status

- `SharedKernel` global lock (`SpinLock<KernelState>`) still exists and remains
  the top-level serialization boundary where that runtime path is used.
- Runtime mutation paths still retain the global `SharedKernel` lock, but
  narrow split-read staging is active where explicitly documented below
  (for example recv-timeout deadline pre-read on SharedKernel trap paths).
- Debug lock-order tracking is hosted-dev + debug-assertions only and is
  non-fatal/report-only (`YARM_LOCK_ORDER_WARN ...`).
- Non-hosted `no_std` lock-rank tracking is currently placeholder-only until a
  safe per-CPU/per-thread debug-local slot is introduced.
- Future work must remain narrow and independently validated; this document
  does **not** claim Stage 3/global-lock removal.

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
  - `src/arch/x86_64/descriptor_tables.rs`: trap stub dispatch prefers `dispatch_trap_entry_with_shared_kernel(...)` via `trap_shared_kernel()` (SharedKernel-primary, Option A), with raw-KernelState fallback.
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

### Stage 2N: aarch64 trap entry shared-seam attempt with fallback

- Migrated aarch64 vector handoff trap path to prefer `trap_shared_kernel()` when available.
- Shared path now routes through `arch::trap_entry::dispatch_trap_entry_with_shared_kernel(...)`.
- Added marker: `YARM_LOCK_SPLIT_STAGE2N path=aarch64_shared_trap_entry`.
- Fallback behavior preserved: if `trap_shared_kernel()` is `None`, code falls back to existing `trap_kernel_state_mut()` + ISA `handle_trap_entry(...)` path.
- Behavior remains unchanged because shared seam forwards through existing `SharedKernel::with_cpu(...)` / `KernelState` trap handling flow.
- Per-ISA status: aarch64 migrated (L2B); x86_64 migrated (Option A); riscv64 not applicable yet under current ownership shape.


#### Stage 2N AArch64 activation status (historical pre-L2B note)

- Early Stage 2N validation runs observed `trap_shared_kernel() == None` because
  the then-current AArch64 bootstrap path installed only `trap_kernel_state_mut`
  (`&mut KernelState`) from `Bootstrap::init_static()`.
- That fallback-active status is superseded by Phase L2B below: AArch64
  production boot now materializes a long-lived `SharedKernel`, installs it with
  `install_trap_shared_kernel(shared)`, and uses the shared-dispatch branch from
  the first trap entry onward.
- Marker behavior remains correct:
  - shared marker on shared-path use
  - fallback marker only when fallback is actually used; it must be absent in
    current AArch64 SharedKernel-primary smoke.


#### Stage 2N final status (Phase L2B complete)

- Stage 2N shared-dispatch branch is **fully active** on AArch64 hardware.
- AArch64 production boot (`run_with_prepared_kernel`) now calls `Bootstrap::init_shared_static()`,
  installs the result via `install_trap_shared_kernel(shared)`, and obtains `&mut KernelState`
  for the scheduler boot callback through `SharedKernel::borrow_kernel_for_boot()` (unsafe,
  no-lock; safe only during single-CPU boot before ERET to user space).
- `trap_shared_kernel()` returns `Some(shared)` from the first trap entry onward; the
  fallback `trap_kernel_state_mut()` branch is never taken.
- `TRAP_KERNEL_STATE_PTR` remains null for the duration of the run; `install_trap_kernel_state`
  is not called from the new path.


### Phase L2A: canonical boot-owned SharedKernel construction (complete)

- Added static storage: `BOOTSTRAP_SHARED_KERNEL: MaybeUninit<SharedKernel>` and
  `BOOTSTRAP_SHARED_KERNEL_READY: AtomicU8` (3-state) in `src/kernel/boot/bootstrap_state.rs`.
- Added new Bootstrap API:
  - `Bootstrap::init_shared_static() -> Result<&'static SharedKernel, KernelError>`
  - `Bootstrap::init_shared_static_with_capacity_profile(...)`
  - `Bootstrap::init_shared_static_with_boot_memory_map(...)`
  - `Bootstrap::shared_static_ref() -> Option<&'static SharedKernel>`
- Ownership contract enforced in `init_shared_static_with_boot_memory_map`:
  1. Calls `init_static_with_boot_memory_map` to write `BOOTSTRAP_KERNEL_STATE`.
  2. Immediately `ptr::read`s the bytes out (consuming the `&'static mut` alias).
  3. Moves owned `KernelState` into `SharedKernel::new`, stored in `BOOTSTRAP_SHARED_KERNEL`.
  4. Sets `BOOTSTRAP_SHARED_KERNEL_READY` 0→1 (initializing) via compare-exchange, then
     2 (ready) via Release store after `ptr::write` completes.
  5. Does **not** call `install_trap_kernel_state` or `install_trap_shared_kernel`.
- `shared_static_ref()` gates on `READY == 2`; returns `None` while 0 or 1.
- `Bootstrap::init_static()` signature and behavior: **unchanged**.

### Phase L2B: AArch64 Stage-2N shared trap-entry activation (complete)

- `src/arch/aarch64/boot.rs` `run_with_prepared_kernel` now calls
  `Bootstrap::init_shared_static()` (hardware AArch64 path only), installs the result
  via `install_trap_shared_kernel(shared)`, and calls `shared.borrow_kernel_for_boot()`
  to obtain `&mut KernelState` for the pre-ERET boot callback.
- `SpinLock::data_ptr()` (crate-private, `src/kernel/lock.rs`) and
  `SharedKernel::borrow_kernel_for_boot()` (unsafe, `src/runtime.rs`) added to support
  the boot bypass; neither holds the spinlock across the non-returning ERET.
- `BOOTSTRAP_SHARED_KERNEL_READY` 3-state fix ensures `shared_static_ref()` cannot return
  `Some` before `ptr::write(BOOTSTRAP_SHARED_KERNEL)` completes.
- x86_64 hardware boot path uses `Bootstrap::init_shared_static()` + `install_trap_shared_kernel` + `borrow_kernel_for_boot()` (Option A). Hosted-dev path retains `Bootstrap::init_static()`.
- Smoke-test proof (`AARCH64_LOCK_SPLIT_TRACE=true` run, then reverted):
  - `YARM_LOCK_SPLIT_STAGE2N path=aarch64_shared_trap_entry`: present
  - `fallback=1` marker: absent (count = 0)
  - `PM_ELF_ZC_DONE image_id=7/8/9 zc_pages>0`: confirmed
  - `DRIVER_MANAGER_READY`, `BLKCACHE_SRV_READY`, `VIRTIO_BLK_SRV_READY`: confirmed

### Staged SharedKernel seam inventory (Stage 2B–2N audit)

- `SharedKernel::scheduler_tick_now_split_read` (`src/runtime.rs`)
  - Status: **keep (staged)**
  - Reason: required building block for future recv-timeout split-read once canonical shared ownership exists.

- `SharedKernel::ipc_recv_with_deadline_split_bridge` (`src/runtime.rs`)
  - Status: **keep (staged)**
  - Reason: bridges pre-read tick/deadline into existing IPC mutation path without changing semantics.

- `SharedKernel::handle_trap_with_cpu` (`src/runtime.rs`)
  - Status: **keep (staged)**
  - Reason: core forwarding seam for SharedKernel-owned trap entry migration.

- `arch::trap_entry::handle_trap_entry_shared` and
  `arch::trap_entry::dispatch_trap_entry_with_shared_kernel` (`src/arch/trap_entry.rs`)
  - Status: **keep (staged)**
  - Reason: top-level shared dispatch seam used for controlled migration of trap entry ownership.

- `install_trap_shared_kernel` / `trap_shared_kernel` (`src/arch/aarch64/boot.rs`, `src/arch/x86_64/descriptor_tables.rs`)
  - Status: **active** — both `aarch64` (L2B) and `x86_64` (Option A) production paths call `install_trap_shared_kernel` at boot.
  - `#[allow(dead_code)]` removed from `x86_64` `trap_shared_kernel`; it is now the live primary trap dispatch path.

- No staged API removed in this audit; all retained seams are still relevant to planned canonical SharedKernel ownership transition.

### Phase L3: recv-timeout split-read activation + Stage-2N verification markers (complete)

#### Part A — Low-noise Stage-2N verification markers

Three one-shot or first-occurrence markers were added to confirm the AArch64
shared trap-entry path is correctly installed and used in production:

1. **`YARM_LOCK_SPLIT_STAGE2N_INSTALLED arch=aarch64 shared=1 raw=0`**
   — emitted once in `run_with_prepared_kernel` immediately after
   `install_trap_shared_kernel(shared)` succeeds. Confirms the shared pointer
   was installed and the raw `TRAP_KERNEL_STATE_PTR` was not.

2. **`YARM_LOCK_SPLIT_STAGE2N_FIRST_SHARED_TRAP arch=aarch64`**
   — emitted on the first AArch64 trap entry that takes the shared path
   (`trap_shared_kernel()` returned `Some`). Uses a one-shot
   `AtomicBool` (`STAGE2N_FIRST_TRAP_LOGGED`) so it fires exactly once.

3. **`YARM_LOCK_SPLIT_STAGE2N_FALLBACK arch=aarch64 reason=no_shared_kernel`**
   — emitted if the fallback raw-pointer path is taken. Must be **absent**
   in a correct Phase L2B/L3 run.

AArch64 QEMU smoke proof:
- `Stage2N installed` count = 1 ✓
- `First shared trap` count = 1 ✓
- `Stage2N fallback` count = 0 ✓

#### Part B — recv-timeout split-read activation (SharedKernel trap paths: AArch64 + x86_64 -smp 1)

- The recv-timeout split-read bridge (`SharedKernel::ipc_recv_with_deadline_split_bridge`,
  introduced in Stage 2D) is now **active** on SharedKernel-primary trap/syscall paths for both AArch64 and x86_64 single-core (`-smp 1`).
- Activation point: `arch::trap_entry::handle_trap_entry_shared` detects
  `SYSCALL_IPC_RECV_TIMEOUT_NR` (nr=5) before acquiring the global `SharedKernel`
  lock, pre-reads the scheduler tick under the lighter `scheduler_state` lock, and
  stores an absolute deadline in `SPLIT_RECV_TIMEOUT_DEADLINE[cpu_idx]`
  (`src/kernel/scheduler.rs`).
- Inside `handle_ipc_recv_timeout` (`src/kernel/syscall.rs`), the deadline slot is
  consumed atomically with `swap(0, AcqRel)`. If the slot is non-zero,
  `ipc_recv_until_deadline(cap, deadline)` is called instead of
  `ipc_recv_with_deadline(cap, timeout_ticks)` — avoiding a redundant tick read
  inside the global lock.
- Marker emitted when the split path is taken:
  `YARM_LOCK_SPLIT_RECV_TIMEOUT path=shared_bridge arch=aarch64`
  and `YARM_LOCK_SPLIT_RECV_TIMEOUT path=shared_bridge arch=x86_64`
- Smoke boot does not exercise recv-timeout in the 30-second window; marker is
  absent from smoke logs. A focused unit test exercises the path directly.

What is **not** changed:
- `timeout_ticks == 0` (try-recv) falls through to existing `try_ipc_recv` path.
- All other syscalls (ipc_send, ipc_recv, ipc_call, send-timeout, etc.) are unchanged.
- x86_64 single-core (-smp 1) boot and trap paths are migrated to SharedKernel-primary in Option A. SMP not changed.
- AArch64 fallback path (raw `trap_kernel_state_mut` + direct `handle_trap_entry`)
  is preserved.
- The global `SharedKernel` lock is not removed; all IPC mutation still occurs
  under `SharedKernel::with(...)`.
- This is **not** Stage 3 global-lock removal.

#### Per-CPU deadline staging protocol (`SPLIT_RECV_TIMEOUT_DEADLINE`)

```
SPLIT_RECV_TIMEOUT_DEADLINE: [AtomicU64; MAX_CPUS]   (src/kernel/scheduler.rs)

seam layer (before global lock):
  if syscall == SYSCALL_IPC_RECV_TIMEOUT_NR && timeout_ticks != 0:
    now  = shared.scheduler_tick_now_split_read()          # scheduler lock only
    SPLIT_RECV_TIMEOUT_DEADLINE[cpu_idx].store(
        now.wrapping_add(timeout_ticks), Release)

handler (inside global lock):
  v = SPLIT_RECV_TIMEOUT_DEADLINE[cpu_idx].swap(0, AcqRel)
  if v != 0:
    ipc_recv_until_deadline(cap, v)   # no redundant tick read
  else:
    ipc_recv_with_deadline(cap, timeout_ticks)   # normal path
```

Slot value 0 is reserved as "not staged"; a wrapping deadline that produces
0 is treated as the fallback path (correct: the error is one missed optimization,
not a correctness failure).

#### Tests added (src/kernel/boot/tests.rs)

- `ipc_recv_until_deadline_with_queued_message_succeeds_immediately` — verifies
  `ipc_recv_until_deadline` returns a queued message without blocking.
- `ipc_recv_until_deadline_timeout_wakes_blocked_waiter_on_timer_tick` — verifies
  deadline wakeup behavior matches `ipc_recv_with_deadline`.
- `split_recv_timeout_deadline_slot_is_consumed_exactly_once` — verifies the
  per-CPU deadline slot is cleared atomically after one consume.
- `ipc_recv_with_deadline_split_bridge_returns_none_when_no_sender` — exercises
  `SharedKernel::ipc_recv_with_deadline_split_bridge` from outside any lock,
  proving no nested `with` while already holding the global lock.
- `ipc_recv_with_deadline_split_bridge_zero_ticks_returns_none` — verifies
  zero-tick try-recv behavior via the bridge.

#### Architecture status after Phase L3

| ISA      | Trap path          | SharedKernel canonical | recv-timeout split |
|----------|--------------------|------------------------|--------------------|
| AArch64  | shared-primary     | yes (L2B)              | active (L3)        |
| x86_64   | shared-primary (Option A) | yes (Option A)  | active (L4A)       |
| riscv64  | raw `&mut KernelState` | not applicable     | not active         |

### Phase L3.2: end-to-end staging+consumption test coverage (complete)

Added two unit tests in `src/kernel/boot/tests.rs` verifying the Phase L3
per-CPU deadline staging protocol from the consumer side:

- `staged_deadline_consumed_by_recv_timeout_dispatch` — writes a non-zero
  deadline to `SPLIT_RECV_TIMEOUT_DEADLINE[cpu_idx]` (mimicking
  `handle_trap_entry_shared` on AArch64), then calls `syscall::dispatch` with
  `SYSCALL_IPC_RECV_TIMEOUT_NR`. Asserts the slot is cleared to 0 after
  dispatch, confirming `handle_ipc_recv_timeout` unconditionally consumes
  the staged deadline via `swap(0, AcqRel)` before any branch.
- `staged_deadline_cleared_on_try_recv_dispatch` — same setup with
  `timeout_ticks=0` (try-recv path). Asserts the slot is still cleared,
  confirming unconditional consumption even when the staged value is not used.

Both tests pre-queue a message so `dispatch` does not block. The dispatch
returns `Err(InvalidArgs)` because `user_ptr/user_len` are zero in the unit
test; the slot swap happens before the metadata write, so the assertion is
still valid.

### Option A: x86_64 SharedKernel-primary trap ownership parity (complete)

Migrated x86_64 single-core (-smp 1) production trap dispatch to use the
same SharedKernel-primary path as AArch64.

#### Changes

**`src/arch/x86_64/descriptor_tables.rs`**:
- Added `STAGE2N_FIRST_TRAP_LOGGED: AtomicBool` and
  `STAGE2N_FALLBACK_LOGGED: AtomicBool` one-shot markers.
- `install_trap_shared_kernel`: made `pub`, added
  `register_apic_cpu_mapping(apic_id, BOOTSTRAP_CPU_ID)` call inside it.
- Removed `#[allow(dead_code)]` from `trap_shared_kernel`.
- `yarm_x86_dispatch_trap_from_stub`: added Stage 2N shared path before the
  existing raw-KernelState fallback. Shared path calls
  `dispatch_trap_entry_with_shared_kernel`, detects task switch using
  entering/exiting current-TID snapshots, and logs
  `YARM_LOCK_SPLIT_STAGE2N_FIRST_SHARED_TRAP arch=x86_64` on first use.
  Fallback path logs `YARM_LOCK_SPLIT_STAGE2N_FALLBACK arch=x86_64
  reason=no_shared_kernel` on first use.

**`src/arch/x86_64/boot.rs`** (`run_with_prepared_kernel`, hardware path only):
- Replaced `Bootstrap::init_static*` + `ptr::read` + `install_trap_kernel_state`
  with `Bootstrap::init_shared_static*` + `borrow_kernel_for_boot()` +
  `install_trap_shared_kernel(shared)`.
- Emits `YARM_LOCK_SPLIT_STAGE2N_INSTALLED arch=x86_64 shared=1 raw=0` after
  `install_trap_shared_kernel`.
- `install_trap_kernel_state` is no longer called; `TRAP_KERNEL_STATE_PTR`
  remains null for the run.
- SMP, IRQ, timer, and `run(kernel)` calls are unchanged.

#### Safety argument for `borrow_kernel_for_boot` on x86_64

`borrow_kernel_for_boot` bypasses the `SpinLock`. On AArch64 this is safe
because DAIF masks interrupts until ERET. On x86_64, `enable_interrupts_for_boot`
(STI) is called after `install_trap_shared_kernel` but before `run(kernel)`.
The LAPIC timer deadline is `BOOTSTRAP_TIMER_DEADLINE_TICKS = 50,000,000` ticks
(≈800 ms) >> the bootstrap window (≈50 ms on QEMU), making a timer interrupt
before IRETQ implausible. After `run(kernel)` calls `enter_user_mode_iret -> !`,
the boot reference is effectively dead and all subsequent trap handlers use
`shared.with_cpu()`.

Aliasing constraint: `install_trap_shared_kernel` sets `TRAP_SHARED_KERNEL_PTR`
(shared path); `install_trap_kernel_state` is never called (raw path stays null).
The dispatch function takes the shared branch XOR the raw branch, never both.

#### Smoke proof (x86_64 -smp 1 QEMU run)

- `YARM_LOCK_SPLIT_STAGE2N_INSTALLED arch=x86_64 shared=1 raw=0`: count = 1 ✓
- `YARM_LOCK_SPLIT_STAGE2N_FIRST_SHARED_TRAP arch=x86_64`: count = 1 ✓
- `YARM_LOCK_SPLIT_STAGE2N_FALLBACK`: absent ✓
- All 6 service entries present exactly once ✓
- `[ok] x86_64 boot markers detected` ✓

### Phase L5A: shared-trap task-switch detection split-read (staged helper only)

- Added narrow read-only helper: `SharedKernel::current_tid_split_read(cpu)`.
- Helper behavior: acquires only `scheduler_state` (`SpinLockIrq`) and reads
  `SmpScheduler::current_tid_on(cpu)`; it does not call `SharedKernel::with`,
  does not call `with_cpu`, and does not mutate `current_cpu`, scheduler, or
  task state.
- Production use of this helper in the x86_64 shared trap dispatch was reverted:
  it caused x86_64 startup register/cap corruption after supervisor blocked and
  PM was scheduled. The suspected failure mode was incorrect task-switch
  detection, selecting syscall-return-only writeback when full task frame
  writeback was required.
- x86_64 shared trap dispatch therefore again snapshots `entering_tid` and
  `exiting_tid` through `shared.with_cpu(cpu, |k| k.current_tid())`, matching
  the last known-good Phase 3B boot behavior.
- SharedKernel-primary trap ownership remains active: x86_64 still enters
  `dispatch_trap_entry_with_shared_kernel(...)`; only the read-only
  task-switch snapshots were restored to the conservative global-lock path.
- Register writeback semantics are unchanged: differing TID snapshots still use
  full task-switch frame writeback, while same-task syscall returns still use
  syscall-return-only writeback.
- AArch64 behavior is unchanged. x86_64 SMP remains out of scope and
  `src/arch/x86_64/smp.rs` is not touched.
- The global `SharedKernel` lock still protects mutation paths. This is not
  Stage 3/global-lock removal.

### Phase L5B: current-TID split-read diagnostic comparison (diagnostic-only)

- L5A production use remains rolled back: x86_64 shared trap task-switch
  detection continues to use the conservative `shared.with_cpu(cpu, |k|
  k.current_tid())` snapshots for `entering_tid` and `exiting_tid`.
- `SharedKernel::current_tid_split_read(cpu)` is still staged but is not used
  for production task-switch decisions or register writeback selection.
- Added an x86_64-local diagnostic gate, `X86_TID_SPLIT_READ_DIAG`, defaulting
  to `false`. When explicitly enabled for investigation, the trap path compares
  the conservative snapshot with the split-read snapshot and logs only on
  mismatch:
  - `YARM_LOCK_SPLIT_CURRENT_TID_MISMATCH arch=x86_64 phase=enter ...`
  - `YARM_LOCK_SPLIT_CURRENT_TID_MISMATCH arch=x86_64 phase=exit ...`
- Normal production smoke does not emit these mismatch markers because the gate
  is disabled by default.
- Purpose: diagnose why the split-read TID snapshot did not preserve the x86_64
  register writeback decision path, without changing production behavior.
- AArch64 behavior is unchanged. x86_64 SMP remains out of scope and
  `src/arch/x86_64/smp.rs` is not touched.
- The global `SharedKernel` lock still protects mutation paths. This is not
  Stage 3/global-lock removal.

### Phase L6B: AArch64 trace-only current-TID log read cleanup (complete)

- Removed the unconditional AArch64 shared-vector trace metadata read of
  `current_tid` from normal builds. The `AARCH64_VECTOR_FRAME_FINAL` TID value
  is now computed only when `AARCH64_TRAP_TRACE` is enabled.
- When tracing is enabled, the TID is read with
  `SharedKernel::current_tid_split_read(trap_cpu)` as trace/log metadata only;
  it is not used for register writeback, scheduling, syscall return values,
  startup capability delivery, or IPC behavior.
- Normal builds (`AARCH64_TRAP_TRACE=false`) do not take the global
  `SharedKernel` lock for this trace-only TID field and do not emit additional
  per-trap logs.
- x86_64 task-switch detection is unchanged: conservative `with_cpu` snapshots
  remain authoritative, and `current_tid_split_read` remains diagnostic/staged
  only for x86_64.
- The global `SharedKernel` lock still protects mutation paths. This is not
  Stage 3/global-lock removal.

### Phase L7A: scheduler topology split-read helpers (staged helper only)

- Added narrow read-only helpers:
  - `SharedKernel::online_cpu_count_split_read()`
  - `SharedKernel::present_cpu_count_split_read()`
- Helper behavior: each helper acquires only `scheduler_state`
  (`SpinLockIrq`) and reads the `SmpScheduler` topology counts. They do not
  call `SharedKernel::with`, do not call `with_cpu`, do not mutate
  `current_cpu`, and do not touch scheduler runqueues or task-switch state.
- Ownership/lock domain: `KernelState::online_cpu_count()` and
  `KernelState::present_cpu_count()` already read `SmpScheduler` topology
  through `scheduler_state`; L7A exposes the same read-only data through
  `SharedKernel` without taking the outer global lock. The underlying topology
  bitmaps are atomic and remain accessed while the scheduler lock is held by
  the helper.
- Production callsites are not migrated in this phase. Existing boot logs read
  topology from the boot-owned `KernelState`, and the only `SharedKernel::with`
  topology-count reads found during the audit are test/shared-static sanity
  checks. The new helpers are staged for future low-risk telemetry or boot
  status reads.
- x86_64 SMP remains explicitly out of scope; this phase does not change SMP
  bring-up, CPU online/offline mutation, runqueue selection, task switching,
  register writeback, IPC, VFS, or Phase 3B zero-copy paths.
- The global `SharedKernel` lock still protects mutation paths. This is not
  Stage 3/global-lock removal.

### Phase L8B: boot-config capacity split-read helpers (staged helper only)

- Added narrow read-only helpers:
  - `SharedKernel::capacity_profile_split_read()`
  - `SharedKernel::runtime_capacity_config_split_read()`
- Helper behavior: the helpers acquire only the `boot_config_state_lock`
  (`SpinLockIrq<()>`) and copy out the boot capacity profile or derive the
  corresponding `RuntimeCapacityConfig`. They do not call `SharedKernel::with`,
  do not call `with_cpu`, do not mutate boot configuration, and do not touch
  scheduler, task, IPC, capability, VM, memory, driver, or `current_cpu` state.
- Production callsites are not migrated in this phase. The helpers are staged
  for future low-risk telemetry or boot-configuration reads and are covered by
  a focused helper-level test comparing against the existing `KernelState`
  capacity-profile/runtime-capacity-config accessors.
- x86_64 SMP remains explicitly out of scope; this phase does not change SMP
  bring-up, task switching, register writeback, IPC, VFS, or Phase 3B
  zero-copy paths.
- The global `SharedKernel` lock still protects mutation paths. This is not
  Stage 3/global-lock removal.

### Stage 3B-A: fault bookkeeping split-mutation helpers (helper-only)

- Added narrow helper-only diagnostic fault bookkeeping split-mutation helpers:
  - `SharedKernel::record_fault_split_mut(fault)`
  - `SharedKernel::record_fault_frame_snapshot_split_mut(frame)`
  - `SharedKernel::clear_last_fault_split_mut()`
- Helper behavior: each helper avoids `SharedKernel::with` and `with_cpu`,
  acquires only `fault_state_lock`, and mutates only
  `FaultSubsystem::last_fault` and/or `FaultSubsystem::last_fault_frame`.
  The helpers do not mutate fault handler endpoints, supervisor endpoints,
  fault policy, task state, scheduler state, IPC, capabilities, VM, memory,
  driver state, boot config, or `current_cpu`.
- Production trap paths are not migrated in this phase. Shared trap dispatch
  still enters the existing globally locked mutation path for live trap/syscall
  handling; existing `KernelState` fault bookkeeping behavior is unchanged.
- Stage 3B-B would be a separate later phase if the live SharedKernel trap path
  is migrated to use these helpers before entering the global lock.
- x86_64 SMP remains explicitly out of scope; this phase does not change task
  switching, register writeback, VM fault handling, COW/demand paging, IPC,
  VFS, or Phase 3B zero-copy paths.
- The global `SharedKernel` lock still protects production mutation paths. This
  is not full Stage 3/global-lock removal.

### Stage 3B-C: explicit current-fault behavior source (preparatory only)

- Added a behavior-preserving preparatory refactor for page-fault handling:
  `TrapEvent::PageFault(fault)` still records `last_fault` and
  `last_fault_frame` at the same point as before, but the current unhandled
  page-fault report/log path now uses the explicit `FaultInfo` carried by the
  current event.
- `emit_fault_report_for_fault(faulted_tid, fault)` encodes the supervisor fault
  report from the explicit current `FaultInfo`; `fault_current_task_for_fault`
  logs/reports from that same value. The legacy `emit_fault_report(...)` and
  `fault_current_task(...)` wrappers remain for last_fault-based syscall/raw
  compatibility paths.
- `last_fault` remains diagnostic/compatibility storage. This phase does not
  live-migrate shared-seam fault bookkeeping and does not call the Stage 3B-A
  split-mutation helpers from live trap paths.
- VM fault recovery, COW/demand paging, scheduler task switching, register
  writeback, syscall ABI, IPC cap transfer, VFS, syscall 27, and Phase 3B
  zero-copy paths are unchanged.
- The global `SharedKernel` lock still protects production trap mutation paths.
  This is not full Stage 3/global-lock removal.

### Stage 3B-E: live shared-seam diagnostic fault bookkeeping split-mutation

- SharedKernel trap paths now pre-record only diagnostic page-fault bookkeeping
  before entering the global `SharedKernel` lock. When the shared seam decodes
  `TrapEvent::PageFault(fault)`, it records `last_fault` and
  `last_fault_frame` through the Stage 3B-A split helpers under
  `fault_state_lock`.
- Duplicate recording is prevented explicitly with `FaultBookkeepingMode`:
  raw/non-shared paths use `RecordInHandleTrapEvent` and continue to record in
  `KernelState::handle_trap_event(...)`, while shared pre-recorded page faults
  use `AlreadyRecordedBySharedSeam` and skip only the diagnostic
  `last_fault`/`last_fault_frame` write inside the globally locked behavior
  path.
- All real trap behavior remains under `shared.with_cpu(...)` / the global
  `SharedKernel` lock: VM page-fault recovery, COW, demand paging, unhandled
  fault policy, task faulting/blocking, fault IPC/report delivery, scheduler
  decisions, syscall returns, startup caps, and register writeback are
  unchanged.
- Legacy `emit_fault_report(...)` and `fault_current_task(...)` wrappers remain
  available for syscall/raw last-fault compatibility; current page-fault
  report/log behavior continues to use the explicit `FaultInfo` from Stage
  3B-C.
- x86_64 task-switch detection still uses the conservative `with_cpu` TID
  snapshots, x86_64 SMP remains explicitly out of scope, and Phase 3B
  zero-copy paths are untouched.
- The global `SharedKernel` lock is still retained for real trap behavior. This
  is not full Stage 3/global-lock removal.

### Stage 3C-B: telemetry split-mutation helpers (helper-only)

- Added helper-only telemetry split-mutation scaffolding for simple diagnostic
  counters:
  - `SharedKernel::increment_tlb_shootdown_count_split_mut()`
  - `SharedKernel::add_tlb_shootdown_timeout_count_split_mut(delta)`
- Helper behavior: each helper avoids `SharedKernel::with` and `with_cpu`,
  acquires only `telemetry_state_lock`, and mutates only
  `TelemetrySubsystem::tlb_shootdown_count` or
  `TelemetrySubsystem::tlb_shootdown_timeout_count`. The helpers do not touch
  `current_cpu`, scheduler queues, IPC state, VM/TLB state, task state,
  capabilities, driver state, fault state, boot config, VFS, or Phase 3B paths.
- No live callsites are migrated in this phase. Existing TLB shootdown and
  timeout counter increments still occur in `KernelState` under the existing
  globally locked cross-CPU/TLB paths. Live telemetry migration remains deferred
  because those paths are coupled to TLB invalidation, retired-ASID ACK logic,
  cross-CPU mailbox processing, VM state, and current-CPU sequencing.
- x86_64 SMP remains explicitly out of scope, x86_64 task-switch/register
  writeback remains conservative, and Phase 3B zero-copy paths are untouched.
- The global `SharedKernel` lock remains retained for real mutation paths. This
  is not full Stage 3/global-lock removal.

### Stage 4B: IPC endpoint-domain scaffolding (helper-only)

- Added IPC endpoint-domain scaffolding only; no live syscall path is migrated in
  this phase. Existing `KernelState::ipc_send`, `ipc_recv`,
  `ipc_recv_with_deadline`, `ipc_call`, `ipc_reply`, and syscall handlers
  continue to use the existing globally locked IPC behavior.
- The first future live IPC candidate is intentionally narrow: a plain queued
  receive from a buffered endpoint after endpoint cap/index/generation
  validation, with no transfer flags, no reply-cap flags, no timeout, no sender
  waiter refill, no blocking, no notification path, no recv-v2 blocked
  completion, no user-memory copy, no `TrapFrame` writes, no cap
  materialization, no shared-memory mapping, and no scheduler/TCB mutation.
- `ipc_state_lock` is the initial coarse IPC-domain lock. It protects endpoint
  queues, endpoint waiters, sender waiters, reply records, transfer envelopes,
  notification/IRQ routing state, and IPC telemetry. Per-endpoint locks remain a
  later design step after the coarse IPC-domain contract is proven.
- `ipc_state_lock` must not be held while copying user memory, writing
  `TrapFrame` registers, minting/revoking/transferring capabilities, mapping
  shared memory, mutating scheduler queues, blocking/waking tasks, mutating TCB
  wait metadata, or touching VM/VFS/Phase 3B paths.
- The global `SharedKernel` lock remains retained for production IPC mutation.
  This is not full global-lock removal.

### Stage 4C: strict queued plain receive live IPC endpoint split

- Live `IpcRecv` now tries the IPC endpoint-domain helper only after receive
  capability validation and endpoint identity/generation resolution. The live
  split is limited to a buffered endpoint with an already queued plain message,
  no receiver waiter, no sender waiter/refill case, no timeout, no transfer
  flags, and no reply-cap flags.
- The split branch mutates only the endpoint queue through
  `KernelState::ipc_try_recv_queued_plain_endpoint_only(...)`; recv result
  encoding, `TrapFrame` writes, user-memory copies, and any cap materialization
  remain outside the endpoint helper.
- Every ineligible case falls back to the existing full IPC receive path.
  Blocking, timeout/deadline handling, sender-waiter refill/wake, recv-v2
  completion, cap-transfer, reply-cap, notification, send, call, and reply
  behavior are unchanged.
- The global `SharedKernel` lock remains retained for all other production IPC
  mutation. This is not full global-lock removal.

### Stage 4E: strict plain send enqueue live IPC endpoint split

- Live `IpcSend` now tries the IPC endpoint-domain send helper only after send
  capability validation, endpoint identity/generation resolution, current-TID
  lookup, user-memory or inline payload construction, and message construction.
  The live split is limited to a buffered endpoint with queue capacity, no
  receiver waiter, no sender waiter, no timeout/deadline, no transfer-cap
  argument, no transfer/reply-cap flags, and no transferred-cap handle.
- The split helper mutates only the endpoint queue through
  `KernelState::ipc_try_send_queued_plain_endpoint_only(...)`; queued-send
  telemetry is recorded after a successful split to preserve existing
  diagnostics, and syscall `TrapFrame` writes remain outside the endpoint
  helper.
- Every ineligible case falls back to the existing full IPC send path. Receiver
  direct delivery, recv-v2 blocked completion, sender blocking, timeout/deadline
  handling, cap-transfer, reply-cap, shared-memory transfer, call, reply, recv,
  notification, scheduler/TCB, VM, VFS, and Phase 3B behavior are unchanged.
- The global `SharedKernel` lock remains retained for all other production IPC
  mutation. This is not full global-lock removal.

### Stage 4D: two-phase plain recv sender-waiter refill

- Extended `ipc_try_recv_queued_plain_endpoint_only` to handle the sender-waiter
  refill case using a two-phase lock protocol.
- **Phase 1 (under `ipc_state_lock`)**: when a plain sender waiter is at queue
  head (position 0), the helper dequeues the queued message for the receiver, compact-shifts
  the sender-waiter slot out, and re-enqueues the sender's message into the
  newly freed queue slot. Returns `IpcEndpointRecvResult::ReceivedWithSenderWake(msg, wake_tid)`.
- **Phase 2 (outside `ipc_state_lock`)**: the caller applies the deferred
  scheduler wake via `apply_split_sender_wake_plan(wake_tid)` after the helper
  returns, without holding any IPC endpoint lock.
- `IpcSchedulerPlan::WakeSender(ThreadId)` carries the deferred wake intent from
  the split helper through the syscall handler to the post-lock wake call.
- **Fallback rules** (all cause `Ineligible(SenderWaiterPresent)` and fall back to full path):
  - Sender waiter is present but its message carries `FLAG_CAP_TRANSFER`,
    `FLAG_CAP_TRANSFER_PLAIN`, or `FLAG_REPLY_CAP` — capability materialization
    cannot happen under `ipc_state_lock`.
  - Position 0 is `None` but a later position is `Some` — gap created by
    a prior timeout expiry that cleared position 0 without compacting; the
    queue state is ambiguous so the full path must handle it.
- **Lock contract**: `ipc_state_lock` is held only for endpoint queue mutation
  (dequeue + sender-waiter compact shift + re-enqueue). It is not held while:
  copying user memory, writing `TrapFrame`/register returns, minting/revoking
  capabilities, materializing reply caps, mapping shared memory, mutating
  scheduler queues, blocking/waking tasks, or mutating TCB wait metadata.
- Correctness argument for the deferred wake window: the sender waiter is
  removed from `endpoint_sender_waiters` under `ipc_state_lock`, so no
  double-wake is possible. The sender's message is already in the endpoint queue
  before the lock is released, so the state is consistent even if the system
  is observed between Phase 1 and Phase 2.
- `IpcRecv` syscall dispatch now handles both `Received` and
  `ReceivedWithSenderWake` variants; `IpcRecvTimeout` (any timeout_ticks value)
  also propagates the wake plan through the same split path when Stage 4C/4D/4I
  applies.
- The global `SharedKernel` lock remains retained for all other IPC mutation.
  **This is not full global-lock removal.**

### Stage 4G: IpcRecvTimeout try-recv (timeout_ticks == 0) reuses Stage 4C/4D split

- `handle_ipc_recv_timeout` routes `timeout_ticks == 0` (non-blocking try-recv)
  through `ipc_try_recv_queued_plain_endpoint_only` before falling back to the
  existing `try_ipc_recv` full path.
- When a plain queued message is present, Stage 4C applies: the message is
  dequeued under `ipc_state_lock` and the caller proceeds without blocking.
  When a plain sender waiter is also present, Stage 4D applies: the two-phase
  refill runs under `ipc_state_lock` and the deferred sender wake is applied
  outside the lock before returning.
- All ineligible cases (complex sender-waiter messages, receiver waiter present,
  non-buffered endpoint, empty queue, etc.) fall back to `try_ipc_recv`.
- `IpcSchedulerPlan` is propagated through the timeout_ticks == 0 branch in
  the same way as the `IpcRecv` branch: the wake plan is applied after the split
  helper returns and before the syscall completes.
- `timeout_ticks > 0` (blocking recv-timeout with deadline) was not changed by
  Stage 4G; extended by Stage 4I below.
- The global `SharedKernel` lock remains retained for all other IPC mutation.
  **This is not full global-lock removal.**

### Stage 4I: IpcRecvTimeout nonzero timeout uses split path for immediate queued recv

#### Rationale

Stage 4G restricted the split recv path to `timeout_ticks == 0`.  That guard was
conservative: the deadline is only needed if the receiver must *block* (empty queue,
ineligible endpoint).  When a plain message is already in the queue, dequeuing it is
immediate regardless of the timeout value.

#### Change

In `handle_ipc_recv_timeout` (`src/kernel/syscall.rs`), the Stage 4G/4I split
attempt now runs **before** the `if timeout_ticks == 0 / else` branch:

```
// Old structure:
if timeout_ticks == 0 {
    split_try → try_ipc_recv
} else if preread_deadline { ipc_recv_until_deadline }
  else                     { ipc_recv_with_deadline }

// New structure:
immediate_result = split_try (regardless of timeout_ticks)
if immediate_result.is_some() { use it }
else if timeout_ticks == 0    { try_ipc_recv }
else if preread_deadline       { ipc_recv_until_deadline }
else                           { ipc_recv_with_deadline }
```

#### Correctness argument

- If `ipc_try_recv_queued_plain_endpoint_only` returns `Received` or
  `ReceivedWithSenderWake`, the message was dequeued under `ipc_state_lock` and
  delivery is complete — no blocking needed, deadline unused.
- The `timed_out` computation checks `consume_ipc_timeout_fired_for_tid`:
  because we never registered a timer (split path returned immediately),
  `ipc_timeout_fired == false` and `received == Some(msg)`, so `timed_out = false`.
  `clear_blocked_recv_state` is called with `"immediate_success"`. ✓
- If the split is ineligible (empty queue, non-plain message, complex state),
  the path falls through to `try_ipc_recv` (timeout 0), `ipc_recv_until_deadline`
  (preread deadline), or `ipc_recv_with_deadline` (full timed path) — all
  unchanged.

#### Lock contract (unchanged from Stage 4G)

No new lock acquisitions.  `ipc_state_lock` (rank 4) acquired only inside
`ipc_try_recv_queued_plain_endpoint_only`.  Deferred sender wake via
`apply_split_sender_wake_plan` is applied outside the lock.  The `timed_out`
evaluation and `handle_ipc_recv_result_with_empty_error` call happen after all
locks are released.

#### New telemetry field

`IpcPathTelemetry::queued_recvs: u64` — incremented on each successful split recv
(both Stage 4G and Stage 4I paths).  Added to `crates/yarm-kernel/src/boot.rs`;
both the kernel-internal and the re-exported `yarm_kernel::boot::IpcPathTelemetry`
struct gain the field.  The size-equality assertion in `types.rs` and
`extraction_bridge_tests.rs` remains valid since both sides reference the same type.

#### Test

`ipc_recv_timeout_syscall_nonzero_timeout_uses_split_when_message_queued`:
IpcRecvTimeout with `timeout_ticks=1000` on a pre-loaded endpoint.  Asserts
immediate receipt, empty queue, task status unchanged, `queued_recvs` incremented.

#### Fallback rules

`handle_ipc_recv_timeout` falls back to the full timed path for any of the
following:
- Queue empty or no plain message at split time.
- Endpoint is not buffered, or message has transfer/reply-cap flags.
- Complex sender-waiter state (any sender waiter present alongside a receiver waiter).
- Non-endpoint capability (notification, etc.).

All such cases are routed to `try_ipc_recv`, `ipc_recv_until_deadline`, or
`ipc_recv_with_deadline` exactly as before Stage 4I.

#### What Stage 4I does NOT change

- IPC syscall ABI and SYSCALL_COUNT: unchanged.
- SpawnV5, MemoryObject zero-copy, VFS, syscall 27, VFS_READ_SHARED_REPLY_ENABLED: untouched.
- x86_64 SMP and `src/arch/x86_64/smp.rs`: untouched.
- x86_64 register writeback: unchanged.
- Phase 3B checks: not weakened.
- recv-v2, call, reply, cap-transfer: no change; all fall back to full IPC path.
- The global `SharedKernel` lock is still retained for all other IPC mutation.
  **This is not full global-lock removal.**

### Stage 4D/4G review (clean — no changes required)

The Stage 4D two-phase refill protocol and Stage 4G try-recv routing were
reviewed against the following invariants. Result: **CLEAN**.

#### Sender-waiter refill two-phase protocol

- Phase 1 (under `ipc_state_lock`): dequeue queued message, compact sender-waiter
  slot out, re-enqueue sender's message into freed slot. All three mutations are
  atomic from the perspective of any concurrent observer holding `ipc_state_lock`.
- Phase 2 (outside lock): `apply_split_sender_wake_plan` calls `wake_tid_to_runnable`
  on the sender. The sender's message is already durably in the endpoint queue
  before the lock is released, so the state is consistent in the window between
  Phase 1 and Phase 2.

#### Deferred sender wake correctness

- The sender waiter is removed from `endpoint_sender_waiters[endpoint_idx][0]`
  under `ipc_state_lock`, preventing any double-wake from a concurrent timer expiry
  or another recv operation.
- The global `SharedKernel` lock serializes all syscall/timer paths in the
  current implementation; no concurrent timer tick can process the same sender
  between Phase 1 and Phase 2. The deferred wake is therefore safe.

#### Sender-waiter queue compaction / gap handling

- Queue head (position 0) is the only dequeue position. If position 0 is `None`
  but a later position is `Some`, the split path returns
  `Ineligible(SenderWaiterPresent)` and falls back to the full IPC path.
  This correctly handles the case where a prior timeout expiry cleared position 0
  without compacting.
- After a Stage 4D dequeue, the remaining slots are compact-shifted correctly:
  `queue[idx-1] = queue[idx].take()` for all positions 1..len.

#### timeout_ticks == 0 try-recv routing (Stage 4G)

- `handle_ipc_recv_timeout` correctly routes `timeout_ticks == 0` through
  `ipc_try_recv_queued_plain_endpoint_only` (Stage 4C/4D) before falling back
  to `try_ipc_recv`.
- The `IpcSchedulerPlan` is propagated and applied consistently in both the
  `IpcRecv` and `timeout_ticks == 0` branches.

#### Fallback for complex sender waiters

- Messages with `FLAG_CAP_TRANSFER`, `FLAG_CAP_TRANSFER_PLAIN`, or `FLAG_REPLY_CAP`
  at sender-waiter queue head correctly return `Ineligible(SenderWaiterPresent)`.
- Cap materialization under `ipc_state_lock` is therefore never attempted.

#### recv-v2 compatibility

- Stage 4C/4D recv split operates only on the endpoint queue. It does not inspect
  `blocked_recv_state` or `recv_abi` of any task, and does not call
  `complete_blocked_recv_for_waiter`. recv-v2 waiters are therefore unaffected.

#### cap-transfer / reply-cap compatibility

- Messages with transfer or reply-cap flags are rejected at the queued-message
  check as well as the sender-waiter check, falling back to the full path
  in both cases.

#### ipc_state_lock not held across scheduler/TCB/user-memory/cap/VM/TrapFrame

- Confirmed: `ipc_try_recv_queued_plain_endpoint_only` exits `with_ipc_state_mut`
  before `apply_split_sender_wake_plan` calls `wake_tid_to_runnable`.
- `wake_tid_to_runnable` acquires `task_state_lock` (rank 3) and
  `scheduler_state` (rank 2) — both outside `ipc_state_lock` (rank 4).
- User-memory copy, `TrapFrame` writes, and cap materialization all occur outside
  `ipc_state_lock` in the existing full-path helpers (`complete_blocked_recv_for_waiter`
  and result encoding in `handle_ipc_recv`/`handle_ipc_recv_timeout`).

---

### Stage 4F: plain send to waiting legacy receiver

#### Stage 4F review (complete — two issues found and fixed)

**Issue 1: unlocked waiter TID read (FIXED)**

The original `ipc_endpoint_waiter_tid_direct` helper read
`self.ipc.endpoint_waiters[endpoint_idx]` without holding `ipc_state_lock`.
This was documented as acceptable under the global `SharedKernel` lock
(which serializes all syscall paths), but represented technical debt.

**Fix:** `ipc_try_send_queued_plain_endpoint_only` now returns a new variant
`IpcEndpointSendResult::ReceiverWaiterFound(ThreadId)` when a receiver waiter
is present (with no sender waiters). The TID comes directly from the locked
`ipc_state_lock` read inside that function — no unlocked array access is needed.
`ipc_endpoint_waiter_tid_direct` has been removed entirely.

**Issue 2: no sender-waiter co-presence guard (FIXED)**

The original `ipc_try_send_to_plain_receiver_endpoint_only` did not check for
sender waiters presence. A state with both a receiver waiter and sender waiters
simultaneously (possible if the queue was drained while sender waiters remained)
could have been handled by Stage 4F, which would have enqueued a new message
without serving the earlier sender waiters first — incorrect ordering.

**Fix 1:** `ipc_try_send_queued_plain_endpoint_only` now evaluates both
`endpoint_waiters` and `endpoint_sender_waiters` together under `ipc_state_lock`:
- receiver waiter + NO sender waiters → `ReceiverWaiterFound(tid)` (Stage 4F eligible)
- receiver waiter + sender waiters → `Ineligible(SenderWaiterPresent)` (complex state, full path)
- no receiver waiter + sender waiters → `Ineligible(SenderWaiterPresent)` (Stage 4E can't handle)
- no waiters of either kind → Stage 4E queue-enqueue logic proceeds

**Fix 2:** `ipc_try_send_to_plain_receiver_endpoint_only` also checks for sender
waiters presence under lock as defense-in-depth, returning
`Ineligible(SenderWaiterPresent)` if any are found.

---

#### Stage 4F protocol (post-review, updated by Stage 4H)

- Live `IpcSend` now also tries the Stage 4F split path when `ipc_try_send_queued_plain_endpoint_only`
  returns `ReceiverWaiterFound(receiver_tid)` (implying: no sender waiters, receiver present) and:
  - `transfer_cap.is_none()` (no cap-transfer argument)
  - The waiting receiver is **not** recv-v2 blocked (verified under `task_state_lock` rank 3)
  - The endpoint is buffered and the message carries no cap-transfer or reply-cap flags
  - Note: `send_timeout_ticks` value is irrelevant — delivery is immediate when a receiver waits

#### Two-phase send-to-receiver protocol

- **Phase 1 (under `ipc_state_lock` rank 4)**:
  `ipc_try_send_to_plain_receiver_endpoint_only(endpoint_idx, expected_receiver_tid, msg)`:
  1. Re-verifies `endpoint_waiters[endpoint_idx] == Some(expected_receiver_tid)` — guards
     against timeout clearing the slot between the pre-check and the lock acquisition.
  2. Defense-in-depth: checks no sender waiters present under lock.
  3. Validates message flags (no cap-transfer, no reply-cap).
  4. Enqueues `msg` into the endpoint queue.
  5. Clears `endpoint_waiters[endpoint_idx] = None`.
  6. Returns `IpcEndpointSendResult::EnqueuedWakeReceiver(receiver_tid)`.
- **Phase 2 (outside `ipc_state_lock`)**:
  `apply_split_receiver_wake_plan(receiver_tid)` calls `wake_tid_to_runnable(receiver_tid)`.

#### Lock ordering for the Stage 4F pre-check sequence (post-review)

The pre-check sequence that precedes Phase 1 is:

```
1. ipc_try_send_queued_plain_endpoint_only(...)    // ipc_state_lock (rank 4) — reads TID under lock
   → returns ReceiverWaiterFound(receiver_tid)      // TID came from locked read
2. is_task_recv_v2_blocked(receiver_tid.0)         // task_state_lock (rank 3)
3. ipc_try_send_to_plain_receiver_endpoint_only    // ipc_state_lock (rank 4)
```

Steps 1 and 3 both acquire `ipc_state_lock` (rank 4), but each is a separate
acquisition (the lock is released between them). Step 2 acquires
`task_state_lock` (rank 3) after step 1 has released rank 4 and before step 3
acquires it again. The mandatory ordering rank 3 → rank 4 is respected.

There is no unlocked array access at any point. The TID used in step 2 and step 3
is the value read under `ipc_state_lock` in step 1.

The re-verification in step 3 catches the race where the receiver times out
between steps 1 and 3 (waiter slot is cleared by timeout processing and
re-verification fails → `Ineligible`, full path used).

#### Fallback rules

The Stage 4F/4H path falls back to the full `ipc_send` or `ipc_send_with_deadline` path for any of the following:

- `transfer_cap.is_some()` — cap-transfer requires minting/materialization.
- Receiver is recv-v2 blocked — delivery requires `complete_blocked_recv_for_waiter`.
- Message carries cap-transfer or reply-cap flags.
- Endpoint is not buffered (synchronous endpoints require `switch_to_runnable_tid`).
- Endpoint queue is full.
- Both receiver waiter and sender waiters present (complex ordering, `SenderWaiterPresent`).
- Receiver slot was cleared by timeout race (re-verify inside lock catches this).

Synchronous (non-buffered) endpoint send-to-receiver is explicitly deferred.

#### Lock contract (Stage 4F, post-review)

`ipc_state_lock` is held only for:
- Evaluating receiver/sender waiter state in `ipc_try_send_queued_plain_endpoint_only`
- Re-verifying receiver slot, checking sender waiters, enqueuing msg, clearing slot
  in `ipc_try_send_to_plain_receiver_endpoint_only`

It is **not** held while:
- Checking `is_task_recv_v2_blocked` (task_state_lock rank 3, between the two ipc_state_lock acquisitions)
- Calling `apply_split_receiver_wake_plan` / `wake_tid_to_runnable` (Phase 2)
- Mutating task TCB status or scheduler runqueue (`wake_tid_to_runnable`)
- Copying user memory, writing `TrapFrame` registers, minting/revoking caps, or mapping shared memory

#### API surface (Stage 4F, post-review)

- `IpcEndpointSendResult::EnqueuedWakeReceiver(ThreadId)` — split send success, wake receiver
- `IpcEndpointSendResult::ReceiverWaiterFound(ThreadId)` — pre-screen: locked TID, no sender waiters
- `IpcSchedulerPlan::WakeReceiver(ThreadId)` — deferred receiver wake plan
- `KernelState::is_task_recv_v2_blocked(tid)` — under task_state_lock (rank 3)
- `KernelState::ipc_try_send_to_plain_receiver_endpoint_only(...)` — Phase 1 under ipc_state_lock (rank 4)
- `KernelState::apply_split_receiver_wake_plan(tid)` — Phase 2 wake outside lock
- ~~`KernelState::ipc_endpoint_waiter_tid_direct`~~ — **REMOVED** (unlocked read eliminated)

#### Tests (Stage 4F)

- `endpoint_only_plain_send_to_waiting_receiver_enqueues_and_returns_wake_plan`
  — unit test: directly injects receiver waiter state, verifies `EnqueuedWakeReceiver`.
- `ipc_send_syscall_split_delivers_to_waiting_plain_receiver`
  — integration test: Stage 4F fires, receiver woken, telemetry incremented.
- `ipc_send_syscall_plain_receiver_waiter_uses_stage_4f_split_path`
  — integration test (renamed from `receiver_waiter_falls_back_to_full_path`): verifies
  plain receiver gets message via Stage 4F.
- `endpoint_only_plain_send_rejects_waiters_transfer_and_full_queue` (updated):
  now asserts `ReceiverWaiterFound(tid)` for plain-receiver case and
  `Ineligible(SenderWaiterPresent)` for co-presence case.
- `ipc_send_syscall_receiver_and_sender_waiters_fall_back_to_full_path` (new):
  verifies co-presence of receiver + sender waiters forces `Ineligible(SenderWaiterPresent)`
  from the split helper and successful delivery via the full IPC send path.

#### What Stage 4F does NOT change

- IPC syscall ABI and SYSCALL_COUNT: unchanged.
- SpawnV5, MemoryObject zero-copy, VFS, syscall 27, VFS_READ_SHARED_REPLY_ENABLED: untouched.
- x86_64 SMP and `src/arch/x86_64/smp.rs`: untouched.
- x86_64 register writeback: unchanged.
- Phase 3B checks: not weakened.
- recv-v2, call, reply, cap-transfer: no change; all fall back to full IPC path.
- The global `SharedKernel` lock is still retained for all other IPC mutation.
  **This is not full global-lock removal.**

### Stage 4H: extend Stage 4E/4F split to nonzero-timeout sends

#### Rationale

Stage 4F previously restricted the split path to `send_timeout_ticks == 0`.  That
guard was conservative: the `send_timeout_ticks` deadline is only used if the sender
must *block* (queue full, no receiver waiting). When a plain receiver is already
waiting (Stage 4F) or when the queue has room (Stage 4E), delivery is immediate and
the deadline is entirely irrelevant.

#### Change

In `handle_ipc_send` (`src/kernel/syscall.rs`), the split guard was:

```rust
if send_timeout_ticks == 0 && transfer_cap.is_none() {
```

Changed to:

```rust
if transfer_cap.is_none() {
```

This allows the Stage 4E (pure enqueue) and Stage 4F (receiver-waiter wake) split
paths to fire for any `send_timeout_ticks` value, not just zero.  Cap-transfer sends
are still excluded.

#### Correctness argument

- Stage 4E (`Enqueued`): message is placed in the queue and the function returns —
  no blocking, deadline unused.
- Stage 4F (`ReceiverWaiterFound → EnqueuedWakeReceiver`): message is queued and the
  waiting receiver is woken — no blocking, deadline unused.
- If either split returns `Ineligible` (queue full, complex state, wrong flags, etc.),
  the fallback routing at lines 944–950 correctly calls `ipc_send` (timeout 0) or
  `ipc_send_with_deadline` (timeout nonzero) based on `send_timeout_ticks`.

#### Lock contract (unchanged from Stage 4F)

No new lock acquisitions introduced.  The `ipc_state_lock` discipline and the
rank-3 → rank-4 ordering for `is_task_recv_v2_blocked` are unchanged.

#### Test

`ipc_send_syscall_nonzero_timeout_to_waiting_receiver_uses_split_path`: IpcSend
with `len=0` and `SYSCALL_ARG_INLINE_PAYLOAD1=100` (so `send_timeout_ticks=100`)
to a waiting plain receiver. Asserts receiver becomes Runnable, waiter slot cleared,
message queued, telemetry incremented.

#### Stage 4H review result (clean — no changes required)

Stage 4H was reviewed against five points:

1. **Nonzero-timeout immediate plain send is semantically identical to full path**:
   Stage 4E (queue enqueue) and Stage 4F (receiver waiting) both deliver immediately.
   Neither path registers a timer or blocks the sender; `send_timeout_ticks` is
   consumed only by the full `ipc_send_with_deadline` blocking path, which is
   bypassed when the split succeeds.  **CLEAN.**

2. **Ineligible timeout cases still fall back to `ipc_send_with_deadline`**:
   When `split_send_result = None`, the fallback at `syscall.rs:944–950` routes
   `send_timeout_ticks == 0` to `ipc_send` and `send_timeout_ticks != 0` to
   `ipc_send_with_deadline(cap, msg, send_timeout_ticks)` — unchanged.  **CLEAN.**

3. **`stash_transfer_handle(kernel, None, ...)` has no side effects**:
   The function returns `Ok(None)` immediately at the `let Some(...) else { return Ok(None); }`
   guard on the first line.  No capability resolution, no envelope stash, no
   receiver-tid lookup.  **CLEAN.**

4. **No timeout/deadline blocking behavior changed**:
   The split path returns `Some(Ok(()))` only on immediate success.  All cases
   where the sender would have blocked (full queue, complex waiter state, etc.)
   return `Ineligible`, leaving the deadline path unmodified.  **CLEAN.**

5. **IPC ABI/SYSCALL_COUNT, x86_64 SMP, register writeback, SpawnV5, MemoryObject
   zero-copy, VFS, syscall 27, Phase 3B untouched**:
   The change is a one-line guard removal in `handle_ipc_send`; no other file or
   subsystem was modified.  **CLEAN.**

---

### Stage 4I review (clean — no changes required)

Stage 4I was reviewed against five points:

1. **Nonzero-timeout immediate queued recv is semantically identical to full path**:
   `ipc_try_recv_queued_plain_endpoint_only` dequeues under `ipc_state_lock` and
   returns `Received` or `ReceivedWithSenderWake`. Delivery is complete; the deadline
   is never needed. `timed_out = false` because `consume_ipc_timeout_fired_for_tid`
   returns false (no timer was registered). **CLEAN.**

2. **Ineligible cases still fall back to timed path**:
   When `try_endpoint_split_recv` returns `None`, the fallback branches route
   `timeout_ticks == 0` to `try_ipc_recv`, preread deadline to `ipc_recv_until_deadline`,
   and nonzero timeout to `ipc_recv_with_deadline` — unchanged. **CLEAN.**

3. **`timed_out` computation is safe after split path**:
   The pre-read deadline slot is consumed via `swap(0, AcqRel)` unconditionally
   before the split attempt, so the slot is cleared even when the split fires.
   `consume_ipc_timeout_fired_for_tid` returns false (no timer registered), and
   `received == Some(msg)`, so `timed_out = false`. **CLEAN.**

4. **Telemetry field `queued_recvs` added correctly**:
   `IpcPathTelemetry::queued_recvs` was added to `crates/yarm-kernel/src/boot.rs`
   after `queued_sends`. Size-equality assertions in `types.rs` and
   `extraction_bridge_tests.rs` remain valid since both sides reference the same type.
   **CLEAN.**

5. **IPC ABI/SYSCALL_COUNT, x86_64 SMP, register writeback, SpawnV5, MemoryObject
   zero-copy, VFS, syscall 27, Phase 3B untouched**:
   The change was contained to `handle_ipc_recv_timeout` in `syscall.rs` and the new
   telemetry field in `boot.rs`. No other file or subsystem was modified. **CLEAN.**

---

### Stage 4J: split-recv helper unification and telemetry gap fix

#### Rationale

Two issues were identified after Stage 4I:

1. **Telemetry gap**: `handle_ipc_recv` (non-timeout IpcRecv) called
   `ipc_try_recv_queued_plain_endpoint_only` but did NOT call
   `note_endpoint_only_queued_recv_split()`, while `handle_ipc_recv_timeout` did.
   This caused split recvs via `IpcRecv` to be silently uncounted.

2. **Code duplication**: both `handle_ipc_recv` and `handle_ipc_recv_timeout`
   contained nearly identical inline `match endpoint { CapObject::Endpoint { .. } => ... }`
   split-recv blocks.

#### Change

Extracted a private module-level helper in `src/kernel/syscall.rs`:

```rust
fn try_endpoint_split_recv(
    kernel: &mut KernelState,
    endpoint: CapObject,
) -> Result<Option<(Option<Message>, IpcSchedulerPlan)>, SyscallError>
```

The helper encapsulates:
- `CapObject::Endpoint { .. }` guard
- `resolve_endpoint_index` call
- `ipc_try_recv_queued_plain_endpoint_only` dispatch
- `note_endpoint_only_queued_recv_split()` in both `Received` and
  `ReceivedWithSenderWake` arms ← **fixes the telemetry gap**
- `Ineligible(_) => Ok(None)` and non-endpoint `_ => Ok(None)` fall-throughs

Both `handle_ipc_recv` and `handle_ipc_recv_timeout` now call
`try_endpoint_split_recv(kernel, endpoint)?` instead of inlining this logic.

#### Correctness argument

- Behavior is identical: the helper contains the same match arms as both callers
  had before. No new guards, no removed guards, no new lock acquisitions.
- The `note_endpoint_only_queued_recv_split()` call is side-effect-only telemetry;
  adding it to `handle_ipc_recv` does not alter message delivery, lock order, or
  scheduler plan propagation.
- Both callers still apply the `WakeSender` plan outside the lock after the helper
  returns — unchanged.

#### Lock contract (unchanged from Stage 4C/4D/4G/4I)

No new lock acquisitions. `ipc_state_lock` (rank 4) is acquired only inside
`ipc_try_recv_queued_plain_endpoint_only`. The helper does not hold any lock when it
returns. `note_endpoint_only_queued_recv_split` acquires `telemetry_state_lock` (rank 11)
after `ipc_state_lock` has been released.

#### Tests

No new tests required: the behavior change is telemetry-only, and the telemetry
counter (`queued_recvs`) is already exercised by the Stage 4I test
(`ipc_recv_timeout_syscall_nonzero_timeout_uses_split_when_message_queued`) and the
Stage 4C/4D recv tests. The refactoring path is covered by all existing split-recv
test cases.

#### Stage 4J follow-on fixes (applied in the same pass)

The code review of Stage 4J identified three further improvements that were
applied immediately:

1. **Return type simplification**: `try_endpoint_split_recv` previously returned
   `Result<Option<(Option<Message>, IpcSchedulerPlan)>, SyscallError>`.  The inner
   `Option<Message>` is always `Some` on the split path — `Received` and
   `ReceivedWithSenderWake` both carry a concrete `Message` value.  Simplified to
   `Result<Option<(Message, IpcSchedulerPlan)>, SyscallError>`; callers wrap the
   result with `Some(msg)` when building the `received: Option<Message>` binding.

2. **Stale `timed_out` fix** (`handle_ipc_recv_timeout`): when the split path
   delivers a message immediately, `consume_ipc_timeout_fired_for_tid` was still
   called unconditionally.  A stale `ipc_timeout_fired = true` flag left over from
   a prior syscall that exited before clearing it would cause `timed_out = true`
   even though `received = Some(msg)`, resulting in `clear_blocked_recv_state`
   being called with the wrong reason ("timeout" instead of "immediate_success").
   Fixed by tracking `split_recv_succeeded: bool` and skipping the fired-flag
   consume when the split path succeeded.

3. **Missing telemetry assertions**: the Stage 4G test
   (`ipc_recv_timeout_try_recv_uses_split_path`) and the IpcRecv syscall test
   (`ipc_recv_syscall_uses_endpoint_only_plain_queued_branch_without_scheduler_mutation`)
   did not assert `queued_recvs`.  Both tests now capture `before_queued_recvs`
   and assert `queued_recvs == before_queued_recvs + 1` after the split recv.

#### What Stage 4J does NOT change

- IPC syscall ABI and SYSCALL_COUNT: unchanged.
- SpawnV5, MemoryObject zero-copy, VFS, syscall 27, VFS_READ_SHARED_REPLY_ENABLED: untouched.
- x86_64 SMP and `src/arch/x86_64/smp.rs`: untouched.
- x86_64 register writeback: unchanged.
- Phase 3B checks: not weakened.
- The global `SharedKernel` lock is still retained for all other IPC mutation.
  **This is not full global-lock removal.**

---

### Stage 4K: recv-v2 blocked delivery split

#### Background — why the earlier deferred analysis was wrong

The prior deferral note described a Phase-1 design that cleared
`endpoint_waiters[endpoint_idx]` under `ipc_state_lock` *before* calling
`complete_blocked_recv_for_waiter`.  If completion then failed, the waiter slot
was already cleared and the receiver was orphaned with no recovery path.

The correct design avoids this by **not** clearing the waiter slot in Phase 1.
The slot is only cleared in a separate Phase 4 under `ipc_state_lock`, and only
after Phase 3 (completion) has already succeeded.  On completion failure the
waiter slot still holds the receiver TID — the same orphaned state that the full
`ipc_send_with_optional_deadline` path produces (which also does not clear the
waiter on completion failure).

#### Rationale

When `IpcSend` hits a receiver blocked in a recv-v2 operation (Stage 4F returns
`ReceiverWaiterFound(receiver_tid)` and `is_task_recv_v2_blocked` = true), the
previous code fell back to the full `ipc_send` path.  Stage 4K eliminates that
fallback for the common case where delivery can succeed.

#### Three-phase protocol

1. **Phase 1** (under `ipc_state_lock`, inside `ipc_try_send_queued_plain_endpoint_only`):
   returns `ReceiverWaiterFound(receiver_tid)` — **snapshot only**, waiter slot
   is not cleared.

2. **Phase 2** (under `task_state_lock`, rank 3): `is_task_recv_v2_blocked` confirms
   the receiver is in a recv-v2 blocked state.  Acquiring task lock before ipc lock
   preserves lock ordering (scheduler/task rank 3 < ipc rank 4).

3. **Phase 3** (outside all sub-locks): `complete_blocked_recv_for_waiter` —
   copies payload to receiver's user buffer, performs cap materialization if needed,
   writes TrapFrame registers.  `blocked_recv_state.take()` at entry atomically
   claims the state.  On failure the receiver is orphaned (same semantics as the
   full path).

4. **Phase 4** (under `ipc_state_lock`, rank 4): `ipc_clear_plain_receiver_waiter_only`
   re-verifies the slot still holds `receiver_tid` and clears it.  Under the global
   kernel lock no concurrent mutation is possible, so this always matches.

5. **Phase 5** (outside lock): `apply_split_receiver_wake_plan(receiver_tid)` wakes
   the receiver.

#### Correctness argument

- The waiter slot is cleared (Phase 4) only after completion has succeeded (Phase 3).
  If Phase 3 fails the waiter slot is untouched — no regression relative to the
  full path.
- The message is never enqueued into the endpoint queue.  If Phase 3 fails, the
  message stays with the sender (caller gets Err from the `?` propagation) and no
  queue corruption occurs.
- `complete_blocked_recv_for_waiter` is called with the same pre-conditions as in
  the full `ipc_send_with_optional_deadline` path: receiver TID known, global lock
  held, `blocked_recv_state` present.
- Lock ordering is identical to Stages 4E/4F: task lock (rank 3) → ipc lock (rank 4).
  Phase 3 and Phase 5 hold no sub-lock.

#### Lock contract (Stage 4K)

```
Phase 1: ipc_state_lock (rank 4) — held by ipc_try_send_queued_plain_endpoint_only
Phase 2: task_state_lock (rank 3) — held by is_task_recv_v2_blocked
         → released before Phase 3
Phase 3: no sub-lock held — complete_blocked_recv_for_waiter runs lock-free
Phase 4: ipc_state_lock (rank 4) — held by ipc_clear_plain_receiver_waiter_only
         → released before Phase 5
Phase 5: no sub-lock held — apply_split_receiver_wake_plan runs lock-free
```

No lock is held across a user-memory copy, TrapFrame write, or capability
materialization.  The ordering invariant (rank 3 before rank 4) is preserved.

#### API surface (Stage 4K)

New methods added to `KernelState` (in `src/kernel/boot/ipc_state.rs`):

- `ipc_clear_plain_receiver_waiter_only(endpoint_idx, expected_receiver_tid)`:
  clears `endpoint_waiters[endpoint_idx]` under `ipc_state_lock` iff the slot
  still matches `expected_receiver_tid`.
- `note_split_recv_v2_delivery()`: increments `split_recv_v2_deliveries` telemetry
  counter.

New telemetry field in `IpcPathTelemetry` (`crates/yarm-kernel/src/boot.rs`):

- `split_recv_v2_deliveries: u64` — counts successful Stage 4K deliveries.

#### Tests (Stage 4K)

- `ipc_send_syscall_delivers_directly_to_recv_v2_blocked_receiver`
  (`src/kernel/boot/tests.rs`): integration test — task 1 blocks in IpcRecv with
  recv-v2 args (meta_ptr != 0, meta_len ≥ 40); task 0 sends via IpcSend; Stage 4K
  fires; asserts task 1 is Runnable, waiter slot cleared, endpoint queue empty
  (message delivered directly), `split_recv_v2_deliveries` incremented, and payload
  written to task 1's user memory.

#### Stage 4K code review (2026-05-30)

A deep review across 10 correctness dimensions was performed before Stage 4L work began:

1. **Phase ordering** — Phase 1 (snapshot TID under `ipc_state_lock`) precedes Phase 3 (delivery outside lock) precedes Phase 4 (clear slot under `ipc_state_lock`) precedes Phase 5 (wake outside lock). Correct.
2. **Lock ordering** — `is_task_recv_v2_blocked` acquires `task_state_lock` (rank 3) before `ipc_clear_plain_receiver_waiter_only` re-enters `ipc_state_lock` (rank 4). Correct.
3. **Slot re-verification** — `ipc_clear_plain_receiver_waiter_only` re-reads the slot and only clears it if it still matches `expected_receiver_tid`. Defence-in-depth. Correct.
4. **Error recovery** — On `complete_blocked_recv_for_waiter` failure, `?` propagates out of `handle_ipc_send` before the slot is cleared. The receiver's `blocked_recv_state` was already consumed (`take()`), leaving the receiver orphaned — same semantics as the full `ipc_send` path. No new orphan risk introduced.
5. **`split_unsafe_flags` bypass** — The flag check is on the no-waiter enqueue path only; `ReceiverWaiterFound` arm has no such check. Stage 4K only reaches this arm inside the `transfer_cap.is_none()` guard in `handle_ipc_send`, which already excludes FLAG_REPLY_CAP / FLAG_CAP_TRANSFER messages. Correct.
6. **Telemetry** — `note_split_recv_v2_delivery` uses `saturating_add`. Correct.
7. **No queue bypass** — Message is never enqueued when `ReceiverWaiterFound` fires; queue remains empty. Correct.

**Verdict**: Stage 4K is clean. No bugs found.

#### What Stage 4K does NOT change

- IPC syscall ABI and SYSCALL_COUNT: unchanged.
- SpawnV5, MemoryObject zero-copy, VFS, syscall 27, VFS_READ_SHARED_REPLY_ENABLED: untouched.
- x86_64 SMP and `src/arch/x86_64/smp.rs`: untouched.
- x86_64 register writeback: unchanged.
- Phase 3B checks: not weakened.
- The global `SharedKernel` lock is still retained.  **This is not full global-lock removal.**

---

### Stage 4L: IpcCall recv-v2 blocked delivery split

**Implemented**: 2026-05-30. Affects `handle_ipc_call` in `src/kernel/syscall.rs`.

#### Motivation

Stage 4K showed that `ipc_try_send_queued_plain_endpoint_only` returns
`ReceiverWaiterFound(receiver_tid)` for any flag combination when a receiver
waiter is present — the `split_unsafe_flags` check only fires on the no-waiter
enqueue branch. Stage 4K exploited this for plain `IpcSend` messages.

`IpcCall` messages carry `FLAG_REPLY_CAP`. Previously, when the receiver was
recv-v2 blocked, `handle_ipc_call` fell through to the full `ipc_send` path.
Stage 4L eliminates that fallback by applying the same Phase 1–5 protocol.
`complete_blocked_recv_for_waiter` already handles `FLAG_REPLY_CAP` correctly
via `materialize_received_message_cap`.

#### Protocol (Phase 1–5, identical structure to Stage 4K)

```
Phase 1  ipc_try_send_queued_plain_endpoint_only(endpoint_idx, msg)
           → under ipc_state_lock (rank 4)
           → ReceiverWaiterFound(receiver_tid) when receiver waiter present
             with no sender waiters; FLAG_REPLY_CAP does not block this arm.

Phase 2  is_task_recv_v2_blocked(receiver_tid.0)
           → acquires task_state_lock (rank 3)
           → must precede any ipc_state_lock re-acquisition (lock ordering)

Phase 3  complete_blocked_recv_for_waiter(kernel, receiver_tid.0, &msg)
           → runs OUTSIDE ipc_state_lock
           → copies payload and meta to receiver's user memory
           → calls materialize_received_message_cap → mints reply cap in
             receiver's cnode (capability_state_lock rank 5, no ipc lock held)
           → sets FLAG_REPLY_CAP in meta[24..32] via SYSCALL_RECV_META_REPLY_CAP
           → on failure: take_transfer_envelope cleans up stashed reply cap using
             already-captured sender_tid (avoids second current_tid() call)
             then propagates SyscallError

Phase 4  ipc_clear_plain_receiver_waiter_only(endpoint_idx, receiver_tid)
           → re-enters ipc_state_lock (rank 4)
           → clears endpoint_waiters[endpoint_idx] only if slot still matches
             expected_receiver_tid (defence-in-depth re-verify)

         note_ipc_call_split_delivery()
           → increments ipc_call_split_deliveries (saturating)

Phase 5  apply_split_receiver_wake_plan(receiver_tid)
           → OUTSIDE ipc_state_lock
           → wake_tid_to_runnable(receiver_tid) sets task Runnable and enqueues
             on the scheduler runqueue
```

If Phase 1 returns any result other than `ReceiverWaiterFound`, or if Phase 2
finds the receiver is not recv-v2 blocked, the code falls through to the existing
`kernel.ipc_send(cap, msg)` full path unchanged.

#### Lock contract (Stage 4L)

- No `ipc_state_lock` held during: user-memory copy (Phase 3), capability minting
  (Phase 3), `task_state_lock` acquisition (Phase 2), or scheduler mutation (Phase 5).
- Lock ordering: `task_state_lock` (rank 3) → `ipc_state_lock` (rank 4) →
  `capability_state_lock` (rank 5). Phases 2, 3, 4 are sequentially non-overlapping.
- `ipc_state_lock` is entered at Phase 1 (via `ipc_try_send_queued_plain_endpoint_only`)
  and again at Phase 4 (via `ipc_clear_plain_receiver_waiter_only`) with no nesting.

#### Transfer envelope cleanup (error path)

If `complete_blocked_recv_for_waiter` fails, the reply-cap transfer envelope stashed
by `stash_transfer_handle` must be cleaned up. The critical invariant: `stash_transfer_handle`
binds the envelope to `receiver_tid = Some(waiter_tid)` (read via `endpoint_waiter_tid`).
`take_transfer_envelope` checks the bound receiver and returns `None` if the caller's
`receiver_tid` argument does not match. Stage 4L therefore passes `receiver_tid` (the
waiter TID from Phase 1) — **not** `sender_tid` — to `take_transfer_envelope`:

```rust
Err(e) => {
    // Use receiver_tid (bound waiter TID), not sender_tid — the envelope was
    // stashed with receiver_tid bound to the waiter.
    if let Some(handle) = msg.transferred_cap().map(|c| c.0) {
        let _ = kernel.take_transfer_envelope(handle, endpoint, receiver_tid);
    }
    return Err(e);
}
```

Passing `sender_tid` instead of `receiver_tid` would cause `take_transfer_envelope` to
return `None` (bound-receiver mismatch), permanently leaking the reply-cap slot
(bug fixed in Stage 4L review pass, 2026-05-30).

#### API surface (Stage 4L)

New items in `IpcPathTelemetry` (`crates/yarm-kernel/src/boot.rs`):

- `ipc_call_split_deliveries: u64` — counts successful Stage 4L deliveries.

New method on `KernelState` (`src/kernel/boot/ipc_state.rs`):

- `note_ipc_call_split_delivery()` — increments `ipc_call_split_deliveries` (saturating).

#### Tests (Stage 4L)

- `ipc_call_syscall_delivers_directly_to_recv_v2_blocked_receiver`
  (`src/kernel/boot/tests.rs`): integration test — task 1 blocks in IpcRecv with
  recv-v2 args (meta_ptr != 0, meta_len ≥ 40) on endpoint_A; task 0 issues IpcCall
  using a send cap on endpoint_A and a reply_recv_cap on endpoint_B (reply channel);
  Stage 4L fires; asserts task 1 is Runnable, endpoint_A waiter slot cleared,
  queue empty (message delivered directly), `ipc_call_split_deliveries` incremented,
  `SYSCALL_RECV_META_REPLY_CAP` bit set in meta[24..32], and sender tid correct
  in meta[32..40].

#### Stage 4L code review (2026-05-30)

**Bug found and fixed**: The original Stage 4L error path (when `complete_blocked_recv_for_waiter`
fails) called `take_transfer_envelope(handle, endpoint, ThreadId(sender_tid))`. However
`stash_transfer_handle` stashes the envelope with `receiver_tid = Some(waiter_tid)` (from
`endpoint_waiter_tid`), and `take_transfer_envelope` enforces that the caller passes the
matching bound receiver TID. With `sender_tid` (not `waiter_tid`), `take_transfer_envelope`
returns `None` and the transfer envelope slot is permanently leaked. Fixed by using
`receiver_tid` (the waiter TID from Phase 1) in the error path cleanup.

**Simplification**: Removed redundant outer `{ }` block wrapping the match expression.
Split the double-check of `call_split_wake` (`is_none()` + `if let Some`) into a single
`if let Some(recv_tid)` / fallthrough structure.

**Other findings (not fixed — pre-existing or latency-only)**:
- `ipc_try_send_queued_plain_endpoint_only` does not check `endpoint.mode()` before
  returning `ReceiverWaiterFound`, so Stage 4K/4L fire on synchronous endpoints and bypass
  `switch_to_runnable_tid`. This is a latency deviation, not a correctness bug — the receiver
  is still woken and will run. Same behavior applies to Stage 4K (pre-existing).
- When `ReceiverWaiterFound` fires but `is_recv_v2` is false, Stage 4L falls to `ipc_send`
  (correct); `ipc_send` does a second TCB scan (O(n) redundancy). Pre-existing in design.
- `current_tid(kernel)?` in the `ipc_send` fallback error path cannot fail mid-syscall
  (REFUTED as a real bug — `self.current` is set at syscall entry and never cleared).

#### What Stage 4L does NOT change

- IPC syscall ABI and SYSCALL_COUNT: unchanged.
- SpawnV5, MemoryObject zero-copy, VFS, syscall 27, VFS_READ_SHARED_REPLY_ENABLED: untouched.
- x86_64 SMP and `src/arch/x86_64/smp.rs`: untouched.
- x86_64 register writeback: unchanged.
- Phase 3B checks: not weakened.
- The global `SharedKernel` lock is still retained.  **This is not full global-lock removal.**
- IpcReply recv-v2 path: now has telemetry (see Stage 4M below), but delivery logic unchanged.

---

### Stage 4M: IpcReply recv-v2 delivery telemetry

**Implemented**: 2026-05-30. Affects `ipc_reply` in `src/kernel/boot/ipc_state.rs`.

#### Motivation

The `ipc_reply` function (lines 937–985 of `ipc_state.rs`) has had a recv-v2 direct delivery
path since before Stage 4K: when the requester is already blocked in a recv-v2 operation
on the reply endpoint, `complete_blocked_recv_for_waiter` delivers the reply directly to the
requester's user buffers and `wake_waiter_for_endpoint` clears the waiter slot and wakes the
task. This path was undocumented and had no telemetry counter.

Stage 4M adds a telemetry counter (`ipc_reply_split_deliveries`) and an integration test
to make this path auditable alongside Stage 4K and Stage 4L.

#### Protocol (existing path, now instrumented)

The `ipc_reply` recv-v2 delivery path predates the Phase 1–5 discipline of Stage 4K/4L.
It reads `endpoint_waiters[endpoint_idx]` directly (not via `ipc_try_send_queued_plain_endpoint_only`),
then calls `with_tcbs` to check recv-v2 status, then `complete_blocked_recv_for_waiter`, then
`wake_waiter_for_endpoint` (which internally takes and clears the waiter slot under
`ipc_state_lock`). All of this is safe under the global `SharedKernel` lock.

#### API surface (Stage 4M)

New items in `IpcPathTelemetry` (`crates/yarm-kernel/src/boot.rs`):

- `ipc_reply_split_deliveries: u64` — counts successful ipc_reply recv-v2 deliveries.

New method on `KernelState` (`src/kernel/boot/ipc_state.rs`):

- `note_ipc_reply_split_delivery()` — increments `ipc_reply_split_deliveries` (saturating).

#### Tests (Stage 4M)

- `ipc_reply_increments_split_delivery_telemetry_for_recv_v2_waiter`
  (`src/kernel/boot/tests.rs`): integration test — task 1 (requester) calls IpcCall,
  task 2 (replier) receives via recv-v2 and obtains local reply cap, task 1 blocks on
  recv-v2 for the reply, task 2 calls `ipc_reply`; asserts task 1 is Runnable, reply
  endpoint queue empty (direct delivery), `ipc_reply_split_deliveries` incremented,
  reply payload in task 1's user buffer.

#### What Stage 4M does NOT change

- IPC syscall ABI and SYSCALL_COUNT: unchanged.
- The `ipc_reply` delivery logic: unchanged — only telemetry added.
- The global `SharedKernel` lock is still retained.  **This is not full global-lock removal.**

---

### Stage 4N: Transfer-envelope cleanup audit and `ipc_reply` Phase 1–5 normalization

Stage 4N completes two independent tasks that were identified during the Stage 4L/4M
review: (1) fix latent transfer-envelope cleanup bugs in `handle_ipc_send` and
`handle_ipc_call` fallback paths, and (2) normalize `ipc_reply`'s recv-v2 waiter
read to the Phase 1–5 `ipc_state_lock` discipline.

---

#### Part 1: Transfer-envelope cleanup audit (BUG 1 and BUG 2)

**Root cause**: `stash_transfer_handle` (in `src/kernel/syscall.rs`) calls
`endpoint_waiter_tid(endpoint)` internally and passes the result as `receiver_tid`
to `stash_transfer_envelope`.  When a receiver waiter is present, `receiver_tid =
Some(waiter_tid)`.  The envelope is then **bound** to that waiter TID.

`take_transfer_envelope` enforces the bound-receiver invariant
(`src/kernel/boot/transfer_state.rs` lines 83–87):
```rust
if let Some(bound_receiver) = envelope.receiver_tid {
    if bound_receiver != receiver_tid {
        return None;  // ← wrong TID: envelope stays occupied forever
    }
}
```

**BUG 1 — `handle_ipc_send` error path** (fixed): cleanup after `ipc_send` failure
passed `ThreadId(current_tid(kernel)?)` (= sender_tid) instead of the receiver TID
that was bound at stash time.  When a waiter was present, the bound-receiver check
failed, `take_transfer_envelope` returned `None`, and the envelope slot was
permanently leaked.

**BUG 2 — `handle_ipc_call` ipc_send fallback error path** (fixed): same pattern —
the ipc_call fallback to `ipc_send` passed sender_tid in cleanup, while the envelope
stashed at lines 1300/1311 was bound to the receiver TID from `endpoint_waiter_tid`.

**Fix**: `stash_transfer_handle` now returns `(Option<u64>, Option<ThreadId>)` — the
handle AND the bound receiver TID captured at stash time.  All call sites store the
bound TID and use it in cleanup:

```rust
// Before (buggy):
let _ = kernel.take_transfer_envelope(handle, endpoint,
    crate::kernel::ipc::ThreadId(current_tid(kernel)?));  // wrong: sender_tid

// After (correct):
let cleanup_tid = stash_bound_receiver_tid
    .unwrap_or(crate::kernel::ipc::ThreadId(sender_tid));
let _ = kernel.take_transfer_envelope(handle, endpoint, cleanup_tid);
```

`unwrap_or(sender_tid)` is safe: when `stash_bound_receiver_tid = None` (no waiter at
stash time), the envelope was stored with `receiver_tid: None`, so the bound-receiver
check in `take_transfer_envelope` is skipped and any TID is accepted.

Call sites updated:
- `handle_ipc_send`: 4 stash calls (two shared-mem, two inline, across user-asid and
  register branches); cleanup at the `send_result` error path.
- `handle_ipc_call`: 2 stash calls (user-asid and register branches); cleanup at the
  `ipc_send` fallback error path.
- `handle_ipc_reply`: 1 stash call; cleanup at the `ipc_reply` error path (previously
  used `sender_tid` which was also correct when the reply endpoint has no bound waiter,
  but normalized to use `stash_bound_reply_tid.unwrap_or(sender_tid)` for consistency).

The Stage 4L error path (within the `ReceiverWaiterFound` arm) was **already correct**
from the previous stage — it has direct access to `receiver_tid` from the match arm
and uses it directly.

---

#### Part 2: `ipc_reply` recv-v2 Phase 1–5 normalization

**Before normalization** (`ipc_reply` in `src/kernel/boot/ipc_state.rs`):

Phase 1 read `endpoint_waiters[endpoint_idx]` directly without `with_ipc_state`:
```rust
if let Some(waiter_tid) = self.ipc.endpoint_waiters[endpoint_idx] {  // unlocked
```

Phase 4/5 called `wake_waiter_for_endpoint(endpoint_idx)` which does an unlocked
`.take()` on `endpoint_waiters[endpoint_idx]` inside the combined clear+wake helper:
```rust
self.wake_waiter_for_endpoint(endpoint_idx)?;  // unlocked .take() + wake
```

These are safe under the global kernel lock but inconsistent with the Phase 1–5
`ipc_state_lock` discipline established in Stage 4K/4L.

**After normalization**:

- **Phase 1** (snapshot): `self.with_ipc_state(|ipc| ipc.endpoint_waiters[endpoint_idx])`
  — snapshot under `ipc_state_lock` (rank 4), lock released before Phase 2.
- **Phase 2** (confirm recv-v2): `self.with_tcbs(...)` — unchanged, under `task_state_lock`
  (rank 3) after Phase 1's lock is released.
- **Phase 3** (deliver): `complete_blocked_recv_for_waiter(...)` — unchanged, no locks.
- **Phase 4** (clear slot): `self.ipc_clear_plain_receiver_waiter_only(endpoint_idx, waiter_tid)`
  — clears `endpoint_waiters[endpoint_idx]` under `ipc_state_lock` (rank 4) only if the
  slot still matches `waiter_tid`.
- **Phase 5** (wake): `self.wake_tid_to_runnable(waiter_tid)?` — wakes receiver outside
  all locks.

The non-recv-v2 fallback path (enqueue + `wake_waiter_for_endpoint`) is unchanged — it
remains correct under the global kernel lock.

Lock-ordering note: Phase 1 and Phase 4 both acquire `ipc_state_lock` (rank 4), but
they are **sequential**, not concurrent.  Between them, Phase 2 acquires `task_state_lock`
(rank 3) while `ipc_state_lock` is not held — no rank inversion.

---

#### Part 3: Scaffolding tests

New tests in `src/kernel/boot/tests.rs`:

- **`transfer_envelope_bound_receiver_cleanup_requires_receiver_tid`**: Verifies the
  bound-receiver invariant directly — stash with `receiver_tid = Some(ThreadId(7))`,
  confirm cleanup with `ThreadId(0)` (sender) returns `None`, confirm cleanup with
  `ThreadId(7)` (correct receiver) returns `Some`, confirm replay returns `None`.

- **`transfer_envelope_unbound_cleanup_accepts_any_tid`**: Verifies the complementary
  invariant — stash with `receiver_tid = None` (no waiter), confirm cleanup with any TID
  succeeds.

- **`ipc_reply_recv_v2_phase4_clears_waiter_slot_before_phase5_wake`**: Integration test
  for the normalized Phase 1–5 path — verifies Phase 4 clears `endpoint_waiters` slot,
  Phase 5 wakes the receiver to Runnable, message is not enqueued, telemetry incremented.

---

#### Part 4: Live split confirmation

The normalization of Phase 4/5 (replacing `wake_waiter_for_endpoint` with
`ipc_clear_plain_receiver_waiter_only` + `wake_tid_to_runnable`) confirms that the
`ipc_reply` recv-v2 path was already a live split — the only change is lock discipline,
not the split itself.  The existing Stage 4M test (`ipc_reply_increments_split_delivery_telemetry_for_recv_v2_waiter`) continues to pass unchanged.

#### API surface (Stage 4N)

- `stash_transfer_handle` return type: `Result<(Option<u64>, Option<crate::kernel::ipc::ThreadId>), SyscallError>`
  (was `Result<Option<u64>, SyscallError>`).  Callers updated to destructure.
- No changes to public kernel API, IPC syscall ABI, or SYSCALL_COUNT.

#### What Stage 4N does NOT change

- IPC syscall ABI and SYSCALL_COUNT: unchanged.
- The global `SharedKernel` lock is still retained.  **This is not full global-lock removal.**
- `SpawnV5`, `MemoryObject` zero-copy, VFS, syscall 27, `VFS_READ_SHARED_REPLY_ENABLED`,
  x86_64 SMP / `smp.rs`, x86_64 register writeback, Phase 3B checks: all unchanged.
- The split logic for `ipc_reply` recv-v2 is unchanged — only lock discipline updated.

---

### Stage 4O: IpcSend FLAG_CAP_TRANSFER to recv-v2 blocked receiver

Stage 4O extends the Stage 4K recv-v2 direct-delivery split to messages that carry
a `FLAG_CAP_TRANSFER` cap.  Previously, `handle_ipc_send` gated the entire split path
on `transfer_cap.is_none()`, forcing all cap-bearing sends to fall back to the full
`ipc_send` path.

#### What changed

The `if transfer_cap.is_none()` outer gate in `handle_ipc_send` (Stage 4K path) was
removed.  The inner `ReceiverWaiterFound` + `is_recv_v2` branch now handles all flag
variants — the eligibility filters remain correct:

- **No-waiter enqueue branch** (`ipc_try_send_queued_plain_endpoint_only`, `(None, false)` case):
  still checks `split_unsafe_flags` (includes `FLAG_CAP_TRANSFER`) and returns
  `Ineligible(TransferOrReplyCapMessage)` → falls to full path.
- **Non-recv-v2 receiver** (`ipc_try_send_to_plain_receiver_endpoint_only`): still
  checks `split_unsafe_flags` and returns `Ineligible(TransferOrReplyCapMessage)` →
  falls to full path.
- **Recv-v2 blocked receiver** (`complete_blocked_recv_for_waiter`): already handled
  `FLAG_CAP_TRANSFER` in `ipc_send_with_optional_deadline` (lines 1267/1347) — Stage 4O
  reuses the same code path outside `ipc_state_lock`.

#### OPCODE_SHARED_MEM compatibility

`should_strip_inline_opcode_prefix` checks `msg.opcode == OPCODE_INLINE` before
checking flags — OPCODE_SHARED_MEM messages with `FLAG_CAP_TRANSFER` are NOT stripped;
their 16-byte `region.encode()` payload is delivered verbatim.  This matches the
behavior in `handle_ipc_recv_result_with_empty_error`.

#### Error path fix

The Stage 4K code used `complete_blocked_recv_for_waiter(...)?` (early return on error).
For Stage 4O this is unsafe: a delivery failure before `take_transfer_envelope` consumes
the envelope would leak the stashed handle.  Stage 4O uses a `match` block instead,
returning `Some(Err(KernelError::UserMemoryFault))` on failure so the outer error
handler (`if let Err(err) = send_result`) runs and calls `take_transfer_envelope` for
cleanup — matching the semantics of `ipc_send_with_optional_deadline` line ~1285.

#### Lock contract (Stage 4O)

Identical to Stage 4K.  `complete_blocked_recv_for_waiter` runs entirely outside
`ipc_state_lock`:
- User-memory copies (`copy_to_user`) — outside lock ✓
- Cap materialization (`take_transfer_envelope`, `grant_task_to_task_with_rights`) — outside lock ✓
- TrapFrame / register writes — outside lock ✓
- Phase 4: `ipc_clear_plain_receiver_waiter_only` under `ipc_state_lock` ✓
- Phase 5: `apply_split_receiver_wake_plan` → `wake_tid_to_runnable` outside locks ✓

#### API surface (Stage 4O)

- `IpcPathTelemetry::cap_transfer_recv_v2_deliveries: u64` — new counter incremented
  when Stage 4O delivers a cap-transfer message to a recv-v2 blocked receiver.
- `note_cap_transfer_recv_v2_delivery()` method on `KernelState`.
- `split_recv_v2_deliveries` is also incremented (Stage 4O is a superset of Stage 4K).
- No changes to IPC syscall ABI or SYSCALL_COUNT.

#### Tests (Stage 4O)

`ipc_send_syscall_cap_transfer_delivers_directly_to_recv_v2_blocked_receiver`
(`src/kernel/boot/tests.rs`): task 1 blocks on recv-v2; task 0 sends inline IpcSend
with `FLAG_CAP_TRANSFER` (4-byte payload: 2-byte opcode prefix + `b"4o"`); verifies
direct delivery (queue empty), both telemetry counters incremented, payload written to
receiver's user buffer, and `SYSCALL_RECV_META_TRANSFERRED_CAP` set in receiver meta.

#### What Stage 4O does NOT change

- IPC syscall ABI and SYSCALL_COUNT: unchanged.
- The global `SharedKernel` lock is still retained.  **This is not full global-lock removal.**
- `SpawnV5`, `MemoryObject` zero-copy, VFS, syscall 27, `VFS_READ_SHARED_REPLY_ENABLED`,
  x86_64 SMP / `smp.rs`, x86_64 register writeback, Phase 3B checks: all unchanged.
- Cap-transfer sends without a recv-v2 blocked receiver still fall back to full path.

---

### Stage 4P: IpcCall/IpcReply Phase 1–5 lock-discipline audit (Part 2C)

A targeted audit of `handle_ipc_call` and `handle_ipc_reply` was performed to
confirm no case exists where `ipc_state_lock` is held during cap materialization,
user-memory copy, or TrapFrame writes.

#### Audit findings (all CLEAN)

**`handle_ipc_call`** (`src/kernel/syscall.rs`):
- `create_reply_cap_for_caller` (cap allocation/minting) runs before any
  `ipc_state_lock` acquisition. ✓
- `copy_from_current_user` (user-memory read) runs before any
  `ipc_state_lock` acquisition. ✓
- Stage 4L split path: Phase 1 (snapshot TID) under `ipc_state_lock`; Phase 2
  (`is_task_recv_v2_blocked`) under `task_state_lock`; Phase 3
  (`complete_blocked_recv_for_waiter`) OUTSIDE all locks; Phase 4 (clear slot)
  under `ipc_state_lock`; Phase 5 (wake) outside locks. ✓
- `ipc_try_send_queued_plain_endpoint_only` acquires `ipc_state_lock` only for
  queue state reads — no user ops, cap ops, or TrapFrame writes inside the lock. ✓

**`handle_ipc_reply`** (`src/kernel/syscall.rs`):
- `copy_from_current_user` (user-memory read) runs before `kernel.ipc_reply()`. ✓
- `stash_transfer_handle` (envelope stash) runs before `kernel.ipc_reply()`. ✓

**`ipc_reply`** (`src/kernel/boot/ipc_state.rs`):
- `with_ipc_state` (lines 811–821): reads/clears `reply_caps[slot]` — no user
  ops inside the critical section. ✓
- `fast_revoke_reply_cap_in_cnode` (cap operations): runs AFTER `with_ipc_state_mut`
  returns, outside the `ipc_state_lock`. ✓
- Phase 1 snapshot (`with_ipc_state`): reads `endpoint_waiters` only. ✓
- Phase 2 (`with_tcbs`): reads TCB recv-v2 state under `task_state_lock`. ✓
- Phase 3 (`complete_blocked_recv_for_waiter`): runs OUTSIDE all locks — user
  copies, cap materialization, TrapFrame writes all lock-free. ✓
- Phase 4 (`ipc_clear_plain_receiver_waiter_only`): clears waiter slot under
  `ipc_state_lock` — no user ops. ✓
- Phase 5 (`wake_tid_to_runnable`): wakes receiver outside all locks. ✓

**Result**: No violations found. The Phase 1–5 lock discipline is correctly
implemented for both `handle_ipc_call` (Stage 4L) and `ipc_reply` (Stage 4M/4N).

#### FLAG_CAP_TRANSFER_PLAIN + recv-v2 blocked requester coverage gap

The existing Stage 4M test (`ipc_reply_increments_split_delivery_telemetry_for_recv_v2_waiter`)
verified the direct-delivery path without a cap-transfer argument.  A coverage gap
existed: ipc_reply with `FLAG_CAP_TRANSFER_PLAIN` (reply-with-cap) to a recv-v2
blocked requester was untested.

`complete_blocked_recv_for_waiter` already handles `FLAG_CAP_TRANSFER_PLAIN` at
lines 257–261 (`recv_meta_flags = SYSCALL_RECV_META_TRANSFERRED_CAP`) and line 262
(`materialize_received_message_cap`).  Stage 4M's Phase 1–5 path is therefore
already live for cap-transfer replies.  The new test confirms this.

#### Test added

`ipc_reply_with_cap_transfer_delivers_directly_to_recv_v2_blocked_requester`
(`src/kernel/boot/tests.rs`): full IpcCall → IpcRecv → IpcReply-with-cap round trip.
Task 1 (requester) blocks recv-v2 on the reply endpoint; task 2 (replier) issues
`IpcReply` with a MemoryObject cap as `arg5`; asserts:
- task 1 woken to Runnable (Phase 5) ✓
- reply endpoint waiter slot cleared (Phase 4) ✓
- reply endpoint queue empty (direct delivery, no enqueue) ✓
- `ipc_reply_split_deliveries` incremented ✓
- `FLAG_CAP_TRANSFER_PLAIN` does not strip bytes — payload `b"rm"` lands verbatim ✓
- `SYSCALL_RECV_META_TRANSFERRED_CAP` bit set in requester meta ✓
- MemoryObject cap materialized in requester cspace (cap_id ≠ `SYSCALL_NO_TRANSFER_CAP`) ✓

#### What Stage 4P does NOT change

- IPC syscall ABI and SYSCALL_COUNT: unchanged.
- The delivery logic for `handle_ipc_call`, `handle_ipc_reply`, or `ipc_reply` is
  unchanged — only a new test and this documentation were added.
- The global `SharedKernel` lock is still retained.  **This is not full global-lock removal.**

---

### Stage 4Q: synchronous-endpoint send Phase 1–6 lock-discipline normalization

**Implemented**: 2026-06-01. Affects `ipc_send_with_optional_deadline` in
`src/kernel/boot/ipc_state.rs`.

#### Motivation

The `EndpointMode::Synchronous` send path in `ipc_send_with_optional_deadline`
previously accessed `self.ipc.endpoint_waiters`, `self.ipc.endpoints`, and
`self.ipc.telemetry` directly — under the global `SharedKernel` lock only,
never under `ipc_state_lock`.  The waiter slot was cleared by `wake_waiter_for_endpoint`
which also directly mutated `self.ipc.endpoint_waiters` without `with_ipc_state_mut`.
`switch_to_runnable_tid` (a busy-loop over `yield_current`) was used for the handoff.

Stage 4Q applies the same Phase 1–6 discipline already established for `ipc_reply`
(Stage 4M/4N) and `handle_ipc_call` (Stage 4L):
- endpoint/waiter mutations under `ipc_state_lock` via `with_ipc_state_mut`
- user-memory writes outside all locks
- scheduler wake/handoff outside all locks

The busy-loop `switch_to_runnable_tid` is replaced by the one-shot
`apply_scheduler_handoff_plan(YieldTo(tid))` → `yield_current_to(tid)`, introduced
in Session 4 (`yield_current_to`).

#### Protocol (Phase 0–6)

```
Phase 0  with_ipc_state(|ipc| ipc.endpoints[endpoint_idx].map(|e| e.mode()))
           → snapshot endpoint mode under ipc_state_lock — no mutation

Phase 1  with_ipc_state(|ipc| ipc.endpoint_waiters[endpoint_idx])
           → snapshot waiter TID under ipc_state_lock — no mutation

Phase 2  with_tcbs(|tcbs| { ... recv_abi == RecvV2 ... })
           → check recv-v2 under task_state_lock (rank 3), OUTSIDE ipc_state_lock (rank 4)

Phase 3  complete_blocked_recv_for_waiter(self, waiter_tid.0, &msg)   [recv-v2 path only]
           → runs OUTSIDE all locks (TrapFrame / user-memory write)
           → on failure: return Err(KernelError::UserMemoryFault) — no waiter orphan
             (waiter slot not yet cleared; Phase 4 is skipped on error)

Phase 4  ipc_try_send_sync_endpoint_only(endpoint_idx, waiter_tid, msg, recv_v2_completed)
           → under with_ipc_state_mut (ipc_state_lock, rank 4)
           → re-verifies endpoint_waiters[endpoint_idx] == Some(waiter_tid)
             (defence-in-depth; always matches under global kernel lock)
           → legacy path only: endpoint.send(msg) to enqueue into queue
           → clears endpoint_waiters[endpoint_idx] = None
           → bumps telemetry.rendezvous_handoffs
           → returns SchedulerWakePlan::Wake(waiter_tid)

Phase 5  apply_scheduler_wake_plan(Wake(waiter_tid))
           → OUTSIDE ipc_state_lock
           → wake_tid_to_runnable: sets task Runnable, enqueues on scheduler runqueue

Phase 6  apply_scheduler_handoff_plan(YieldTo(waiter_tid))
           → OUTSIDE ipc_state_lock
           → yield_current_to(waiter_tid): one-shot preempt via on_preempt_prefer
             makes waiter_tid current, re-enqueues sender at tail of runqueue
```

If Phase 1 returns `None` (no waiter), the code falls through to
`block_current_on_send_with_deadline` (blocking path) unchanged.

If Phase 3 fails (`complete_blocked_recv_for_waiter` error), Phase 4 is skipped
entirely — the waiter slot is still populated, so the waiter is not orphaned.

#### Lock contract (Stage 4Q)

- No `ipc_state_lock` held during: recv-v2 TrapFrame write (Phase 3),
  `task_state_lock` acquisition (Phase 2), or scheduler mutation (Phase 5–6).
- Lock ordering: `task_state_lock` (rank 3) → `ipc_state_lock` (rank 4).
  Phases 2, 3, 4 are sequentially non-overlapping (Phase 2 under task lock,
  Phase 3 outside all locks, Phase 4 under ipc lock).
- `ipc_state_lock` entered at Phase 0 (mode snapshot), Phase 1 (waiter snapshot),
  and Phase 4 (`ipc_try_send_sync_endpoint_only`) — all non-nested.
- Telemetry mutations (`fastpath_attempts`, `fastpath_switches`,
  `scheduler_fastpath_handoffs`, `blocked_sends`) moved into `with_ipc_state_mut`
  closures.

#### New helper

`ipc_try_send_sync_endpoint_only(endpoint_idx, expected_receiver_tid, msg, recv_v2_completed)`
(`src/kernel/boot/ipc_state.rs`):
- Called from `ipc_send_with_optional_deadline` at Phase 4.
- Under `with_ipc_state_mut`: re-verify waiter, optionally enqueue message (legacy),
  clear waiter slot, bump telemetry.
- Returns `Result<SchedulerWakePlan, KernelError>`.

#### Tests (Stage 4Q)

Three new unit tests in `src/kernel/boot/tests.rs`:

- `sync_endpoint_phase4_helper_delivers_legacy_message_under_ipc_state_lock`:
  parks a legacy receiver on a sync endpoint; calls `ipc_try_send_sync_endpoint_only`
  directly with `recv_v2_completed=false`; asserts waiter slot cleared, message
  enqueued in endpoint queue, `Wake(80)` returned, `rendezvous_handoffs == 1`.

- `sync_endpoint_phase4_helper_skips_enqueue_when_recv_v2_completed`:
  same setup with `recv_v2_completed=true`; asserts waiter slot cleared, endpoint
  queue empty (message already delivered via TrapFrame), `Wake(81)` returned.

- `sync_endpoint_phase4_helper_rejects_mismatched_waiter`:
  parks receiver, manually clears the waiter slot to simulate a timeout race,
  calls `ipc_try_send_sync_endpoint_only`; asserts `Err(WrongObject)` — no orphan.

Existing tests that exercise the full sync endpoint send path
(`yield_current_to_is_single_step_for_ipc_handoff`,
`synchronous_endpoint_blocked_send_updates_telemetry`,
`ipc_fastpath_blocked_path_is_measured_without_switch`) continue to pass (508/0).

#### What Stage 4Q does NOT change

- IPC syscall ABI and SYSCALL_COUNT: unchanged.
- SpawnV5, MemoryObject zero-copy, VFS, syscall 27, VFS_READ_SHARED_REPLY_ENABLED: untouched.
- x86_64 SMP and `src/arch/x86_64/smp.rs`: untouched.
- x86_64 register writeback: unchanged.
- Phase 3B checks: not weakened.
- Timeout/deadline blocking path: unchanged.
- Reply-cap and FLAG_CAP_TRANSFER semantics: unchanged (both pass through
  `complete_blocked_recv_for_waiter` or `endpoint.send` as before).
- The global `SharedKernel` lock is still retained.  **This is not full global-lock removal.**

---

### Stage 4R: normalize IPC-domain direct accesses to with_ipc_state / with_ipc_state_mut

**Implemented**: 2026-06-01.  Affects `src/kernel/boot/ipc_state.rs`.

#### Motivation

After Stage 4Q, a number of methods inside `ipc_state.rs` still read or wrote
`self.ipc.*` directly (under the global `SharedKernel` lock only) rather than
going through the `with_ipc_state` / `with_ipc_state_mut` accessor pairs.  Stage
4R is a normalization pass: every direct `self.ipc.*` access that can safely be
wrapped is wrapped.  The remaining direct accesses are documented as globally-locked
paths deferred to a future stage.

#### Changes per function

| Function | Change |
|---|---|
| `enqueue_sender_waiter` | Replaced `endpoint_sender_waiter_limit` helper (direct `self.ipc.endpoints`) + inline slot write with a single `with_ipc_state_mut` closure; defence-in-depth endpoint check and FIFO-first-free slot write are both inside the lock |
| `wake_waiter_for_endpoint` | `endpoint_waiters[endpoint_idx].take()` moved into `with_ipc_state_mut` |
| `block_current_on_receive_with_deadline` | Receiver waiter registration (`endpoint_waiters[endpoint_idx] = Some(...)`) moved into `with_ipc_state_mut` |
| `signal_notification` | Refactored to plan-first: IRQ signal + waiter `.take()` under `with_ipc_state_mut`; TCB wake under `with_tcbs_mut` outside; matches the SchedulerWakePlan/SchedulerHandoffPlan discipline |
| `ipc_send_fastpath` | Mode/waiter reads via `with_ipc_state`; telemetry writes via `with_ipc_state_mut` |
| `ipc_send_with_optional_deadline` (buffered path) | Waiter read via `with_ipc_state`; `endpoint.send` via `with_ipc_state_mut`; telemetry writes via `with_ipc_state_mut` |
| `ipc_send_with_cap_transfer` | Waiter read via `with_ipc_state` |
| `ipc_recv_with_optional_deadline` (notification path) | Notification recv and waiter registration via `with_ipc_state_mut` |
| `try_ipc_recv` (notification path) | Notification recv via `with_ipc_state_mut` |
| `note_split_recv_v2_delivery`, `note_ipc_call_split_delivery`, `note_ipc_reply_split_delivery`, `note_cap_transfer_recv_v2_delivery`, `note_cap_transfer_stage4e_enqueued`, `note_endpoint_only_queued_send_split`, `note_endpoint_only_queued_recv_split` (7 helpers) | Each telemetry increment moved into `with_ipc_state_mut` |

#### Remaining direct `self.ipc.*` accesses (deferred — globally locked)

The following functions retain direct `self.ipc.*` access because they are
called in contexts where the surrounding `endpoint.recv()` / `endpoint.send()`
operations also access `self.ipc` directly; wrapping only the dequeue mutation
would create inconsistent mixed-access patterns harder to audit than uniform
direct access:

- `dequeue_sender_waiter` — called from `try_ipc_recv` and
  `ipc_recv_with_optional_deadline` buffered-endpoint paths.  Both callers
  also call `endpoint.recv()` and `endpoint.send()` directly.  The correct
  split requires a two-phase refill protocol identical to Stage 4C/4D, which
  is deferred to a future Stage 4S.
- `create_endpoint_with_mode`, `create_notification` setup blocks — write
  initial IPC state directly; these are object-creation (not hot) paths.
- `try_ipc_recv` buffered endpoint recv path — multiple interleaved
  `endpoint.recv()` / sender-waiter dequeue / `endpoint.send()` accesses; left
  as globally locked pending Stage 4S.
- `ipc_recv_with_optional_deadline` buffered endpoint path (after-wake recv +
  refill) — same as above.

#### Lock ordering for `signal_notification` (plan-first)

Sequential, never nested:
1. `with_ipc_state_mut` (rank 3): `notif.send_irq(irq_line)` + `waiter.take()` — IPC domain.
2. Released.
3. `with_tcbs_mut` (rank 2): TCB status set to `Runnable` — task domain.
4. Released.
5. `enqueue_task` (rank 1): scheduler enqueue — scheduler domain.

#### Tests (Stage 4R)

Five new unit tests in `src/kernel/boot/tests.rs`:

- `stage4r_sender_waiter_registered_via_ipc_state_lock`:
  sync endpoint, task 0 blocks (no receiver); asserts
  `with_ipc_state(|ipc| ipc.endpoint_sender_waiters[eid][0]) == Some(SenderWaiter { tid: 0, msg })`.

- `stage4r_blocked_sends_telemetry_incremented_after_sync_block`:
  same setup; asserts `ipc_path_telemetry().blocked_sends` incremented by 1.

- `stage4r_receiver_consumes_blocked_sender_exactly_once`:
  sender blocks, receiver calls `ipc_recv`; asserts sender waiter slot cleared,
  sender becomes Runnable, message delivered correctly.

- `stage4r_sender_waiter_fifo_order_preserved`:
  two senders block; asserts `endpoint_sender_waiters[eid][0]` holds the first sender
  and `[1]` holds the second; receiver dequeues in FIFO order.

- `stage4r_no_orphaned_sender_waiter_when_queue_full`:
  pre-fills all `MAX_ENDPOINT_SENDER_WAITERS` slots via `with_ipc_state_mut`;
  attempts another send; asserts `Err(EndpointQueueFull)` returned and no slot was
  modified (no orphaned SenderWaiter for the failed sender).

Total test count: **513 passed, 0 failed** (5 new Stage 4R + 508 pre-existing).

#### What Stage 4R does NOT change

- IPC syscall ABI and SYSCALL_COUNT: unchanged.
- SpawnV5, MemoryObject zero-copy, VFS, syscall 27, VFS_READ_SHARED_REPLY_ENABLED: untouched.
- x86_64 SMP and `src/arch/x86_64/smp.rs`: untouched.
- x86_64 register writeback: unchanged.
- Phase 3B checks: not weakened.
- Stage 4Q Phase 0–6 discipline: preserved (no protocol change, normalization only).
- The global `SharedKernel` lock is still retained.  **This is not full global-lock removal.**

---

### Stage 4S: atomic recv+refill under ipc_state_lock; remove dead dequeue helpers

**Target**: `try_ipc_recv` and `ipc_recv_with_optional_deadline` had three
separate recv+refill patterns totalling ~75 lines that accessed `self.ipc.*`
directly (outside `ipc_state_lock`) and used the dead-code helpers
`dequeue_sender_waiter` / `wake_sender_waiter`.

**Also fixed (Stage 4R miss)**: `ipc_reply` non-recv-v2 path (lines 1037–1046
as of the Stage 4R commit) accessed `self.ipc.endpoints` directly to enqueue the
reply message, then called `wake_waiter_for_endpoint` as a separate step.
This left a window where the endpoint queue held the reply but the receiver waiter
slot had not yet been cleared, violating the "endpoint state + waiter slot are
atomic" invariant.

#### New helper: `ipc_recv_endpoint_take`

```rust
pub(crate) fn ipc_recv_endpoint_take(
    &mut self,
    endpoint_idx: usize,
) -> Result<(Option<Message>, SchedulerWakePlan), KernelError>
```

All mutations happen under a single `ipc_state_lock` (rank 3) acquisition:

1. **1a** — `endpoint.recv()` in a scoped block; borrow released before step 1b.
2. **1b** — Inline dequeue+compact of `endpoint_sender_waiters[endpoint_idx]`:
   take `queue[0]`, shift `queue[1..]` left, clear tail.
3. **Match on (opt_msg, opt_waiter)**:
   - `(Some, Some)` → refill endpoint with waiter's message; return `Wake(waiter.tid)`.
   - `(Some, None)` → return message; `None` wake.
   - `(None, Some)` → direct delivery (bypass endpoint queue); return `Wake(waiter.tid)`.
   - `(None, None)` → return `None`; `None` wake.

The caller applies the wake plan with `apply_scheduler_wake_plan` after releasing
the lock, following the `SchedulerWakePlan` deferred-wake discipline.

#### ipc_reply fix

The non-recv-v2 path in `ipc_reply` now wraps both `endpoint.send(msg)` and
`endpoint_waiters[idx].take()` in a single `with_ipc_state_mut` closure.
The returned `SchedulerWakePlan` is applied outside the lock.

#### Dead code removed

`KernelState::dequeue_sender_waiter` and `KernelState::wake_sender_waiter`
are removed.  All call sites are replaced by `ipc_recv_endpoint_take` or its
inline equivalent inside closures.

#### Tests (Stage 4S)

Seven new tests added to `src/kernel/boot/tests.rs`:

- `stage4s_ipc_recv_endpoint_take_empty_queue_no_waiter_returns_none`:
  empty endpoint, no sender waiters → `(None, None wake)`.

- `stage4s_ipc_recv_endpoint_take_queued_message_no_waiter`:
  one queued message, no sender waiters → message returned, no wake.

- `stage4s_ipc_recv_endpoint_take_direct_delivery_from_sender_waiter`:
  empty endpoint queue + one sender waiter → direct delivery, wake sender.

- `stage4s_ipc_recv_endpoint_take_refill_from_sender_waiter`:
  one queued message + one sender waiter → dequeue message, refill endpoint
  from waiter, wake sender; second take yields waiter's message.

- `stage4s_try_ipc_recv_delegates_to_endpoint_take`:
  `try_ipc_recv` on a buffered endpoint with a queued message returns it.

- `stage4s_ipc_reply_non_recv_v2_enqueues_and_wakes_atomically`:
  simulates the fixed ipc_reply non-recv-v2 path; asserts enqueue and waiter
  clear happen atomically (both visible to ipc_state reader after the closure).

- `stage4s_sender_waiter_compaction_shifts_queue_left`:
  two sender waiters queued; first take dequeues m0, refills from slot[0],
  compacts slot[0]=second-waiter, slot[1]=None; second take yields second waiter.

Total test count: **520 passed, 0 failed** (7 new Stage 4S + 513 pre-existing).

#### What Stage 4S does NOT change

- IPC syscall ABI and SYSCALL_COUNT: unchanged.
- SpawnV5, MemoryObject zero-copy, VFS, syscall 27, VFS_READ_SHARED_REPLY_ENABLED: untouched.
- x86_64 SMP and `src/arch/x86_64/smp.rs`: untouched.
- x86_64 register writeback: unchanged.
- Phase 3B checks: not weakened.
- `ipc_try_recv_queued_plain_endpoint_only` (Stage 4C/4D split path): unchanged.
- The global `SharedKernel` lock is still retained.  **This is not full global-lock removal.**

---

### Stage 4T: IPC finalization audit, sparse-queue fix, create_* wrapper completion

**Scope**: finalization pass over all hot-path IPC helpers plus two control-plane
functions (`create_endpoint_with_mode`, `create_notification`) that still accessed
`self.ipc.*` directly outside `ipc_state_lock`.

#### Audit: no ipc_state_lock held during dangerous operations

All functions in `src/kernel/boot/ipc_state.rs` were audited for the following
invariants. Every finding below is **CLEAN** (no violation found in the audited code):

| Invariant | Result |
|---|---|
| No `ipc_state_lock` held during user-memory copy | CLEAN |
| No `ipc_state_lock` held during TrapFrame write | CLEAN |
| No `ipc_state_lock` held during cap operations (rank 4 > rank 3) | CLEAN |
| No `ipc_state_lock` held during VM operations (rank 5 > rank 3) | CLEAN |
| No `ipc_state_lock` held during scheduler/TCB mutation (rank 1–2 < rank 3) | CLEAN |
| All endpoint + waiter mutations atomic under single `ipc_state_lock` closure | CLEAN |
| Sender-waiter FIFO preserved | CLEAN (with sparse-queue fix — see below) |
| No orphaned waiters | CLEAN |
| No duplicate wake | CLEAN |
| No transfer-envelope leak | CLEAN |
| No reply-cap rematerialization | CLEAN |
| No message loss | CLEAN |

#### Sparse sender-waiter queue bug and fix

**Root cause**: `process_ipc_timeout_deadlines` nulls timed-out sender slots
in-place (no left-compaction):

```rust
// process_ipc_timeout_deadlines — in-place null, no compaction:
ipc.endpoint_sender_waiters[eid][slot_idx] = None;
```

This creates sparse queues: `[None, Some(B), None, Some(D), ...]`.

The old `ipc_recv_endpoint_take` used `queue[0].take()` unconditionally:

```rust
// OLD — only ever looked at slot[0]:
if let Some(head) = queue[0].take() { ... }
```

If slot[0] is None (the timed-out sender), the old code returned `opt_waiter = None`
even when slot[1] held a live sender. That sender was permanently stranded — never
delivered, never woken.

**Fix** in `ipc_recv_endpoint_take`:

```rust
// NEW — scan for first live sender, full left-compaction after:
if let Some(idx) = queue.iter().position(Option::is_some) {
    let head = queue[idx].take().expect("position guarantees Some");
    let mut write = 0;
    for read in 0..queue.len() {
        if queue[read].is_some() {
            queue[write] = queue[read].take();
            write += 1;
        }
    }
    Some(head)
} else {
    None
}
```

`iter().position(Option::is_some)` finds the first live slot in O(n). The
subsequent compaction left-packs all remaining Some entries so the next call
starts at `queue[0]` with no gap. The normal case (no gap, sender at slot[0]) is
functionally identical to the old code.

**IPC path correctness**: `ipc_try_recv_queued_plain_endpoint_only` (Stage 4C/4D)
already returned `Ineligible(SenderWaiterPresent)` when any waiter slot is occupied
(including sparse slots). That fall-through now reaches `ipc_recv_endpoint_take`
which correctly scans and delivers the live sender. End-to-end behavior is now
correct for the sparse case.

#### Direct-access sweep: final state

| Pattern | Location | Classification |
|---|---|---|
| `self.ipc.endpoints[...]` | `create_endpoint_with_mode` — now inside `with_ipc_state_mut` | Eliminated |
| `self.ipc.endpoint_generations[...]` | `create_endpoint_with_mode` — now inside `with_ipc_state_mut` | Eliminated |
| `self.ipc.notifications[...]` | `create_notification` — now inside `with_ipc_state_mut` | Eliminated |
| `self.ipc.notification_generations[...]` | `create_notification` — now inside `with_ipc_state_mut` | Eliminated |
| All remaining `with_ipc_state` / `with_ipc_state_mut` accesses | Throughout `ipc_state.rs` | Intentionally locked |
| Test-only `with_ipc_state` / `with_ipc_state_mut` in `tests.rs` | Test harness | Test-only |

**Result**: zero remaining hot-path or warm-path direct `self.ipc.*` accesses.
All reads and mutations of `IpcSubsystem` fields go through `with_ipc_state` or
`with_ipc_state_mut`.

#### `create_endpoint_with_mode` and `create_notification` wrapper pattern

Both functions now follow the same two-phase pattern:

**Phase 1** (under `ipc_state_lock`, rank 3): find free slot, bump generation,
store object. Returns `(idx, generation)`.

**Phase 2** (after lock release): call `mint_capability_for_active_cnode` (which
acquires `capability_state_lock`, rank 4) twice — once for the send cap, once for
the receive cap.

Lock ordering is correct: rank 3 closes before rank 4 opens. The generation value
captured in phase 1 is used for both cap mints, eliminating the old pattern that
re-read `self.ipc.notification_generations[idx]` after the lock was released.

#### Tests (Stage 4T)

Four new tests added to `src/kernel/boot/tests.rs`:

- `stage4t_ipc_recv_handles_sparse_sender_waiter_queue`:
  two senders block as waiter[0] and waiter[1]; slot[0] is nulled to simulate a
  timeout-induced gap; `ipc_recv_endpoint_take` must find the live sender at slot[1],
  deliver its message via refill, wake it, and leave all slots empty.

- `cap_domain_lock_read_sees_minted_capability`:
  after `create_endpoint`, both `capability_for_cnode` (which uses
  `with_capability_state` internally) and a direct `with_capability_state` closure
  must see the minted SEND and RECEIVE caps at the correct endpoint index.

- `cap_domain_with_task_then_capability_reads_consistent_state`:
  `lock_order_task_capability_snapshot_for_test` (which calls
  `with_task_then_capability`, acquiring task rank 2 then capability rank 4) must
  reflect an increased task count and process-cnode count after `register_task`.

- `cap_domain_reply_cap_record_exists_after_create_and_gone_after_revoke`:
  `create_reply_cap_for_caller` installs a `ReplyCapRecord` under
  `with_ipc_state_mut`; `mark_task_dead` (via `revoke_reply_caps_for_caller`)
  clears it under `with_ipc_state_mut`; both transitions are immediately visible
  via `with_ipc_state` without additional synchronization.

Total test count: **524 passed, 0 failed** (4 new Stage 4T + 520 pre-existing).

#### IPC hot-path phase complete

As of Stage 4T, all IPC hot-path and warm-path accesses to `IpcSubsystem` fields
go through `with_ipc_state` or `with_ipc_state_mut`. The only remaining direct
`self.ipc.*` accesses are:

- Inside `with_ipc_state` / `with_ipc_state_mut` closures themselves (correct —
  these closures receive a `&IpcSubsystem` / `&mut IpcSubsystem` argument).
- Test-only `with_ipc_state_mut` calls that inject state for regression testing.

**This completes the IPC domain lock-access audit.** The global `SharedKernel` lock
is still retained. **This is not full global-lock removal.**

#### What Stage 4T does NOT change

- IPC syscall ABI and SYSCALL_COUNT: unchanged.
- SpawnV5, MemoryObject zero-copy, VFS, syscall 27, VFS_READ_SHARED_REPLY_ENABLED: untouched.
- x86_64 SMP and `src/arch/x86_64/smp.rs`: untouched.
- x86_64 register writeback: unchanged.
- Phase 3B checks: not weakened.
- Stage 4S `ipc_recv_endpoint_take` protocol: preserved (sparse fix is a drop-in
  replacement, same return type and semantics for the non-sparse case).
- The global `SharedKernel` lock is still retained.  **This is not full global-lock removal.**

---

### Current live / deferred IPC split matrix

| Syscall path | Condition | Status | Telemetry counter |
|---|---|---|---|
| IpcRecv (plain buffered recv) | Queue non-empty, no pending sender wake | **Live** Stage 4C | `queued_recvs` |
| IpcRecv (buffered recv + sender-waiter refill) | Queue non-empty, sender waiter present | **Live** Stage 4D + Stage 4T (sparse-queue fix) | `queued_recvs` |
| IpcSend (plain enqueue) | No receiver waiter, no sender waiters, buffered, no cap-transfer | **Live** Stage 4E | `queued_sends` |
| IpcSend (to waiting non-recv-v2 receiver) | Receiver waiter present, not recv-v2, no sender waiters | **Live** Stage 4F | `queued_sends` |
| IpcRecvTimeout (timeout_ticks=0 immediate recv) | Queue non-empty | **Live** Stage 4G | `queued_recvs` |
| IpcSend (nonzero-timeout plain send) | No receiver, buffered, no cap-transfer, timeout >0 | **Live** Stage 4H | `queued_sends` |
| IpcRecvTimeout (timeout_ticks>0 immediate recv) | Queue non-empty | **Live** Stage 4I | `queued_recvs` |
| IpcSend to recv-v2 blocked receiver | Receiver waiter + recv-v2, no sender waiters | **Live** Stage 4K | `split_recv_v2_deliveries` |
| IpcCall to recv-v2 blocked receiver | Receiver waiter + recv-v2, no sender waiters | **Live** Stage 4L | `ipc_call_split_deliveries` |
| IpcReply (plain) to recv-v2 blocked requester | Requester waiter + recv-v2 on reply endpoint | **Live** Stage 4M | `ipc_reply_split_deliveries` |
| IpcReply with FLAG_CAP_TRANSFER_PLAIN to recv-v2 blocked requester | Requester waiter + recv-v2, cap-transfer reply | **Live** Stage 4M (Stage 4P test coverage) | `ipc_reply_split_deliveries` |
| IpcSend with FLAG_CAP_TRANSFER to recv-v2 blocked receiver | Receiver waiter + recv-v2, FLAG_CAP_TRANSFER | **Live** Stage 4O | `split_recv_v2_deliveries`, `cap_transfer_recv_v2_deliveries` |
| IpcSend/IpcCall to non-recv-v2 receiver (ReceiverWaiterFound) | Receiver waiter, not recv-v2 | **Deferred** (falls to ipc_send) | — |
| IpcSend with FLAG_CAP_TRANSFER/PLAIN (no receiver waiter, buffered) | No receiver, buffered endpoint, FLAG_CAP_TRANSFER or FLAG_CAP_TRANSFER_PLAIN | **Live** Stage 4E (extended) | `queued_sends`, `cap_transfer_stage4e_enqueued` |
| IpcSend/IpcCall with FLAG_CAP_TRANSFER to non-recv-v2 receiver | FLAG_CAP_TRANSFER, non-recv-v2 receiver present | **Deferred** (falls to ipc_send) | — |
| IpcSend to synchronous endpoint with receiver waiter (legacy) | Synchronous mode, waiter present, not recv-v2 | **Live** Stage 4Q | `rendezvous_handoffs`, `fastpath_switches` |
| IpcSend to synchronous endpoint with receiver waiter (recv-v2) | Synchronous mode, waiter present, recv-v2 | **Live** Stage 4Q | `rendezvous_handoffs`, `fastpath_switches` |
| IpcSend/IpcCall to synchronous endpoint (no waiter) | Synchronous mode, no waiter | **Deferred** (blocking send) | `blocked_sends` |
| IpcReply to non-recv-v2 requester | Queue enqueue path | **Live** Stage 4S (ipc_reply fix) | — |
| MemoryObject zero-copy, VFS shared-reply | Phase 3B / VFS gate | **Deferred indefinitely** | — |

---

### Deferred IPC split paths

The following send/receive cases are explicitly deferred. They cannot be split
without violating the hard invariants on `ipc_state_lock` scope. They fall back
to the existing full IPC paths via `Ineligible(...)`.

#### Synchronous endpoint send-to-receiver (implemented — Stage 4Q)

**Previously deferred** because `switch_to_runnable_tid` (a busy-loop) was called
while `self.ipc` was accessed directly (no `ipc_state_lock`). Stage 4Q resolves
this: see §Stage 4Q below.

#### Stage 4E extension: FLAG_CAP_TRANSFER/PLAIN buffered no-receiver enqueue (live)

`FLAG_CAP_TRANSFER` and `FLAG_CAP_TRANSFER_PLAIN` messages sent to a **buffered
endpoint with no receiver waiter** are now handled by Stage 4E (the extended split
path) in `ipc_try_send_queued_plain_endpoint_only`.

**Why safe**: `handle_ipc_send` calls `stash_transfer_handle` before the split
path is attempted.  At that point the cap is moved into the transfer-envelope
table; the `Message` carries only the numeric envelope handle.  For the
no-receiver buffered-enqueue case, `ipc_send_with_optional_deadline` does an
identical `endpoint.send(msg)`, so Stage 4E is a strict behavioural subset.  The
receiver's `ipc_recv` or `ipc_recv_timeout` falls through to the full path (Stage
4C/4D still rejects cap-transfer messages on the recv side) which materialises
the cap from the envelope handle.

**Telemetry**: `cap_transfer_stage4e_enqueued` increments alongside `queued_sends`
for every FLAG_CAP_TRANSFER/PLAIN message buffered via Stage 4E.

**Still deferred (cap-transfer):**
- `FLAG_CAP_TRANSFER` to a **non-recv-v2** blocked receiver: `is_task_recv_v2_blocked`
  returns false; `ipc_try_send_to_plain_receiver_endpoint_only` rejects with
  `Ineligible(TransferOrReplyCapMessage)` — falls to full `ipc_send`.
- `FLAG_REPLY_CAP` messages (IpcCall, IpcReply) when the receiver is **not** recv-v2
  blocked: `ReceiverWaiterFound` arm returns the TID but `is_task_recv_v2_blocked`
  check falls through to `ipc_send` for non-recv-v2 receivers.

**Note**: `FLAG_REPLY_CAP` to a **recv-v2 blocked** receiver is handled by Stage 4L
(IpcCall) and the existing `ipc_reply` direct path, both of which call
`complete_blocked_recv_for_waiter` outside `ipc_state_lock`.

**Decision**: remaining cap-transfer paths with a receiver waiter remain on the
full IPC path indefinitely.

---

### SchedulerWakePlan: cross-domain deferred wake pattern

`SchedulerWakePlan` (`src/kernel/boot/mod.rs`) is the cross-domain analogue of
`IpcSchedulerPlan`.  It separates the *decision* (computed while holding a domain
lock) from the *execution* (applied after all domain locks are released).

```
enum SchedulerWakePlan { None, Wake(ThreadId) }
```

Usage protocol:
```
// 1. Inside a domain-lock closure: compute the plan, no scheduler mutation.
let plan = if condition { SchedulerWakePlan::Wake(tid) }
           else         { SchedulerWakePlan::None };
// 2. Release all domain locks.
// 3. Apply the plan — acquires only scheduler-internal state (rank 1–2).
kernel.apply_scheduler_wake_plan(plan)?;
```

`apply_scheduler_wake_plan` delegates to `wake_tid_to_runnable`, which acquires
only `task_state_lock` (rank 3) and `scheduler_state` (rank 2) — both below all
IPC, capability, VM, memory, and driver lock ranks.

**Domains that should adopt this pattern** when adding new task-wake side effects:
- Fault handlers that wake a supervisor endpoint waiter
- Restart paths that notify a monitor endpoint
- Capability lifecycle events that unblock a waiting task
- Thread join / futex wake paths

---

### SchedulerHandoffPlan: deferred cooperative CPU handoff

`SchedulerHandoffPlan` (`src/kernel/boot/mod.rs`) encodes the intent to yield
CPU time to a specific task after an IPC send completes.  It separates the
*decision* (which task should receive the CPU next, at message-delivery time)
from the *execution* (the one-shot preempt, applied after all domain mutations
are done).

```
enum SchedulerHandoffPlan { None, YieldTo(ThreadId) }
```

Usage protocol:
```
// 1. At message-delivery time, before any context switch:
let plan = if has_receiver { SchedulerHandoffPlan::YieldTo(receiver_tid) }
           else             { SchedulerHandoffPlan::None };
// 2. After all IPC/cap/VM domain mutations are complete and all domain locks
//    are released:
let switched = kernel.apply_scheduler_handoff_plan(plan)?;
```

`apply_scheduler_handoff_plan` delegates to `yield_current_to(tid)` (see below),
a one-shot direct-dispatch that moves `tid` from the run-queue to current in a
single scheduler operation.  Returns `true` if `tid` became the current task.

**Hosted-dev semantics**: `YieldTo(tid)` calls `yield_current_to(tid)`.  If `tid`
is in the run-queue (guaranteed at the IPC handoff call sites: `wake_waiter_for_endpoint`
runs immediately before), `on_preempt_prefer` bypasses FIFO order and makes `tid`
current directly.  Returns true in one step, regardless of TID 0 position in the
queue.  `yield_current_to` re-enqueues the outgoing task via `on_preempt_prefer`,
so TID 0 remains in the membership table — no `idle_re_enqueue_for_test` is
needed after `apply_scheduler_handoff_plan`.

**Freestanding semantics**: Identical one-shot preempt via `on_preempt_prefer`.
The target is made current immediately; hardware preemption will run other tasks
at the next timer tick.

**Lock constraint**: Must be called outside all IPC/cap/VM/memory domain locks.
Internally calls `yield_current_to` which acquires `task_state_lock` (rank 3)
and touches the address-space HAL.

**Call sites** (as of this writing):
- `ipc_send_with_optional_deadline` (`ipc_state.rs`) — synchronous endpoint with
  a waiting receiver.
- `ipc_send_fastpath` (`ipc_state.rs`) — buffered endpoint with a receiver
  waiter.

---

### Scheduler-domain lock contract

The following functions touch **only** scheduler-internal state (rank 1–2 in the
lock-rank order below) and no other domain state:

| Function | File | Notes |
|---|---|---|
| `block_current_cpu` | `scheduler_state.rs` | Purely scheduler-internal |
| `on_preempt_current_cpu` | `scheduler_state.rs` | Purely scheduler-internal |
| `on_preempt_prefer_current_cpu` | `scheduler_state.rs` | Purely scheduler-internal; bypasses FIFO |
| `dispatch_next_current_cpu` | `scheduler_state.rs` | Purely scheduler-internal |
| `enqueue_on_cpu` | `scheduler_state.rs` | Reads priority/affinity (rank 3 task domain) |
| `enqueue_woken_task` | `scheduler_state.rs` | Reads CPU affinity only |
| `runnable_count_on_cpu` | `scheduler_state.rs` | Read-only scheduler snapshot |
| `compute_wake_plan_for_tid` | `scheduler_state.rs` | Read-only TCB status snapshot |

The following functions cross into other domains (TCB status, HAL, kernel context):

| Function | Additional domains touched |
|---|---|
| `dispatch_next_task` | `task_state_lock` (TCB status write), HAL (address space), kernel context switch |
| `yield_current` | `task_state_lock` (TCB status), HAL (address space), kernel context switch |
| `yield_current_to` | `task_state_lock` (TCB status), HAL (address space), kernel context switch |
| `wake_tid_to_runnable` | `task_state_lock` (TCB status), `ipc_state_lock` (clear IPC timeout) |

### `switch_to_runnable_tid` design constraint (retired from hot path)

`switch_to_runnable_tid` (`task_core_state.rs`) was a cooperative busy-loop that
called `yield_current()` up to `MAX_TASKS` times until a target TID became the
current task.  It is no longer in the production hot path: all call sites that
went through `apply_scheduler_handoff_plan(YieldTo(...))` now use `yield_current_to`
instead.  The function is retained as a fallback (marked `#[allow(dead_code)]`)
and as the documented baseline for the design constraint rationale.

**Why it existed**: In hosted-dev / test builds there is no real preemption. IPC
send paths that need the receiver to run immediately used this to drive the
cooperative scheduler.  The typical case took ≥2 iterations because TID 0 (idle)
was at the head of the queue.

**Successor**: `yield_current_to(target)` + `on_preempt_prefer(preferred)`.
`yield_current_to` calls `on_preempt_prefer` once: re-enqueues the outgoing task,
then scans the queues for `target` and makes it current directly if found — O(n)
in queue size (MAX_RUN_QUEUE = 64), no loop.

**Invariant (unchanged)**: Must never be called while holding any domain lock.

### Idle/TID 0 semantics

TID 0 is the idle task. Key invariants:

1. `dispatch_next` in `PriorityScheduler` preempts TID 0 when any real task is
   runnable, and removes TID 0 from the membership table so it can be re-enqueued.
2. After `dispatch_next_task()` in tests, TID 0 is displaced from `current`.  Call
   `idle_re_enqueue_for_test()` (or `enqueue_on_cpu(CpuId(0), 0)`) immediately so
   subsequent yields have TID 0 available (see `KERNEL_TEST_RULES.md §Rule 2`).
3. In freestanding builds, when no real task is runnable, arch-specific code enters
   the idle halt loop (WFI on AArch64, HLT on x86_64).  This is signalled by the
   `SCHED_ENTER_IDLE_HLT` log event emitted from `idle_no_eret_loop()` (AArch64).
4. Idle (TID 0) is never blocked via `block_current_cpu`; it only transitions
   between `current` (Running) and queued (Runnable / membership table).
5. In hosted-dev, idle burns host CPU cycles — this is expected; no WFI equivalent
   exists in the test shim.

### `yield_current_to` and `on_preempt_prefer`: one-shot cooperative handoff

`yield_current_to(target)` (`exec_state.rs`) is the non-busy-loop successor to
`switch_to_runnable_tid`.  It performs a single scheduler operation to hand off
the CPU to a specific target task:

1. Sets outgoing (current) task TCB status to `Runnable`.
2. Calls `on_preempt_prefer_current_cpu(target)`:
   - Re-enqueues the outgoing task at the tail of its priority queue.
   - Searches all three priority queues for `target`; if found, removes it and
     sets it as `current` directly (bypasses FIFO order).
   - If `target` is not found, falls back to normal FIFO `dispatch_next`.
3. Sets incoming task TCB status to `Running`.
4. Switches address space (HAL) and kernel context.
5. Returns `true` if `target` became current, `false` otherwise.

**Complexity**: O(P × Q) where P = 3 priority levels, Q ≤ MAX_RUN_QUEUE = 64.
**Loop count**: exactly 1 (no busy-loop). `switch_to_runnable_tid` took ≥2 in
the typical IPC case (TID 0 at queue head); `yield_current_to` always takes 1.

**Idle/TID 0 behavior**: `on_preempt_prefer` re-enqueues the outgoing task before
searching for the target.  If outgoing = TID 0, TID 0 is re-enqueued and remains
in the membership table.  No `idle_re_enqueue_for_test` call is needed after
`yield_current_to` — unlike after `dispatch_next_task`.

**`RingQueue::remove_tid`**: the underlying building block.  Compacts the ring
buffer by shifting elements after the removed slot toward the head.  Safe for
arbitrary removal positions (not just head/tail).  O(Q) where Q ≤ 64.

---

### Capability domain bridge: lock contract and invariants

The capability domain (`capability_state_lock`, lock rank 4) is already fully
wrapped. No direct `self.capability.*` field accesses exist outside the two
accessor closures. This section documents the contract and the bridge to adjacent
domains.

#### Accessors

| Accessor | Rank | Mutability | File |
|---|---|---|---|
| `with_capability_state(f)` | 4 | read-only | `orchestrator_state.rs:263` |
| `with_capability_state_mut(f)` | 4 | read-write | `orchestrator_state.rs:272` |
| `with_task_then_capability(f)` | 2+4 | read-only (both) | `orchestrator_state.rs:315` |

`CapabilitySubsystem` is not `KernelStorage`-wrapped at the top level; the closures
receive `&self.capability` / `&mut self.capability` directly (not `kernel_ref`).

#### Lock rank 4 — what may NOT be called inside a capability closure

Never acquire a lower-ranked lock inside a capability-domain closure:

| Forbidden | Rank | Reason |
|---|---|---|
| Any IPC state mutation | 3 | rank inversion (ipc < cap) |
| `task_state_lock` | 2 | rank inversion |
| `scheduler_state` | 1 | rank inversion |

Capability closures may call functions that:
- Read/write only `CapabilitySubsystem` fields.
- Read/write `KernelStorage<CNodeSpace>` via `kernel_ref` / `kernel_mut`.
- Do not acquire any other named subsystem lock.

#### `with_task_then_capability` ordering invariant

`with_task_then_capability` acquires:
1. `task_state_lock` (rank 2)
2. `capability_state_lock` (rank 4)

This is the only legal multi-lock combination involving the capability domain. Never
acquire `capability_state_lock` first then `task_state_lock` — that inverts the rank
order and can deadlock.

#### Two-phase create pattern (control-plane endpoints and notifications)

`create_endpoint_with_mode` and `create_notification` follow this ordering:

```
Phase 1: with_ipc_state_mut (rank 3) → find slot, bump gen, store object → (idx, gen)
Phase 2: mint_capability_for_active_cnode (rank 4, via with_capability_state_mut) × 2
```

Rank 3 closes before rank 4 opens. Caps are minted using the generation value
captured in Phase 1 — no re-read of the IPC domain after the lock is released.

#### Reply cap record lifecycle

`ReplyCapRecord` entries live in `IpcSubsystem::reply_caps` (rank 3). All create,
update, and delete operations use `with_ipc_state_mut`:

| Operation | Function | Lock |
|---|---|---|
| Create | `create_reply_cap_for_caller` Phase 1 | `with_ipc_state_mut` |
| Update `caller_cap_id` | `create_reply_cap_for_caller` Phase 3 | `with_ipc_state_mut` |
| Consume (ipc_reply) | `ipc_reply` | `with_ipc_state_mut` |
| Revoke (task death / restart) | `revoke_reply_caps_for_caller` | `with_ipc_state_mut` |

After any of these operations returns, the change is immediately visible to any
subsequent `with_ipc_state` call without additional synchronization (the global
`SharedKernel` lock serializes all callers end-to-end).

#### Capability domain bridge tests

Four tests in `src/kernel/boot/tests.rs` verify lock-domain invariants:

- `cap_domain_lock_read_sees_minted_capability` (Stage 4T)
- `cap_domain_with_task_then_capability_reads_consistent_state` (Stage 4T)
- `cap_domain_reply_cap_record_exists_after_create_and_gone_after_revoke` (Stage 4T)
- `lock_order_snapshot_reads_task_then_capability_domains` (pre-4T)

---

### Stage 3: remove global lock from syscall fast path


- Route trap/syscall dispatch directly to subsystem locks where safe.
- Keep global lock only for coarse-grain control-plane operations, if needed.

### Stage 4: per-CPU scheduler/runqueue locking

- Move scheduler queues and CPU-local runnable state to per-CPU lock domains.
- Retain cross-CPU coordination only for explicit migration/work-queue paths.

## 6) Scope note

This document is audit/design-only and does not change runtime lock behavior.

---

## 7) Capability domain audit (post Stage 4T)

### 7.1 Audit scope

Full sweep of all methods in `KernelState` that touch `self.capability.*` fields,
classified by access pattern.  Objective: confirm that every production-code access
to `CapabilitySubsystem` goes through `with_capability_state` / `with_capability_state_mut`.

### 7.2 Classification buckets

| Bucket | Description | Files |
|--------|-------------|-------|
| A | All accesses through `with_capability_state` wrapper | `capability_state.rs`, `capability_lifecycle_state.rs`, `cnode_state.rs`, `delegation_state.rs`, `capability_service_state.rs` |
| B | Multi-lock helper: `with_task_then_capability` (rank 2→4) | `cnode_state.rs::task_cnode` |
| C | `#[cfg(test)]` direct access — test scaffolding only, acceptable | `task_core_state.rs::cspace_for_cnode`, `cspace_for_cnode_mut` |

All production methods are in bucket A or B.  Bucket C is `#[cfg(test)]` only and
acceptable because it never runs in freestanding/production builds.

### 7.3 Capability lock-rank contract

| Rank | Lock | Notes |
|------|------|-------|
| 1 | scheduler_state | Per-CPU runqueue, dispatch, preemption |
| 2 | task_state_lock | TCB allocation, status, affinity |
| 3 | ipc_state_lock | Endpoints, notifications, reply_caps, transfer_envelopes, cross_cpu_work |
| 4 | capability_state_lock | CNode spaces, process_cnodes, delegated_capability_links |
| 5 | vm_state_lock | Page tables, ASID, TLB shootdown coordination |

Acquisition order must be **strictly ascending** (lower rank first).  Acquiring
rank-4 then rank-3 (capability then IPC) is a deadlock hazard and is forbidden.

### 7.4 Direct-access sweep results

The sweep searched all `src/kernel/boot/` files for direct `self.ipc.*`,
`self.capability.*`, and `self.scheduler.*` field accesses outside the approved
accessor wrappers.

**Capability domain (`self.capability.*`)**: All clean. No direct field access in
production code.  Two `#[cfg(test)]` exceptions in `task_core_state.rs` (bucket C
above) are acceptable.

**IPC domain (`self.ipc.*`)**: Two bugs found and fixed in `scheduler_state.rs`:

| Bug | Location | Pattern | Fix |
|-----|----------|---------|-----|
| `escalate_tlb_shootdown_timeout` | `scheduler_state.rs` ~line 341 | Direct `self.ipc.endpoints.get_mut(...)` bypassing `ipc_state_lock` | Wrapped endpoint send in `with_ipc_state_mut`; wake call kept outside lock |
| `process_cross_cpu_work_for_cpu` | `scheduler_state.rs` ~line 440 | Direct `self.ipc.cross_cpu_work.take_for_cpu(cpu)` bypassing `ipc_state_lock` | One-item-at-a-time take under `with_ipc_state`; lock released before `apply_cross_cpu_work` which may itself acquire IPC lock on `TlbShootdownAck` path |

Both fixes preserve lock-rank order (IPC rank 3; scheduler operations that follow
are rank 1/2 which are lower — no inversion possible).

### 7.5 Capability invariant tests added (Stage 4T+1)

Six new tests in `src/kernel/boot/tests.rs`:

| Test | Invariant verified |
|------|--------------------|
| `cap_rights_grant_cannot_widen_rights_beyond_source` | `grant_capability_task_to_task_with_rights` must return `MissingRight` when requested rights exceed source |
| `create_endpoint_both_domains_visible_after_two_phase_create` | After `create_endpoint`, IPC domain (endpoint slot) and capability domain (send/recv caps) are both coherent at call return |
| `create_notification_both_domains_visible_after_two_phase_create` | Same for `create_notification` / notification slot |
| `ipc_timeout_deadline_cleared_in_tcb_after_deadline_fires` | `TCB.ipc_timeout_deadline` is `None` after the timer fires for a deadline-blocked recv |
| `user_task_cnode_isolated_from_system_server_cnode` | Revoking a cap from task 1's cnode does not affect task 2's cnode |
| `cap_materialization_reply_cap_visible_in_capability_domain` | After `create_reply_cap_for_caller`, the reply cap resolves via both `resolve_capability_for_task` and `capability_for_cnode` |

### 7.6 Live conversions made vs deferred

**Made (Stage 4T+1)**:
- `escalate_tlb_shootdown_timeout`: IPC endpoint send now under `with_ipc_state_mut` (rank 3).
- `process_cross_cpu_work_for_cpu`: cross_cpu_work take now under `with_ipc_state` per iteration.

**Deferred**:
- Scheduler block/deadline/futex domain passes — no unsafe direct accesses found; deferred to next campaign.
- VM/fault/TLB audit — helper-only, deferred; all known paths clean.
- Global `SharedKernel` lock removal from syscall fast path — Stage 3+ work; not in scope for Stage 4T+1.

---

## 8) Stage 4T+2 scheduler/lifecycle/IPC-lock audit

### 8.1 Audit scope

Full sweep of `scheduler_state.rs`, `restart_state.rs`, `fault_state.rs`,
`thread_state.rs`, `task_policy_state.rs`, `exec_state.rs`, `ipc_state.rs` and
all scheduler/block/deadline/futex/join/exit/restart/timer/cross-CPU/TLB paths.

### 8.2 Classification table

| Path | File | Category | Status |
|------|------|----------|--------|
| `block_current_on_receive_with_deadline` | `ipc_state.rs` | E (IPC-coupled block) | CLEAN — scheduler→task(2)→IPC(3) order correct |
| `block_current_on_send_with_deadline` | `ipc_state.rs` | E (IPC-coupled block) | CLEAN — same ordering |
| `process_ipc_timeout_deadlines` | `ipc_state.rs` | D (deadline/timer mutation) | CLEAN — `with_tcbs_mut` only |
| `wake_tid_to_runnable` | `ipc_state.rs` | B+C (wake+status) | CLEAN — `with_tcbs_mut` then enqueue |
| `clear_ipc_timeout_for_tid` | `ipc_state.rs` | D | CLEAN — `with_tcbs_mut` |
| `exit_task` | `restart_state.rs` | G (lifecycle) | CLEAN — `with_tcbs_mut`; calls report_task_exit (Bug A, fixed) |
| `restart_task` | `restart_state.rs` | G | CLEAN — `with_tcbs_mut` |
| `mark_task_dead` | `restart_state.rs` | G | CLEAN — `with_tcbs_mut` |
| `report_task_exit_to_supervisor` | `restart_state.rs` | G+E | **BUG A fixed** — now uses `send_message_to_endpoint_and_wake` |
| `report_transfer_revoke_to_supervisor` | `restart_state.rs` | G+E | **BUG B fixed** — now uses `send_message_to_endpoint_and_wake` |
| `emit_fault_report_for_fault` | `fault_state.rs` | G+E | **BUG C fixed** — now uses `send_message_to_endpoint_and_wake` |
| `escalate_tlb_shootdown_timeout` | `scheduler_state.rs` | H+E | **Stage 4T+1 fix** refactored to `send_message_to_endpoint_and_wake` |
| `register_task_with_class_and_cnode_slots_in_process` | `task_policy_state.rs` | C (task status mutation) | **BUG D fixed** — now uses `with_tcbs_mut` for TCB insertion |
| `join_thread` | `thread_state.rs` | F/G | CLEAN — `with_tcbs_mut` + `block_current_cpu` + `dispatch_next_task` |
| `wake_joiners_for` | `thread_state.rs` | B+C | CLEAN — `with_tcbs_mut` then `enqueue_task` per woken tid |
| `dispatch_next_task` | `ipc_state.rs`/scheduler | A (scheduler read) | CLEAN — `scheduler_state()` guard |
| `yield_current` | ipc/scheduler | B | CLEAN — `scheduler_state()` guard |
| `yield_current_to` | scheduler | B | CLEAN — one-shot handoff pattern |
| `futex_wait_current` | (ipc_state) | F | CLEAN — `with_tcbs_mut` + block pattern |
| `futex_wake` | (ipc_state) | F | CLEAN — `with_tcbs_mut` then enqueue |
| `process_cross_cpu_work_for_cpu` | `scheduler_state.rs` | I | CLEAN (Stage 4T+1 fix) |
| `apply_cross_cpu_work` (TlbShootdownAck) | `scheduler_state.rs` | I+H | CLEAN — `with_ipc_state_mut` |

### 8.3 Bugs found and fixed

| Bug | Location | Pattern | Fix |
|-----|----------|---------|-----|
| A | `restart_state.rs::report_task_exit_to_supervisor` | Direct `self.ipc.endpoints.get_mut(...)` without `ipc_state_lock` | `send_message_to_endpoint_and_wake` |
| B | `restart_state.rs::report_transfer_revoke_to_supervisor` | Same direct bypass | `send_message_to_endpoint_and_wake` |
| C | `fault_state.rs::emit_fault_report_for_fault` | Same direct bypass | `send_message_to_endpoint_and_wake` |
| D | `task_policy_state.rs::register_task_with_class_and_cnode_slots_in_process` | Direct `self.tcbs[idx] = Some(tcb)` without `task_state_lock` | `with_tcbs_mut` for TCB slot, companion `task_classes[idx]` set after |

### 8.4 New plan-first scaffold

`send_message_to_endpoint_and_wake` (`ipc_state.rs`) — canonical supervisor-notify
pattern: enqueues a message under `ipc_state_lock` (rank 3) then wakes the waiter
after releasing the lock.  Enforces the ordering: task lock (rank 2) must not be
held while acquiring IPC lock (rank 3).  All 4 supervisor-endpoint send sites now
use this helper.

### 8.5 Lock-rank contract additions

| Operation | Correct order | Forbidden |
|-----------|-------------|----------|
| Supervisor endpoint notify | Read `fault_state` (rank 8) → enqueue under `ipc_state_lock` (rank 3) → wake after lock release (acquires task rank 2) | Do NOT hold `ipc_state_lock` when calling `wake_tid_to_runnable` |
| TCB registration | `with_tcbs_mut` (rank 2) → set companion `task_classes` after release | Do NOT mutate `self.tcbs[idx]` directly |
| Deadline registration/clear | `with_tcbs_mut` (rank 2) | Do NOT hold `ipc_state_lock` during `ipc_timeout_deadline` mutation |

### 8.6 Direct-access sweep results (Stage 4T+2)

**scheduler domain** (`self.scheduler_state`): All clean — all access via `scheduler_state()` guard or `with_scheduler_state*` wrappers.

**task/TCB domain** (`self.tcbs`, `self.task_classes`): Clean after fix D.  `self.task_classes[idx]` accessed immutably inside `with_tcbs` closures (under `task_state_lock`) is acceptable.

**IPC domain** (`self.ipc.*`): Clean after fixes A/B/C.

**Other direct fields** (`self.robust_futex`, `self.tls_restore_pending`): These are `KernelStorage` fields with no dedicated subsystem lock.  Accesses are serialized by the top-level `SharedKernel` lock.  Acceptable.

**`exec_state.rs:1227`** (`&self.tcbs as *const _ as usize`): Diagnostic raw-pointer log, no data access.  Acceptable.

### 8.7 Tests added (Stage 4T+2)

| Test | Invariant |
|------|-----------|
| `task_exit_supervisor_report_message_visible_via_ipc_state` | Bug A regression: message enqueued via `with_ipc_state` |
| `transfer_revoke_supervisor_report_message_visible_via_ipc_state` | Bug B regression |
| `fault_handler_report_message_visible_via_ipc_state` | Bug C regression |
| `register_task_tcb_and_class_consistent_after_allocation` | Bug D regression: both `tcbs` and `task_classes` set correctly |
| `send_message_to_endpoint_and_wake_enqueues_and_wakes` | New helper contract: message enqueued and waiter woken |
| `exit_task_leaves_exited_status_not_runnable_in_queue` | Exited task never appears Runnable |
| `restart_task_makes_task_runnable_with_new_token` | Restart is idempotent; stale token rejected |
| `ipc_timeout_not_fired_when_message_delivered_before_deadline` | No spurious timeout_fired after direct delivery before deadline |

### 8.8 VM/fault/TLB audit

Scoped to helper-only:
- `emit_fault_report_for_fault` (Bug C) was the only direct bypass; fixed.
- `try_handle_demand_page_fault` uses `self.user_spaces.get(asid)` directly (not through `with_user_spaces`) — this is inside a method that does NOT hold any subsystem lock, and `user_spaces` is the `pub` field protected by `vm_state_lock` in the `with_user_spaces*` wrappers.  This is a minor inconsistency but not a race hazard given the global `SharedKernel` lock.  Classified as deferred for a future VM domain pass.
- All other VM mutation paths (`map_user_page_in_asid_raw`, `clone_user_address_space_cow`, etc.) use `with_user_spaces_mut`.

### 8.9 Paths still globally locked and why

All paths remain serialized by `SharedKernel::with(...)` at the top level.  The subsystem locks exist to document future domain-split intent and to enforce ordering when splitting becomes safe.

- `dispatch_next_task`, `yield_current`, scheduler queue operations: Not yet split from the global lock.  Requires per-CPU runqueue architecture (Stage 4+).
- `futex_wait_current`, `futex_wake`: Not yet split; require per-futex-key lock + scheduler integration.
- Fork/spawn/exec paths: Not split; large compound operations touching VM, cap, and task domains simultaneously.

### 8.10 Next recommended domain pass

VM/fault/TLB domain split:
- Audit `user_spaces` direct accesses in `fault_state.rs` and `thread_state.rs`.
- Enforce `with_user_spaces*` wrappers consistently (currently ~3 inconsistent direct accesses).
- Audit `memory_lifecycle_state.rs` for `with_memory_state_mut` coverage.

---

## 9) Stage 4T+3 VM/fault/TLB/user-space domain audit

### 9.1 Scope

Full sweep of direct `self.user_spaces.*` and `self.memory.*` accesses outside the
`with_user_spaces`/`with_user_spaces_mut` (rank 5) and `with_memory_state`/
`with_memory_state_mut` (rank 6) wrappers across all `boot/*.rs` files.

Files audited: `memory_lifecycle_state.rs`, `memory_state.rs`, `user_memory_state.rs`,
`exec_state.rs`, `fault_state.rs`, `thread_state.rs`.

### 9.2 Audit results

| File | Status |
|------|--------|
| `fault_state.rs` | CLEAN — all user_spaces accesses via `with_user_spaces` |
| `thread_state.rs` | CLEAN — all accesses via wrappers |
| `exec_state.rs` | CLEAN (prod); one `#[cfg(test)]` direct access kept (test-only) |
| `memory_state.rs` (lines 1–610) | CLEAN — COW, clone, destroy, alloc paths all via wrappers |
| `memory_state.rs` (lines 611+) | Bug G (6 functions) — fixed in this pass |
| `user_memory_state.rs` | Bug F (2 cfg variants of `validate_user_access_for_asid`) — fixed |
| `memory_lifecycle_state.rs` | Bug E (6 functions) — fixed in this pass |

### 9.3 Bugs found and fixed

| ID | File | Function(s) | Pattern | Fix |
|----|------|-------------|---------|-----|
| E | `memory_lifecycle_state.rs` | `adjust_memory_object_cap_refcount`, `adjust_memory_object_pin_refcount`, `note_mapping_inserted`, `note_mapping_removed`, `reclaim_memory_object_if_unreferenced`, `reclaim_memory_object_for_phys` | Direct `self.memory.memory_objects[slot]` and `self.memory.frame_allocator` without `with_memory_state_mut` | Wrapped mutation in `with_memory_state_mut`; `reclaim_memory_object_for_phys` reads id under `with_memory_state` then calls `reclaim_memory_object_if_unreferenced` |
| F | `user_memory_state.rs` | `validate_user_access_for_asid` (both cfg variants) | Direct `self.user_spaces.get(asid)` — 1 site in hosted-dev, 3 sites in non-hosted | Consolidated all user_spaces reads into single `with_user_spaces` call per variant; preserved all trace logging |
| G | `memory_state.rs` | `unmap_user_page_in_current_asid`, `is_user_page_mapped_in_current_asid`, `unmap_user_page`, `unmap_user_page_in_asid`, `protect_user_page`, `protect_user_page_in_asid` | Direct `self.user_spaces.get_mut/get(asid)` | Wrapped page-table mutations in `with_user_spaces_mut`; for protect functions, extracted `(old, current_phys)` tuple from closure |

### 9.4 Lock-ordering preserved

All fixes produce strictly ordered lock acquisitions:
- `with_user_spaces_mut` (rank 5) → released → `with_memory_state_mut` (rank 6) for
  lifecycle ops (`note_mapping_inserted/removed`, `clear_cow_page`).
- No rank inversion: memory lock never acquired while vm lock is held.
- IPC lock (rank 3) for `request_live_asid_shootdown` is acquired after both vm and
  memory locks are released.

### 9.5 Remaining known direct accesses (deferred)

| File | Location | Justification |
|------|----------|---------------|
| `exec_state.rs` test | `load_elf_copies_into_staging_then_finalizes_rx_permissions` | `#[cfg(test)]` only; single-threaded, no lock discipline needed |
| `spawn_user_task_from_image` test helper | `spawn_user_task_from_image_registers_asid_and_class` in `tests.rs` | Test direct access; deferred |
| Various `tests.rs` helpers | Test-only code | Tests may access fields directly; production paths are covered |

### 9.6 Test coverage

Six new invariant tests added at the bottom of `tests.rs` (total: 544 / 0 failed):

| Test | Invariant |
|------|-----------|
| `memory_lifecycle_note_mapping_inserted_increments_map_refcount_via_with_memory_state` | Bug E regression: `note_mapping_inserted` increments map_refcount under memory lock |
| `memory_lifecycle_note_mapping_removed_decrements_map_refcount_via_with_memory_state` | Round-trip insert→remove restores map_refcount to 0 |
| `memory_lifecycle_cap_refcount_delta_visible_via_with_memory_state` | `adjust_memory_object_cap_refcount` ±1 delta visible via `with_memory_state` |
| `vm_domain_unmap_in_asid_removes_mapping_visible_via_with_user_spaces` | Bug G regression: unmap visible via `with_user_spaces` |
| `vm_domain_is_user_page_mapped_in_current_asid_reflects_mapping_state` | `is_user_page_mapped_in_current_asid` returns true/false correctly via vm lock |
| `vm_domain_map_page_increments_memory_object_map_refcount_consistent_end_to_end` | End-to-end: map increments refcount (rank 5 → rank 6 sequential), unmap decrements it |

### 9.7 Paths still globally locked

All paths remain serialized by `SharedKernel::with(...)`.  The vm and memory locks
document future domain-split intent and enforce ordering when splitting becomes safe.

The `validate_user_access_for_asid` fix merges 3 separate `self.user_spaces` reads
into one `with_user_spaces` call, reducing lock contention on the future hot path
while preserving all trace logging.

---

## §10 Stage 4T+4 — Compound lifecycle and domain-integration audit

### 10.1 Audit scope

Stage 4T+4 audited compound lifecycle paths, cross-domain interactions, and
remaining direct-field bypasses across the kernel boot subsystem.

**TLB/shootdown audit result (agent 3):** All `live_tlb_shootdown` and
`cross_cpu_work` field accesses are within `with_ipc_state` / `with_ipc_state_mut`
closures. No bypass accesses found. The backward lock ordering (memory rank 6 acquires
IPC rank 3) in `request_live_asid_shootdown` is acceptable because no nested
acquisitions to lower ranks occur.

**task_policy_state.rs `task_classes` window (intentional):** After `with_tcbs_mut`
releases the task lock, `task_classes[inserted_idx] = Some(class)` runs outside the
closure. The developer-documented invariant is: no other path can observe the new TCB
slot until `provision_default_kernel_context` completes, because `KernelState` requires
`&mut self` (exclusive) and the kernel runs with interrupts disabled during task
creation. This window is documented and not a real race.

**`tls_restore_pending` / `robust_futex` companion arrays** in `thread_state.rs`:
These companion arrays share the task-lock rank by design — they are accessed alongside
`tcbs` in the same `&mut self` scope. No dedicated wrapper is needed.

### 10.2 Bugs fixed

| Bug | File | Location | Description |
|-----|------|----------|-------------|
| H | `user_memory_state.rs` | `write_user_byte` (hosted-dev, L11) | Direct `self.memory.user_memory` access — wrapped in `with_memory_state_mut` |
| H | `user_memory_state.rs` | `read_user_byte` (hosted-dev, L31) | Direct `self.memory.user_memory` access — wrapped in `with_memory_state` |
| I | `task_core_state.rs` | `tcb_mut()` (L38) | Production bypass exposing `&mut ThreadControlBlock` — restricted to `#[cfg(test)]`, new `with_tcb_mut` helper added |
| I | `fault_endpoint_state.rs` | `set_task_fault_policy` (L66) | Used `tcb_mut()` — converted to `with_tcb_mut` |
| I | `fault_endpoint_state.rs` | `bind_task_asid` (L116) | Used `tcb_mut()` — converted to `with_tcb_mut` |
| I | `syscall.rs` | `clear_blocked_recv_state` (L201) | Used `tcb_mut()` — converted to `with_tcb_mut` |
| I | `syscall.rs` | `complete_blocked_recv_for_waiter` (L214,308) | Used `tcb_mut()` — converted to `with_tcb_mut` |
| I | `syscall.rs` | recv blocking path (L1122) | Used `tcb_mut()` — converted to `with_tcb_mut` |

### 10.3 New `with_tcb_mut` helper

`task_core_state.rs` now exposes:

```rust
pub(crate) fn with_tcb_mut<R>(
    &mut self,
    tid: u64,
    f: impl FnOnce(&mut ThreadControlBlock) -> R,
) -> Option<R>
```

This acquires the task lock (rank 2) via `with_tcbs_mut`, finds the TCB by TID,
and calls the closure with `&mut ThreadControlBlock`. The old `tcb_mut()` method
which returned `&mut ThreadControlBlock` without the lock is now `#[cfg(test)]` only.

### 10.4 Lock-ordering preserved

- Bug H: memory domain lock (rank 6) used for `user_memory` hashmap mutations.
- Bug I: task lock (rank 2) via `with_tcb_mut` for all TCB field mutations.
- No new lock-rank inversions introduced.

### 10.5 Test coverage

Three new invariant tests added (total: 547 / 0 failed):

| Test | Invariant |
|------|-----------|
| `task_domain_with_tcb_mut_set_fault_policy_visible_via_effective_fault_policy_for` | Bug I regression: `set_task_fault_policy` uses task lock, result visible via `effective_fault_policy_for` |
| `task_domain_with_tcb_mut_bind_task_asid_visible_via_task_asid` | Bug I regression: `bind_task_asid` uses task lock, ASID visible via `task_asid` |
| `memory_domain_write_user_byte_goes_through_memory_lock_round_trip` | Bug H regression (hosted-dev): `copy_to_user` → `read_user_memory_for_asid` round-trip preserves data through memory lock |

---

## §11 Stage 4T+5 — Global-lock split-read/split-mut readiness pass

### 11.1 Audit scope

Stage 4T+5 audited all `SharedKernel::with` and `SharedKernel::with_cpu` production
call sites for split-read/split-mut conversion readiness.

### 11.2 SharedKernel call-site classification

| Location | Method | Class | Reason |
|----------|--------|-------|--------|
| `arch/trap_entry.rs:149` | `with_cpu → handle_trap_entry_with_fault_bookkeeping_mode` | **F** | Arch/trap boundary — sets current_cpu, full trap dispatch |
| `arch/x86_64/descriptor_tables.rs:823,857` | `with_cpu → current_tid()` (entering/exiting diagnostic) | **F** | Arch/trap boundary — used for GPR writeback decision; defer |
| `arch/x86_64/descriptor_tables.rs:850` | `with_cpu → log_decoded_fatal_trap` | **F** | Arch fatal-trap path — must hold global lock |
| `runtime.rs:193` | `with → try_ipc_recv` | **H** | Mutates IPC endpoint state |
| `runtime.rs:197` | `with → ipc_recv_until_deadline` | **D** | Pre-staged: `ipc_recv_with_deadline_split_bridge` handles deadline staging |
| `runtime.rs:207` | `with_cpu → handle_trap` | **F** | Arch/trap boundary — `handle_trap_with_cpu` entry |
| `runtime.rs:217-222` | `with → task/enqueue/dispatch ops` | **H** | Multi-domain mutations (scheduler+task+IPC) |
| `runtime.rs:260-278` | `with → cross-CPU work submit/process` | **H** | IPC+scheduler mutations |
| Telemetry mutation paths | `increment_tlb_shootdown_count_split_mut` etc. | **C** | Already split (telemetry_state_lock rank 10) |
| Fault mutation paths | `record_fault_split_mut` etc. | **C** | Already split (fault_state_lock rank 8) |
| Scheduler reads | `scheduler_tick_now_split_read` etc. | **C** | Already split (scheduler lock rank 1) |
| Boot config reads | `capacity_profile_split_read` etc. | **C** | Already split (boot_config_state_lock rank 11) |

**Class codes:** A=ready split-read, B=ready split-mut, C=already split, D=helper-only staged,
E=needs plan-first decomp, F=arch/trap boundary defer, G=boot-only/global OK, H=unsafe to split yet.

### 11.3 New split-read helpers added

Five new `SharedKernel` split-read methods (all under subsystem lock only, no global lock):

| Helper | Lock domain | Rank | Pattern |
|--------|-------------|------|---------|
| `last_fault_split_read()` | `fault_state_lock` | 8 | Fault subsystem |
| `last_fault_frame_split_read()` | `fault_state_lock` | 8 | Fault subsystem |
| `fault_policy_split_read()` | `fault_state_lock` | 8 | Fault subsystem |
| `tlb_shootdown_count_split_read()` | `telemetry_state_lock` | 10 | Telemetry subsystem |
| `tlb_shootdown_timeout_count_split_read()` | `telemetry_state_lock` | 10 | Telemetry subsystem |

Two private `SharedKernel` infrastructure helpers:
- `with_fault_split_read<R>(&self, f)` — reuses `fault_split_mut_ptrs_from_raw`, downgrades `*mut` to `*const` for read
- `with_telemetry_split_read<R>(&self, f)` — reuses `telemetry_split_mut_ptrs_from_raw`, same pattern

All new helpers document:
- which lock they acquire
- which locks must not be held
- that they do not acquire the outer `SharedKernel` lock

### 11.4 Complete split helper inventory (post-Stage 4T+5)

**Scheduler domain (rank 1):**
- `scheduler_tick_now_split_read()` — timer tick read
- `current_tid_split_read(cpu)` — per-CPU current TID read
- `online_cpu_count_split_read()` — topology read
- `present_cpu_count_split_read()` — topology read

**Fault domain (rank 8):**
- `record_fault_split_mut(fault)` — write last_fault
- `record_fault_frame_snapshot_split_mut(frame)` — write last_fault_frame
- `clear_last_fault_split_mut()` — clear both fault fields
- `last_fault_split_read()` **NEW** — read last_fault
- `last_fault_frame_split_read()` **NEW** — read last_fault_frame
- `fault_policy_split_read()` **NEW** — read fault_policy

**Telemetry domain (rank 10):**
- `increment_tlb_shootdown_count_split_mut()` — counter increment
- `add_tlb_shootdown_timeout_count_split_mut(delta)` — counter add
- `tlb_shootdown_count_split_read()` **NEW** — counter read
- `tlb_shootdown_timeout_count_split_read()` **NEW** — counter read

**Boot config domain (rank 11):**
- `capacity_profile_split_read()` — immutable config read
- `runtime_capacity_config_split_read()` — computed config read

**IPC recv bridge:**
- `ipc_recv_with_deadline_split_bridge(cap, timeout)` — pre-stages deadline then calls `with()`

### 11.5 Remaining direct bypass sweep result

All direct field accesses classified (no new bypasses found vs Stage 4T+4):

| Category | Status |
|----------|--------|
| `orchestrator_state.rs` wrapper implementations | Legitimate — inside lock closures |
| Test-only `tcb_mut`, `cspace_for_cnode`, `cspace_for_cnode_mut` | `#[cfg(test)]` gated |
| `tls_restore_pending` / `robust_futex` in `thread_state.rs` | Companion arrays, task lock covers them; acceptable |
| `task_classes[idx]` post-lock in `task_policy_state.rs:51` | Documented intentional window, `&mut self` exclusivity |
| `task_classes[idx]` inside `with_tcbs` in `task_policy_state.rs:138` | Protected by task lock via closure capture |

### 11.6 Paths still globally locked and why

| Path | Why globally locked |
|------|-------------------|
| `handle_trap_with_cpu` / `dispatch_trap_entry_with_shared_kernel` | Full trap dispatch: current_cpu mutation, TrapFrame writeback, IPC/cap/VM/scheduler coupling |
| x86_64 `entering_tid` / `exiting_tid` reads at trap boundary | Correctness: used to determine `task_switched` → GPR writeback. Arch boundary (F). Diagnostic `current_tid_split_read` comparison shows they always match; live conversion deferred. |
| IPC recv/send/call/reply | Mutates IPC endpoints, scheduler queues, TCBs simultaneously |
| Task lifecycle (register/enqueue/dispatch) | Multi-domain: task + scheduler + capability + IPC |
| SpawnV5 / exec / COW / VM_ANON_MAP | Multi-domain with TrapFrame writeback |

### 11.7 Tests

Three new split-read correctness tests added (`runtime::tests`):

| Test | Invariant |
|------|-----------|
| `fault_split_read_helpers_match_kernel_state_accessors` | `last_fault_split_read` / `last_fault_frame_split_read` match global-lock reads; clear propagates |
| `fault_policy_split_read_matches_kernel_state_accessor` | `fault_policy_split_read` matches `state.fault_policy()`, default is `KillTask` |
| `telemetry_split_read_helpers_match_kernel_state_accessors` | `tlb_shootdown_count_split_read` / `timeout_count_split_read` match global reads, see split_mut updates |

Total: 547 → 550 / 0 failed.

---

## §12 Stage 4T+6 — x86_64 trap TID split-read conversion

### 12.1 Motivation

The x86_64 shared-trap dispatch function
(`yarm_x86_dispatch_trap_from_stub` in `src/arch/x86_64/descriptor_tables.rs`)
previously read `entering_tid` and `exiting_tid` using:

```rust
shared.with_cpu(cpu, |k| k.current_tid()).unwrap_or(None)
```

Each call acquired the global `SharedKernel` `SpinLock<KernelState>`, called
`set_current_cpu(cpu)` (acquiring the scheduler lock to set `current_cpu`), and then
read `current_tid_on(current_cpu)`.  This imposed 2 additional global lock
acquisitions per trap — one for the entering snapshot, one for the exiting snapshot —
in addition to the global lock already held for `dispatch_trap_entry_with_shared_kernel`.

Stage L5A introduced `SharedKernel::current_tid_split_read(cpu)` as a staged helper
but immediately reverted its use after observing x86_64 startup corruption. Stage L5B
re-introduced it as a diagnostic-only comparison gate (`X86_TID_SPLIT_READ_DIAG: bool
= false`).  Stage 4T+5 confirmed in `§11.6` that "diagnostic `current_tid_split_read`
comparison shows they always match; live conversion deferred."

Stage 4T+6 completes the conversion after establishing a formal arch-boundary safety
proof.

### 12.2 Arch-boundary safety proof

**`current_tid_split_read(cpu)` is equivalent to
`with_cpu(cpu, |k| k.current_tid()).unwrap_or(None)` for all reachable CPU states.**

#### Case 1 — CPU offline (`validate_online_cpu` would fail)

- `with_cpu` path: `with_cpu(cpu, ...)` calls `set_current_cpu(cpu)` which calls
  `validate_online_cpu(cpu)`; if the CPU is offline this returns `Err`, and
  `unwrap_or(None)` produces `None`.
- Split-read path: `current_tid_split_read(cpu)` calls `check_online_cpu(cpu).ok()?`;
  if the CPU is offline this returns `None`.
- **Both paths produce `None`. Equivalent.**

#### Case 2 — CPU online

- `with_cpu` path: `set_current_cpu(cpu)` sets `scheduler_state.current_cpu = cpu`;
  then `k.current_tid()` reads `current_tid_on(scheduler_state.current_cpu)` =
  `current_tid_on(cpu)`.
- Split-read path: `current_tid_split_read(cpu)` directly reads
  `scheduler.current_tid_on(cpu)` under the scheduler lock.
- **Both paths read `current_tid_on(cpu)` for the same CPU.  Equivalent.**

#### Side-effect analysis: `set_current_cpu(cpu)`

`set_current_cpu(cpu)` has one side effect beyond the `current_cpu` field write: it
establishes `current_cpu = cpu` for any code that reads `scheduler_state.current_cpu`
later in the same lock critical section.

- **For `entering_tid`**: the `with_cpu` call happens before
  `dispatch_trap_entry_with_shared_kernel(shared, cpu, context, frame)`, which
  internally also calls `shared.with_cpu(cpu, |k| ...)` — this second `with_cpu` call
  will again call `set_current_cpu(cpu)` and override any value set by the
  entering-snapshot `with_cpu` call.  The `set_current_cpu` side effect from the
  entering snapshot is therefore completely shadowed before any code can observe it.
- **For `exiting_tid`**: the `with_cpu` call happens after `dispatch_trap_entry_with_shared_kernel`
  has returned and released all its locks.  After the dispatch returns, no other code
  path reads `scheduler_state.current_cpu` before the trap handler returns to hardware.
  The side effect of setting `current_cpu = cpu` is harmless — it sets the field to the
  same value it already holds from the last `set_current_cpu(cpu)` call inside dispatch.

**Conclusion**: removing `set_current_cpu` side effects does not change any observable
behavior at the entering or exiting TID call sites.

#### `task_switched` detection correctness

`task_switched = entering_tid != exiting_tid` determines whether
`write_task_gprs_to_saved_regs` (full task-switch frame writeback) or
`write_trap_returns_to_saved_regs` (syscall-return-only writeback) is used.

The split-read produces the same `entering_tid` and `exiting_tid` values as the
conservative path (proved above), so `task_switched` is computed identically and the
correct writeback path is always selected.

### 12.3 Changes made

**`src/arch/x86_64/descriptor_tables.rs`**:

1. Removed dead constant `X86_TID_SPLIT_READ_DIAG: bool = false` (and the dead
   diagnostic comparison blocks it guarded, which were compiled out by the `if false`
   branch optimizer in all builds).

2. Replaced the `entering_tid` snapshot:
   ```rust
   // REMOVED (Class F guard, 2 lock acquisitions):
   let entering_tid: Option<u64> = shared
       .with_cpu(cpu, |k| k.current_tid())
       .unwrap_or(None);

   // NEW (Class E, scheduler lock rank 1 only):
   // Stage 4T+6: current_tid_split_read(cpu) is equivalent to with_cpu→current_tid
   // because current_tid_on(cpu) == set_current_cpu(cpu)→current_tid_on(current_cpu)
   // for online CPUs, and both return None for offline CPUs.  The set_current_cpu
   // side effect was immediately overridden by dispatch's own with_cpu call.
   let entering_tid: Option<u64> = shared.current_tid_split_read(cpu);
   ```

3. Replaced the `exiting_tid` snapshot identically, with an additional comment noting
   that at exit time the dispatch has already released all its locks and the scheduler
   state reflects the final dispatched task.

4. **Fatal-trap `with_cpu` kept as-is (Class F)**:
   ```rust
   let _ = shared.with_cpu(cpu, |k| {
       log_decoded_fatal_trap(Some(k), vector, error_code, frame, fault_addr);
   });
   ```
   This call passes `&mut KernelState` to the fatal-trap logger, which is required for
   the logger to access `KernelState` subsystem state.  No split-read can replace it.
   (This was subsequently replaced by `log_decoded_fatal_trap_from_snapshot` in Stage 4T+7.)

**Net effect (Stage 4T+6 intent)**: 2 global lock acquisitions per trap eliminated (entering + exiting
snapshots).  The fatal-trap path (1 global lock, Class F) and the main dispatch path
(1 global lock for `dispatch_trap_entry_with_shared_kernel`) are unchanged.

> **⚠ Stage 4T+6R REVERT**: The entering_tid and exiting_tid conversions were subsequently
> reverted (Stage 4T+6R, same commit series) because x86_64 smoke testing showed the
> service chain stalling (service_entries=0, repeated SCHED_ENTER_IDLE_HLT) after the
> conversion.  See §12.8 for the full revert record.  `current_tid_split_read` remains
> available as a helper for other callers (e.g., AArch64 trace path, Stage L6B) but is
> **not live in the x86_64 trap entering/exiting TID snapshots**.

### 12.4 Classification update (§11.2 revision)

After Stage 4T+6R revert, the entering_tid and exiting_tid sites remain Class F:

| Location | Method | Stage 4T+5 Class | Stage 4T+6 (reverted) | Final (4T+6R) |
|----------|--------|------------------|-----------------------|---------------|
| `arch/x86_64/descriptor_tables.rs` (entering_tid) | `with_cpu → current_tid` | F | E (broken, reverted) | **F (restored)** |
| `arch/x86_64/descriptor_tables.rs` (exiting_tid) | `with_cpu → current_tid` | F | E (broken, reverted) | **F (restored)** |
| `arch/x86_64/descriptor_tables.rs` (fatal-trap) | `with_cpu → log_decoded_fatal_trap` | F | F (kept) | **E (converted Stage 4T+7)** |

### 12.5 Complete split helper inventory update

**Scheduler domain (rank 1):** (updated from §11.4)
- `scheduler_tick_now_split_read()` — timer tick read
- `current_tid_split_read(cpu)` — per-CPU current TID read (**now live in x86_64 trap**)
- `online_cpu_count_split_read()` — topology read
- `present_cpu_count_split_read()` — topology read

### 12.6 Tests

Stage 4T+6 added four split-read helper tests; Stage 4T+6R added three with_cpu path tests.

**Stage 4T+6 split-read helper tests** (value-equivalence; helper is still used by AArch64 trace):

| Test | Invariant |
|------|-----------|
| `current_tid_split_read_matches_with_cpu_current_tid_entering_snapshot` | `current_tid_split_read(cpu)` == `with_cpu(cpu, \|k\| k.current_tid()).unwrap_or(None)` after dispatch; both return `Some(77)` |
| `current_tid_split_read_reflects_task_switch_for_exiting_snapshot` | After `yield_current()` from task 81 (with task 82 queued), split read returns `Some(82) ≠ Some(81)`; `task_switched` flag is true |
| `current_tid_split_read_no_switch_detection_for_same_task_return` | After dispatch to task 71 with no yield, split read returns same TID for both entering and exiting snapshots; `task_switched` is false |
| `current_tid_split_read_offline_cpu_returns_none` | `current_tid_split_read(CpuId(255))` returns `None` for an offline/nonexistent CPU |

**Stage 4T+6R with_cpu path tests** (cover the reverted live code):

| Test | Invariant |
|------|-----------|
| `with_cpu_entering_exiting_tid_detects_task_switch` | `with_cpu→current_tid` entering_tid=Some(83), exiting_tid=Some(84) after yield; `task_switched` is true |
| `with_cpu_entering_exiting_tid_no_switch_same_task` | Two consecutive `with_cpu→current_tid` calls without yield return equal TIDs; `task_switched` is false |
| `with_cpu_entering_tid_offline_cpu_returns_none` | `with_cpu(CpuId(7), …).unwrap_or(None)` returns `None` for offline CPU |

Total: 550 (pre-4T+6) → 554 (4T+6) → 557 (4T+7) → **560 (4T+6R)** / 0 failed.

### 12.7 Correctness note: value-equivalence is not behavior-equivalence

The Stage 4T+6 conversion proved that `current_tid_split_read(cpu)` and
`with_cpu(cpu, |k| k.current_tid()).unwrap_or(None)` return the same value for all
reachable scheduler states (online/offline CPU, dispatched/idle task).

However, the x86_64 smoke test showed the service chain stalling after the conversion
(service_entries=0, repeated SCHED_ENTER_IDLE_HLT) despite all unit tests passing.

The root cause was not identified through static analysis — the `set_current_cpu` side
effect of `with_cpu` is provably redundant (the main dispatch's `with_cpu` also calls
`set_current_cpu(cpu)`, and `handle_trap_entry_with_fault_bookkeeping_mode` calls it
again defensively). Yet removing the entering/exiting TID `with_cpu` calls broke
hardware behavior.

**Lesson**: For arch-boundary trap paths, smoke-level acceptance testing (service chain
running, tasks dispatching correctly) is the required acceptance criterion. Unit tests
proving return-value equivalence are necessary but not sufficient. A conversion must pass
smoke testing before it can be considered complete.

### 12.8 Stage 4T+6R — Revert record

**What was reverted**: Both entering_tid and exiting_tid reads in `yarm_x86_dispatch_trap_from_stub`
were restored to `with_cpu(cpu, |k| k.current_tid()).unwrap_or(None)`.

**What was NOT reverted**: 
- `current_tid_split_read` helper remains; it is still used by AArch64 trace (Stage L6B, Class C).
- Stage 4T+7 fatal-trap snapshot conversion remains (smoke shows `real_fatal_ish=0`; that code path was never triggered by the regression).

**Final state of x86_64 shared trap TID reads**:
```rust
// entering_tid — with_cpu global lock (Class F, restored Stage 4T+6R)
let entering_tid: Option<u64> = shared
    .with_cpu(cpu, |k| k.current_tid())
    .unwrap_or(None);
// ... dispatch ...
// exiting_tid — with_cpu global lock (Class F, restored Stage 4T+6R)
let exiting_tid: Option<u64> = shared
    .with_cpu(cpu, |k| k.current_tid())
    .unwrap_or(None);
```

### 12.9 What Stage 4T+6 does NOT change (final state after 4T+6R)

- IPC syscall ABI and SYSCALL_COUNT: unchanged.
- SpawnV5, MemoryObject zero-copy, VFS, syscall 27, VFS_READ_SHARED_REPLY_ENABLED: untouched.
- x86_64 SMP and `src/arch/x86_64/smp.rs`: untouched.
- x86_64 register writeback semantics: unchanged — `task_switched` computation and
  `write_task_gprs_to_saved_regs` / `write_trap_returns_to_saved_regs` selection are
  identical to the pre-Stage-4T+6 behavior.
- TrapFrame contents: unchanged.
- Task switch behavior: unchanged.
- Scheduling decisions: unchanged.
- AArch64 behavior: unchanged.
- The fatal-trap path and main dispatch `with_cpu` calls are retained.
- The global `SharedKernel` lock is still retained for all mutation paths.
  **This is not Stage 3/global-lock removal.**

## §13 Stage 4T+7 — Trap-boundary split-read audit and fatal-trap snapshot conversion

### 13.1 Audit scope

A rigorous arch-boundary pass classified every `current_tid` read and every
`SharedKernel` global-lock acquisition across all four trap stacks (x86_64,
AArch64, RISC-V, `arch/trap_entry.rs`) plus `runtime.rs`.  The classification
scheme from §11 and §12 was applied:

| Class | Meaning |
|-------|---------|
| A/B | Read-only / diagnostic — convertible with existing split-read helpers |
| C | Already converted in a prior stage |
| D | Requires TrapFrame writeback — deferred |
| E | Already converted this stage |
| F | Requires `&mut KernelState` / global lock — keep |
| G | Arch/SMP-sensitive — deferred |
| H | Unsafe to split |

### 13.2 Per-arch audit findings

**x86_64 (`src/arch/x86_64/descriptor_tables.rs`)**

| Location | Description | Class |
|----------|-------------|-------|
| `entering_tid` (shared path) | `with_cpu(cpu, \|k\| k.current_tid()).unwrap_or(None)` — **reverted Stage 4T+6R** (was C after 4T+6, now **F** again) |
| `exiting_tid` (shared path) | `with_cpu(cpu, \|k\| k.current_tid()).unwrap_or(None)` — **reverted Stage 4T+6R** (was C after 4T+6, now **F** again) |
| Fatal-trap `with_cpu` (shared path) | `shared.with_cpu(cpu, \|k\| log_decoded_fatal_trap(Some(k), ...))` — previously F, now **E** (converted, Stage 4T+7) |
| `entering_tid` (raw fallback) | `kernel.current_tid()` — inside raw `&mut KernelState` critical section | F |
| `exiting_tid` (raw fallback) | `kernel.current_tid()` — same critical section | F |
| Fatal-trap (raw fallback) | `log_decoded_fatal_trap(Some(kernel), ...)` — same critical section | F |
| Main dispatch `with_cpu` (shared path) | `dispatch_trap_entry_with_shared_kernel` | F (required) |

**AArch64 (`src/arch/aarch64/boot.rs`, `src/arch/aarch64/trap.rs`)**

| Location | Description | Class |
|----------|-------------|-------|
| AArch64 trace TID (shared path) | `current_tid_split_read(trap_cpu)` under `AARCH64_TRAP_TRACE = false` | C (Stage L6B) |
| All `kernel.current_tid()` in `handle_trap_entry_with_fault_bookkeeping_mode` | All inside `&mut KernelState` under global lock | F |
| Raw fallback `current_tid()` calls | Inside raw `&mut KernelState` path | F |
| Main dispatch `with_cpu` | `handle_trap_entry_shared` → `with_cpu(cpu, \|k\| ...)` | F (required) |

**RISC-V (`src/arch/riscv64/trap.rs`)**

No `current_tid` reads in the handler body. No conversion needed.

**`src/arch/trap_entry.rs`**

| Location | Description | Class |
|----------|-------------|-------|
| `scheduler_tick_now_split_read()` | Timer tick, rank 1 | C (Stage L4A) |
| `record_fault_split_mut(fault)` | Fault bookkeeping, rank 8 | C (Stage 3B-E) |
| Main dispatch `with_cpu` | `handle_trap_entry_shared` | F (required) |

**Conclusion**: The only remaining global-lock acquisition in the arch trap
dispatch path that was potentially convertible was the x86_64 shared-path fatal
trap `with_cpu`.  All other remaining `with_cpu` calls require `&mut KernelState`
(Class F) and must be kept.

### 13.3 Fatal-trap `with_cpu` conversion

**Before (Stage 4T+6, Class F)**:
```rust
let _ = shared.with_cpu(cpu, |k| {
    log_decoded_fatal_trap(Some(k), vector, error_code, frame, fault_addr);
});
```
This acquired the global `SharedKernel` lock solely to read `k.current_tid()` and
`k.task_asid(current_tid)` for diagnostic logging — two read-only accesses inside a
fatal error path that never returns.

**Safety proof**: The fatal-trap logger only reads two fields:
1. `k.current_tid()` — reads `scheduler_state.current_tid_on(current_cpu)` under the
   scheduler lock (rank 1).
2. `k.task_asid(current_tid)` — reads the TCB array under `task_state_lock` (rank 2).

Both reads can be performed without the global lock by acquiring the subsystem locks
directly. Since neither field is mutated, no write-order guarantee is needed.

**Lock sequence for `fatal_trap_read_snapshot`**:
1. Acquire scheduler lock (rank 1) → read `current_tid_on(cpu)` → release.
2. If `current_tid != 0`: acquire task lock (rank 2) → scan TCBs for ASID → release.

Lock ranks are strictly ascending (1 → 2); no lock inversion.

**After (Stage 4T+7, Class E)**:
```rust
// Stage 4T+7: pre-read TID and ASID via split-read helpers (scheduler
// lock rank 1, task lock rank 2) before logging. Avoids the global
// SharedKernel lock in the fatal error path.
let snapshot = shared.fatal_trap_read_snapshot(cpu);
log_decoded_fatal_trap_from_snapshot(snapshot, vector, error_code, frame, fault_addr);
```

### 13.4 New infrastructure

**`src/kernel/boot/orchestrator_state.rs`**:

```rust
pub(crate) unsafe fn task_asid_for_tid_from_raw(state: *const KernelState, tid: u64) -> u64
```
Acquires `task_state_lock` (rank 2) via `addr_of!`-derived raw pointer. Scans TCBs;
returns `asid.0 as u64` or `0` if the task has no ASID binding.  Kept in
`orchestrator_state.rs` (not `runtime.rs`) because `MAX_TASKS` and `ThreadControlBlock`
are private to the `boot` module and accessible there via `use super::*`.

**`src/runtime.rs`**:

```rust
pub struct FatalTrapReadSnapshot {
    pub current_tid: u64,
    pub current_asid: u64,
}

pub fn task_asid_for_tid_split_read(&self, tid: u64) -> u64
// Acquires task_state_lock (rank 2) only via task_asid_for_tid_from_raw.

pub fn fatal_trap_read_snapshot(&self, cpu: CpuId) -> FatalTrapReadSnapshot
// Acquires scheduler lock (rank 1) then task lock (rank 2), in order, both transiently.
```

**`src/arch/x86_64/descriptor_tables.rs`**:

```rust
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
fn log_decoded_fatal_trap_from_snapshot(
    snapshot: crate::runtime::FatalTrapReadSnapshot,
    vector: u64, error_code: u64, frame: &X86InterruptStackFrame, fault_addr: u64,
)
```
Emits identical UART output to `log_decoded_fatal_trap(Some(k), ...)`.  Uses
`snapshot.current_tid` and `snapshot.current_asid` instead of live reads through
`&KernelState`.

### 13.5 Classification update

| Location | Method | Stage 4T+6 Class | Stage 4T+7 Class |
|----------|--------|-----------------|-----------------|
| `descriptor_tables.rs` (fatal-trap, shared path) | `with_cpu → log_decoded_fatal_trap(Some(k), ...)` | F (kept) | **E (converted)** |

### 13.6 Complete split-read helper inventory (updated from §12.5)

**Scheduler domain (rank 1):**
- `scheduler_tick_now_split_read()` — timer tick read
- `current_tid_split_read(cpu)` — per-CPU current TID read (live in x86_64 trap since Stage 4T+6)
- `online_cpu_count_split_read()` — topology read
- `present_cpu_count_split_read()` — topology read

**Task domain (rank 2) — new in Stage 4T+7:**
- `task_asid_for_tid_split_read(tid)` — look up bound ASID for a TID; returns 0 if unbound

**Fault domain (rank 8):**
- `last_fault_split_read()` — last recorded fault
- `last_fault_frame_split_read()` — last fault trap frame
- `fault_policy_split_read()` — global fault policy

**Telemetry domain (rank 10):**
- `tlb_shootdown_count_split_read()` — TLB shootdown counter
- `tlb_shootdown_timeout_count_split_read()` — TLB timeout counter

**Composite snapshot (ranks 1 + 2) — new in Stage 4T+7:**
- `fatal_trap_read_snapshot(cpu)` → `FatalTrapReadSnapshot` — TID + ASID snapshot for fatal trap logging

### 13.7 Tests

Three new split-read correctness tests added (`runtime::tests`):

| Test | Invariant |
|------|-----------|
| `fatal_trap_read_snapshot_tid_matches_split_read` | `snapshot.current_tid` equals `current_tid_split_read(cpu).unwrap_or(0)` after dispatch to task 73 |
| `fatal_trap_read_snapshot_asid_matches_kernel_state_task_asid` | `snapshot.current_asid` equals `task_asid_for_tid_split_read(74)` equals `global_lock task_asid(74)`, all zero for a task without an ASID binding |
| `fatal_trap_read_snapshot_offline_cpu_returns_zeros` | `fatal_trap_read_snapshot(CpuId(255))` returns `current_tid=0`, `current_asid=0` |

Total: 554 → 557 / 0 failed.

### 13.8 What Stage 4T+7 does NOT change

- IPC syscall ABI and SYSCALL_COUNT: unchanged.
- SpawnV5, MemoryObject zero-copy, VFS, syscall 27, VFS_READ_SHARED_REPLY_ENABLED: untouched.
- x86_64 SMP and `src/arch/x86_64/smp.rs`: untouched.
- x86_64 register writeback semantics: unchanged.
- TrapFrame contents: unchanged.
- Task switch behavior and scheduling decisions: unchanged.
- AArch64 and RISC-V behavior: unchanged.
- The main dispatch `with_cpu` call and all raw-fallback Class F paths: retained.
- The global `SharedKernel` lock is still retained for all mutation paths.
  **This is not Stage 3/global-lock removal.**

---

## 14. x86_64 bootstrap timer guard (Phase BT1)

### 14.1 Root cause

`borrow_kernel_for_boot()` bypasses the `SpinLock<KernelState>` and returns a raw
`&mut KernelState`. The x86_64 boot sequence calls `enable_interrupts_for_boot`
(STI) before `run(kernel)` / `bootstrap_first_user_task`. ELF loading for three
~16 MB images in QEMU takes longer than the LAPIC timer deadline of
`BOOTSTRAP_TIMER_DEADLINE_TICKS = 50,000,000 ticks ≈ 800 ms`, so the timer IRQ
fires while bootstrap holds the raw `borrow_kernel_for_boot` reference.

The timer ISR enters via `dispatch_trap_entry_with_shared_kernel` →
`SharedKernel::with_cpu(...)`, which acquires `SpinLock<KernelState>`. This
succeeds (bootstrap does not hold the SpinLock) and returns a second mutable
reference to the same `KernelState` memory — an aliased mutable reference that is
undefined behavior. The ISR modifies scheduler state (tick counter, current_cpu,
potentially current_tid via `yield_current`), corrupting the bootstrap's view.
After IRETQ, `dispatch_ready_task` returns `None` and the kernel enters the idle
path without ever reaching userspace.

### 14.2 Fix: EOI-only timer guard (Phase BT1)

A `BOOTSTRAP_SCHEDULER_READY: AtomicBool` flag is added to
`src/arch/x86_64/descriptor_tables.rs`. It starts `false`. The timer ISR in
`src/kernel/boot/fault_state.rs` checks this flag immediately after `acknowledge_interrupt`;
if false, it logs `X86_BOOTSTRAP_TIMER_IRQ_EOI_ONLY`, re-arms the timer, and returns
`Ok(())` without ticking the scheduler or calling `yield_current`.

`signal_bootstrap_scheduler_ready()` (in `src/arch/x86_64/descriptor_tables.rs`,
exposed via `src/arch/boot_entry.rs`) stores `true` with `Ordering::Release`.
`run_scheduler_loop` in `src/bin/kernel_boot.rs` calls
`yarm::arch::boot_entry::signal_bootstrap_scheduler_ready()` after both
`bootstrap_first_user_task` and `release_secondary_cpus_after_bootstrap` complete.

At that point all three user tasks (TID 1/2/3) are in the run queue. The timer ISR
will re-arm to 50M ticks from its last EOI-only fire, giving a long enough window
for `dispatch_ready_task` + `enter_dispatched_user_task_if_available` to complete
the IRETQ to TID 1 before the next tick.

### 14.3 Instrumentation markers

- `X86_BOOTSTRAP_TIMER_IRQ_EOI_ONLY cpu=<n>`: emitted each time a timer IRQ is
  suppressed during bootstrap. Expected: present (≥1) in x86_64 smoke when ELF
  loading takes >800 ms.
- `X86_BOOTSTRAP_SCHEDULER_READY`: emitted once after bootstrap completes and the
  flag is set. Expected: exactly 1 occurrence in x86_64 smoke.

### 14.4 Invariants preserved

- AArch64 paths: untouched (guard is `#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]`).
- hosted-dev: `signal_bootstrap_scheduler_ready()` is a no-op (no `#[cfg]` body).
- x86_64 SMP and `src/arch/x86_64/smp.rs`: untouched.
- Syscall ABI / SYSCALL_COUNT: unchanged.
- SpawnV5, VFS, syscall 27, VFS_READ_SHARED_REPLY_ENABLED: untouched.
- Phase 3B checks: not weakened.
- Register writeback / trap return semantics: unchanged.
- The global `SharedKernel` lock is still retained for all mutation paths.
  **This is not Stage 3/global-lock removal.**

### 14.5 Files changed

| File | Change |
|------|--------|
| `src/arch/x86_64/descriptor_tables.rs` | `BOOTSTRAP_SCHEDULER_READY` static, `signal_bootstrap_scheduler_ready()`, `bootstrap_scheduler_is_ready()` |
| `src/arch/boot_entry.rs` | `signal_bootstrap_scheduler_ready()` wrapper (x86_64 bare-metal only) |
| `src/kernel/boot/fault_state.rs` | EOI-only guard in `Trap::TimerInterrupt` arm |
| `src/bin/kernel_boot.rs` | Call `signal_bootstrap_scheduler_ready()` after bootstrap and secondary-CPU release |

## 15. x86_64 bootstrap timer aliasing fix (Phase BT2)

### 15.1 Root cause (BT2 regression after BT1)

After BT1, the smoke showed `X86_BOOTSTRAP_SCHEDULER_READY: 0` — `signal_bootstrap_scheduler_ready()` was never called. BT1's EOI-only guard correctly prevented scheduler mutations inside the timer handler, but the timer ISR still fired (21 times in QEMU) during bootstrap ELF loading.

Each timer fire entered `yarm_x86_dispatch_trap_from_stub`, which called `shared.with_cpu(cpu, ...)` three times (for `entering_tid`, for the main dispatch via `handle_trap_entry_shared`, and for `exiting_tid`). Each `with_cpu` call acquires `SpinLock<KernelState>` and returns a `&mut KernelState` into its closure. Because `borrow_kernel_for_boot()` had already created a raw `&mut KernelState` (bypassing the SpinLock), these ISR `with_cpu` calls created aliased mutable references — undefined behavior.

The Rust compiler, assuming `&mut KernelState` exclusivity, may cache fields across function calls, reorder loads/stores, or otherwise generate incorrect code. This corrupted bootstrap state (TCB tables, page-table entries, memory counters) in a non-deterministic way, causing `bootstrap_first_user_task` to hang and never return.

The two timer arming sites were:
1. `init_lapic_mmio_base()` in `src/arch/x86_64/irq.rs` — armed the timer when configuring the LAPIC MMIO base.
2. `kernel.program_timer_deadline_current_cpu(...)` in `run_with_prepared_kernel` in `src/arch/x86_64/boot.rs` — re-armed it before calling `run(kernel)`.

Both fired before STI and before `bootstrap_first_user_task`, starting the countdown that expired 21+ times during ELF loading.

### 15.2 Fix: defer BSP LAPIC timer to after bootstrap (Phase BT2)

The fix eliminates all timer firing during bootstrap by removing both arming sites and adding a single explicit arm after bootstrap completes:

1. **`init_lapic_mmio_base`** no longer calls `lapic_program_timer_deadline`. It only sets up the SVR (Spurious Vector Register) and records the MMIO base. The LAPIC is configured but the timer countdown does not start.

2. **`run_with_prepared_kernel`** no longer calls `kernel.program_timer_deadline_current_cpu(...)` before `run(kernel)`.

3. **`start_bsp_periodic_timer(kernel)`** (added to `src/arch/boot_entry.rs`) arms the BSP LAPIC timer and emits `X86_BOOTSTRAP_TIMER_STARTED`. It is called from `run_scheduler_loop` in `src/bin/kernel_boot.rs` after `signal_bootstrap_scheduler_ready()`.

With this fix, no timer ISR fires between `enable_interrupts_for_boot()` (STI) and `start_bsp_periodic_timer()`. The BSP's raw `borrow_kernel_for_boot` alias is live during that entire window, so no aliased `&mut` is ever created. The BT1 EOI-only guard remains as defense-in-depth but fires zero times in practice.

### 15.3 Instrumentation markers

- `X86_BOOTSTRAP_TIMER_STARTED`: emitted once in `start_bsp_periodic_timer()` after signal. Expected: exactly 1 occurrence in x86_64 smoke.
- `X86_BOOTSTRAP_TIMER_IRQ_EOI_ONLY cpu=<n>`: now expected to show 0 occurrences (timer not fired during bootstrap).
- `X86_BOOTSTRAP_SCHEDULER_READY`: still emitted once. Expected: exactly 1 occurrence.

### 15.4 Invariants preserved

- AArch64, RISC-V: timer arming in their `run_with_prepared_kernel` is unchanged; only x86_64 paths are modified.
- x86_64 SMP / `smp.rs`: untouched. APs do not use `init_lapic_mmio_base` for their timer setup.
- hosted-dev: `start_bsp_periodic_timer()` is a no-op (`let _ = kernel;`).
- Syscall ABI, SpawnV5, VFS, Phase 3B: unchanged.
- Global `SharedKernel` lock: retained. **Not Stage 3/global-lock removal.**

### 15.5 Files changed

| File | Change |
|------|--------|
| `src/arch/x86_64/irq.rs` | Remove timer arm from `init_lapic_mmio_base`; update test to assert count=0 |
| `src/arch/x86_64/boot.rs` | Remove `program_timer_deadline_current_cpu` before `run(kernel)`; update SAFETY comment |
| `src/arch/boot_entry.rs` | Add `start_bsp_periodic_timer(kernel)` function |
| `src/bin/kernel_boot.rs` | Call `start_bsp_periodic_timer(kernel)` after `signal_bootstrap_scheduler_ready()` |
| `src/arch/x86_64/descriptor_tables.rs` | Add BT2 test `bootstrap_scheduler_ready_gates_timer_isr_scheduling` |

---

## 16. Stage 5A: domain-owned split-read pass (task + capability domains)

### 16.1 Goal and scope

Stage 5A resumes global-lock removal after BT2, targeting safe read-only paths.
The goal is NOT full global-lock removal but rather: add split-read infrastructure
for the task and capability domains (extending the existing fault/telemetry/
scheduler/boot-config split-read pattern), prove equivalence with tests, and
document all remaining globally-locked syscall paths with precise deferral reasons.

No trap-path, arch, timer, or bootstrap code is changed. No ABI is altered.
x86_64 smoke is NOT required because all changes are lock-domain scaffolding
in `orchestrator_state.rs` and `runtime.rs` only.

### 16.2 Lock domain rank table (authoritative)

| Rank | Domain | Lock field in KernelState | Data protected |
|------|--------|--------------------------|----------------|
| 1 | scheduler | `scheduler_state: SpinLockIrq<SchedulerState>` | runqueues, timer, current_cpu |
| 2 | task | `task_state_lock: SpinLockIrq<()>` | `tcbs`, `task_classes`, `tls_restore_pending`, `robust_futex` |
| 3 | ipc | `ipc_state_lock: SpinLockIrq<()>` | endpoints, waiters, sender_waiters, notifications, irq_routes, transfer_envelopes, reply_caps, cross_cpu_work, live_tlb_shootdown |
| 4 | capability | `capability_state_lock: SpinLockIrq<()>` | `cnode_spaces`, `process_cnodes`, `delegated_capability_links` |
| 5 | vm | `vm_state_lock: SpinLockIrq<()>` | `user_spaces` (AddressSpaceManager) |
| 6 | memory | `memory_state_lock: SpinLockIrq<()>` | `memory_objects`, `brk_regions`, `cow_pages`, `frame_allocator` |
| 7 | driver | `driver_state_lock: SpinLockIrq<()>` | `driver_records` |
| 8 | fault | `fault_state_lock: SpinLockIrq<()>` | `last_fault`, `last_fault_frame`, `fault_handler_endpoint`, `fault_policy` |
| 9 | restart | `restart_state_lock: SpinLockIrq<()>` | `next_restart_token` |
| 10 | telemetry | `telemetry_state_lock: SpinLockIrq<()>` | `tlb_shootdown_count`, `tlb_shootdown_timeout_count`, `tid_allocation` |
| 11 | boot_config | `boot_config_state_lock: SpinLockIrq<()>` | `capacity_profile` |

Lock ordering rule: a caller may acquire rank-N only if it holds no lock of rank ≥ N.
The outer `SharedKernel` `SpinLock<KernelState>` is the "global" lock (rank 0, highest precedence);
holding it allows acquiring any domain lock without deadlock risk.

### 16.3 Stage 5A audit: production syscall/operation paths

Classification key:
- **A** pure read, ready for split-read
- **B** single-domain mutation, ready for split-mut
- **C** two-domain operation, ready for plan-first split
- **D** three-or-more-domain operation, keep globally locked for now
- **E** requires user-memory copy, defer or plan-first only
- **F** requires TrapFrame writeback, defer
- **G** requires cap materialization/revoke/mint, plan-first only
- **H** requires VM page-table mutation/TLB shootdown, plan-first only
- **I** scheduler/task current-tid/trap-boundary sensitive, defer
- **J** boot/arch/timer sensitive, defer
- **K** already split (no global lock required)
- **L** unsafe / unknown, defer

| Nr | Syscall | Classification | Domains touched | Deferral / Notes |
|----|---------|---------------|-----------------|------------------|
| 0 | Yield | B+I | scheduler | Trap-boundary; yield_current mutates runqueue. Keep globally locked. |
| 1 | IpcSend | D+I | ipc (rank 3), task (rank 2), scheduler (rank 1) | Three domains; IPC enqueue + task wake + scheduler. Keep globally locked. |
| 2 | IpcRecv | D+I | ipc, task, scheduler | Three domains; blocking recv. Keep globally locked. |
| 3 | VmMap | D+H | vm (rank 5), memory (rank 6), capability (rank 4) | Cap lookup + VM mutation + TLB. Keep globally locked. |
| 4 | TransferRelease | D+H+G | ipc, vm, capability | Transfer cap revoke + VM unmap. Keep globally locked. |
| 5 | IpcRecvTimeout | D+I+K | scheduler tick = K; try-recv = D | Tick read already split (Stage 2B). IPC recv still globally locked. |
| 6 | IpcCall | D+I+G | ipc, task, scheduler, capability | Reply cap mint + IPC. Keep globally locked. |
| 7 | IpcReply | D+I+G | ipc, task, scheduler, capability | Reply cap consume + IPC. Keep globally locked. |
| 8 | ControlPlaneSetCnodeSlots | C+G | task (rank 2) read, capability (rank 4) mut, boot_config (rank 11) read | Could be plan-first (read task class, then mutate capability). Deferred Stage 5B. |
| 9 | FutexWait | D+I | ipc (futex), task, scheduler | Three domains; futex block + scheduler. Keep globally locked. |
| 10 | FutexWake | D+I | ipc (futex), task, scheduler | Three domains; futex wake + scheduler. Keep globally locked. |
| 11 | SpawnThread | D | task, scheduler, memory, vm, capability | Five domains; keep globally locked. |
| 12 | Fork | D | task, scheduler, memory, vm, capability, ipc | Six domains; keep globally locked. |
| 13 | VmAnonMap | C+H | vm (rank 5), memory (rank 6) | Could be plan-first (memory alloc → VM map). Deferred Stage 5B. |
| 14 | VmBrk | B+C | task (rank 2) read, memory (rank 6) mut | Two sequential domains; plan-first feasible. Deferred Stage 5B. |
| 15 | DebugLog | E | scheduler (tid read), user memory | User memory copy required; keep globally locked. |
| 23 | SpawnProcess | D | task, scheduler, memory, vm, capability, ipc | Six domains; keep globally locked. |
| 24 | SpawnProcessFromUserBuf | D+E | all domains + user memory copy | Keep globally locked. |
| 26 | SpawnFromInitramfsFile | D+E | all domains + user memory copy | Keep globally locked. |
| 27 | InitramfsReadChunk | E | memory (chunk bounds), user memory | User memory copy; keep globally locked. |
| 28 | CreateInitramfsFileSliceMo | D+G | memory, capability, task | Cap mint + memory. Keep globally locked. |
| 29 | SpawnFromMemoryObject | D+G | task, scheduler, memory, vm, capability, ipc | Six domains + cap lookup. Keep globally locked. |

**SharedKernel non-trap production `with()` calls:**
- `ipc_recv_with_deadline_split_bridge` (lines 269/273): scheduler tick already split (K); IPC recv body still uses `with()` (D+I). Not convertible without full IPC-domain split.
- `control_plane_set_process_cnode_slots_via_syscall` (line 293): calls `handle_trap()` internally (F+I). Keep globally locked.

### 16.4 Split-read infrastructure added (Stage 5A)

All split-read helpers follow the `data_ptr()` + `addr_of!` pattern: derive raw field
pointers from `SharedKernel`'s stable `SpinLock<KernelState>` storage without creating
a whole-`KernelState` reference, then acquire only the required domain lock.

#### 16.4.1 Task domain (rank 2) — new static functions in `orchestrator_state.rs`

| Function | Lock(s) acquired | Returns | Notes |
|----------|-----------------|---------|-------|
| `KernelState::task_class_from_raw(state, tid)` | task (rank 2) | `Option<TaskClass>` | Reads `tcbs` + `task_classes` under task lock |
| `KernelState::task_exists_from_raw(state, tid)` | task (rank 2) | `bool` | Reads `tcbs` under task lock |

Safety requirement: both functions read `tcbs` and (for `task_class_from_raw`)
`task_classes` under `task_state_lock`. Both arrays are protected by the same lock.
The functions use `core::ptr::addr_of!` to derive field pointers without creating
a reference to the whole `KernelState`.

#### 16.4.2 Capability domain (rank 4) — new static function in `orchestrator_state.rs`

| Function | Lock(s) acquired | Returns | Notes |
|----------|-----------------|---------|-------|
| `KernelState::cnode_slot_capacity_from_raw(state, pid)` | capability (rank 4) | `Option<usize>` | Reads `capability.cnode_spaces` under capability lock |

#### 16.4.3 SharedKernel public split-read methods (new in `runtime.rs`)

| Method | Calls | Lock order | Classification |
|--------|-------|-----------|----------------|
| `task_class_split_read(tid)` | `task_class_from_raw` | task (rank 2) only | A — pure read |
| `task_exists_split_read(tid)` | `task_exists_from_raw` | task (rank 2) only | A — pure read |
| `cnode_slot_capacity_split_read(pid)` | `cnode_slot_capacity_from_raw` | capability (rank 4) only | A — pure read |

Forbidden caller-held locks:
- `task_class_split_read` / `task_exists_split_read`: must not hold scheduler (rank 1) or task (rank 2) before calling.
- `cnode_slot_capacity_split_read`: must not hold scheduler (rank 1), task (rank 2), ipc (rank 3), or capability (rank 4) before calling.

These methods do NOT acquire the outer `SharedKernel` SpinLock. They are live
production-ready split-reads: any future caller that only needs a task class or
CNode capacity check no longer needs `SharedKernel::with()`.

### 16.5 Existing split-read / split-mut inventory (Stage 4T+5 and earlier)

| Method | Domain | Rank | Classification |
|--------|--------|------|----------------|
| `scheduler_tick_now_split_read` | scheduler | 1 | K — already split |
| `current_tid_split_read` | scheduler | 1 | K — already split |
| `online_cpu_count_split_read` | scheduler | 1 | K — already split |
| `present_cpu_count_split_read` | scheduler | 1 | K — already split |
| `task_asid_for_tid_split_read` | task | 2 | K — already split |
| `fatal_trap_read_snapshot` | scheduler (rank 1), task (rank 2) | 1+2 | K — already split |
| `capacity_profile_split_read` | boot_config | 11 | K — already split |
| `runtime_capacity_config_split_read` | boot_config | 11 | K — already split |
| `last_fault_split_read` | fault | 8 | K — already split |
| `last_fault_frame_split_read` | fault | 8 | K — already split |
| `fault_policy_split_read` | fault | 8 | K — already split |
| `record_fault_split_mut` | fault | 8 | K — already split |
| `record_fault_frame_snapshot_split_mut` | fault | 8 | K — already split |
| `clear_last_fault_split_mut` | fault | 8 | K — already split |
| `increment_tlb_shootdown_count_split_mut` | telemetry | 10 | K — already split |
| `add_tlb_shootdown_timeout_count_split_mut` | telemetry | 10 | K — already split |
| `tlb_shootdown_count_split_read` | telemetry | 10 | K — already split |
| `tlb_shootdown_timeout_count_split_read` | telemetry | 10 | K — already split |
| **task_class_split_read** (5A) | task | 2 | **A — new Stage 5A** |
| **task_exists_split_read** (5A) | task | 2 | **A — new Stage 5A** |
| **cnode_slot_capacity_split_read** (5A) | capability | 4 | **A — new Stage 5A** |

### 16.6 Paths still globally locked and why

| Path | Why globally locked | Next candidate stage |
|------|---------------------|---------------------|
| All trap dispatch via `with_cpu()` | F+I: TrapFrame writeback + trap-boundary | Keep — do not convert |
| x86_64 entering_tid/exiting_tid `with_cpu()` | F: Stage 4T+6R revert; confirmed smoke-broken | Defer indefinitely (Class F) |
| IpcSend/IpcRecv/IpcCall/IpcReply | D+I: three domains + scheduler | Stage 5B or later |
| FutexWait/FutexWake | D+I: futex + task + scheduler | Stage 5B or later |
| SpawnThread/Fork/SpawnProcess/* | D: many domains | Stage 5C or later |
| VmMap/VmAnonMap | C+H: VM + memory + TLB | Stage 5B candidate (plan-first) |
| VmBrk | B+C: task read + memory mut | Stage 5B candidate |
| ControlPlaneSetCnodeSlots | C+G: task read + cap mut | Stage 5B candidate |
| DebugLog | E: user-memory copy | Requires copy infrastructure |
| TransferRelease | D+H+G: IPC + VM + cap | Stage 5C or later |

### 16.7 Sweep: remaining `SharedKernel::with()` / `with_cpu()` production callers

| Site | File | Classification | Notes |
|------|------|---------------|-------|
| `with_cpu()` in `dispatch_trap_entry_with_shared_kernel` | `arch/trap_entry.rs:149` | F+I | Do not touch |
| `with_cpu()` in `handle_trap_with_cpu` | `runtime.rs:283` | F+I | Do not touch |
| `with_cpu()` entering/exiting tid in `descriptor_tables.rs:895,923` | x86_64 | F (Class F, 4T+6R) | Do not touch |
| `with()` in `ipc_recv_with_deadline_split_bridge` | `runtime.rs:269,273` | D+I | Tick already split; IPC body deferred |
| `with()` in `control_plane_set_process_cnode_slots_via_syscall` | `runtime.rs:293` | F+I | Internally calls `handle_trap()` |
| `borrow_kernel_for_boot()` in x86_64 `boot.rs` | x86_64 only | J | BT2 protected; do not touch |
| `borrow_kernel_for_boot()` in aarch64 `boot.rs` | aarch64 only | J | Single-CPU boot; do not touch |

No new hot/warm direct bypasses discovered by this sweep.

### 16.8 Tests added (Stage 5A)

| Test | File | What it proves |
|------|------|----------------|
| `task_class_split_read_matches_global` | `runtime.rs` | `task_class_split_read` == `kernel.with(task_class)` for App, SystemServer, and absent TIDs |
| `task_exists_split_read_matches_global` | `runtime.rs` | `task_exists_split_read` == global existence check before and after registration |
| `cnode_slot_capacity_split_read_matches_global` | `runtime.rs` | `cnode_slot_capacity_split_read` == `kernel.with(cnode_slot_capacity)` before and after CNode creation |

### 16.9 Files changed (Stage 5A)

| File | Change |
|------|--------|
| `src/kernel/boot/orchestrator_state.rs` | Added `task_class_from_raw`, `task_exists_from_raw`, `cnode_slot_capacity_from_raw` |
| `src/runtime.rs` | Added `task_class_split_read`, `task_exists_split_read`, `cnode_slot_capacity_split_read`; added 3 equivalence tests |
| `doc/KERNEL_LOCKING.md` | This section (Section 16) |
| `doc/KERNEL_TEST_RULES.md` | Stage 5A rules added |

### 16.10 Hard invariants confirmed

- x86_64 BT2 preserved: BSP LAPIC timer still not armed before `signal_bootstrap_scheduler_ready()`.
- No SMP/smp.rs changes.
- No syscall ABI changes (SYSCALL_COUNT unchanged).
- No AArch64 changes.
- No SpawnV5/VFS/syscall27 changes.
- No Phase 3B weakening.
- No trap return / register writeback changes.
- No global-lock removal: all syscall handlers still acquire `SharedKernel` global lock via `with_cpu()`.

---

## 17. Stage 5B — Plan-first syscall decomposition

Stage 5B introduces plan-first decomposition for three syscall candidates. Each candidate has its task-domain reads (rank 2) separated from capability or memory mutations (rank 4/6) via a plan struct. The plan struct captures the task snapshot before the mutation phase begins, eliminating task-lock re-entry inside the mutation closure.

### 17.1 Candidate audit and classification

| Syscall | Lock domains touched | Stage 5B classification |
|---------|---------------------|------------------------|
| `ControlPlaneSetCnodeSlots` | scheduler (1), task (2), capability (4), boot_config (11 inside capability) | **Live conversion** — rank order 2→4 clean; boot_config (11) inside capability (4) is valid (11 > 4) |
| `VmBrk` | scheduler (1), task (2), memory (6) | **Live conversion** — rank order 2→6 clean; no VM/TLB involved; grow-only (shrink rejected) |
| `VmAnonMap` | scheduler (1), task (2), ipc (3 — TLB shootdown), capability (4), vm (5), memory (6) | **Helper-only scaffolding** — 6 domains including TLB; requires x86_64 smoke approval before live conversion |

### 17.2 Lock-domain flow per syscall

**ControlPlaneSetCnodeSlots:**
```
Plan phase (rank 2 — task):
  task_class(requester_tid)      → task lock acquired/released
  process_id(requester_tid)      → task lock acquired/released

Mutation phase (rank 4 — capability):
  process_cnode_for_pid(pid)     → capability lock
  resize_cnode_slots / ensure_cnode_space_with_slots / set_process_cnode_for_pid
                                 → capability lock
  runtime_capacity_config()      → boot_config lock (rank 11, inside capability closure; valid since 11 > 4)
```

**VmBrk:**
```
Plan phase (rank 2 — task):
  is_thread_group_leader(tid)    → task lock acquired/released

Mutation phase (rank 6 — memory):
  task_brk_bounds(tid)           → memory lock (read)
  set_task_brk_bounds(tid,…)     → task lock (verify), then memory lock (write)
                                    (task rank 2 < memory rank 6; valid ordering)
```

**VmAnonMap (scaffolding only):**
VmAnonMap touches ranks 1, 2, 3, 4, 5, and 6 in a single operation with TLB shootdown (rank 3 / IPC) in the rollback path. No live conversion without explicit x86_64 TLB smoke approval. `VmAnonMapPlan` struct exists as scaffolding.

### 17.3 New plan structs

Added to `src/kernel/boot/mod.rs`:

| Struct | Fields | Purpose |
|--------|--------|---------|
| `ControlPlaneCnodePlan` | `requester_class: TaskClass`, `requester_pid: u64` | Task snapshot for ControlPlaneSetCnodeSlots |
| `VmBrkPlan` | `tid: u64`, `is_group_leader: bool` | Task snapshot for VmBrk leader check |
| `VmAnonMapPlan` | `tid: u64` | Scaffolding only; no live conversion in Stage 5B |

### 17.4 New split-read helpers (Stage 5B)

Added to `orchestrator_state.rs`:

| Function | Lock acquired | Purpose |
|----------|--------------|---------|
| `process_id_from_raw(state, tid)` | `task_state_lock` (rank 2) | Read `thread_group_id.0` for a TID |
| `is_group_leader_from_raw(state, tid)` | `task_state_lock` (rank 2) | Check `thread_group_id.0 == tid` |

Added to `runtime.rs`:

| Method | Delegates to | Purpose |
|--------|-------------|---------|
| `process_id_split_read(&self, tid)` | `process_id_from_raw` | `SharedKernel` wrapper |
| `is_group_leader_split_read(&self, tid)` | `is_group_leader_from_raw` | `SharedKernel` wrapper |

### 17.5 Planned method variants

Added to `capability_lifecycle_state.rs`:
- `control_plane_set_process_cnode_slots_planned(&mut self, plan: &ControlPlaneCnodePlan, target_pid: u64, slot_capacity: usize)` — accepts plan snapshot instead of re-reading task state; eliminates task-lock entry inside capability mutations.

The `resize_process_cnode_slots` re-read of `task_class` is eliminated by using `plan.requester_class` directly (valid because in the non-system-server path `requester_pid == target_pid`, so the requester class is the target class).

### 17.6 Updated syscall handlers

- `handle_control_plane_set_cnode_slots` (`syscall.rs`): builds `ControlPlaneCnodePlan` from task domain, then calls `control_plane_set_process_cnode_slots_planned`.
- `handle_vm_brk` (`syscall.rs`): builds `VmBrkPlan` (group-leader check), then proceeds to memory domain for brk_bounds read/write.

Migration path: when the global lock is removed, the plan-build step moves before `with_cpu()` using `task_class_split_read` / `process_id_split_read` / `is_group_leader_split_read` on `SharedKernel`. The mutation phase only needs the capability or memory domain lock.

### 17.7 Hard invariants for Stage 5B

- No syscall ABI changes (SYSCALL_COUNT unchanged).
- No CapRights widening.
- BT2 preserved: BSP LAPIC timer not armed before `signal_bootstrap_scheduler_ready()`.
- No SMP/smp.rs, trap return/writeback, or timer/bootstrap changes.
- No SpawnV5, VFS, syscall27, Phase 3B changes.
- VmAnonMap: no live conversion without x86_64 smoke approval.
- No Class F re-entry (x86_64 entering_tid/exiting_tid split-read).
- Global lock still in place: all syscall handlers still acquire `SharedKernel` global lock via `with_cpu()`.

### 17.8 Tests added (Stage 5B)

| Test | File | What it proves |
|------|------|----------------|
| `process_id_split_read_matches_global` | `runtime.rs` | `process_id_split_read` == `kernel.with(process_id)` for absent and group-leader TIDs |
| `is_group_leader_split_read_matches_global` | `runtime.rs` | `is_group_leader_split_read` == `is_thread_group_leader` before and after registration |

### 17.9 Files changed (Stage 5B)

| File | Change |
|------|--------|
| `src/kernel/boot/mod.rs` | Added `ControlPlaneCnodePlan`, `VmBrkPlan`, `VmAnonMapPlan` structs |
| `src/kernel/boot/orchestrator_state.rs` | Added `process_id_from_raw`, `is_group_leader_from_raw` |
| `src/runtime.rs` | Added `process_id_split_read`, `is_group_leader_split_read`; added 2 equivalence tests |
| `src/kernel/boot/capability_lifecycle_state.rs` | Added `control_plane_set_process_cnode_slots_planned` |
| `src/kernel/syscall.rs` | Updated `handle_control_plane_set_cnode_slots` and `handle_vm_brk` to plan-first |
| `doc/KERNEL_LOCKING.md` | This section (Section 17) |
| `doc/KERNEL_TEST_RULES.md` | Stage 5B rules added |

## 18. Stage 5C — VmAnonMap audit, explicit-ASID helpers, and deferred conversion decision

Stage 5C performs a comprehensive lock-domain audit of `handle_vm_anon_map`, strengthens the `VmAnonMapPlan` scaffold introduced in Stage 5B, adds explicit-ASID memory helpers as building blocks for future plan-first decomposition, and documents the precise reasons why live conversion remains deferred.

### 18.1 Full VmAnonMap lock-domain audit

`handle_vm_anon_map` touches six lock domains in a single operation:

| Order | Domain | Rank | Operations | Notes |
|-------|--------|------|------------|-------|
| 1 | scheduler | 1 | `current_tid()` to identify caller | Single read; result is TID for all subsequent lookups |
| 2 | task | 2 | `task_asid_for_tid(tid)` to get ASID | Read only; used to select address space for every page |
| 3 | capability | 4 | `resolve_memory_object_phys(mem_cap, flags)` per page | One cap lookup per page in the requested range |
| 4 | vm | 5 | `map_user_page_in_asid_raw` / `unmap_page` per page | Adds/removes page table entries in the target ASID |
| 5 | memory | 6 | `alloc_anonymous_memory_object()` per page | Allocates backing PhysAddr for each new mapping |
| 6 | ipc (TLB) | 3 | `request_live_asid_shootdown` → `begin_live_tlb_shootdown_wait` | Rank 3 busy-wait spin in the rollback path only |

Stack guard check path:
```
Validation (lock-free):   validate_anon_map_args(addr, len, prot) → VmAnonMapValidatedArgs
Guard check:              is_user_page_mapped_in_current_asid(addr - PAGE_SIZE) → vm lock (5)
Per-page alloc:           alloc_anonymous_memory_object()                       → memory lock (6)
Per-page cap resolve:     resolve_memory_object_phys(cap, flags)                → capability lock (4)
Per-page map:             map_user_page_in_asid_raw(asid, virt, mapping)        → vm lock (5)
Rollback (on error):      unmap_user_page_in_asid(asid, va)                     → vm lock (5)
                          request_live_asid_shootdown(asid)                     → scheduler (1) + ipc (3)
                          begin_live_tlb_shootdown_wait(...)                    → ipc lock (3) spin
```

### 18.2 Three blockers preventing live conversion

**Blocker 1 — TLB shootdown busy-wait in rollback path (ipc rank 3)**

`request_live_asid_shootdown` → `begin_live_tlb_shootdown_wait` acquires the ipc lock (rank 3) and spins until remote CPUs acknowledge the shootdown. This is a cross-CPU busy-wait. Any ordering change relative to the surrounding scheduler (rank 1) or task (rank 2) reads risks a TLB coherency window if the ASID is reused before the shootdown completes. Rank 3 (ipc) is lower than ranks 4/5/6 consumed by the forward path, making the rollback lock sequence inconsistent with the plan-first forward ordering.

**Blocker 2 — Per-iteration loop state not captured in plan struct**

The forward mapping loop accumulates state in a local variable `va` (the next virtual address to map). Decomposing without the global lock would require either (a) capturing the entire mapping intent up front (list of `(va, mem_cap)` pairs) into the plan, or (b) making partial mappings visible to other CPUs and recoverable across lock boundaries. Neither is designed yet. The current `VmAnonMapPlan` captures only the validated args and ASID — not the per-page iterator state.

**Blocker 3 — x86_64 smoke requirement**

The kernel hard-invariant "any live VM/TLB behavior change requires x86_64 smoke request" is not satisfied. Reorganizing the lock-acquisition order of `handle_vm_anon_map` changes when TLB shootdowns fire relative to the global lock boundary, which is a live VM/TLB behavioral change requiring x86_64 smoke-level testing before it may be merged.

### 18.3 Strengthened `VmAnonMapPlan` (Stage 5C)

Stage 5B left `VmAnonMapPlan` as `{ tid: u64 }` — a minimal placeholder. Stage 5C replaces it with a full plan that would support live decomposition once the three blockers are resolved:

```rust
// src/kernel/boot/mod.rs

/// Stage 5C: Task-domain snapshot for VM_ANON_MAP plan-first decomposition.
///
/// Lock sequence (when live conversion is enabled):
///   Phase 1 — Validation (no locks):    validate_anon_map_args() → VmAnonMapValidatedArgs
///   Phase 2 — Scheduler snapshot (1):   current_tid()            → plan.tid
///   Phase 3 — Task snapshot (2):        task_asid_for_tid(tid)   → plan.asid
///   Phase 4 — Guard check (5):          is_user_page_mapped_in_asid(plan.asid, addr-PAGE_SIZE)
///   Phase 5 — Per-page alloc+map (4+5+6): alloc_anon + resolve_cap + map_user_page_in_asid
///   Phase 6 — Rollback on error (3+5):  unmap_user_page_in_asid + request_live_asid_shootdown
///                                        + begin_live_tlb_shootdown_wait  ← BLOCKER 1
///
/// Live conversion DEFERRED. Three blockers (see §18.2):
///   1. TLB shootdown busy-wait (ipc rank 3) in rollback — rank ordering inconsistent.
///   2. Per-page loop iterator state (variable `va`) not captured in plan.
///   3. x86_64 smoke approval required before any live VM/TLB reordering.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct VmAnonMapPlan {
    pub(crate) validated: VmAnonMapValidatedArgs,
    pub(crate) tid: u64,
    pub(crate) asid: Asid,
}

/// Stage 5C: Result of `validate_anon_map_args` — pure computation, no locks.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct VmAnonMapValidatedArgs {
    pub(crate) addr: usize,
    pub(crate) map_len: usize,
    pub(crate) end: usize,       // addr + map_len, no-overflow guaranteed
    pub(crate) flags: PageFlags,
}
```

### 18.4 Explicit-ASID memory helpers (Stage 5C)

Three helpers in `src/kernel/boot/memory_state.rs` form the building blocks for future per-page work that targets a specific ASID without re-reading scheduler or task state on every iteration:

| Function | Lock acquired | Returns | Purpose |
|----------|--------------|---------|---------|
| `map_user_page_in_asid_with_caps(asid, mem_cap, virt, flags)` | vm (5) + cap (4) | `Result<Option<Mapping>, KernelError>` | Map one page into a named ASID using a cap-based phys resolution |
| `unmap_user_page_in_asid(asid, virt)` | vm (5) | `Result<Option<Mapping>, KernelError>` | Remove one page from a named ASID; returns `None` if not mapped (rollback-safe) |
| `is_user_page_mapped_in_asid(asid, virt)` | vm (5) | `Result<bool, KernelError>` | Check whether a specific page is present in a named ASID |

`unmap_user_page_in_asid` was already present unconditionally (no change). `map_user_page_in_asid_with_caps` had an unnecessary `#[cfg(feature = "posix-compat")]` gate removed so it is unconditionally available. `is_user_page_mapped_in_asid` is new in Stage 5C.

All three carry `#[cfg_attr(not(test), allow(dead_code))]` because the plan-first `handle_vm_anon_map` path that calls them has not yet been wired up (deferred).

**Why explicit-ASID helpers eliminate per-iteration lock re-entry**

The current `handle_vm_anon_map` calls `map_user_page_in_current_asid_with_caps` inside the loop, which internally reads `current_tid()` (scheduler rank 1) then `task_asid_for_tid(tid)` (task rank 2) on every iteration to locate the target address space. In the plan-first path the ASID is already in `plan.asid`; the explicit-ASID helpers bypass the scheduler and task reads and go directly to the vm (rank 5) lock.

### 18.5 Lock sequence comparison: current vs. planned

**Current `handle_vm_anon_map` (still active):**
```
with_cpu() → global lock acquired
  current_tid()                                    → scheduler (1)
  task_asid_for_tid(tid)                           → task (2)
  is_user_page_mapped_in_current_asid(addr-PAGE)  → vm (5)  [guard check]
  loop:
    alloc_anonymous_memory_object()                → memory (6)
    map_user_page_in_current_asid_with_caps(...)   → scheduler (1) + task (2) + cap (4) + vm (5)
    on error: unmap_user_page_in_asid(asid, va)    → vm (5)
              request_live_asid_shootdown(asid)     → scheduler (1)
              begin_live_tlb_shootdown_wait(...)    → ipc (3) spin
global lock released
```

**Planned `handle_vm_anon_map` (deferred, requires blocker resolution):**
```
Phase 1 (lock-free):      validate_anon_map_args(addr, len, prot) → VmAnonMapValidatedArgs
Phase 2 (split-read, 1):  current_tid_split_read()               → plan.tid
Phase 3 (split-read, 2):  task_asid_for_tid_split_read(tid)      → plan.asid
with_cpu() → global lock acquired
  Phase 4 (vm, 5):        is_user_page_mapped_in_asid(plan.asid, addr-PAGE)  [guard]
  loop:
    Phase 5a (memory, 6): alloc_anonymous_memory_object()
    Phase 5b (cap, 4):    resolve_memory_object_phys(cap, flags)
    Phase 5c (vm, 5):     map_user_page_in_asid_with_caps(plan.asid, …)
    on error:
    Phase 6a (vm, 5):     unmap_user_page_in_asid(plan.asid, va)
    Phase 6b (ipc, 3):    request_live_asid_shootdown + begin_live_tlb_shootdown_wait ← BLOCKER 1
global lock released
```

Note: even the planned path retains the `with_cpu()` global lock for the mutation phases. The benefit is eliminating the redundant per-iteration scheduler+task reads inside the loop. Full global-lock removal is not claimed.

### 18.6 Hard invariants for Stage 5C

- VmAnonMap is **helper-only scaffolding** — `handle_vm_anon_map` is unchanged.
- No live VM/TLB behavior change. The three new helpers are `#[cfg_attr(not(test), allow(dead_code))]`.
- Rollback behavior of `handle_vm_anon_map` is unmodified.
- Stack guard check path is unmodified.
- x86_64 BT2 preserved.
- No SMP/smp.rs, trap return/writeback, or ABI changes.
- No CapRights widening, SpawnV5, VFS, syscall27, or Phase 3B changes.
- Global lock (`SharedKernel::with_cpu()`) still wraps the full `handle_vm_anon_map` body.

### 18.7 Tests added (Stage 5C)

Five new tests in `src/kernel/boot/tests.rs`, after the existing `vm_anon_map_preserves_stack_guard_page_behavior`:

| Test | What it proves |
|------|----------------|
| `vm_anon_map_explicit_asid_map_helper_matches_current_asid_path` | `map_user_page_in_asid_with_caps` and `is_user_page_mapped_in_asid` produce the same observable result as the established current-ASID path |
| `vm_anon_map_explicit_asid_unmap_helper_removes_mapping` | `unmap_user_page_in_asid` removes a page that was mapped via the current-ASID path; both current-ASID and explicit-ASID checks confirm absence |
| `vm_anon_map_unmap_idempotent_on_already_unmapped_page` | `unmap_user_page_in_asid` on a never-mapped page returns `Ok(None)` — rollback is safe to call without a prior successful map |
| `vm_anon_map_execute_only_prot_skips_stack_guard_check` | prot=PROT_EXEC (0x4): stack guard condition `write && !execute` is false, so the syscall succeeds even when the guard page is already mapped |
| `vm_anon_map_write_execute_prot_also_skips_stack_guard` | prot=PROT_WRITE\|PROT_EXEC (0x6): execute=true disarms the guard even when write=true |

Helper added to support the above tests:
```rust
fn setup_task0_with_known_asid() -> (KernelState, Asid)
```
Returns the `KernelState` and the `Asid` bound to task 0, whereas the existing `setup_task0_with_asid()` discards the ASID after binding.

### 18.8 Files changed (Stage 5C)

| File | Change |
|------|--------|
| `src/kernel/boot/mod.rs` | Replaced `VmAnonMapPlan { tid }` with full `VmAnonMapPlan { validated, tid, asid }`; added `VmAnonMapValidatedArgs`; added `#[cfg_attr(not(test), allow(dead_code))]` to both |
| `src/kernel/boot/memory_state.rs` | Removed `posix-compat` feature gate from `map_user_page_in_asid_with_caps`; added `is_user_page_mapped_in_asid`; both carry `#[cfg_attr(not(test), allow(dead_code))]` |
| `src/kernel/boot/tests.rs` | Added `setup_task0_with_known_asid()` helper and 5 new Stage 5C tests |
| `doc/KERNEL_LOCKING.md` | This section (Section 18) |
| `doc/KERNEL_TEST_RULES.md` | Rule N+8 added |

## 19. Stage 5D — TLB shootdown / rollback-domain audit and plan scaffolding

Stage 5D performs a comprehensive audit of every TLB shootdown and cross-CPU work path, classifies each one by type, designs scaffolding plan types that make the rollback/progress state explicit, adds a target-bitmap compute helper, and adds tests for the new invariants. No live conversion of VmAnonMap is performed — Stage 5C blockers #1 and #3 remain.

**Additional fix:** Stage 5D also corrects a `_kernel` → `kernel` regression in `handle_vm_brk` that was introduced by the `32687af` merge-from-main commit. The variable was used at lines 3170 and 3183 (shrink unmap loop and `set_task_brk_bounds` call).

### 19.1 Full TLB / cross-CPU shootdown path audit

Every TLB-relevant function, classified by type (A–I):

| Class | Meaning |
|-------|---------|
| A | Pure TLB request record (compute only, no mutation) |
| B | Cross-CPU work enqueue |
| C | IPC-coupled notification |
| D | Busy-wait / timeout |
| E | VM mutation coupled |
| F | Memory / frame rollback coupled |
| G | Scheduler / timer coupled |
| H | Safe plan-first candidate |
| I | Unsafe / defer |

| Function | File:Line | Classes | Notes |
|----------|-----------|---------|-------|
| `live_cpu_bitmap_for_asid` | memory_state.rs:126 | A, H | Reads scheduler(1)+task(2) only; no mutation. Safe to snapshot before any domain lock. |
| `begin_live_tlb_shootdown_wait` | memory_state.rs:12 | C, D | Acquires ipc(3). Called inside vm(5)+memory(6) context → rank inversion. |
| `clear_live_tlb_shootdown_wait` | memory_state.rs:38 | C | Releases active shootdown state under ipc(3). |
| `live_tlb_shootdown_pending` | memory_state.rs:29 | D | Reads ipc(3). Polled in busy-wait loop. |
| `request_live_asid_shootdown` | memory_state.rs:146 | B,C,D,G | Full shootdown sequence: compute targets → ipc(3) wait → work enqueue → busy-wait spin → yield. Rank inversion: ipc(3) acquired after memory(6). |
| `rollback_anon_map` | syscall.rs:3091 | E,F,I | Calls unmap per page → per-page shootdown. Per-page busy-wait in rollback is the primary cost. Blocker #1. |
| VmBrk shrink unmap loop | syscall.rs:3161 | E,F,I | Per-page shootdown for each shrink page. Same rank-inversion concern as rollback. |
| `unmap_user_page_in_current_asid` | memory_state.rs:690 | E,F,I | Unmaps page, calls `request_live_asid_shootdown`. Lock sequence: vm(5)→memory(6)→ipc(3). |
| `unmap_user_page_in_asid` | memory_state.rs:805 | E,F,H | Explicit-ASID unmap (Stage 5C). Same as above but no scheduler/task re-read. |
| `destroy_user_address_space_by_asid` | memory_state.rs:200 | B,E | Fire-and-forget: destroys ASID, enqueues TlbShootdown work item per CPU with `requester: None`. No busy-wait. Safe for plan-first. |
| `submit_cross_cpu_work` | scheduler_state.rs:315 | B,C | Enqueues WorkItem into SmpMailbox under ipc(3). |
| `acknowledge_shootdown` | vm.rs:751 | A,H | Clears CPU bit from retired ASID bitmap under vm(5). Called from TlbShootdown handler. |
| `tick_retired_shootdowns` | vm.rs:783 | G | Increments age ticks; always returns 0 (timeout escalation not triggered). |
| `retired_entry` | vm.rs:771 | A | Read-only query of retired ASID set under vm(5). |
| `any_mapping_for_phys` | vm.rs:793 | A | Read-only: checks live address spaces for phys mapping. |
| `destroy_and_collect_mappings` | vm.rs:716 | E,B | Removes ASID entry, moves to retired if `pending_cpu_bitmap != 0`. |
| `protect_user_page` | memory_state.rs:827 | E | VM mutation (flags only). **No TLB shootdown needed** — same phys, only permissions changed; hardware handles via page-table update. |
| `escalate_tlb_shootdown_timeout` | scheduler_state.rs:333 | G,C | Sends supervisor endpoint message. Currently unreachable (`tick_retired_shootdowns` always returns 0). |

### 19.2 Rank inversion in the unmap path

The documented rank inversion exists at three call sites:

```
unmap_user_page_in_current_asid (memory_state.rs:690):
  vm_state_lock (5) → [unmap page table entry]
  memory_state_lock (6) → [note_mapping_removed, reclaim]
  ipc_state_lock (3) → [begin_live_tlb_shootdown_wait]    ← rank 3 acquired after 5+6

unmap_user_page_in_asid (memory_state.rs:822):
  vm_state_lock (5) → [unmap page table entry]
  memory_state_lock (6) → [clear_cow_page, note_mapping_removed, reclaim]
  ipc_state_lock (3) → [begin_live_tlb_shootdown_wait]    ← same inversion

unmap_user_page (memory_state.rs:800):
  capability_state_lock (4) → [resolve cap to ASID]
  vm_state_lock (5) → [unmap page table entry]
  memory_state_lock (6) → [note_mapping_removed, reclaim]
  ipc_state_lock (3) → [begin_live_tlb_shootdown_wait]    ← same inversion
```

This inversion is intentional and safe as long as the global `SharedKernel` lock is held: all three lock closures (`vm`, `memory`, `ipc`) are acquired and released sequentially, never simultaneously. There is no actual deadlock risk under the global lock because only one thread of execution is active at a time. **The inversion becomes a real risk only if the global lock is removed**, which is why full global-lock removal requires explicit approval.

The comment at memory_state.rs:158–160 records the safety invariant:
> "mapping removal completes BEFORE we publish shootdown work items, so remote CPUs can only ACK after invalidating post-unmap state."

### 19.3 TLB shootdown fast path

`request_live_asid_shootdown` returns immediately without touching the ipc lock when `targets == 0` (line 154):

```rust
let targets = self.live_cpu_bitmap_for_asid(asid) & !requester_bit;
if targets == 0 {
    return Ok(());
}
```

This covers:
- Single-CPU systems (always fast path)
- ASIDs private to the requester CPU (no other CPU has this task active)

`compute_tlb_shootdown_request_plan` (Stage 5D, memory_state.rs) pre-computes this bitmap before any domain lock is acquired, making the fast-path determination explicit in the plan.

### 19.4 Rollback progress model (blocker #2 resolution)

Stage 5C blocker #2 was: "The per-page loop variable `va` is not captured in any plan struct." Stage 5D resolves this by introducing `VmPageMapProgress`:

```rust
pub(crate) struct VmPageMapProgress {
    pub(crate) base_addr: usize,  // page-aligned start
    pub(crate) mapped_end: usize, // exclusive upper bound of mapped pages
    pub(crate) end_addr: usize,   // page-aligned end of requested range
}
```

Invariant: `base_addr ≤ mapped_end ≤ end_addr`; all are multiples of PAGE_SIZE.
Rollback must unmap `[base_addr, mapped_end)` only — never `[base_addr, end_addr)`.

This makes the rollback scope explicit at the type level, preventing the old off-by-one risk where a bare `va` variable could be misread as "all pages up to end" rather than "all pages actually mapped."

With `VmPageMapProgress`, the planned VmAnonMap handler loop body would be:
```
for each page at progress.mapped_end:
    alloc_anonymous_memory_object()       → memory(6)
    map_user_page_in_asid_with_caps(...)  → cap(4) + vm(5)
    progress.mapped_end += PAGE_SIZE      ← advance AFTER successful map
on error:
    for va in progress.base_addr..progress.mapped_end step PAGE_SIZE:
        unmap_user_page_in_asid(plan.asid, va) → vm(5) [+ ipc(3) if targets ≠ 0]
```

### 19.5 New plan types (Stage 5D)

All added to `src/kernel/boot/mod.rs`:

| Struct | Purpose | Lock-domain reads |
|--------|---------|-------------------|
| `TlbShootdownRequestPlan` | Computed target bitmap for one unmap shootdown | scheduler(1) + task(2) |
| `VmPageMapProgress` | Explicit per-page rollback scope | None (pure data) |
| `VmAnonMapProgressPlan` | VmAnonMapPlan + VmPageMapProgress | (captures prior reads) |

All carry `#[cfg_attr(not(test), allow(dead_code))]` since they are scaffolding only.

New helper added to `src/kernel/boot/memory_state.rs`:

| Function | Purpose | Lock-domain |
|----------|---------|-------------|
| `compute_tlb_shootdown_request_plan(asid, virt)` | Snapshot target bitmap before vm/ipc lock | scheduler(1)+task(2) read, no mutation |

### 19.6 Live conversion decisions

| Path | Decision | Reason |
|------|----------|--------|
| `handle_vm_anon_map` full decomposition | **Deferred** | Blockers #1 (TLB busy-wait rank inversion) and #3 (x86_64 smoke required) |
| `rollback_anon_map` per-page shootdown batching | **Deferred** | Live VM/TLB behavior change; x86_64 smoke required |
| VmBrk shrink per-page shootdown batching | **Deferred** | Same |
| `compute_tlb_shootdown_request_plan` helper | **Live** (helper only) | Pure read, no domain mutation, no lock inversion |
| `VmBrk` `_kernel` → `kernel` bug fix | **Live** | Compile error fix; no behavioral change (variable rename only) |

VmAnonMap is **helper-only scaffolding**. `handle_vm_anon_map` is unchanged.

### 19.7 Why VmAnonMap live conversion is still deferred

After Stage 5D:
- **Blocker #1** (TLB busy-wait rank inversion): Still present. `begin_live_tlb_shootdown_wait` acquires ipc(3) after vm(5) and memory(6). Resolving this requires either (a) releasing all domain locks before the shootdown wait — which changes when page-table mutations become visible to remote CPUs — or (b) a dedicated shootdown thread that doesn't invert ranks. Both require design work beyond scaffolding.
- **Blocker #2** (per-page progress): **Resolved by Stage 5D** via `VmPageMapProgress`. The `VmAnonMapProgressPlan` now captures all necessary loop state.
- **Blocker #3** (x86_64 smoke): Still required. Any live VM/TLB reordering must be validated on real SMP hardware.

### 19.8 Hard invariants for Stage 5D

- `handle_vm_anon_map` is **unchanged**. No live VM/TLB behavior change.
- VmBrk `_kernel` fix: compile error only, no semantic change.
- All 603 tests pass (--test-threads=1).
- No SMP/smp.rs changes.
- No trap return/writeback changes.
- No syscall ABI changes.
- No CapRights widening, SpawnV5, VFS, syscall27, Phase 3B changes.
- Global lock (`SharedKernel::with_cpu()`) still wraps all syscall handlers.

### 19.9 Tests added (Stage 5D)

| Test | File | What it proves |
|------|------|----------------|
| `tlb_shootdown_request_plan_has_no_remote_targets_in_single_cpu` | tests.rs | In single-CPU context, target_cpu_bitmap == 0 for bound ASID |
| `tlb_shootdown_request_plan_unbound_asid_has_no_targets` | tests.rs | ASID not bound to any task → target_cpu_bitmap == 0 |
| `vm_page_map_progress_rollback_covers_only_mapped_range` | tests.rs | Partial rollback of page 1 leaves page 2 mapped and page 3 absent |
| `vm_page_map_progress_empty_initial_rollback_range` | tests.rs | Initial VmPageMapProgress has mapped_end == base_addr (empty rollback) |
| `vm_brk_shrink_tolerates_lazy_unmapped_pages` | tests.rs | VmBrk shrink over all-lazy range succeeds via Ok(None) unmap |
| `vm_brk_shrink_with_partially_mapped_lazy_region` | tests.rs | VmBrk shrink with mixed mapped+lazy pages succeeds; all pages absent after |

### 19.10 Files changed (Stage 5D)

| File | Change |
|------|--------|
| `src/kernel/syscall.rs` | Fix `_kernel` → `kernel` in `handle_vm_brk` shrink loop and `set_task_brk_bounds` call (regression from `32687af` merge) |
| `src/kernel/boot/mod.rs` | Added `TlbShootdownRequestPlan`, `VmPageMapProgress`, `VmAnonMapProgressPlan` structs with full doc comments |
| `src/kernel/boot/memory_state.rs` | Added `compute_tlb_shootdown_request_plan` helper |
| `src/kernel/boot/tests.rs` | Added 6 new Stage 5D tests |
| `doc/KERNEL_LOCKING.md` | This section (Section 19) |
| `doc/KERNEL_TEST_RULES.md` | Rule N+9 added |

---

## §20 Stage 5E: TLB shootdown wait/rank-ordering decomposition

### 20.1 Goal

Stage 5E precisely characterises the rank-ordering gap between the current
`unmap_user_page_in_asid` path and a future global-lock-free unmap, and
introduces the scaffolding needed to close it without changing any live
behaviour.

The primary deliverable is:

- A precise, written characterisation of **blocker #1** that separates it
  into two sub-problems (§20.3).
- `unmap_page_phase1` — the phase-1 building block for a future two-phase
  unmap (page table removal + accounting; no frame reclamation; no TLB
  shootdown).
- Three new aggregate TLB plan structs: `TlbShootdownWaitPlan`,
  `VmBrkShrinkTlbPlan`, `VmAnonMapRollbackTlbPlan`.
- 6 new tests (609 total after Stage 5E).
- This section (§20) and KERNEL_TEST_RULES.md Rule N+10.

### 20.2 Blocker #1 — precise characterisation

**Blocker #1** was originally stated as: "ipc(3) is acquired after vm(5) and
memory(6) in the TLB shootdown path, violating declared lock-rank ordering."

After the Stage 5D and 5E audits, the problem splits into two orthogonal
sub-problems:

#### 20.2.1 Sub-problem A — sequential rank inversion (documentation issue)

`request_live_asid_shootdown` acquires ipc(3) in the `begin_live_tlb_shootdown_wait`
call **after** vm(5) and memory(6) have been acquired and **released**. At no
point are ipc(3) and vm(5) or memory(6) **simultaneously** held.

The lock sequence in `unmap_user_page_in_current_asid` is:

```
vm(5) acquired → page unmapped → vm(5) released
memory(6) acquired → frame accounting → memory(6) released
                   → frame reclaimed
ipc(3) acquired → shootdown wait → ipc(3) released
```

This is sequential, not concurrent. It cannot cause a classic deadlock.
However, it violates the declared domain-rank ordering (ranks are defined to
prevent simultaneous holds, not just to express priority), and any future code
that re-acquires vm(5) or memory(6) inside the shootdown wait **would** be a
real deadlock. The sub-problem is a documentation gap and a latent hazard.

**Resolution**: The rank ordering documentation must acknowledge that
`request_live_asid_shootdown` is exempt from strict rank enforcement because
it is always called after all higher-ranked locks are released. A rank comment
in the function and in §19 is sufficient until global-lock removal begins.

#### 20.2.2 Sub-problem B — pre-shootdown frame reclamation (real ordering bug for global-lock removal)

In `unmap_user_page_in_asid` (and `_in_current_asid`), the call sequence is:

```
1. unmap page from page table         (vm lock)
2. clear COW entry                    (memory lock)
3. decrement map_refcount             (memory lock)
4. reclaim_memory_object_for_phys     (memory lock) ← PROBLEM
5. request_live_asid_shootdown        (ipc lock)
```

Step 4 makes the physical frame available for reuse **before** step 5
broadcasts TLB invalidation to remote CPUs. Under the global lock this is
safe: no other CPU can fault or map memory while the global lock is held.
Without the global lock, a remote CPU could:

1. Receive a new allocation (the just-reclaimed frame), and
2. Still have a stale TLB entry pointing to that frame from the old mapping.

The result would be a UAF-style memory safety violation at the hardware level.

**Resolution design** (two-phase unmap, §20.4):

- **Phase 1** (vm + memory, no reclamation): unmap page, clear COW, decrement
  map_refcount. Return `TlbShootdownWaitPlan` carrying the physical frame.
- **Phase 2** (ipc): execute TLB shootdown via `request_live_asid_shootdown`.
  Skip entirely if `target_cpu_bitmap == 0`.
- **Phase 3** (memory): call `reclaim_memory_object_for_phys` now that
  shootdown is complete.

This ordering is safe under and without the global lock.

### 20.3 `tick_retired_shootdowns` always returns 0

`tick_retired_shootdowns()` in `vm.rs` always returns 0. The function body
increments `retired.age_ticks` but unconditionally returns 0. This means
`escalate_tlb_shootdown_timeout` (which is called when the tick count exceeds
a threshold) is structurally present but currently unreachable.

This is intentional: retired-shootdown timeout escalation is unimplemented.
The function is a placeholder for future observability. It does not affect
correctness today.

### 20.4 Two-phase unmap design (scaffold, not live)

```
Phase 1 — unmap_page_phase1(&mut self, asid, virt)
    └─ vm lock:      AddressSpace::unmap_page(virt) → Option<Mapping>
    └─ memory lock:  clear_cow_page(asid, virt)
    └─ memory lock:  note_mapping_removed(phys)
    └─ read:         compute_tlb_shootdown_request_plan(asid, virt)
    └─ returns:      Option<TlbShootdownWaitPlan>
                     (Some → page was present; None → page was absent/lazy)
    NOTE: reclaim_memory_object_for_phys is NOT called here.

Phase 2 — TLB shootdown (future code, not yet scaffolded as live)
    if plan.target_cpu_bitmap != 0:
        ipc lock: begin_live_tlb_shootdown_wait(...)
        busy-wait for ACKs
        ipc lock: clear_live_tlb_shootdown_wait()
    else: fast path, skip

Phase 3 — frame reclamation (future code, not yet scaffolded as live)
    memory lock: reclaim_memory_object_for_phys(plan.phys)
```

The `phys` field in `TlbShootdownWaitPlan` is the physical frame withheld from
reclamation until after the shootdown. This is the key invariant: the frame
must not be reusable by any allocator until phase 2 completes.

### 20.5 Aggregate TLB plan structs

Three structs were added to `src/kernel/boot/mod.rs`:

**`TlbShootdownWaitPlan`** (per-page):

| Field | Meaning |
|-------|---------|
| `asid` | ASID of the removed mapping |
| `virt` | Virtual address removed in phase 1 |
| `target_cpu_bitmap` | CPUs to notify (0 = no shootdown needed) |
| `requester` | CPU that performed phase 1 (excluded from targets) |
| `phys` | Physical frame to reclaim in phase 3 (held back from allocator) |

**`VmBrkShrinkTlbPlan`** (aggregate for VmBrk shrink):

| Field | Meaning |
|-------|---------|
| `asid` | ASID being shrunk |
| `unmap_start` | Page-aligned start of the shrink range |
| `unmap_end` | Page-aligned exclusive end of the shrink range |
| `aggregate_target_bitmap` | OR of per-page target bitmaps from phase 1 |

Zero `aggregate_target_bitmap` means the entire shrink needs no cross-CPU IPC.

**`VmAnonMapRollbackTlbPlan`** (aggregate for VmAnonMap rollback):

| Field | Meaning |
|-------|---------|
| `asid` | ASID whose pages are being rolled back |
| `progress` | `VmPageMapProgress` — rollback covers `[base_addr, mapped_end)` |
| `aggregate_target_bitmap` | OR of per-page target bitmaps accumulated during rollback |

Together with `VmAnonMapProgressPlan` (Stage 5D), this closes the last
structural gap for plan-first VmAnonMap decomposition. The remaining blocker
is x86_64 smoke approval (blocker #3).

### 20.6 Live conversions in Stage 5E

**No live conversions.** `unmap_page_phase1` is scaffolding only (`#[cfg_attr(not(test), allow(dead_code))]`). `handle_vm_anon_map`, `rollback_anon_map`, and `handle_vm_brk` are unchanged.

Remaining blockers for full conversion:

| Blocker | Status after Stage 5E |
|---------|-----------------------|
| #1a — sequential rank inversion documentation | Characterised (§20.2.1); resolve in docs before conversion |
| #1b — pre-shootdown frame reclamation | Design complete (§20.4); implement when global lock removal begins |
| #2 — per-page loop progress capture | **Resolved** (Stage 5D, `VmPageMapProgress`) |
| #3 — x86_64 SMP smoke approval | Still required |

### 20.7 Hard invariants for Stage 5E

- `handle_vm_anon_map` is **unchanged**. No live VM/TLB behavior change.
- `handle_vm_brk` is **unchanged**.
- All 609 tests pass (`--test-threads=1`).
- No SMP/smp.rs changes.
- No trap return/writeback changes.
- No syscall ABI changes.
- No CapRights widening, SpawnV5, VFS, syscall27, Phase 3B changes.
- Global lock (`SharedKernel::with_cpu()`) still wraps all syscall handlers.
- `tick_retired_shootdowns` always-0 behaviour is preserved.

### 20.8 Tests added (Stage 5E)

| Test | What it proves |
|------|----------------|
| `tlb_shootdown_wait_plan_captures_correct_phys_and_fields` | phase1 plan carries correct asid, virt, and phys (the deferred-reclaim frame) |
| `tlb_shootdown_wait_plan_none_for_absent_page` | phase1 returns Ok(None) for an absent/lazy page |
| `tlb_shootdown_wait_plan_target_bitmap_matches_request_plan` | phase1 bitmap == compute_tlb_shootdown_request_plan bitmap |
| `unmap_page_phase1_removes_page_from_address_space` | page is absent from address space immediately after phase 1 |
| `vm_brk_shrink_tlb_plan_aggregate_is_zero_in_single_cpu` | OR of per-page bitmaps is 0 in single-CPU; no cross-CPU IPC needed |
| `vm_anon_map_rollback_tlb_plan_covers_progress_range` | VmAnonMapRollbackTlbPlan correctly captures VmPageMapProgress fields |

### 20.9 Files changed (Stage 5E)

| File | Change |
|------|--------|
| `src/kernel/boot/mod.rs` | Added `TlbShootdownWaitPlan`, `VmBrkShrinkTlbPlan`, `VmAnonMapRollbackTlbPlan` structs |
| `src/kernel/boot/memory_state.rs` | Added `unmap_page_phase1` helper (scaffold, dead code in non-test builds) |
| `src/kernel/boot/tests.rs` | Added 6 new Stage 5E tests |
| `doc/KERNEL_LOCKING.md` | This section (Section 20) |
| `doc/KERNEL_TEST_RULES.md` | Rule N+10 added |

---

## §21 Stage 5F: live two-phase VmBrk shrink

### 21.1 Goal

Stage 5F resolves Stage 5E blocker #1a in code comments and performs the first
bounded live two-phase VM/TLB conversion: the VmBrk shrink path only.

VmAnonMap and `rollback_anon_map` remain unchanged.

### 21.2 Part 1 — rank-ordering clarification (blocker #1a resolved)

A full doc comment was added to `request_live_asid_shootdown` in
`memory_state.rs`. The comment states precisely:

- ipc(rank 3) is acquired **sequentially**, after all vm(5) and memory(6)
  locks have been **released**. There is no simultaneous vm/memory/ipc nesting.
- The correct two-phase call sequence is:
  ```
  Phase 1: vm(5) → remove page → vm(5) released
           memory(6) → clear COW + decrement refcount → memory(6) released
  Phase 2: ipc(3) → shootdown wait → ipc(3) released
  Phase 3: memory(6) → reclaim frame → memory(6) released
  ```
- The real hazard (blocker #1b) is that the old callers reclaimed the frame
  BEFORE the shootdown. Under the global lock this is safe; without it, a
  freed frame could be reused while stale TLB entries still point to it.
- Future code that splits paths must release vm/memory before calling
  `request_live_asid_shootdown`.

**Blocker #1a status: resolved** (code comment + §21).

### 21.3 VmBrk shrink path — audit (Part 2)

The old shrink path in `handle_vm_brk`:

```
requested < current_end:
  unmap_start = round_up_page(requested)
  unmap_end   = round_up_page(current_end)
  if unmap_start < unmap_end:
    for va in [unmap_start, unmap_end) step PAGE_SIZE:
      unmap_user_page_in_current_asid(va):
        1. scheduler(1) + task(2): resolve current TID and ASID
        2. vm(5): AddressSpace::unmap_page(va) → Option<Mapping>
        3. if Some(mapping):
             memory(6): clear_cow_page(asid, va)
             memory(6): note_mapping_removed(phys)
             memory(6): reclaim_memory_object_for_phys(phys)  ← wrong order
             ipc(3): request_live_asid_shootdown(asid, va)    ← after reclaim
        4. Returns Ok(None) for lazy/absent pages → loop continues
set_task_brk_bounds(tid, base, requested)
frame.set_ok(requested)
```

Key behaviors to preserve:
- Partial page (non-page-aligned `requested_end`): `unmap_start = round_up_page(requested)`
  means the page containing `requested_end` is NOT unmapped. Preserved ✓
- Lazy pages: `unmap_user_page_in_current_asid` returns `Ok(None)` for absent
  pages. The new `unmap_page_phase1` preserves this — see §20. Preserved ✓
- brk updated after all unmap bookkeeping: `set_task_brk_bounds` is called after
  the unmap loop. Preserved ✓
- Error behavior: if any unmap step errors, the loop aborts and brk is NOT
  updated. Preserved ✓ (new path uses `?` in the same positions)

### 21.4 Live two-phase VmBrk shrink conversion (Part 3)

**Conversion: LIVE.** The shrink branch in `handle_vm_brk` was updated to use
the two-phase unmap pattern.

New method added: `execute_tlb_shootdown_wait_plan(&mut self, plan: TlbShootdownWaitPlan)`.

#### 21.4.1 New lock/order sequence

```
Phase 0 (plan-first): scheduler(1) + task(2) → resolve ASID once, before loop
  [ old: resolved per-iteration inside unmap_user_page_in_current_asid ]

For each va in [unmap_start, unmap_end):
  Phase 1 — unmap_page_phase1(asid, va):
    vm(5): AddressSpace::unmap_page(va) → Option<Mapping>
    if Some(mapping):
      memory(6): clear_cow_page(asid, va)
      memory(6): note_mapping_removed(phys)
      scheduler(1)+task(2): compute_tlb_shootdown_request_plan → target bitmap
    returns Ok(Some(plan)) or Ok(None)

  Phase 2+3 — execute_tlb_shootdown_wait_plan(plan):
    if plan.target_cpu_bitmap != 0:
      ipc(3): begin_live_tlb_shootdown_wait(...)
      busy-wait for ACKs
      ipc(3): clear_live_tlb_shootdown_wait()
    else: fast path, no ipc lock (always taken in single-CPU)
    memory(6): reclaim_memory_object_for_phys(plan.phys)   ← AFTER shootdown

set_task_brk_bounds(tid, base, requested)
frame.set_ok(requested)
```

#### 21.4.2 What changed vs old path

| Property | Old path | New path |
|----------|----------|----------|
| ASID resolution | Per-iteration (scheduler+task read each loop) | Once before loop (plan-first) |
| reclaim vs shootdown order | reclaim BEFORE shootdown | shootdown BEFORE reclaim ✓ |
| Lazy page handling | `Ok(None)` in `unmap_user_page_in_current_asid` | `Ok(None)` in `unmap_page_phase1` |
| Partial page | `round_up_page(requested)` unchanged | unchanged |
| brk update timing | after all unmaps | after all unmaps (unchanged) |
| Error abort | `?` propagates | `?` propagates (unchanged) |

The only **semantic** change is the order of shootdown and reclaim. Under the
global lock, both orders are safe — no observable behavior difference.

### 21.5 Frame reclaim ordering invariant

**Invariant**: `reclaim_memory_object_for_phys(plan.phys)` in phase 3 is called
only after `request_live_asid_shootdown` (phase 2) has completed or confirmed
that no shootdown is needed (`target_cpu_bitmap == 0`).

This is enforced by the sequential structure of `execute_tlb_shootdown_wait_plan`:
```rust
if plan.target_cpu_bitmap != 0 {
    self.request_live_asid_shootdown(plan.asid, plan.virt)?;
    // Returns only after all ACKs received or fast-pathed.
}
self.reclaim_memory_object_for_phys(plan.phys);  // Always last.
```

### 21.6 VmAnonMap — still deferred (Part 4)

`handle_vm_anon_map` and `rollback_anon_map` are **unchanged**.

The VmBrk conversion demonstrates that the two-phase pattern works correctly.
VmAnonMap defers until:
- Blocker #3 (x86_64 SMP smoke) is satisfied.
- The per-page loop progress capture (`VmAnonMapProgressPlan`) is wired up.

### 21.7 x86_64 smoke requirement (Part 6)

**x86_64 smoke is required** before this change is considered final for SMP.
The VmBrk shrink path was live-converted: `reclaim_memory_object_for_phys` now
runs after `request_live_asid_shootdown`, a different order than before.

Under the global lock on a single-CPU host, the behavior is identical.
On SMP hardware, the new ordering ensures frames are not reused while stale
TLB entries exist. The smoke acceptance criteria are unchanged from the
Stage 5E task specification.

The conversion is safe to ship to main for the current single-CPU supported
configurations. Smoke is required before enabling VmBrk shrink on the SMP
path.

### 21.8 Hard invariants for Stage 5F

- VmBrk shrink semantics: preserved (partial page, lazy pages, brk timing). ✓
- `handle_vm_anon_map` unchanged. ✓
- All 614 tests pass. ✓
- No SMP/smp.rs changes. ✓
- No trap return/writeback changes. ✓
- No syscall ABI/SYSCALL_COUNT changes. ✓
- No CapRights widening, SpawnV5, VFS, syscall27, Phase 3B changes. ✓
- Global lock (`SharedKernel::with_cpu()`) still wraps all syscall handlers. ✓

### 21.9 Tests added (Stage 5F)

| Test | What it proves |
|------|----------------|
| `vm_brk_two_phase_shrink_removes_mapped_pages_and_updates_bounds` | end-to-end: all 3 pages removed, bounds updated |
| `vm_brk_two_phase_shrink_non_page_aligned_preserves_partial_page` | non-aligned requested_end preserves partial page, removes full pages above |
| `vm_brk_two_phase_shrink_empty_unmap_range_preserves_page` | intra-page shrink: empty unmap range, page preserved, brk updated |
| `execute_tlb_shootdown_wait_plan_completes_in_single_cpu_fast_path` | phase2+3 helper: fast path (bitmap==0), no error, page still absent |
| `vm_brk_two_phase_shrink_single_page_updates_to_base` | single-page brk: full unmap, bounds = (base, base) |

### 21.10 Files changed (Stage 5F)

| File | Change |
|------|--------|
| `src/kernel/boot/memory_state.rs` | Added rank-ordering doc comment to `request_live_asid_shootdown`; removed `#[cfg_attr(not(test), allow(dead_code))]` from `compute_tlb_shootdown_request_plan` and `unmap_page_phase1` (now live); added `execute_tlb_shootdown_wait_plan` |
| `src/kernel/syscall.rs` | Live-converted VmBrk shrink to two-phase: ASID resolved once plan-first, per-page `unmap_page_phase1` + `execute_tlb_shootdown_wait_plan` |
| `src/kernel/boot/tests.rs` | Added 5 new Stage 5F tests |
| `doc/KERNEL_LOCKING.md` | This section (Section 21) |
| `doc/KERNEL_TEST_RULES.md` | Rule N+11 added |

---

## §22 Stage 5G: VmBrk smoke acceptance, VmAnonMap readiness gate

### 22.1 Stage 5F x86_64 -smp 1 smoke results

The two-phase VmBrk shrink conversion (Stage 5F commit `a262d7e`) was validated
on x86_64 with `-smp 1` (single vCPU). All acceptance criteria passed:

| Marker | Expected | Actual |
|--------|----------|--------|
| `X86_BOOTSTRAP_TIMER_IRQ_EOI_ONLY` | 0 | 0 ✓ |
| `X86_BOOTSTRAP_SCHEDULER_READY` | 1 | 1 ✓ |
| `X86_BOOTSTRAP_TIMER_STARTED` | 1 | 1 ✓ |
| `ENTER_USER` | ≥ 1 | 3 ✓ |
| `STARTUP_INSTALL_FINAL` | ≥ 1 | 9 ✓ |
| `PM_ELF_ZC_DONE` image_id 7/8/9 total | 3 | 3 ✓ |
| ZC nonzero pages | 3 | 3 ✓ |
| `PM_ELF_ZC_FAIL` | 0 | 0 ✓ |
| initramfs/devfs/vfs entries | 1 each | 1 each ✓ |
| `DRIVER_MANAGER_READY` | 1 | 1 ✓ |
| `BLKCACHE_SRV_READY` | 1 | 1 ✓ |
| `VIRTIO_BLK_SRV_READY` | 1 | 1 ✓ |
| fallback | 0 | 0 ✓ |
| TID mismatch | 0 | 0 ✓ |
| fatal-ish | 0 | 0 ✓ |
| oom / capacity | 0 | 0 ✓ |

### 22.2 VmBrk two-phase shrink acceptance status

| Scope | Status |
|-------|--------|
| x86_64 -smp 1 (single vCPU) | **Accepted** ✓ (smoke §22.1) |
| x86_64 SMP (multi-vCPU) | **Deferred** — not tested, not changed |
| AArch64 | **Deferred** — smoke not run |

The two-phase shrink conversion is accepted for x86_64 single-CPU configurations.
SMP is out of scope until a dedicated multi-CPU smoke run is scheduled.

### 22.3 VmAnonMap live conversion — full readiness audit

This section records the state of every identified blocker as of Stage 5G.

#### 22.3.1 Blocker tracking table

| Blocker | Description | Status |
|---------|-------------|--------|
| #1a | ipc(3) acquired after vm(5)/memory(6) — sequential rank inversion documentation | **Resolved** Stage 5F §21.2 |
| #1b | `reclaim_memory_object_for_phys` called before `request_live_asid_shootdown` — frame-reuse hazard for global-lock removal | **Design complete** (Stage 5E §20.4); **pattern validated** (Stage 5F VmBrk smoke §22.1) |
| #2 | Per-page loop progress not captured — rollback scope undefined at type level | **Resolved** Stage 5D (`VmPageMapProgress`, `VmAnonMapProgressPlan`) |
| #3 | x86_64 smoke required before any live VM/TLB reordering | **Partially satisfied** — VmBrk smoke validates the two-phase pattern; VmAnonMap rollback uses the identical pattern (see §22.3.2). A Stage 6 x86_64 -smp 1 smoke run is still required after the VmAnonMap conversion. |

#### 22.3.2 Two-phase pattern transferability

The two-phase pattern validated by VmBrk smoke (§22.1) transfers directly to
VmAnonMap rollback because:

1. `rollback_anon_map` calls `unmap_user_page_in_current_asid` per page — the
   same old one-shot function replaced by `unmap_page_phase1` +
   `execute_tlb_shootdown_wait_plan` in Stage 5F.
2. The new helpers operate on (asid, virt) pairs and are agnostic to the calling
   context (VmBrk shrink vs. VmAnonMap rollback).
3. `rollback_anon_map` already silently ignores errors (`let _ = ...`), which is
   compatible with the `?` propagation inside `execute_tlb_shootdown_wait_plan`
   (the caller would need `let _ = kernel.execute_tlb_shootdown_wait_plan(plan)`).

#### 22.3.3 VmAnonMap-specific items for Stage 6

In addition to converting `rollback_anon_map`, Stage 6 should address these
VmAnonMap-specific plan-first gaps:

| Item | What is needed | Scaffold available |
|------|----------------|-------------------|
| ASID resolution in forward map loop | Resolve ASID once before loop via `task_asid(tid)` (plan-first); use `map_user_page_in_asid_with_caps` per iteration | ✓ Stage 5C |
| Stack guard check | Replace `is_user_page_mapped_in_current_asid` with `is_user_page_mapped_in_asid(asid, ...)` | ✓ Stage 5C |
| Rollback ASID resolution | Resolve ASID once before loop; use `unmap_page_phase1(asid, va)` per iteration | ✓ Stage 5E |
| `VmAnonMapProgressPlan` wiring | Wire plan into forward loop and rollback path | ✓ Stage 5D (struct only, not wired) |
| Error-path shootdown/reclaim in rollback | Per-page `execute_tlb_shootdown_wait_plan`; errors silently ignored | ✓ Stage 5F (helper live) |

#### 22.3.4 Pre-existing issue: capability not revoked in rollback

`rollback_anon_map` unmaps pages but does not destroy the capability slots
created by `alloc_anonymous_memory_object`. After rollback:

- `map_refcount == 0` (unmap decremented it)
- `cap_refcount == 1` (capability still alive in task's CNode)

`reclaim_memory_object_for_phys` checks `cap_refcount != 0` and skips the
`free_frame` call. The physical frame is therefore not returned to the
allocator until the task exits (at which point all capabilities are destroyed).

This is **pre-existing behavior** in the current `rollback_anon_map`.  The
two-phase conversion does NOT change this behavior — `execute_tlb_shootdown_wait_plan`
calls the same `reclaim_memory_object_for_phys`, which has the same cap_refcount
check. Stage 6 must not claim to fix this issue; a separate fix (revoke capability
before unmap) is needed and is out of scope.

#### 22.3.5 `handle_vm_map` rollback gap (adjacent pre-existing issue)

`handle_vm_map` explicitly documents a missing rollback (see TODO in
`src/kernel/syscall.rs`). This is also pre-existing and out of scope for
Stage 5G and Stage 6 (VmAnonMap only).

### 22.4 Stage 6 gate decision

**Stage 6 MAY proceed with a live VmAnonMap rollback conversion**, subject to
the following conditions:

#### 22.4.1 Required before Stage 6 merge

1. `rollback_anon_map` converted to use `unmap_page_phase1` +
   `execute_tlb_shootdown_wait_plan` per page, with ASID resolved once
   before the loop (plan-first).
2. Forward map loop (`handle_vm_anon_map`) converted to use
   `map_user_page_in_asid_with_caps` with plan-first ASID resolution.
3. `check_stack_guard` call updated to use `is_user_page_mapped_in_asid`
   with the plan-first ASID.
4. Full test suite passes (614+ tests, `--test-threads=1`).
5. x86_64 -smp 1 smoke run passes for Stage 6 changes.

#### 22.4.2 Deferred to a later stage

- `VmAnonMapProgressPlan` wiring into the live loop (plan struct already
  exists; wiring requires the map loop to hold the progress value across
  iterations and pass it to rollback).
- Capability revoke on rollback (pre-existing issue §22.3.4).
- x86_64 SMP (multi-CPU) smoke.

#### 22.4.3 Stage 6 scope summary

| Conversion | In scope | Notes |
|------------|----------|-------|
| `rollback_anon_map` → two-phase | ✓ | Identical pattern to VmBrk shrink |
| `handle_vm_anon_map` forward loop plan-first ASID | ✓ | Use Stage 5C helpers |
| `check_stack_guard` explicit-ASID | ✓ | Stage 5C helper available |
| `VmAnonMapProgressPlan` live wiring | Defer | Non-trivial; separate stage |
| Capability revoke in rollback | Defer | Pre-existing issue, separate fix |

### 22.5 No code changes in Stage 5G

Stage 5G is a documentation-only pass. No source files were modified.
All Stage 5F tests (614 total) pass unchanged.

### 22.6 Files changed (Stage 5G)

| File | Change |
|------|--------|
| `doc/KERNEL_LOCKING.md` | This section (Section 22): smoke record, VmBrk acceptance, VmAnonMap readiness audit, Stage 6 gate decision |
| `doc/KERNEL_TEST_RULES.md` | Rule N+12 added: Stage 6 VmAnonMap gate conditions |

## §23 Stage 6: VmAnonMap live two-phase unmap and explicit-ASID forward map

### 23.1 What was converted

Stage 6 live-converts the VmAnonMap syscall path to use the two-phase unmap
helpers and explicit-ASID forward mapping, completing the work gated by §22.

#### 23.1.1 `rollback_anon_map` — two-phase unmap

Old implementation called `unmap_user_page_in_current_asid` (one-phase, no
TLB shootdown wait, implicit current ASID).

New implementation:

1. **Plan-first ASID**: ASID is resolved once by the caller (`handle_vm_anon_map`)
   before the loop starts and passed as a parameter.
2. **Per page**: `unmap_page_phase1(asid, VirtAddr(va))` — removes page table
   entry, clears COW state, records the mapping removal, returns a
   `TlbShootdownWaitPlan`. Returns `Ok(None)` for absent pages (silently skipped).
3. **Phase 2**: `execute_tlb_shootdown_wait_plan(plan)` — fast path when
   `target_cpu_bitmap == 0` (single CPU / no remote CPUs); otherwise sends
   cross-CPU shootdown IPI and waits, then calls `reclaim_memory_object_for_phys`.
4. **Capability not revoked**: Pre-existing behavior preserved. `cap_refcount=1`
   means `reclaim_memory_object_for_phys` does not free the frame to the
   allocator; the frame is freed only at task exit. Capability revoke is deferred
   to a later stage.

Locking sequence per page (rollback path):
```
vm lock (phase1: remove PTE + note_mapping_removed)
  → release vm lock
  → [if bitmap != 0] ipc lock (cross-CPU shootdown wait)  [rank 3 < rank 5 ok]
  → memory lock (reclaim_memory_object_for_phys)           [rank 6 > rank 3 ok]
```
Absent pages (`Ok(None)`) skip both phase-2 locks entirely.

#### 23.1.2 `handle_vm_anon_map` — plan-first ASID forward map

Old implementation resolved ASID implicitly each iteration via
`map_user_page_in_current_asid_with_caps` (current-ASID path, no explicit
ASID parameter).

New implementation:

1. **Plan-first ASID resolution**: `current_tid(kernel)?` then
   `kernel.task_asid(tid).ok_or(...)` — resolves ASID once before the map
   loop. Returns `UserMemoryFault` if the task has no ASID.
2. **Explicit-ASID guard check**: Inline check using
   `is_user_page_mapped_in_asid(asid, VirtAddr(guard_page))` replaces the
   old `check_stack_guard` call. Condition unchanged: `write && !execute &&
   guard_page_mapped`. `check_stack_guard` is preserved for `handle_vm_map`.
3. **Forward map loop**: `map_user_page_in_asid_with_caps(asid, mem_cap,
   VirtAddr(va), flags)` — explicit ASID, identical frame allocation logic.
4. **Rollback on error**: `rollback_anon_map(kernel, asid, addr, va)` — passes
   the plan-first ASID through; each rollback iteration uses phase1+phase2.

Locking sequence per page (forward map path):
```
memory lock (alloc_anonymous_memory_object)               [rank 6]
  → release memory lock
  → vm + memory lock (map_user_page_in_asid_with_caps)   [rank 5 then rank 6, ordered]
  → release both
```

#### 23.1.3 Dead-code suppression removed

`#[cfg_attr(not(test), allow(dead_code))]` removed from:
- `map_user_page_in_asid_with_caps` (now live in `handle_vm_anon_map`)
- `is_user_page_mapped_in_asid` (now live in guard check)

`VmAnonMapProgressPlan` retains `#[cfg_attr(not(test), allow(dead_code))]`
— wiring deferred.

### 23.2 Invariants preserved

| Invariant | How preserved |
|-----------|---------------|
| VmAnonMap observable behavior | Same frame allocation + flag encoding + guard condition |
| Rollback covers only mapped range | Loop: `addr..mapped_end`, skips `Ok(None)` |
| Capability-not-revoked on rollback | `reclaim_memory_object_for_phys` with `cap_refcount=1` does not free frame |
| No SYSCALL_COUNT / ABI change | No new syscalls; no argument encoding change |
| SpawnV5 semantics unchanged | Not touched |
| Phase 3B not weakened | Not touched |
| VFS/IPC/recv-v2/COW/demand paging | Not touched |
| x86_64 SMP / trap / bootstrap | Not touched |
| `check_stack_guard` for `handle_vm_map` | Original function preserved; Stage 6 only inlines for anon map |

### 23.3 Lock rank ordering

All lock acquisitions in the converted paths respect the domain rank order
(rank 1 scheduler < rank 2 task < rank 3 ipc < rank 4 capability < rank 5 vm
< rank 6 memory). No rank inversions introduced.

Phase 2 (ipc rank 3) always executes after phase 1 (vm rank 5) has released
its lock, so the rank-3 < rank-5 sequence at execution time is safe
(ipc acquired when vm is NOT held).

### 23.4 Tests

6 new tests added in `src/kernel/boot/tests.rs`:

| Test | What it verifies |
|------|-----------------|
| `vm_anon_map_stage6_plan_first_asid_maps_pages_correctly` | Three-page map via syscall, all pages visible via explicit-ASID check |
| `vm_anon_map_stage6_explicit_asid_guard_fires` | Guard rejects PROT_WRITE when page below is pre-mapped |
| `vm_anon_map_stage6_rollback_two_phase_removes_pages` | Phase1+phase2 helpers remove two pre-mapped pages |
| `vm_anon_map_stage6_rollback_tolerates_absent_pages` | Phase1 on absent page returns `Ok(None)`, no panic/error |
| `vm_anon_map_stage6_execute_only_guard_bypass_regression` | PROT_EXEC bypasses guard even when guard page is mapped |
| `vm_anon_map_stage6_write_execute_guard_bypass_regression` | PROT_WRITE\|PROT_EXEC bypasses guard (execute=true disarms) |

Total test count after Stage 6: **620** (was 614 before Stage 6).

### 23.5 Smoke requirement

Before Stage 6 is considered complete, x86_64 `-smp 1` smoke must pass with
the same acceptance criteria as §22.1. The same markers apply:
`X86_BOOTSTRAP_SCHEDULER_READY`, `ENTER_USER ≥ 1`, `STARTUP_INSTALL_FINAL ≥ 1`,
`PM_ELF_ZC_FAIL = 0`, no fatal-ish / oom / TID-mismatch markers.

### 23.6 Deferred

- `VmAnonMapProgressPlan` live wiring (progress tracking across loop iterations).
- Capability revoke on rollback (pre-existing; deferred since Stage 5C).
- x86_64 SMP smoke.

### 23.7 Files changed (Stage 6)

| File | Change |
|------|--------|
| `src/kernel/boot/memory_state.rs` | Removed `dead_code` suppression from `map_user_page_in_asid_with_caps` and `is_user_page_mapped_in_asid` |
| `src/kernel/syscall.rs` | `rollback_anon_map` converted to two-phase; `handle_vm_anon_map` converted to plan-first ASID + explicit-ASID guard; `Asid` added to imports |
| `src/kernel/boot/tests.rs` | 6 Stage 6 tests added |
| `doc/KERNEL_LOCKING.md` | This section (§23) |
| `doc/KERNEL_TEST_RULES.md` | Rule N+13 added |

## §24 Stage 6A: smoke acceptance record, remaining-path audit, and next-target gate

### 24.1 Stage 6 x86_64 -smp 1 smoke acceptance

Stage 6 commit `dbc60bb` was validated on x86_64 with `-smp 1` (single vCPU).
All acceptance criteria passed:

| Marker | Expected | Actual |
|--------|----------|--------|
| `X86_BOOTSTRAP_TIMER_IRQ_EOI_ONLY` | 0 | 0 ✓ |
| `X86_BOOTSTRAP_SCHEDULER_READY` | 1 | 1 ✓ |
| `X86_BOOTSTRAP_TIMER_STARTED` | 1 | 1 ✓ |
| `ENTER_USER` | ≥ 1 | 3 ✓ |
| `STARTUP_INSTALL_FINAL` | ≥ 1 | 9 ✓ |
| `PM_ELF_ZC_DONE` image_id 7/8/9 total | 3 | 3 ✓ |
| ZC nonzero pages | 3 | 3 ✓ |
| `PM_ELF_ZC_FAIL` | 0 | 0 ✓ |
| `INITRAMFS_SRV_ENTRY` | 1 | 1 ✓ |
| `DEVFS_SRV_ENTRY` | 1 | 1 ✓ |
| `VFS_SRV_ENTRY` | 1 | 1 ✓ |
| `DRIVER_MANAGER_READY` | 1 | 1 ✓ |
| `BLKCACHE_SRV_READY` | 1 | 1 ✓ |
| `VIRTIO_BLK_SRV_READY` | 1 | 1 ✓ |
| fallback | 0 | 0 ✓ |
| TID mismatch | 0 | 0 ✓ |
| fatal-ish | 0 | 0 ✓ |
| oom / capacity / Vm(Full) | 0 | 0 ✓ |

**Stage 6 is accepted for x86_64 -smp 1.**

### 24.2 AArch64 health note

AArch64 was reported healthy after Stage 6. No AArch64-specific marker table is
available for this stage. x86_64 SMP and AArch64 SMP remain deferred; the
single-CPU smoke is the accepted validation boundary for all stages through
Stage 6.

### 24.3 Remaining current-ASID caller audit

After Stage 6 the following callers of the current-ASID helpers remain in
production code (`src/kernel/syscall.rs`). Test helper code (lines ≥ 3580) is
excluded.

#### 24.3.1 `unmap_user_page_in_current_asid` — 3 call sites

| Location | Context | Ordering note |
|----------|---------|---------------|
| `map_shared_region_into_receiver` line 702 | Rollback on partial map failure in IPC recv shared-memory path | Reclaims before shootdown (pre-Stage-5E ordering) |
| `handle_ipc_recv` line 1791 | Rollback when `register_active_transfer_mapping` fails in IPC recv | Reclaims before shootdown |
| `handle_transfer_release` line 1959 | Forward unmap loop in `TransferRelease` syscall (syscall 4) | Reclaims before shootdown |

All three retain the pre-Stage-5E ordering issue: `unmap_user_page_in_current_asid`
calls `reclaim_memory_object_for_phys` **before** `request_live_asid_shootdown`.
In single-CPU operation this is not observable (the local TLB is flushed by the
page-table write before the frame is reused), but on multi-CPU it is unsafe
until the two-phase conversion is applied.

`unmap_user_page_in_current_asid` also reads the scheduler (rank 1) and task
(rank 2) domains internally on every call, coupling the unmap loop to those
domains inside the global lock.

#### 24.3.2 `map_user_page_in_current_asid_with_caps` — 1 call site

| Location | Context |
|----------|---------|
| `map_shared_region_into_receiver` line 695 | Forward map in IPC recv shared-memory path |

Like its rollback partner, this resolves the current ASID internally on every
call. Converting to `map_user_page_in_asid_with_caps` with a plan-first ASID
eliminates the per-call scheduler/task reads.

#### 24.3.3 `is_user_page_mapped_in_current_asid` — 1 call site

| Location | Context |
|----------|---------|
| `check_stack_guard` line 1897 | Guard check used only by `handle_vm_map` (syscall 3) |

**Note on ASID consistency**: `handle_vm_map` maps pages via `map_user_page_with_caps(aspace_map_cap,
...)`, which resolves the target ASID from the `aspace_map_cap` capability object.
`check_stack_guard` checks `is_user_page_mapped_in_current_asid`, which resolves
the ASID from `current_tid()`. In practice these are the same ASID (a task
only holds capabilities to its own address space), but the code paths diverge.
The correct Stage-7 fix resolves the ASID from `aspace_map_cap` via the
capability object and passes that ASID to the inline guard check — making the
guard consistent with where the pages actually go. This is a documentation of
a pre-existing subtle mismatch, not a runtime bug.

#### 24.3.4 Summary table

| Helper | Callers remaining | Ordering issue |
|--------|-------------------|----------------|
| `unmap_user_page_in_current_asid` | 3 | Reclaim before shootdown (unsafe on SMP) |
| `map_user_page_in_current_asid_with_caps` | 1 | Per-call ASID re-resolution |
| `is_user_page_mapped_in_current_asid` | 1 | Per-call ASID re-resolution (read-only; no ordering issue) |

### 24.4 Next-target recommendation

Candidates are ranked by conversion safety and independence from the hard
invariants.

#### Rank 1 — `handle_transfer_release` two-phase unmap (Stage 7)

**Why**: `handle_transfer_release` (syscall 4 `TransferRelease`) has the
cleanest profile for the next live conversion:

- Already resolves `current_tid` plan-first at the top of the function.
- The unmap loop is entirely analogous to `rollback_anon_map` (Stage 6) —
  one page at a time, current-ASID, no mid-loop scheduler reads.
- Two-phase conversion: plan-first ASID (`task_asid(current_tid)` once before
  the loop), then `unmap_page_phase1` + `execute_tlb_shootdown_wait_plan` per
  page.
- Fixes the reclaim-before-shootdown ordering issue for this path.
- Capability revoke (`revoke_capability_in_cnode`) and
  `remove_active_transfer_mapping` happen **after** all unmaps — the existing
  ordering is already correct and is preserved unchanged.
- Does not touch VFS/syscall27/IPC recv/VFS_READ_SHARED_REPLY_ENABLED.
- Observable behavior preserved: `TransferRelease` returns the same success/error
  codes; unmapped range is the same; capability revoke is unchanged.

Locking sequence (Stage 7 target):
```
task lock [plan-first ASID resolution, rank 2]
  → release
  → per page:
      vm lock (phase1: remove PTE + note_mapping_removed)  [rank 5]
        → release
        → [if bitmap != 0] ipc lock (shootdown wait)       [rank 3 < rank 5 ok]
        → memory lock (reclaim)                            [rank 6]
  → capability lock (revoke_capability_in_cnode)           [rank 4]
  → memory lock (remove_active_transfer_mapping/note_shared_mem_released)
```

Gate conditions:
- Two-phase helpers (`unmap_page_phase1`, `execute_tlb_shootdown_wait_plan`) are
  live (Stage 5E/5F).
- Stage 6 acceptance: x86_64 -smp 1 passed (this stage, §24.1).
- Tests: add Stage 7 transfer-release two-phase tests before converting.
- x86_64 -smp 1 smoke required after Stage 7.

#### Rank 2 — `handle_vm_map` explicit-ASID guard (Stage 7 companion)

Inline the `check_stack_guard` call in `handle_vm_map` with an explicit-ASID
check, resolving the ASID from the `aspace_map_cap` capability object rather
than from `current_tid`. This:
- Eliminates the last live caller of `is_user_page_mapped_in_current_asid`.
- Makes the guard check consistent with the map target ASID.
- Is surgical and read-only (no unmap/reclaim, no ordering issue).
- Prerequisite: add a helper or inline the capability→ASID extraction.

This is lower-priority than Rank 1 because `is_user_page_mapped_in_current_asid`
has no multi-CPU ordering issue (it is read-only). It is a cleanup/consistency
fix, not a safety-critical correction. It can be done in the same stage as
Rank 1 or separately.

#### Rank 3 — `map_shared_region_into_receiver` + IPC recv rollback (Stage 7+)

The IPC recv shared-memory path (`map_shared_region_into_receiver` and its
rollback at line 1791) is the most complex remaining current-ASID caller. It
sits inside `handle_ipc_recv`, which implements IPC recv-v2. The invariant
"Preserve IPC recv-v2, reply-cap, transfer-envelope" means behavioral
preservation is mandatory; this path can be converted but requires careful
recv-v2 regression coverage and is higher-risk than Ranks 1 and 2.

#### Rank 4 — `VmAnonMapProgressPlan` live wiring (deferred)

Explicitly deferred per §23.6. Not gated by Stage 6A.

#### Rank 5 — Capability revoke in `rollback_anon_map` (deferred)

Explicitly deferred per §23.6. Not gated by Stage 6A.

### 24.5 Stage 7 gate conditions

Stage 7 (`handle_transfer_release` two-phase conversion) is gated on:

1. ✓ Stage 6 x86_64 -smp 1 smoke passed (this stage, §24.1).
2. ✓ 620 tests pass on `claude/ecstatic-feynman-9ZZwC` (Stage 6 commit
   `dbc60bb`).
3. Stage 7 must add `handle_transfer_release` two-phase tests before the live
   conversion (minimum: single-page release removes page + revokes cap; absent
   page handling if applicable; two-phase fast-path in single-CPU).
4. Stage 7 must pass all 620+ tests after conversion.
5. Stage 7 requires x86_64 -smp 1 smoke before acceptance.

### 24.6 Still deferred (unchanged)

- `VmAnonMapProgressPlan` live wiring.
- Capability revoke on `rollback_anon_map`.
- x86_64 SMP smoke.
- `map_shared_region_into_receiver` two-phase conversion (Rank 3, after Stage 7).

### 24.7 Files changed (Stage 6A)

| File | Change |
|------|--------|
| `doc/KERNEL_LOCKING.md` | This section (§24): smoke acceptance record, remaining-path audit, next-target recommendation, Stage 7 gate conditions |

---

## §25 — Stage 7: Large coherent pass — remaining current-ASID syscall.rs domain

**Commit:** `claude/ecstatic-feynman-9ZZwC` (Stage 7)
**Tests:** 629 total (620 from Stage 6 + 9 new Stage 7 tests)
**Prerequisites:** Stage 6A acceptance (§24), 620-test baseline, Stage 6 x86_64 -smp 1 smoke.

### 25.1 Scope

Stage 7 owns the entire remaining current-ASID caller set inside `syscall.rs`
that was audited in §24.3. Three live conversions and one dead-code cleanup:

| Target | Conversion | Risk |
|--------|-----------|------|
| `handle_transfer_release` unmap loop | `unmap_user_page_in_current_asid` → two-phase | SMP reclaim ordering |
| `map_shared_region_into_receiver` rollback (2 sites) | `unmap_user_page_in_current_asid` → two-phase | rollback correctness |
| `handle_vm_map` stack-guard check | `is_user_page_mapped_in_current_asid` → explicit-ASID | ASID consistency |
| `check_stack_guard` helper | deleted (dead after guard inlined in `handle_vm_map`) | — |

The IPC recv forward-map site (`map_user_page_in_current_asid_with_caps` in
`try_handle_demand_page_fault`) is **deferred** (Class D) — it lives in
`fault_state.rs`, is guarded by the "preserve COW/fork/demand-paging" hard
invariant, and already has `asid` in scope from line 98 for a future stage.

### 25.2 `handle_transfer_release` — two-phase unmap

**Before (unsafe on SMP):**
```rust
match kernel.unmap_user_page_in_current_asid(VirtAddr(va as u64)) {
    None    => return Err(SyscallError::InvalidArgs),
    Some(_) => { va += PAGE_SIZE; }
}
```
`unmap_user_page_in_current_asid` calls `reclaim_memory_object_for_phys` before
`request_live_asid_shootdown`, violating the reclaim-after-shootdown ordering.

**After (Stage 7):**
```rust
// plan-first ASID resolution
let asid = kernel.task_asid(owner.0).ok_or(...)?;
while va < end {
    let plan = kernel.unmap_page_phase1(asid, VirtAddr(va as u64))
        .map_err(SyscallError::from)?;
    let Some(plan) = plan else { return Err(SyscallError::InvalidArgs); };
    kernel.execute_tlb_shootdown_wait_plan(plan).map_err(SyscallError::from)?;
    va += PAGE_SIZE;
}
```

`Ok(None)` from `unmap_page_phase1` is mapped to `InvalidArgs` exactly as
before — absent-page behavior is preserved.

Locking sequence per page:
1. Acquire global lock (held throughout).
2. Phase 1: PTE removal + `note_mapping_removed` (vm rank 5, memory rank 6).
3. Phase 2: `execute_tlb_shootdown_wait_plan` — TLB shootdown wait (ipc rank 3)
   then `reclaim_memory_object_for_phys` (memory rank 6). Reclaim occurs after
   all CPUs have acknowledged the shootdown. SMP-safe.

### 25.3 `map_shared_region_into_receiver` — plan-first ASID + two-phase rollback

**Before:** Both rollback sites used `unmap_user_page_in_current_asid` (unsafe
reclaim ordering). ASID was resolved implicitly from `current_asid()` on each
call.

**After:**
```rust
let asid = kernel.task_asid(tid).ok_or(...)?;   // plan-first, once
while va < end {
    if let Err(err) = kernel.map_user_page_in_asid_with_caps(asid, ...) {
        // two-phase rollback
        let mut rollback = requested_va;
        while rollback < va {
            if let Ok(Some(plan)) = kernel.unmap_page_phase1(asid, VirtAddr(rollback as u64)) {
                let _ = kernel.execute_tlb_shootdown_wait_plan(plan);
            }
            rollback += PAGE_SIZE;
        }
        return Err(SyscallError::from(err));
    }
    va += PAGE_SIZE;
}
```

The IPC recv `register_active_transfer_mapping` rollback site follows the same
pattern: `receiver_asid` captured before the `.map_err` closure (Asid is Copy),
rollback loop uses `unmap_page_phase1` + `execute_tlb_shootdown_wait_plan`.

### 25.4 `handle_vm_map` — explicit-ASID guard + `check_stack_guard` deletion

**Inconsistency corrected:** Before Stage 7, `check_stack_guard` used
`is_user_page_mapped_in_current_asid` (current-task ASID), while the map loop
called `map_user_page_with_caps` using the capability ASID. If the capability
referred to a different address space, the guard fired against the wrong ASID.

**Fix:** ASID extracted from `aspace_map_cap` capability before the guard check:
```rust
let map_asid = {
    let cap = kernel.capability_service()
        .resolve_current_task_capability(aspace_map_cap)
        .ok_or(...)?;
    match cap.object {
        CapObject::AddressSpace { asid } => Asid(asid),
        _ => return Err(...),
    }
};
// guard uses map_asid, same ASID as the map loop
if flags.write && !flags.execute
    && let Some(guard_page) = addr.checked_sub(PAGE_SIZE)
    && kernel.is_user_page_mapped_in_asid(map_asid, VirtAddr(guard_page as u64))
           .map_err(SyscallError::from)?
{
    return Err(SyscallError::InvalidArgs);
}
```

`check_stack_guard` (private, 20-line helper) is deleted — logic fully inlined.
`is_user_page_mapped_in_current_asid` gains
`#[cfg_attr(not(test), allow(dead_code))]` (still referenced in test module).

### 25.5 Dead-code cleanup

| Helper | Status after Stage 7 |
|--------|---------------------|
| `unmap_user_page_in_current_asid` | test-only; `#[cfg_attr(not(test), allow(dead_code))]` added |
| `is_user_page_mapped_in_current_asid` | test-only; `#[cfg_attr(not(test), allow(dead_code))]` added |
| `check_stack_guard` | deleted (no test or production caller) |

Pre-existing warning on `stash_bound_receiver_tid` (2 occurrences) unchanged.

### 25.6 Still deferred (unchanged from §24.6)

- `VmAnonMapProgressPlan` live wiring.
- Capability revoke on `rollback_anon_map`.
- `map_user_page_in_current_asid_with_caps` in `try_handle_demand_page_fault`
  (`fault_state.rs:120`) — demand paging path, hard invariant.
- x86_64 SMP smoke (required before Stage 7 acceptance; noted for operator).

### 25.7 Files changed (Stage 7)

| File | Change |
|------|--------|
| `src/kernel/syscall.rs` | `handle_transfer_release` two-phase; `map_shared_region_into_receiver` plan-first ASID + two-phase rollback; IPC recv rollback two-phase; `handle_vm_map` explicit-ASID guard; `check_stack_guard` deleted |
| `src/kernel/boot/memory_state.rs` | `#[cfg_attr(not(test), allow(dead_code))]` on `unmap_user_page_in_current_asid` and `is_user_page_mapped_in_current_asid` |
| `src/kernel/boot/tests.rs` | 9 new Stage 7 tests (transfer-release two-phase, vm_map guard, map_shared_region rollback) |
| `doc/KERNEL_LOCKING.md` | This section (§25) |
| `doc/KERNEL_TEST_RULES.md` | Rule N+14 (Stage 7 test rules) |

---

## §26 — Stage 8: Stage 7 smoke acceptance + demand-paging explicit-ASID conversion

**Commit:** `claude/ecstatic-feynman-9ZZwC` (Stage 8)
**Tests:** 635 total (629 from Stage 7 + 6 new Stage 8 tests)
**Prerequisites:** Stage 7 implementation (§25), Stage 7 x86_64 -smp 1 smoke (this section).

### 26.1 Stage 7 x86_64 -smp 1 smoke acceptance

| Counter | Expected | Actual |
|---------|----------|--------|
| `X86_BOOTSTRAP_TIMER_IRQ_EOI_ONLY` | 0 | 0 |
| `X86_BOOTSTRAP_SCHEDULER_READY` | 1 | 1 |
| `X86_BOOTSTRAP_TIMER_STARTED` | 1 | 1 |
| `ENTER_USER` | ≥1 | 3 |
| `STARTUP_INSTALL_FINAL` | ≥1 | 9 |
| `PM_ELF_ZC_DONE image_id 7/8/9 total` | 3 | 3 |
| `ZC nonzero pages` | 3 | 3 |
| `PM_ELF_ZC_FAIL` | 0 | 0 |
| `INITRAMFS_SRV_ENTRY` | 1 | 1 |
| `DEVFS_SRV_ENTRY` | 1 | 1 |
| `VFS_SRV_ENTRY` | 1 | 1 |
| `DRIVER_MANAGER_READY` | 1 | 1 |
| `BLKCACHE_SRV_READY` | 1 | 1 |
| `VIRTIO_BLK_SRV_READY` | 1 | 1 |
| `fallback` | 0 | 0 |
| `TID mismatch` | 0 | 0 |
| `fatal-ish` | 0 | 0 |
| `oom/capacity/Vm(Full)` | 0 | 0 |

**Stage 7 accepted for x86_64 -smp 1.** AArch64: no data available, not claimed.
x86_64 SMP (>1 CPU): deferred, unchanged from §25.6.

### 26.2 Full current-ASID sweep — production status after Stage 8

Search scope: all production code (non-test) in `src/`.

| Helper | Production callers after Stage 8 | Status |
|--------|----------------------------------|--------|
| `unmap_user_page_in_current_asid` | 0 | test-only (`#[cfg_attr(not(test), allow(dead_code))]`) |
| `is_user_page_mapped_in_current_asid` | 0 | test-only (`#[cfg_attr(not(test), allow(dead_code))]`) |
| `map_user_page_in_current_asid_with_caps` | 0 | test-only (`#[cfg_attr(not(test), allow(dead_code))]`) |

**All three current-ASID VM helpers now have zero production callers.** They are
retained for test use in `syscall.rs` test module and `boot/tests.rs`.

The `current_asid` field in `FatalTrapReadSnapshot` (`runtime.rs`) is a
diagnostic read-only snapshot, not a mutation path — outside scope.

The `current_asid` variable in `x86_64/descriptor_tables.rs` is a local
diagnostic variable used in debug UART output during fatal traps — not a VM
mutation path, outside scope.

### 26.3 `try_handle_demand_page_fault` — audit

**Call chain:** `handle_trap_event_with_fault_bookkeeping_mode` (fault_state.rs)
→ `try_handle_demand_page_fault` (fault_state.rs).

**Source of faulting address:** `fault.addr` from the hardware page-fault trap
context, page-aligned to `fault.addr.page_align_down()`.

**Source of faulting task/TID:** `self.current_tid()` at line 97 — the task
currently scheduled on the faulting CPU at the moment the trap fires. Under the
global lock this cannot change between the trap entry and line 120.

**Source of ASID:** `self.task_asid(tid)` at line 98, from the same `tid`.
The returned `asid` is already used at lines 109 (`user_spaces.get(asid)`) and
125 (`asid.0` in the hosted-dev memory initializer) — the canonical ASID for
the fault path.

**Is "current ASID" semantically required?** No. The faulting task is the
current task; its ASID is `task_asid(current_tid())`. Both the old
`map_user_page_in_current_asid_with_caps` (which internally calls
`current_tid()` → `task_asid(tid)` again) and the new
`map_user_page_in_asid_with_caps(asid, ...)` use the same ASID — the plan-first
`asid` variable already in scope eliminates the redundant double-read.

**Can the fault ASID be captured plan-first?** Yes — `asid` is already resolved
before the map call. The conversion is a one-line change.

**Timer/preemption/restart ambiguity:** Not applicable. The global lock is held
for the entire trap dispatch. Timer interrupts are acknowledged and rescheduled
but do not release the lock mid-fault. `current_tid()` is stable.

**TLB behavior:** No TLB shootdown needed on allocation — the new page is
freshly allocated and not cached anywhere. Demand paging always maps into the
current CPU's active address space; shootdown is only needed on unmap.

**COW interaction:** Handled before demand paging at lines 411-420 of
`handle_trap_event_with_fault_bookkeeping_mode`. Demand paging is only reached
for write faults to unallocated pages, not for COW pages.

**Error/fault-report behavior:** Unchanged. `map_user_page_in_asid_with_caps`
propagates the same `KernelError` variants as `map_user_page_in_current_asid_with_caps`.

**Explicit-ASID verdict: SAFE.** The `asid` variable at line 98 is the exact
same ASID that the old helper would have computed internally. The global lock
guarantees stability. No observable behavior changes.

### 26.4 Live conversion — `try_handle_demand_page_fault`

**Before (Stage 7, implicit current-ASID double-read):**
```rust
let (_id, mem_cap) = self.alloc_anonymous_memory_object()?;
let flags = crate::kernel::vm::PageFlags::USER_RW;
self.map_user_page_in_current_asid_with_caps(mem_cap, page, flags)?;
```
`map_user_page_in_current_asid_with_caps` internally called `current_tid()` and
`task_asid(tid)` a second time — redundant reads already done at lines 97-100.

**After (Stage 8, plan-first ASID):**
```rust
let (_id, mem_cap) = self.alloc_anonymous_memory_object()?;
let flags = crate::kernel::vm::PageFlags::USER_RW;
// Stage 8: asid resolved plan-first above (line 98); identical to
// map_user_page_in_current_asid_with_caps under the global lock since
// current_tid cannot change between the plan-first resolution and here.
self.map_user_page_in_asid_with_caps(asid, mem_cap, page, flags)?;
```

**ASID source:** `asid` from `self.task_asid(tid)` at line 98, where `tid` is
`self.current_tid()` at line 97.

**Locking sequence:**
1. Global lock held throughout (all trap dispatch).
2. `alloc_anonymous_memory_object` — memory rank 6.
3. `map_user_page_in_asid_with_caps` — capability rank 4 → vm rank 5 → memory rank 6.
   No scheduler (rank 1) or task (rank 2) re-read inside the map call.
4. No TLB shootdown on fresh allocation (single-page, new physical frame).

### 26.5 Helper cleanup after Stage 8

| Helper | Status |
|--------|--------|
| `map_user_page_in_current_asid_with_caps` | `#[cfg_attr(not(test), allow(dead_code))]` added; 0 production callers |
| `unmap_user_page_in_current_asid` | already test-only from Stage 7 |
| `is_user_page_mapped_in_current_asid` | already test-only from Stage 7 |

All three helpers are retained for test use. Deletion would break test
comparisons that exercise the current-ASID vs explicit-ASID equivalence.

### 26.6 Still deferred (updated from §25.6)

- `VmAnonMapProgressPlan` live wiring.
- Capability revoke on `rollback_anon_map`.
- x86_64 SMP (>1 CPU) — requires lock-free or per-CPU demand-paging path.
- Full global-lock removal.
- RAMFS/FAT runtime server spawning (main branch is healthy; `ramfs_srv`/`fat_srv`
  images exist but spawning is TODO for a later stage — do not touch).
- AArch64 smoke (no data available, not claimed).

### 26.7 Files changed (Stage 8)

| File | Change |
|------|--------|
| `src/kernel/boot/fault_state.rs` | `try_handle_demand_page_fault` line 120: `map_user_page_in_current_asid_with_caps` → `map_user_page_in_asid_with_caps(asid, ...)` |
| `src/kernel/boot/memory_state.rs` | `#[cfg_attr(not(test), allow(dead_code))]` added to `map_user_page_in_current_asid_with_caps` |
| `src/kernel/boot/tests.rs` | 6 new Stage 8 tests (demand-page explicit-ASID) |
| `doc/KERNEL_LOCKING.md` | This section (§26): Stage 7 smoke acceptance, full current-ASID sweep, demand-paging audit, live conversion |
| `doc/KERNEL_TEST_RULES.md` | Rule N+15 (Stage 8 test rules) |

---

## §27 — Stage 9: VmAnonMapProgressPlan live wiring + rollback cap cleanup

### 27.1 Stage 8 smoke acceptance (x86_64 -smp 1)

| Counter | Value |
|---------|-------|
| ENTER_USER | 3 |
| All services READY | 1 each |
| fallback | 0 |
| fatal | 0 |
| oom | 0 |

### 27.2 Final current-ASID helper sweep

Stage 9 deletes the three helpers that became test-only in Stage 7 and Stage 8:

| Helper | Action |
|--------|--------|
| `map_user_page_in_current_asid_with_caps` | Deleted from `memory_state.rs` |
| `unmap_user_page_in_current_asid` | Deleted from `memory_state.rs` |
| `is_user_page_mapped_in_current_asid` | Deleted from `memory_state.rs` |

All call sites in `syscall.rs` tests and `boot/tests.rs` migrated to explicit-ASID
variants (`map_user_page_in_asid_with_caps`, `is_user_page_mapped_in_asid`).

### 27.3 VmAnonMapProgressPlan live wiring in handle_vm_anon_map

`handle_vm_anon_map` in `syscall.rs` now constructs a `VmAnonMapProgressPlan`
explicitly, replacing the bare local variables `tid`, `asid`, and `va`. The plan
struct's `progress.mapped_end` field is advanced on each successful page map,
matching the Stage 5D scaffold exactly.

The `#[cfg_attr(not(test), allow(dead_code))]` attributes are removed from:
- `VmAnonMapValidatedArgs`
- `VmPageMapProgress`
- `VmAnonMapProgressPlan`

These structs are now used in production code.

### 27.4 Rollback cap cleanup: revoke_capability_in_cnode before execute_tlb_shootdown_wait_plan

`rollback_anon_map` in `syscall.rs` gains an `unmapped_cap: Option<CapId>` parameter
and two new cap-cleanup steps:

1. **Un-mapped cap revocation** (`unmapped_cap`): If `map_user_page_in_asid_with_caps`
   failed, the MemoryObject cap was allocated but never inserted into the address
   space. `revoke_capability_in_cnode` is called directly (no phase-1 unmap needed).

2. **Mapped-page cap revocation**: For each already-mapped page in `[addr, mapped_end)`,
   after `unmap_page_phase1` returns a `TlbShootdownWaitPlan` (which carries the
   physical address), `find_current_task_cap_for_memory_object_phys` locates the
   MemoryObject cap and `revoke_capability_in_cnode` revokes it. Then
   `execute_tlb_shootdown_wait_plan` completes the two-phase unmap, which now sees
   both `cap_refcount=0` and `map_refcount=0`, and frees the physical frame.

A new helper `find_current_task_cap_for_memory_object_phys` is added to
`capability_lifecycle_state.rs`. It searches the current task's cnode for a
MemoryObject cap whose physical address matches a given `PhysAddr`.

### 27.5 Remaining deferred items

- x86_64 SMP (>1 CPU) — requires lock-free or per-CPU demand-paging path.
- Full global-lock removal.
- RAMFS/FAT runtime server spawning (main branch is healthy; images exist but
  spawning is deferred).
- AArch64 smoke (no data available, not claimed).

### 27.6 Files changed (Stage 9)

| File | Change |
|------|--------|
| `src/kernel/boot/capability_lifecycle_state.rs` | Added `find_current_task_cap_for_memory_object_phys` |
| `src/kernel/boot/memory_state.rs` | Deleted `map_user_page_in_current_asid_with_caps`, `unmap_user_page_in_current_asid`, `is_user_page_mapped_in_current_asid` |
| `src/kernel/boot/mod.rs` | Removed `#[cfg_attr(not(test), allow(dead_code))]` from `VmAnonMapValidatedArgs`, `VmPageMapProgress`, `VmAnonMapProgressPlan`; updated docstring |
| `src/kernel/boot/tests.rs` | Migrated all current-ASID test calls; 5 new Stage 9 tests; test renamed |
| `src/kernel/syscall.rs` | `rollback_anon_map` gains `unmapped_cap` param + cap cleanup; `handle_vm_anon_map` wires `VmAnonMapProgressPlan`; test helpers migrated |
| `doc/KERNEL_LOCKING.md` | This section (§27) |
| `doc/KERNEL_TEST_RULES.md` | Rule N+16 (Stage 9 test rules) |

---

## §27 Stage 9: current-ASID helper deletion, VmAnonMapProgressPlan live-wiring, rollback cap cleanup

### 27.1 Stage 8 smoke acceptance

**x86_64 -smp 1 smoke:** Stage 8 commit `c376e6e` passes with ENTER_USER=3,
all services READY=1, fallback=0, fatal=0, oom=0. Accepted.

### 27.2 Current-ASID helper deletion

All three current-ASID helpers are deleted from `memory_state.rs`:

| Helper | Stage introduced | Deleted in |
|--------|-----------------|------------|
| `map_user_page_in_current_asid_with_caps` | Stage 5 | Stage 9 |
| `unmap_user_page_in_current_asid` | Stage 5 | Stage 9 |
| `is_user_page_mapped_in_current_asid` | Stage 5 | Stage 9 |

All test callers migrated to explicit-ASID equivalents:
- `map_user_page_in_asid_with_caps(asid, cap, virt, flags)` (5 test sites in tests.rs + 1 in syscall.rs)
- `is_user_page_mapped_in_asid(asid, virt)` (4 test sites)

The `_current_asid` helpers were wrappers that read `current_tid()` +
`task_asid(tid)` on every call — semantically correct under the global lock,
but structurally inconsistent with the plan-first pattern. With zero callers
outside tests, and all test callers migrated, deletion is safe.

### 27.3 VmAnonMapProgressPlan live wiring

`handle_vm_anon_map` now constructs a `VmAnonMapProgressPlan` before the map
loop, capturing all plan-first fields in one struct:

```
VmAnonMapProgressPlan {
    validated: VmAnonMapValidatedArgs { addr, map_len, end, flags },
    tid,
    asid,
    progress: VmPageMapProgress { base_addr: addr, mapped_end: addr, end_addr: end },
}
```

The loop variable `va` is replaced by `plan.progress.mapped_end`; all rollback
calls pass `plan.progress.base_addr` and `plan.progress.mapped_end` from the
struct. The `#[cfg_attr(not(test), allow(dead_code))]` guard is removed from
`VmAnonMapValidatedArgs`, `VmPageMapProgress`, and `VmAnonMapProgressPlan`.

**Locking sequence (unchanged from Stage 6):**
1. Global lock held throughout.
2. `validate_anon_map_args` — lock-free computation.
3. `current_tid` + `task_asid` — scheduler rank 1 + task rank 2 (plan-first).
4. `alloc_anonymous_memory_object` — memory rank 6.
5. `map_user_page_in_asid_with_caps` — cap rank 4 → vm rank 5 → memory rank 6.
6. On failure: `rollback_anon_map` — see §27.4.

### 27.4 Rollback capability cleanup

`rollback_anon_map` now accepts `unmapped_cap: Option<CapId>` and fully cleans
up MemoryObject capability slots during rollback:

**Case A — map failure** (`unmapped_cap = Some(cap)`):
The cap was allocated by `alloc_anonymous_memory_object` (cap_refcount=1,
map_refcount=0) but `map_user_page_in_asid_with_caps` failed before the page
was inserted into the address space. `revoke_capability_in_cnode` drops
cap_refcount to 0 and calls `reclaim_memory_object_if_unreferenced`, freeing
the frame immediately.

**Case B — alloc failure** (`unmapped_cap = None`):
`alloc_anonymous_memory_object` itself failed; no cap was produced. Nothing
extra to revoke.

**Case C — already-mapped pages** (loop over `[addr, mapped_end)`):
For each page, `unmap_page_phase1` removes the PTE (map_refcount → 0) and
returns the physical address via the `TlbShootdownWaitPlan`. Then
`find_current_task_cap_for_memory_object_phys(phys)` locates the CapId in the
current task's cnode. `revoke_capability_in_cnode` drops cap_refcount to 0;
`reclaim_memory_object_if_unreferenced` sees both refcounts=0 and frees the
frame. `execute_tlb_shootdown_wait_plan` performs the TLB shootdown (or fast
path if no remote CPUs are live on the ASID); the reclaim call inside the wait
plan is now a no-op (slot already freed).

**Safety:** All operations execute under the global lock. No other CPU can
reuse the freed frame between `revoke_capability_in_cnode` and the TLB
shootdown because the global lock prevents any concurrent allocation.

**New helper:** `find_current_task_cap_for_memory_object_phys(phys)` in
`capability_lifecycle_state.rs` — scans `memory_objects` for the MemoryObjectId
whose `phys` matches, then scans the current task's cnode for the CapId whose
`object == CapObject::MemoryObject { id }`. O(MO_slots × cnode_size); safe
because both loops run under the global lock.

### 27.5 Still deferred

- x86_64 SMP (>1 CPU) — requires lock-free or per-CPU demand-paging path.
- Full global-lock removal.
- RAMFS/FAT runtime server spawning.
- AArch64 smoke (no data available, not claimed).

### 27.6 Files changed (Stage 9)

| File | Change |
|------|--------|
| `src/kernel/boot/capability_lifecycle_state.rs` | New `find_current_task_cap_for_memory_object_phys` |
| `src/kernel/boot/memory_state.rs` | Deleted 3 current-ASID helpers |
| `src/kernel/boot/mod.rs` | Removed `dead_code` guards from 3 plan structs; updated `VmAnonMapProgressPlan` comment |
| `src/kernel/syscall.rs` | `rollback_anon_map` + `unmapped_cap` + cap cleanup; `handle_vm_anon_map` wired with `VmAnonMapProgressPlan`; test helpers migrated to explicit-ASID |
| `src/kernel/boot/tests.rs` | 5 new Stage 9 tests; 9 test sites migrated from current-ASID to explicit-ASID; 1 test renamed |
| `doc/KERNEL_LOCKING.md` | This section (§27) |
| `doc/KERNEL_TEST_RULES.md` | Rule N+16 (Stage 9 test rules) |

---

## §28 Stage 10: MemoryObject/cap lifetime audit + VmMap rollback hardening

### 28.1 Stage 9 smoke acceptance

**x86_64 -smp 1 smoke:** Stage 9 commit `61261ed` accepted. Smoke table from
Stage 9 matches Stage 8 baseline (all services READY=1, fallback=0, fatal=0, oom=0).
Stage 9 is accepted for x86_64 -smp 1.

### 28.2 MemoryObject/cap lifetime audit

Every production path touching `memory_objects`, `cap_refcount`, `map_refcount`,
or `pin_refcount` is classified below.

#### Lifetime classes

| Class | Description |
|-------|-------------|
| A | Cap-only lifetime (no page mapping) |
| B | Map-only lifetime (raw/test-only) |
| C | Cap+map paired (anonymous allocation) |
| D | Transfer-envelope pin ownership |
| E | Shared-region active mapping |
| F | COW/fork ownership |
| G | Demand-page ownership |
| H | Rollback path |

#### Path table

| Path | Class | cap_refcount | map_refcount | pin_refcount | Frame reclaim |
|------|-------|-------------|-------------|-------------|--------------|
| `alloc_anonymous_memory_object` | A | +1 on alloc | 0 | 0 | Never |
| `map_user_page_in_asid_with_caps` | C | (unchanged) | +1 per page | 0 | After both=0 |
| `map_user_page_in_asid_raw` (test) | B | (unchanged) | +1 per page | 0 | After both=0 |
| `unmap_user_page_in_asid` (one-phase) | C/E | (unchanged) | -1 per page | 0 | If cap=0 too |
| `unmap_page_phase1` | C/H | (unchanged) | -1 per page | 0 | Deferred |
| `execute_tlb_shootdown_wait_plan` | H | (unchanged) | (unchanged) | (unchanged) | After shootdown |
| `revoke_capability_in_cnode` | A/C | -1 | (unchanged) | 0 | If map=0 too |
| `stash_transfer_envelope` | D | (unchanged) | (unchanged) | +1 if shared | Never |
| `take_transfer_envelope` | D | (unchanged) | (unchanged) | -1 if shared | Never |
| `map_shared_region_into_receiver` | E | (unchanged) | +1 per page | 0 | After cap+map=0 |
| `handle_transfer_release` (phase1+phase2) | E | (unchanged) | -1 per page | 0 | After cap=0 |
| `revoke_active_transfer_mappings_for_cap` | E | -1 at caller | -1 per page | 0 | After cap=0 |
| `purge_active_transfer_mappings_for_pid` | E | -1 at line 249 | -1 per page | 0 | After cap=0 |
| COW swap (`try_cow_page` / `map_cow_page`) | F | (unchanged) | -1 old +1 new | 0 | If old unreferenced |
| Demand-page (`try_handle_demand_page_fault`) | G | +1 | +1 | 0 | Never (success) |
| `rollback_anon_map` (Stage 9) | H | -1 per page | -1 per page | 0 | After both=0 |
| `handle_vm_map` rollback (Stage 10) | H | -1 per page | -1 per page | 0 | After both=0 |

#### Key invariants

1. **Reclaim condition**: Frame is freed exactly when `cap_refcount == 0 && map_refcount == 0 && pin_refcount == 0`.
2. **TLB before reclaim**: For pages involved in TLB shootdown, reclaim MUST happen after `execute_tlb_shootdown_wait_plan`. Under the global lock this is trivially satisfied.
3. **Cap before map**: In the anonymous path, `cap_refcount` is decremented (revoke) BEFORE TLB shootdown completes; the frame is freed by `reclaim_memory_object_if_unreferenced` inside the revoke call only if `map_refcount` is already 0. If `map_refcount` is still 1, `reclaim_memory_object_if_unreferenced` is a no-op and the frame remains live until phase-2 unmap clears `map_refcount`.
4. **Shared-region ordering**: `purge_active_transfer_mappings_for_pid` and `revoke_capability_direct_in_process_cnode` both call `revoke_active_transfer_mappings_for_cap` BEFORE decrementing `cap_refcount`. This is safe: the one-phase unmap decrements `map_refcount`, and `reclaim_memory_object_for_phys` (inside unmap) is a no-op because `cap_refcount` is still 1. Cap decrement happens next, then `reclaim_memory_object_if_unreferenced` frees the frame.

### 28.3 IPC shared-memory forward-map audit (Part 2)

`map_shared_region_into_receiver` was already converted to plan-first explicit-ASID
in Stage 7. No further conversion is needed. The path is correct:

- Plan-first ASID: resolved from `current_tid` + `task_asid` before the map loop.
- Map loop: `map_user_page_in_asid_with_caps(asid, receiver_mem_cap, ...)` — one cap, multiple pages.
- Rollback on partial failure: `unmap_page_phase1` + `execute_tlb_shootdown_wait_plan` for `[requested_va, va)`.
- Cap revoke: `revoke_current_transfer_cap_best_effort(kernel, transfer_cap)` in the outer error handler.
- `register_active_transfer_mapping` failure also rolls back with the same two-phase pattern.

**The shared-region cap is NOT revoked inside `map_shared_region_into_receiver` rollback** — by design: the rollback only unmaps the shared pages; the transfer cap revoke happens in the outer error handler, which properly decrements `cap_refcount` and triggers `reclaim_memory_object_if_unreferenced`.

### 28.4 VmMap rollback hardening (Stage 10 live change)

`handle_vm_map` had a TODO: on partial failure, already-mapped pages remained with
no rollback, and the corresponding caps leaked until task exit.

**Stage 10 fix:**
- Switched map loop from `map_user_page_with_caps(aspace_map_cap, ...)` (re-resolves cap
  on every page) to `map_user_page_in_asid_with_caps(map_asid, ...)` using the ASID
  already resolved plan-first from `aspace_map_cap`.
- Added `mapped_end` progress counter.
- On alloc failure: `rollback_anon_map(kernel, map_asid, addr, mapped_end, None)`.
- On map failure: `rollback_anon_map(kernel, map_asid, addr, mapped_end, Some(mem_cap))`.
- `rollback_anon_map` is reused directly: anonymous memory is always allocated in the
  current task's cnode regardless of which address space it is mapped into, so
  `find_current_task_cap_for_memory_object_phys` locates the cap correctly.

**Locking sequence (unchanged from Stage 7):**
1. Global lock held throughout.
2. `validate_anon_map_args` — lock-free.
3. Resolve `map_asid` from `aspace_map_cap` — capability rank 4 read.
4. Stack guard check — vm rank 5 read.
5. Per page: `alloc_anonymous_memory_object` (memory rank 6), then `map_user_page_in_asid_with_caps` (cap rank 4 → vm rank 5 → memory rank 6).
6. On failure: `rollback_anon_map` — cap rank 4 + vm rank 5 + memory rank 6 (via phase-1 + revoke + phase-2).

### 28.5 Still deferred

- x86_64 SMP (>1 CPU) — requires lock-free or per-CPU demand-paging path.
- Full global-lock removal.
- RAMFS/FAT runtime server spawning (main branch healthy; spawning TODO).
- `purge_active_transfer_mappings_for_pid` / `revoke_active_transfer_mappings_for_cap` — still use one-phase `unmap_user_page_in_asid`. Safe under global lock; two-phase conversion deferred to global-lock-removal stage.
- COW/fork MemoryObject lifetime (separate complexity domain).
- AArch64 smoke (no data available).
- x86_64 SMP smoke (deferred).

### 28.6 Files changed (Stage 10)

| File | Change |
|------|--------|
| `src/kernel/syscall.rs` | `handle_vm_map`: explicit-ASID map loop + rollback via `rollback_anon_map` |
| `src/kernel/boot/tests.rs` | 7 new Stage 10 tests (VmMap refcounts, non-current ASID, MemoryObject invariants, rollback) |
| `doc/KERNEL_LOCKING.md` | This section (§28): Stage 9 acceptance, full audit table, IPC/shared-region audit, VmMap fix |
| `doc/KERNEL_TEST_RULES.md` | Rule N+17 (Stage 10 test rules) |

---

## §29 Stage 11: Two-phase unmap for active transfer cleanup

### 29.1 Scope

Stage 11 converts `purge_active_transfer_mappings_for_pid` and
`revoke_active_transfer_mappings_for_cap` from one-phase `unmap_user_page_in_asid`
to two-phase `unmap_page_phase1` + `execute_tlb_shootdown_wait_plan`, completing
the two-phase migration for all VM-mutating paths.

### 29.2 Changed paths

| Function | Location | Change |
|----------|----------|--------|
| `purge_active_transfer_mappings_for_pid` | `cnode_state.rs` | Per-mapping loop replaced with `unmap_range_two_phase(asid, base, len)` |
| `revoke_active_transfer_mappings_for_cap` | `capability_lifecycle_state.rs` | Same: `unmap_range_two_phase(asid, base, len)` |
| `unmap_range_two_phase` | `memory_state.rs` | New helper: phase-1 loop over pages, then phase-2 per page |

### 29.3 Invariants preserved

- **map_refcount**: decremented by `unmap_page_phase1` via `note_mapping_removed`
  before the TLB shootdown.  Reclaim fires inside `execute_tlb_shootdown_wait_plan`
  only when all three refcounts (cap, map, pin) are zero at that point.
- **cap_refcount**: NOT decremented inside `revoke_active_transfer_mappings_for_cap`.
  The caller (`revoke_capability_in_cnode` or `revoke_capability_direct_in_process_cnode`)
  decrements cap_refcount **after** this function returns, then calls
  `reclaim_memory_object_if_unreferenced` for the final reclaim.
- **Absent pages**: `unmap_range_two_phase` silently skips pages for which
  `unmap_page_phase1` returns `Ok(None)` (PTE not present — demand paging never
  faulted the page in).
- **Active transfer semantics**: unchanged — the active mapping record is still cleared
  before `revoke_capability_in_cnode` is called inside the purge path.

### 29.4 Locking sequence

Both callers hold the global lock throughout (single-CPU hosted-dev environment).
Domain rank sequence for `unmap_range_two_phase`:
1. vm rank 5 read (PTE lookup in `unmap_page_phase1`)
2. memory rank 6 write (`note_mapping_removed` decrement)
3. No cross-CPU TLB invalidation in hosted-dev (stub shootdown)
4. memory rank 6 write (`reclaim_memory_object_for_phys` if unreferenced)

### 29.5 Stage 10 acceptance

Stage 10 (commit `b550353`) accepted — 647 tests pass (single-threaded), all
MemoryObject cap/map/pin refcount invariants validated by 7 new tests.

### 29.6 Still deferred

- x86_64 SMP (>1 CPU) — requires lock-free or per-CPU demand-paging path.
- Full global-lock removal.
- COW/fork MemoryObject lifetime.
- AArch64 smoke.
- x86_64 SMP smoke (requested for Stage 10+11 together).

### 29.7 Files changed (Stage 11)

| File | Change |
|------|--------|
| `src/kernel/boot/memory_state.rs` | `unmap_range_two_phase` helper added |
| `src/kernel/boot/cnode_state.rs` | `purge_active_transfer_mappings_for_pid`: one-phase → `unmap_range_two_phase` |
| `src/kernel/boot/capability_lifecycle_state.rs` | `revoke_active_transfer_mappings_for_cap`: one-phase → `unmap_range_two_phase` |
| `src/kernel/boot/tests.rs` | 5 new Stage 11 tests |
| `doc/KERNEL_LOCKING.md` | This section (§29): Stage 10 acceptance, Stage 11 audit |
| `doc/KERNEL_TEST_RULES.md` | Rule N+18 (Stage 11 test rules) |

---

## §30 — Stage 12: COW/fork MemoryObject lifetime mega-pass

### 30.1 Motivation

Stage 11 validated active-transfer two-phase unmap. Stage 12 audits all
COW/fork code paths that affect MemoryObject refcounts (cap_refcount,
map_refcount). No existing behaviors are weakened; only latent bugs are fixed.

### 30.2 Smoke acceptance (Stage 10 + Stage 11, -smp 1)

x86_64 -smp 1 smoke requested for the combined Stage 10+11 changes.
Expected markers (from user-provided values):

| Marker | Value |
|--------|-------|
| `X86_BOOTSTRAP_TIMER_IRQ_EOI_ONLY` | 0 |
| `X86_BOOTSTRAP_SCHEDULER_READY` | 1 |
| `X86_BOOTSTRAP_TIMER_STARTED` | 1 |
| `ENTER_USER` | 3 |
| `STARTUP_INSTALL_FINAL` | 9 |
| `PM_ELF_ZC_DONE` total | 3 |
| ZC nonzero pages | 3 |
| `PM_ELF_ZC_FAIL` | 0 |
| `INITRAMFS_SRV_ENTRY` | 1 |
| `DEVFS_SRV_ENTRY` | 1 |
| `VFS_SRV_ENTRY` | 1 |
| `DRIVER_MANAGER_READY` | 1 |
| `BLKCACHE_SRV_READY` | 1 |
| `VIRTIO_BLK_SRV_READY` | 1 |
| fallback | 0 |
| TID mismatch | 0 |
| fatal-ish | 0 |
| oom/capacity/Vm(Full) | 0 |
| cap/refcount suspicious | 0 |

### 30.3 COW/fork audit findings and fixes

| Bug | Location | Severity | Fix |
|-----|----------|----------|-----|
| Parent pages left read-only after failed clone (COW record not recorded) | `clone_user_address_space_cow` | CRASH | Track `wp_parent_virts: Vec<VirtAddr>` before `mark_cow_page`; call `restore_parent_write_permissions` on any error |
| `child_asid` leaked when post-clone step fails | `fork_user_process_cow` | LEAK | Wrap post-clone steps in `fork_complete_post_clone`; `destroy_user_address_space_by_asid(child_asid)` on error |
| `try_handle_cow_fault` skips page-content copy | `try_handle_cow_fault` | bare-metal only | Documented with TODO comment; not a bug in hosted-dev (content keyed by virt_addr) |

### 30.4 Refcount transition table (MemoryObject shared during fork)

| Event | cap_refcount | map_refcount |
|-------|-------------|-------------|
| `alloc_anonymous_memory_object` | 1 | 0 |
| `map_user_page_in_asid_with_caps` | 1 | 1 |
| `fork_user_process_cow` (clone asid) | 1 | 2 |
| `fork_user_process_cow` (inherit caps) | 2 | 2 |
| child COW write fault | 2 | 1 (child unmaps shared; maps private) |
| parent COW write fault | 2 | 0 (parent unmaps shared; maps private) |
| child revokes inherited cap | 1 | 0 |
| parent revokes cap | 0 | 0 → frame reclaimed |

### 30.5 `restore_parent_write_permissions` safety

`map_user_page_in_asid_raw(parent_asid, virt, {same_phys, write=true})`:
- Removes old read-only PTE → `note_mapping_removed` (map_refcount−1)
- Inserts write-enabled PTE → `note_mapping_inserted` (map_refcount+1)
- Net: map_refcount unchanged
- `clear_cow_page` called automatically (flags.write=true): COW record removed
- cap_refcount unaffected (no cap minting/revoking)

### 30.6 New helpers

| Helper | File | Purpose |
|--------|------|---------|
| `restore_parent_write_permissions` | `memory_state.rs` | Rollback write-protect on clone failure |
| `fork_complete_post_clone` | `thread_state.rs` | All post-clone steps; caller destroys child_asid on error |

### 30.7 Stage 12 acceptance

663 tests pass single-threaded (`cargo test --lib -- --test-threads=1`).
11 new COW/fork lifetime tests (tids 41-51) added.

#### Stage 12 x86_64 -smp 1 smoke (accepted)

| Marker | Value |
|--------|-------|
| `X86_BOOTSTRAP_TIMER_IRQ_EOI_ONLY` | 0 |
| `X86_BOOTSTRAP_SCHEDULER_READY` | 1 |
| `X86_BOOTSTRAP_TIMER_STARTED` | 1 |
| `ENTER_USER` | 3 |
| `STARTUP_INSTALL_FINAL` | 9 |
| `PM_ELF_ZC_DONE` total | 3 |
| ZC nonzero pages | 3 |
| `PM_ELF_ZC_FAIL` | 0 |
| `INITRAMFS_SRV_ENTRY` | 1 |
| `DEVFS_SRV_ENTRY` | 1 |
| `VFS_SRV_ENTRY` | 1 |
| `DRIVER_MANAGER_READY` | 1 |
| `BLKCACHE_SRV_READY` | 1 |
| `VIRTIO_BLK_SRV_READY` | 1 |
| fallback | 0 |
| TID mismatch | 0 |
| fatal-ish | 0 |
| oom/capacity/Vm(Full) | 0 |
| cap/refcount suspicious | 0 |

### 30.8 Files changed (Stage 12)

| File | Change |
|------|--------|
| `src/kernel/boot/memory_state.rs` | `clone_user_address_space_cow` rollback fix; `restore_parent_write_permissions` helper; bare-metal TODO in `try_handle_cow_fault` |
| `src/kernel/boot/thread_state.rs` | `fork_user_process_cow` ASID leak fix; `fork_complete_post_clone` helper; imports |
| `src/kernel/boot/tests.rs` | 11 new Stage 12 COW/fork lifetime tests; task-switch fix (yield_current_to before alloc) |
| `doc/KERNEL_LOCKING.md` | This section (§30) |
| `doc/KERNEL_TEST_RULES.md` | Rule N+19 (Stage 12 test rules) |

## §31 — Stage 13: COW content copy + COW table scalability

### 31.1 Motivation

Stage 12 fixed COW/fork refcount lifetime bugs and rollback. Stage 13 completes
the COW pass: (1) bare-metal content copy on COW fault was a documented TODO;
(2) the hosted-dev clone copy used virtual addresses as UserMemoryStore keys
instead of physical addresses; (3) the COW table was a fixed-size global array
that one process could exhaust for all others.

### 31.2 Bug inventory

| Bug | Location | Severity | Fix |
|-----|----------|----------|-----|
| `try_handle_cow_fault` had no content copy | `memory_state.rs` | SILENT DATA LOSS (bare-metal) | Add `copy_frame_contents_for_cow`; wire before remap |
| `clone_user_address_space_cow` hosted-dev copy used `virt` key | `memory_state.rs:368` | SILENT DATA LOSS (hosted-dev) | Use `mapping.phys` key |
| `try_handle_cow_fault` leaked new frame on remap failure | `memory_state.rs` | LEAK | Revoke `new_mem_cap` on all error paths |
| Global fixed-size `[Option<CowPageRecord>; MAX_COW_PAGES]` | `defs.rs` | EXHAUSTION | Replace with `Vec<CowPageRecord>` |

### 31.3 `copy_frame_contents_for_cow`

New private helper in `memory_state.rs`:

```
fn copy_frame_contents_for_cow(&mut self, asid, old_phys, new_phys) -> Result<(), KernelError>
```

- **hosted-dev**: copies `(asid, old_phys+offset) → (asid, new_phys+offset)` for offset in `0..PAGE_SIZE`
  using the `UserMemoryStore` BTreeMap.
- **bare-metal**: `phys_to_direct_map_ptr(old_phys)` → `copy_nonoverlapping PAGE_SIZE bytes`
  to `phys_to_direct_map_ptr(new_phys)`.

`phys_to_direct_map_ptr` visibility lifted to `pub(super)` to allow cross-module use within
the `boot` module.

### 31.4 COW table → dynamic Vec

| Before | After |
|--------|-------|
| `KernelStorage<[Option<CowPageRecord>; MAX_COW_PAGES]>` | `Vec<CowPageRecord>` |
| `mark_cow_page`: linear scan for `None` slot, `Err(MemoryObjectFull)` if full | `push` after dedup check; `#[cfg(test)]` capacity limit for exhaustion tests |
| `clear_cow_page`: iterate setting matching entries to `None` | `retain(|e| !(asid && virt match))` |
| `clear_cow_pages_for_asid`: iterate setting asid entries to `None` | `retain(|e| e.asid != asid)` |
| `is_cow_page`: `iter().flatten().any(...)` | `iter().any(...)` |
| `MAX_COW_PAGES` constant (100/256) | Removed |

The `#[cfg(test)]` field `cow_page_capacity_limit: Option<usize>` in `MemorySubsystem`
allows tests to simulate exhaustion without a production hard cap. Default is `None`
(unlimited). Tests set it to a small value (e.g. `Some(5)`) to trigger `Err(MemoryObjectFull)`
from `mark_cow_page`.

### 31.5 Refcount transition table (updated, no changes from Stage 12)

Table unchanged — Vec switch does not affect refcount semantics.

### 31.6 Stage 13 acceptance

667 tests pass single-threaded (`cargo test --lib -- --test-threads=1`).
4 new Stage 13 tests added (content correctness × 2, Vec scalability × 2).
`cargo check --no-default-features` clean.
x86_64 -smp 1 smoke required for Stage 13 (live COW fault content-copy behavior changed).

### 31.7 Files changed (Stage 13)

| File | Change |
|------|--------|
| `src/kernel/boot/defs.rs` | `cow_pages` field type → `Vec<CowPageRecord>`; `#[cfg(test)] cow_page_capacity_limit` |
| `src/kernel/boot/bootstrap_state.rs` | Init `cow_pages: Vec::new()`; `#[cfg(test)] cow_page_capacity_limit: None` |
| `src/kernel/boot/mod.rs` | Remove `MAX_COW_PAGES` constants |
| `src/kernel/boot/memory_state.rs` | 4 COW functions updated; `copy_frame_contents_for_cow` added; `try_handle_cow_fault` content copy + rollback; clone copy key fix |
| `src/kernel/boot/user_memory_state.rs` | `phys_to_direct_map_ptr` visibility → `pub(super)` |
| `src/kernel/boot/tests.rs` | 2 exhaustion tests redesigned (capacity limit field); `.iter().flatten()` → `.iter()`; 4 new Stage 13 tests |
| `doc/KERNEL_LOCKING.md` | §30.7 smoke acceptance table; this section (§31) |
| `doc/KERNEL_TEST_RULES.md` | Rule N+20 (Stage 13 test rules) |
