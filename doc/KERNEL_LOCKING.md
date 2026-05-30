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

#### What Stage 4J does NOT change

- IPC syscall ABI and SYSCALL_COUNT: unchanged.
- SpawnV5, MemoryObject zero-copy, VFS, syscall 27, VFS_READ_SHARED_REPLY_ENABLED: untouched.
- x86_64 SMP and `src/arch/x86_64/smp.rs`: untouched.
- x86_64 register writeback: unchanged.
- Phase 3B checks: not weakened.
- The global `SharedKernel` lock is still retained for all other IPC mutation.
  **This is not full global-lock removal.**

---

### Deferred IPC split paths (analysis only — not implemented)

The following send/receive cases were audited and are explicitly deferred. They
cannot be split without violating the hard invariants on `ipc_state_lock` scope.
They fall back to the existing full IPC paths via `Ineligible(...)`.

#### Synchronous endpoint send-to-receiver (deferred)

**Blocker**: `ipc_send_with_optional_deadline` for `EndpointMode::Synchronous` calls
`switch_to_runnable_tid(waiter_tid)` (`src/kernel/boot/task_core_state.rs:7`).
`switch_to_runnable_tid` busy-loops calling `yield_current()` until
`current_tid_on(cpu) == waiter_tid` — a scheduler context switch that:
- mutates scheduler runqueues and TCB run-state
- may preempt the current thread
- cannot be decomposed into a lock-free pre-check + deferred wake

Any split-path version would need to clear the receiver's endpoint waiter slot
under `ipc_state_lock` before calling `switch_to_runnable_tid`. If the scheduler
handoff then fails or races, the waiter is orphaned with no recovery path.

**Decision**: deferred indefinitely. Synchronous sends remain on the full path.
The Stage 4F buffered-path split intentionally excludes non-buffered endpoints
(`Ineligible(NonBufferedEndpoint)`).

#### Recv-v2 blocked delivery (deferred)

**Blocker**: `complete_blocked_recv_for_waiter` performs user-memory copy, capability
materialization (cap minting), and `TrapFrame` register writes — none of which may
occur under `ipc_state_lock`.

A split version would require:
- Phase 1 (under `ipc_state_lock`): clear `endpoint_waiters[endpoint_idx]` and
  snapshot receiver `BlockedRecvState`.
- Phase 2 (outside lock): call `complete_blocked_recv_for_waiter`.

**Problem**: if Phase 2 fails (e.g. `UserMemoryFault`), the receiver's waiter slot
is already cleared under `ipc_state_lock`. There is no safe way to re-register
the receiver as a waiter post-failure: the endpoint queue may have changed, the
slot may have been reused, and re-registering requires acquiring `ipc_state_lock`
again with the existing state unknown. The full path avoids this by never clearing
the waiter until delivery succeeds.

**Decision**: deferred. Recv-v2 blocked delivery remains on the full path.
`is_task_recv_v2_blocked` check in Stage 4F correctly rejects these cases
(`Ineligible(SenderWaiterPresent)` or full-path fallback).

#### Cap-transfer / call / reply sends (deferred)

**Blocker**: these paths require capability minting, reply-cap materialization,
transfer-envelope allocation, and/or `TrapFrame` writes — all forbidden under
`ipc_state_lock`. The `FLAG_CAP_TRANSFER`, `FLAG_CAP_TRANSFER_PLAIN`, and
`FLAG_REPLY_CAP` message flag checks in `ipc_try_recv_queued_plain_endpoint_only`
and `ipc_try_send_queued_plain_endpoint_only` correctly reject these messages
and return `Ineligible(TransferOrReplyCapMessage)`.

**Decision**: deferred indefinitely. Cap-transfer, call, and reply paths remain
on the full IPC path.

---

### Stage 3: remove global lock from syscall fast path


- Route trap/syscall dispatch directly to subsystem locks where safe.
- Keep global lock only for coarse-grain control-plane operations, if needed.

### Stage 4: per-CPU scheduler/runqueue locking

- Move scheduler queues and CPU-local runnable state to per-CPU lock domains.
- Retain cross-CPU coordination only for explicit migration/work-queue paths.

## 6) Scope note

This document is audit/design-only and does not change runtime lock behavior.
