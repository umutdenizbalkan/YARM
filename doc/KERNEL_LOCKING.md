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
