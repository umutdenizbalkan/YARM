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

## 0) Stage 185 (GLOBAL-LOCK-RETIRE) — status and honest finding

Stage 185 inventoried every global-lock site and its finding is recorded here so
later work is not misled: **the global `SpinLock<KernelState>` is still the
authoritative live-runtime serialization boundary and was NOT retired from live
runtime in this stage.** Retiring it from all live syscall/IPC/scheduler/
capability/VM/fault paths means converting the whole
`with_cpu → handle_trap → syscall::dispatch` model to per-subsystem locks with
rank ordering — the multi-stage rewrite this document's *Current status*
disclaims. Stage 185 is explicitly **not a rewrite stage**, so it did not perform
that conversion. What it did do:

- **Inventory (classified).**
  - *Live runtime (by design, retained):* `SharedKernel::with` / `with_cpu`
    (`src/runtime.rs`, ~94 `with` + ~20 `with_cpu` acquisition sites; the
    `runtime.rs` methods are the global-lock-wrapped kernel API). This is the
    authoritative serialization for the accepted single-dispatcher model
    (x86_64 `-smp 1`; AArch64/RISC-V `in_lock_single_dispatcher`). NOT an
    obsolete crutch.
  - *Relocated out-of-lock seams (live, already proven + guarded):* the D2
    recv/send and D6 dispatch/switch seams run with the global lock dropped via
    `DISPATCH_SWITCH_PLAN_STASH` + `GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE`
    (`boot/exec_state.rs`, `boot/mod.rs`, `arch/*/trap.rs`, `arch/trap_entry.rs`).
    Guarded by the Stage 116/117 `D6_GLOBAL_LOCK_DROP_*` tests.
  - *Boot-only escape (confined + guarded):* `SharedKernel::borrow_kernel_for_boot`
    (returns a raw `&mut KernelState` outside any `with` closure) is
    `pub(crate) unsafe` and called from exactly two sites —
    `arch/x86_64/boot.rs` and `arch/aarch64/boot.rs` — during bootstrap ELF load
    on the boot CPU, with the timer ISR quiesced and `BootRawKernelBorrowGuard`
    tracking the aliasing window (Stage 30 tests). Stage 185 adds
    `stage185_boot_only_global_borrow_confined` guards that fail if this escape
    is loosened to `pub`, loses `unsafe`, or appears on any live-runtime dispatch
    file.
  - *Obsolete fallbacks:* none remain to remove — Stage 182 (REMOVE-FALLBACKS)
    already deleted them and the codebase forbids dead "just in case" fallback
    branches (see `orchestrator_state.rs`).
- **Non-goals (unchanged by Stage 185):** not a syscall ABI change; not a
  userspace ABI change; not multi-dispatcher AP user scheduling (full retirement
  is coupled to that later work); not RPi5 / DRS / SUP-L7H.

Full live-runtime retirement remains future multi-stage work; it must be done
per subsystem, each increment independently validated on the full QEMU matrix,
preserving the Stage 181–184 markers and the capability/lock-rank invariants
below.

### 0.1) Stage 186A (SPLIT-MUT-INFRA) — per-domain split-mut seam set completed

Stage 186A is **infrastructure only** — no live syscall/op was migrated. It adds
the one missing per-domain split-mut seam so later vertical slices have the full
set to build on. A split-mut seam exposes ONLY `&mut <Subsystem>` (never a broad
`&mut KernelState`) under the subsystem's own `SpinLockIrq`, derived via raw field
projectors (`*_split_mut_ptrs_from_raw`) so no whole-`KernelState` reference is
formed; each is `M2_SEAM_HELPER_ONLY` until a live path is genuinely moved onto it.

| Rank | Domain | Projector | `SharedKernel` seam | Since | Live callers |
|------|--------|-----------|---------------------|-------|--------------|
| 1 | scheduler | `scheduler_split_mut_ptr_from_raw` | `with_scheduler_split_mut` | 108 | D6 dispatch (graduated) |
| 2 | task/TCB | `task_split_mut_ptrs_from_raw` | `with_task_tcbs_split_mut` | 108 | VmBrk-shrink split |
| 3 | IPC | `ipc_split_mut_ptrs_from_raw` | `with_ipc_split_mut` | 115 | none (helper-only) |
| **4** | **capability/cnode** | **`capability_split_mut_ptrs_from_raw`** | **`with_capability_state_split_mut`** | **186A** | **none (helper-only)** |
| 5 | VM/user-spaces | `vm_split_mut_ptrs_from_raw` | `with_vm_user_spaces_split_mut` | 108 | VmBrk-shrink split |
| 6 | memory/frames | `memory_split_mut_ptrs_from_raw` | `with_memory_split_mut` | 108 | VmBrk-shrink split |

The rank-4 capability seam is the Stage 186A deliverable; ranks 1/2/3/5/6 predate
it. The **rank order is strictly ascending** (`doc/CAPABILITY_MODEL.md §3`): a path
needing IPC (rank 3) and capability (rank 4) acquires IPC first and drops it before
the capability seam — i.e. **no cap materialization under `ipc_state_lock`**, the
two-phase invariant these seams exist to enable. Guarded by
`stage186a_capability_split_mut_infra`. This is **not** global-lock retirement: the
`with`/`with_cpu` boundary in §1 remains authoritative for every live path.

### 0.2) Stage 186E-prereq (VM-USER-COPY-SEAM) — seam-based user-memory copy

Built on the rank-5 (VM) + rank-6 (memory) seams, `SharedKernel` gains
`validate_user_access_for_asid_split`, `copy_from_user_split`, and
`copy_to_user_split` (in `boot/user_memory_state.rs`) — seam mirrors of the legacy
`KernelState::copy_to_user`/`copy_from_user`. They take ONLY the VM (rank 5) +
memory (rank 6) locks, **never** IPC (3) / capability (4) / task (2) / scheduler (1),
and **never** a broad `&mut KernelState`. Like the legacy path they perform **no COW
fault-in** — a non-writable/unmapped target returns `UserMemoryFault` (byte-identical
errors). `M2_SEAM_HELPER_ONLY`: not wired into any live path. This is the user-copy
prerequisite for a future `ipc_reply` vertical conversion; the cap-transfer
materialization engine remains a separate (not-yet-built) seam blocker. Guarded by
`stage186e_vm_user_copy_seam`. Not global-lock retirement.

### 0.3) Stage 186D-prereq (CAP-TRANSFER-ENGINE-SEAM) — HARD-STOPPED

The second `ipc_reply` blocker — a cap-transfer materialization seam via the rank-4
`with_capability_state_split_mut` seam — was audited and **hard-stopped**: the
materialize path is not cap-only. A single "materialize a received transfer/reply
cap" spans task (2), IPC (3), capability (4), and memory (6): `task_cnode` fuses
task+capability (`with_task_then_capability`); `capability_object_live` reads IPC
generations for endpoint/notification objects; `mint_capability_in_cnode` installs
the cnode slot (rank 4) **and** bumps the memory-object `cap_refcount` (rank 6) in the
**same** critical section (splitting opens a reclaim race); and the reply arm records
the waiter cap under IPC (rank 3) after the rank-4 mint (rank inversion). The rank-4
capability seam hands out only `&mut CapabilitySubsystem`, so it cannot express any of
these cross-subsystem steps. Disposition `CAP_TRANSFER_SEAM_DEFERRED` — documented,
never emitted on a legacy path. No runtime change. Pinned by
`stage186d_cap_transfer_engine_seam_entanglement`. The real next move is a joint
capability↔memory decomposition giving the mint+refcount a shared atomicity discipline.

### 0.4) Stage 186D-proper (CAPABILITY-MEMORY-MINT-ATOMICITY) — atomic cap↔memory mint

Removes the mint/refcount atomicity blocker as seam-only infrastructure. `SharedKernel`
gains `mint_capability_with_memory_ref_split` (in `boot/cap_memory_mint_split.rs`), which
mints a capability into an existing cnode while keeping the referenced memory-object's
`cap_refcount` and the published cnode slot mutually consistent. **Model A —
pre-bump then install:** (1) rank-6 `with_memory_split_mut` validates object liveness and
bumps `cap_refcount` so the object is protected *before* any slot references it; (2) rank-4
`with_capability_state_split_mut` publishes the slot with a fresh receiver-local `CapId`;
(3) if publish fails (`CapabilityFull` / absent space → `TaskMissing`), the refcount bump is
rolled back — no leak. The two critical sections are disjoint (Phase 1 releases the memory
lock before Phase 2 takes the capability lock), so despite rank 6 preceding rank 4 the helper
holds only one subsystem lock at a time → deadlock-free; it never forms a broad
`&mut KernelState` and never takes `ipc_state_lock` (no cap materialization under IPC, no
cap→IPC rank inversion). Real errors (`StaleCapability`/`CapabilityFull`/`TaskMissing`) are
never hidden. `M2_SEAM_HELPER_ONLY`: not wired into any live path — it does not by itself
convert `ipc_reply` or retire the global lock, and does not solve the reply-cap IPC
rank-inversion blocker. Guarded by `stage186d_proper_cap_memory_mint_atomicity`. This is the
atomic-mint prerequisite a future cap-transfer materialization seam is built on top of.

### 0.5) Stage 186D2 (CAP-TRANSFER-MATERIALIZATION-SEAM-FIRST-SLICE)

First seam-based cap-transfer materialization, built on §0.4's atomic mint. `SharedKernel`
gains `materialize_received_cap_snapshot_split` and
`materialize_received_message_cap_routed_split` (in `boot/cap_transfer_materialize_split.rs`),
which take a plain IPC-lock-free `TransferCapSnapshot { receiver_cnode, object, rights }`
(captured after the transfer envelope was consumed under `ipc_state_lock`) and mint an
ordinary object cap into the receiver's cnode via `mint_capability_with_memory_ref_split` —
rank-4 capability seam + rank-6 memory seam only, **never** `ipc_state_lock`, never a broad
`&mut KernelState`, no cap→IPC rank inversion. The snapshot carries object + rights, never a
sender-local `CapId` (local CapIds are not transferable authority); the receiver-local CapId
is freshly minted. Reply objects route to an explicit `DeferredReplyCap`
(`reply_cap_ipc_rank_inversion`) — never faked. Real errors
(`StaleCapability`/`CapabilityFull`/`TaskMissing`) preserved; `WrongObject`/`MissingRight`
are upstream. `M2_SEAM_HELPER_ONLY`: not wired live, and **not yet a live-equivalent of the
legacy grant** — it does not yet record the source→dest delegation link (a rank-4 follow-on),
so it must not be wired into live delivery until that lands. It does not by itself convert
`ipc_reply` or retire the global lock. Guarded by
`stage186d2_cap_transfer_materialize_seam_first_slice`.

### 0.6) Stage 186D3 (CAP-TRANSFER-DELEGATION-LINK-SEAM)

Makes ordinary cap-transfer materialization seam **live-equivalent** by adding the
sender→receiver delegation-link recording the legacy grant performs (so revoking a source cap
propagates to the derived receiver cap). The delegation link is pure capability-domain (rank 4)
metadata (`delegated_capability_links`), so `SharedKernel::record_cap_delegation_link_split`
(in `boot/cap_transfer_delegation_split.rs`) records it via the rank-4 capability seam only —
no IPC/task/memory lock. `materialize_received_cap_snapshot_with_delegation_split` mints via the
Stage 186D2 seam (atomic mint), records the link (when `source_tid != dest_tid`), and on record
failure rolls the mint back via `rollback_minted_cap_split` (in `boot/cap_memory_mint_split.rs`):
clear the receiver cnode slot (rank 4) THEN drop `cap_refcount` + reclaim (rank 6) — teardown
mirrors the mint's install order, so no live slot references a dropped-refcount object (no
reclaim race, no stale slot, no stale delegation edge). Never `ipc_state_lock`, never a broad
`&mut KernelState`, no cap→IPC rank inversion. The delegation carries `source_cap` as a recorded
edge only — never resolved-to-mint / receiver authority. Reply objects stay `DeferredReplyCap`
(`reply_cap_ipc_rank_inversion`), never delegated. `M2_SEAM_HELPER_ONLY`: not wired live; it does
not by itself convert `ipc_reply` or retire the global lock. Guarded by
`stage186d3_cap_transfer_delegation_link_seam`.

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

#### Stage 13 x86_64 -smp 1 smoke (accepted)

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

## §32 — Stage 14: COW metadata scalability/indexing + lifecycle stress

### 32.1 Motivation

Stage 13 replaced the fixed-size COW array with `Vec<CowPageRecord>`. The Vec
has O(N) lookup and O(N) `clear_cow_pages_for_asid` — every COW check during a
write fault scans the entire global list, and every task exit scans it again.
With many forked processes and pages, this becomes O(tasks × pages_per_task).

Stage 14 replaces the Vec with `BTreeMap<u16, BTreeSet<u64>>` (ASID → virtual
addresses), providing:

- O(log num_asids + log pages_per_asid) lookup and mark
- O(log num_asids) `clear_cow_pages_for_asid` (single `remove(&asid.0)`)
- Natural ASID isolation (per-ASID sets cannot alias)
- No ghost buckets: empty sets are removed immediately when the last entry is
  cleared

### 32.2 Design decision

| Option | Lookup | clear_for_asid | Notes |
|--------|--------|----------------|-------|
| `Vec<CowPageRecord>` (Stage 13) | O(N) | O(N) | Simple; collapses under load |
| `BTreeSet<(u16, u64)>` flat | O(log N) | O(N_asid × log N) | Awkward range delete |
| `BTreeMap<u16, BTreeSet<u64>>` | O(log A + log P) | O(log A) | Chosen |
| `HashMap<u16, HashSet<u64>>` | O(1) avg | O(1) avg | Needs std or ahash; `no_std` hostile |

**Chosen: `BTreeMap<u16, BTreeSet<u64>>`** — all operations are
worst-case O(log N); `clear_for_asid` is a single tree node removal;
fully `no_std + alloc` compatible.

### 32.3 Data model

```
MemorySubsystem.cow_pages: BTreeMap<u16 /*asid*/, BTreeSet<u64 /*virt*/>>
```

- Key: `asid.0` (`u16`)
- Value: `BTreeSet<u64>` of virtual page addresses for that ASID
- Empty-bucket invariant: when the last address is removed from a set, the
  ASID key is also removed (`BTreeMap::remove(&asid.0)`)
- `#[cfg(test)] cow_page_capacity_limit: Option<usize>` remains for
  exhaustion testing (counts `values().map(|s| s.len()).sum()`)

### 32.4 COW function complexity table

| Function | Stage 13 | Stage 14 |
|----------|----------|----------|
| `mark_cow_page` | O(N) scan then push | O(log A + log P) via `BTreeSet::insert` |
| `clear_cow_page` | O(N) `Vec::retain` | O(log A + log P); collapses empty bucket |
| `clear_cow_pages_for_asid` | O(N) `Vec::retain` | O(log A) single `BTreeMap::remove` |
| `is_cow_page` | O(N) `Vec::iter().any` | O(log A + log P) `BTreeSet::contains` |

A = number of distinct ASIDs with COW pages; P = pages per ASID.

### 32.5 `#[cfg(test)]` helper API

Three stable test-API methods added to `KernelState` (never in production):

```rust
pub(crate) fn cow_page_count(&self) -> usize          // total across all ASIDs
pub(crate) fn cow_page_count_for_asid(&self, asid: Asid) -> usize
pub(crate) fn cow_asid_bucket_count(&self) -> usize   // number of ASID keys
```

These methods abstract over the internal storage type. Tests use these rather
than directly accessing `memory.cow_pages`, keeping tests stable across future
storage changes.

### 32.6 Rollback rules (unchanged from Stage 13)

- `try_handle_cow_fault`: on any error after `new_mem_cap` is minted, revoke
  `new_mem_cap` before returning the error.
- `clone_user_address_space_cow`: on any error after a partial COW mark loop,
  there is no rollback of already-marked pages — the clone itself fails, so
  the partially-marked ASID is not reachable by any task.

### 32.7 Stage 14 lifecycle stress tests

Tests added in `tests.rs` (TIDs 64-75 and 169):

| Test | What it verifies |
|------|-----------------|
| `cow_fork_exit_cycles` | Fork + exit cycles leave zero COW records |
| `cow_child_exits_first_then_parent` | Child exit before parent clears child bucket |
| `cow_parent_exits_first_then_child` | Parent exit before child clears parent bucket |
| `cow_multiple_generations` | Three-generation fork tree: each exit clears exactly its bucket |
| `cow_both_sides_split_independently` | Parent and child COW-fault independently; records removed per-side |
| `cow_duplicate_mark_is_idempotent` | `BTreeSet::insert` deduplicates; count stays 1 after two marks |
| `cow_asid_isolation_lookup_not_confused` | Two ASIDs at same virt addr are independent |
| `cow_large_asid_cleared_efficiently` | 50 pages in one ASID; `clear_cow_pages_for_asid` removes all |
| `cow_map_empty_bucket_removed_after_last_entry_cleared` | Empty bucket is removed; `cow_asid_bucket_count()` drops |

### 32.8 Stage 14 acceptance

676 tests pass single-threaded (`cargo test --lib -- --test-threads=1`).
9 new Stage 14 tests added (lifecycle × 5, scalability/isolation × 4).
`cargo check --no-default-features` clean.
`cargo check --features hosted-dev` clean.
x86_64 -smp 1 smoke required for Stage 14 (live COW fault metadata lookup behavior changed).

### 32.9 Files changed (Stage 14)

| File | Change |
|------|--------|
| `src/kernel/boot/defs.rs` | `cow_pages` field type → `BTreeMap<u16, BTreeSet<u64>>`; `cow_page_capacity_limit` retained |
| `src/kernel/boot/bootstrap_state.rs` | Init `cow_pages: BTreeMap::new()` |
| `src/kernel/boot/memory_state.rs` | 4 COW functions rewritten for BTreeMap; 3 `#[cfg(test)]` helpers added |
| `src/kernel/boot/tests.rs` | Stage 13 tests updated to use new helper API; 9 new Stage 14 tests |
| `doc/KERNEL_LOCKING.md` | §31.6 Stage 13 smoke acceptance; this section (§32) |
| `doc/KERNEL_TEST_RULES.md` | Rule N+21 (Stage 14 test rules) |

## §33 — Stage 15: task/process lifecycle cleanup + exit/revoke/join/futex decomposition

### 33.1 Stage 14 x86_64 -smp 1 smoke acceptance

| Counter | Value |
|---------|-------|
| X86_BOOTSTRAP_TIMER_IRQ_EOI_ONLY | 0 |
| X86_BOOTSTRAP_SCHEDULER_READY | 1 |
| X86_BOOTSTRAP_TIMER_STARTED | 1 |
| ENTER_USER | 3 |
| STARTUP_INSTALL_FINAL | 9 |
| PM_ELF_ZC_DONE total | 3 |
| ZC nonzero pages | 3 |
| PM_ELF_ZC_FAIL | 0 |
| INITRAMFS_SRV_ENTRY | 1 |
| DEVFS_SRV_ENTRY | 1 |
| VFS_SRV_ENTRY | 1 |
| DRIVER_MANAGER_READY | 1 |
| BLKCACHE_SRV_READY | 1 |
| VIRTIO_BLK_SRV_READY | 1 |
| fallback | 0 |
| TID mismatch | 0 |
| fatal-ish | 0 |
| oom/capacity | 0 |
| cap/refcount suspicious | 0 |

### 33.2 Lifecycle domain map

Stage 15 audits and repairs three lifecycle cleanup gaps that existed before
this stage:

| Gap | Affected path | Root cause | Fix |
|-----|--------------|------------|-----|
| IPC waiter leak | `exit_task`, `mark_task_dead` | Neither path cleared `endpoint_waiters`, `endpoint_sender_waiters`, or `notification_waiters` for the exiting TID | Added `clear_ipc_waiters_for_tid` called from both paths |
| Join cleanup bypass | `join_thread` when target already `Exited` | Set `Dead` status inline, skipping `mark_task_dead` and thus `maybe_cleanup_process_cnode_for_pid` | `join_thread` now calls `mark_task_dead` |
| Robust futex ASID mismatch | `exit_task` robust futex wake loop | Called `futex_wake` which validates addresses against `current_tid()`'s ASID; fails silently when exit is externally driven | Added `futex_wake_on_exit` that skips ASID validation and calls `futex_wake_inner` directly |

### 33.3 Lock-rank table (unchanged from Stage 14)

| Domain | Rank |
|--------|------|
| scheduler | 1 |
| task | 2 |
| ipc | 3 |
| capability | 4 |
| vm | 5 |
| memory | 6 |

### 33.4 Cleanup ordering table

`exit_task` ordering (must not reorder):

1. Advance restart token counter (restart_state lock)
2. Snapshot robust futex state and detach flag from TCB (task lock)
3. Set `status = Exited(code)`, clear `blocked_recv_state` (task lock)
4. `revoke_reply_caps_for_caller` (capability lock)
5. `clear_ipc_waiters_for_tid` (ipc lock) ← Stage 15 addition
6. `report_task_exit_to_supervisor` (ipc lock, sends message)
7. Robust futex wake loop via `futex_wake_on_exit` (task lock per TCB)
8. `wake_joiners_for` (task lock, sets joiners Runnable, enqueues)
9. If self-exiting: `block_current_cpu` + `dispatch_next_task`
10. If detached: `reap_if_detached` → `mark_task_dead`

`mark_task_dead` ordering:

1. Snapshot `thread_group_id` (task lock)
2. Set `status = Dead`, clear `restart.token` (task lock)
3. `revoke_reply_caps_for_caller` (capability lock)
4. `clear_ipc_waiters_for_tid` (ipc lock) ← Stage 15 addition
5. `release_kernel_context` (memory lock, frees kernel stack)
6. `revoke_driver_runtime_caps` (capability lock)
7. `maybe_cleanup_process_cnode_for_pid` (capability lock, revokes all caps)

### 33.5 Wake-outside-lock plan

`futex_wake_on_exit` is called from `exit_task` without holding any lock.  The
wake path (`futex_wake_inner`) acquires the task lock internally per TCB to
transition `Blocked(Futex(addr))` → `Runnable` and enqueue.  This is the same
pattern as `futex_wake` and is safe.

### 33.6 Path table

| Call site | Calls `clear_ipc_waiters_for_tid`? | Calls `mark_task_dead`? |
|-----------|-----------------------------------|------------------------|
| `exit_task` | Yes (Stage 15) | No (caller calls later) |
| `mark_task_dead` | Yes (Stage 15) | N/A |
| `join_thread` (target Exited) | Via `mark_task_dead` (Stage 15) | Yes (Stage 15) |
| `process_ipc_timeout_deadlines` | Yes (pre-existing) | No |

### 33.7 Resource lifetime table

| Resource | Created by | Freed by | When |
|----------|-----------|---------|------|
| Kernel stack | `provision_default_kernel_context` | `release_kernel_context` in `mark_task_dead` | On Dead |
| CNode + capability space | `ensure_cnode_space_with_slots` | `maybe_cleanup_process_cnode_for_pid` in `mark_task_dead` | On Dead (last thread in group) |
| IPC waiter slot | `futex_wait`, endpoint recv/send | `clear_ipc_waiters_for_tid` in `exit_task`/`mark_task_dead` | On Exited or Dead |
| Reply cap | `ipc_call` | `revoke_reply_caps_for_caller` | On Exited or Dead |
| Driver runtime caps | driver grant | `revoke_driver_runtime_caps` | On Dead |
| User stack frames (raw) | `alloc_user_data_frame` in `spawn_user_task_from_image` | ASID destroy via page-table walk | On ASID destroy |
| MemoryObject | `alloc_anonymous_memory_object` | `reclaim_memory_object_if_unreferenced` when refcounts → 0 | When all refs released |

### 33.8 Stage 15 new functions

| Function | File | Purpose |
|----------|------|---------|
| `clear_ipc_waiters_for_tid` | `ipc_state.rs` | Remove TID from all IPC waiter arrays |
| `futex_wake_on_exit` | `exec_state.rs` | Wake robust futex holders without ASID validation |
| `futex_wake_inner` | `exec_state.rs` | Extracted wake logic shared by `futex_wake` and `futex_wake_on_exit` |
| `endpoint_waiter_count` (test) | `ipc_state.rs` | Count receiver-blocked tasks on endpoint |
| `sender_waiter_count` (test) | `ipc_state.rs` | Count sender-blocked tasks on endpoint |
| `futex_waiter_count` (test) | `ipc_state.rs` | Count futex-blocked tasks at address |
| `join_waiter_count` (test) | `ipc_state.rs` | Count join-blocked tasks for a target TID |
| `task_exists` (test) | `task_core_state.rs` | Check TCB slot exists regardless of status |
| `task_is_dead` (test) | `task_core_state.rs` | Check TCB is in Dead status |
| `task_is_exited` (test) | `task_core_state.rs` | Check TCB is in Exited status |
| `memory_object_refcounts` (test) | `memory_lifecycle_state.rs` | Return (cap, map, pin) refcount tuple |
| `memory_object_exists_for_phys` (test) | `memory_lifecycle_state.rs` | Check MemoryObject slot is live |

### 33.9 Stage 15 test inventory

| Test | TID range | Verifies |
|------|-----------|---------|
| `exit_task_clears_endpoint_receiver_waiter_slot` | 200 | IPC receiver waiter cleared on exit |
| `exit_task_clears_sender_waiter_slot` | 201, 202 | IPC sender waiter cleared on exit |
| `exit_task_clears_notification_waiter_slot` | 203 | Notification waiter cleared on exit |
| `mark_task_dead_clears_endpoint_receiver_waiter` | 210 | IPC receiver waiter cleared on dead |
| `join_thread_calls_mark_task_dead_for_already_exited_target` | 220, 221 | Join → mark_task_dead runs process cnode cleanup |
| `repeated_fork_exit_cycles_leave_no_cow_records` | 230, 231 | Fork/exit loop leaves no COW state |
| `exit_without_joiner_does_not_crash` | 232 | Self-exit with no joiners is safe |
| `repeated_futex_wait_exit_wake_cycles_no_stale_waiters` | 240–243 | Exit clears Futex-blocked status |
| `memory_object_reclaimed_after_all_refs_released_on_task_exit` | 250 | MemoryObject reclaimed after cap+map refs drop to 0 |
| `restart_task_re_enqueues_and_clears_exited_status` | 260, 261 | Restart transitions Exited → Runnable |
| `supervisor_endpoint_receives_task_exit_event` | 270, 271 | Supervisor notified on exit |

### 33.10 Stage 15 acceptance

690 tests pass single-threaded (`cargo test --lib -- --test-threads=1`).
14 new Stage 15 tests added.
`cargo check --no-default-features` clean.
`cargo check --features hosted-dev` clean.
x86_64 -smp 1 smoke required: live lifecycle/scheduler/task behavior changed
(IPC waiter cleanup, join cleanup, futex wake path).

### 33.11 Files changed (Stage 15)

| File | Change |
|------|--------|
| `src/kernel/boot/ipc_state.rs` | `clear_ipc_waiters_for_tid`; 4 `#[cfg(test)]` count helpers |
| `src/kernel/boot/restart_state.rs` | `exit_task`: add `clear_ipc_waiters_for_tid`; `futex_wake_on_exit` for robust list; `mark_task_dead`: add `clear_ipc_waiters_for_tid` |
| `src/kernel/boot/exec_state.rs` | `futex_wake_on_exit`; extract `futex_wake_inner` |
| `src/kernel/boot/thread_state.rs` | `join_thread`: call `mark_task_dead` instead of inline Dead set |
| `src/kernel/boot/task_core_state.rs` | 3 `#[cfg(test)]` helpers: `task_exists`, `task_is_dead`, `task_is_exited` |
| `src/kernel/boot/memory_lifecycle_state.rs` | 2 `#[cfg(test)]` helpers: `memory_object_refcounts`, `memory_object_exists_for_phys` |
| `src/kernel/boot/tests.rs` | 14 new Stage 15 lifecycle tests (TIDs 200–270) |
| `doc/KERNEL_LOCKING.md` | §33 (this section) |
| `doc/KERNEL_TEST_RULES.md` | Rule N+22 (Stage 15 test rules) |

## §34 — Stage 16: timeout/deadline/block-state cleanup + scheduler wait-state consistency

### 34.1 Stage 15 x86_64 -smp 1 smoke acceptance

Stage 15 accepted. x86_64 -smp 1 smoke passed clean.

| Counter | Value |
|---------|-------|
| X86_BOOTSTRAP_TIMER_IRQ_EOI_ONLY | 0 |
| X86_BOOTSTRAP_SCHEDULER_READY | 1 |
| X86_BOOTSTRAP_TIMER_STARTED | 1 |
| ENTER_USER | 3 |
| STARTUP_INSTALL_FINAL | 9 |
| PM_ELF_ZC_DONE total | 3 |
| ZC nonzero pages | 3 |
| PM_ELF_ZC_FAIL | 0 |
| INITRAMFS_SRV_ENTRY | 1 |
| DEVFS_SRV_ENTRY | 1 |
| VFS_SRV_ENTRY | 1 |
| DRIVER_MANAGER_READY | 1 |
| BLKCACHE_SRV_READY | 1 |
| VIRTIO_BLK_SRV_READY | 1 |
| fallback | 0 |
| TID mismatch | 0 |
| fatal-ish | 0 |
| oom/capacity | 0 |
| cap/refcount suspicious | 0 |

x86_64 SMP remains deferred. AArch64 smoke data not available.

### 34.2 Blocking/timeout domain map

| Path | Class | Waiter state owner | Deadline state | Scheduler state | Stale-entry risk |
|------|-------|-------------------|----------------|-----------------|-----------------|
| IPC recv block | A/K | `endpoint_waiters[ep]` | `tcb.ipc_timeout_deadline` | task_state_lock (rank 2) | Low — slot cleared on wake/exit |
| IPC send block | A/K | `endpoint_sender_waiters[ep][i]` | `tcb.ipc_timeout_deadline` | task_state_lock | Low — slot cleared on wake/timeout/exit |
| IPC recv timeout | C/G | same slot | cleared in `process_ipc_timeout_deadlines` | set Runnable, enqueue | **Fixed**: skip Exited/Dead (stage 16 WakeTask fix) |
| IPC send deadline | C/F | same slot | cleared in `process_ipc_timeout_deadlines` | set Runnable, enqueue | Low — blocked_ipc guard |
| IPC delivery before timeout | B/G | `take()` from slot | cleared by `clear_ipc_timeout_for_tid` via `wake_tid_to_runnable` | set Runnable, enqueue | None — delivery clears both |
| IPC exit before timeout | D/G | cleared by `clear_ipc_waiters_for_tid` | stale in TCB but harmless (Exited skipped) | Exited status prevents re-enqueue | Low |
| Futex wait | A | TCB status = Blocked(Futex) | none (no futex timeout) | task_state_lock | N/A |
| Futex wake | B/H | TCB status scan | n/a | set Runnable, enqueue | Low — status match required |
| Futex exit cleanup | D/H | via `exit_task` Blocked(Futex) check | n/a | Exited overrides Blocked | Low |
| Join wait | A | TCB status = Blocked(Join) | none (no join timeout) | task_state_lock | N/A |
| Join target exit | B/I | `wake_joiners_for` sets Runnable | n/a | enqueue | Low |
| Joiner exit | E | `exit_task` → status = Exited | n/a | Exited | Low |
| mark_task_dead while blocked | E | `clear_ipc_waiters_for_tid` | stale in TCB (harmless) | Dead status prevents re-enqueue | Low |
| notification waiter exit | J | `clear_ipc_waiters_for_tid` | n/a | Exited | Low |
| WakeTask cross-CPU | B/L | TCB status check | n/a | **Fixed**: only Blocked→Runnable | **Fixed** — was unconditional |
| Timer tick deadline handling | C/K | `process_ipc_timeout_deadlines` | cleared | set Runnable, enqueue | Low — blocked_ipc guard filters non-IPC |
| Scheduler yield/preempt | L | n/a | n/a | runs on Runnable tasks | Low |

### 34.3 Block-state invariants

1. A task may be in a run queue only if its TCB status is Runnable/Running-compatible.
2. A task blocked on endpoint receiver waiter must not also be in the run queue.
3. A task blocked as endpoint sender waiter must not also be in the run queue.
4. A task blocked on futex must not also be in the run queue.
5. A task blocked on join must not also be in the run queue.
6. Dead/Exited tasks must not be endpoint receiver waiters (enforced by `clear_ipc_waiters_for_tid`).
7. Dead/Exited tasks must not be sender waiters (enforced by `clear_ipc_waiters_for_tid`).
8. Dead/Exited tasks must not be notification waiters (enforced by `clear_ipc_waiters_for_tid`).
9. Dead/Exited tasks must not be futex waiters (status overwritten; `futex_waiter_count` checks Blocked(Futex)).
10. Dead/Exited tasks must not be join waiters (status overwritten; `wake_joiners_for` pattern match required).
11. Dead/Exited tasks must not be resurrected by `WakeTask` cross-CPU work items (**fixed Stage 16**).
12. A successful delivery/wake calls `clear_ipc_timeout_for_tid` to prevent later timeout from misfiring.
13. A timeout clears the waiter slot and sets `ipc_timeout_deadline = None` before enqueuing.
14. Exit/death calls `clear_ipc_waiters_for_tid`; stale `ipc_timeout_deadline` in Exited TCBs is harmless (blocked_ipc guard in `process_ipc_timeout_deadlines`).
15. No timeout path may set Runnable on a Dead or Exited task (blocked_ipc guard + status check).
16. No IPC message delivered to a cleared waiter slot (slot is atomically taken under ipc_state_lock).
17. Futex wake requires `Blocked(Futex(addr))` status match; stale/dead waiters do not match.
18. Join wake requires `Blocked(Join(tid))` status match.
19. Duplicate wake is harmless: `wake_tid_to_runnable` accepts Runnable/Running and skips status update if already Runnable.
20. Duplicate timeout is prevented: `ipc_timeout_deadline` is cleared on first fire; subsequent `process_ipc_timeout_deadlines` calls skip it.

### 34.4 Timeout/deadline lifecycle rules

- IPC recv/send timeouts are per-TCB (`ipc_timeout_deadline`, `ipc_timeout_fired`).
- Deadline set in `block_current_on_receive_with_deadline` / `block_current_on_send_with_deadline`.
- Deadline cleared in `clear_ipc_timeout_for_tid` (called from `wake_tid_to_runnable`).
- Timeout fired in `process_ipc_timeout_deadlines` (timer ISR path).
- Stale deadline in Exited TCB is harmless: `blocked_ipc` guard skips non-IPC-blocked tasks.
- Futex and join have no timeouts in this kernel version.

### 34.5 Cancel-on-exit/death rules

1. `exit_task` calls `clear_ipc_waiters_for_tid` (all three waiter arrays).
2. `mark_task_dead` calls `clear_ipc_waiters_for_tid` (idempotent).
3. `clear_ipc_waiters_for_tid` is idempotent — calling twice is safe.
4. After `exit_task`, `ipc_timeout_deadline` in TCB may be non-None (harmless: Exited status blocks timeout path).
5. After `mark_task_dead`, the TCB status is Dead which also blocks the timeout path.
6. Robust futex cleanup uses `futex_wake_on_exit` (no ASID validation) — Stage 15 fix preserved.

### 34.6 Wake-outside-lock rules

| Path | Mutation under lock | Wake outside lock |
|------|--------------------|--------------------|
| `send_message_to_endpoint_and_wake` | enqueue under `ipc_state_lock` (rank 3) | `wake_waiter_for_endpoint` after release |
| `ipc_recv_endpoint_take` (Stage 4D) | dequeue + refill under `ipc_state_lock` | `apply_split_sender_wake_plan` after release |
| `process_ipc_timeout_deadlines` | TCB scan + waiter clear under both locks | `enqueue_task` after both locks released |
| `futex_wake_inner` | TCB scan + status set under `task_state_lock` | `enqueue_task` after lock released |
| `wake_joiners_for` | TCB scan + status set under `task_state_lock` | `enqueue_task` after lock released |

### 34.7 Production bug fixed (Stage 16)

**`WorkItem::WakeTask` in `scheduler_state.rs`**: The cross-CPU wake path set
`tcb.status = TaskStatus::Runnable` unconditionally, including for Dead/Exited/Runnable
tasks.  Fixed: only Blocked tasks are transitioned to Runnable; Dead/Exited/Runnable
skip the enqueue.  This prevents stale cross-CPU wake items from resurrecting terminated
tasks or duplicating run-queue entries.

### 34.8 Stage 16 test inventory

| Test | TID(s) | Verifies |
|------|--------|---------|
| `recv_timeout_process_clears_endpoint_waiter_and_deadline` | 280 | waiter + deadline cleared after timeout fires |
| `send_deadline_process_clears_sender_waiter_and_deadline` | 281 | sender waiter + deadline cleared after send timeout |
| `exit_before_ipc_recv_timeout_clears_waiter_and_deadline` | 282 | exit_task clears recv waiter; Exited skipped by timeout |
| `ipc_deadline_count_helper_reports_set_and_cleared` | 283 | `ipc_deadline_count_for_tid` helper |
| `ipc_timeout_does_not_fire_for_futex_blocked_task` | 284 | IPC timeout path skips Futex-blocked tasks |
| `repeated_recv_timeout_cycles_no_stale_receiver_waiter` | 285-288 | Stress: 4 recv-timeout cycles no stale waiter |
| `task_helpers_runnable_blocked_dead_consistent` | 290 | `task_is_runnable`, `task_is_blocked`, `task_blocked_reason` helpers |
| `notification_waiter_count_reflects_exit_cleanup` | 291 | `notification_waiter_count` helper + exit cleanup |
| `wake_endpoint_waiter_dead_task_does_not_resurrect_task` | 292 | Dead waiter not resurrected by wake_waiter_for_endpoint |
| `wake_endpoint_waiter_exited_task_does_not_resurrect_task` | 293 | Exited waiter not resurrected |
| `exit_then_mark_dead_waiter_cleanup_is_idempotent` | 300 | Idempotency of exit + mark_dead waiter cleanup |
| `clear_ipc_waiters_is_idempotent_for_all_waiter_types` | 301 | Double-clear is safe for all three waiter arrays |
| `timeout_fires_then_exit_no_double_disruption` | 302 | Timeout then exit: no double-clean panic |
| `wake_task_cross_cpu_work_skips_dead_task` | 306 | WakeTask noop for Dead task (**new bug fix test**) |
| `wake_task_cross_cpu_work_skips_exited_task` | 307 | WakeTask noop for Exited task |
| `wake_task_cross_cpu_work_skips_runnable_task` | 308 | WakeTask noop for already-Runnable task |
| `repeated_send_deadline_cycles_no_stale_sender_waiter` | 311-314 | Stress: 4 send-deadline cycles no stale waiter |
| `repeated_mixed_waiter_block_exit_no_stale_state` | 315-317 | Exit clears recv/sender/notification waiters |
| `ipc_deadline_cleared_after_delivery_before_timeout` | 318 | Delivery clears deadline; later timeout is no-op |
| `repeated_recv_block_timeout_delivery_no_stale_timeout` | 319-322 | Stress: 4 delivery-before-timeout cycles, no stale flag |

### 34.9 Deferred items

- Futex timeout (timed futex_wait): not implemented. Current futex wait is non-timeout.
- Join timeout (timed join_thread): not implemented. Current join wait is non-timeout.
- Full global-lock-removal: deferred (requires cooperative dispatch for hosted-dev).
- x86_64 SMP (multi-core run-queue balancing): deferred.
- RAMFS/FAT runtime spawning: untouched, deferred.

### 34.10 Stage 16 acceptance

710 tests pass single-threaded (`cargo test --lib -- --test-threads=1`).
20 new Stage 16 tests added (TIDs 280–322).
`cargo check --no-default-features` clean.
`cargo check --features hosted-dev` clean.
x86_64 -smp 1 smoke required: live scheduler (WakeTask path) behavior changed.

### 34.11 Files changed (Stage 16)

| File | Change |
|------|--------|
| `src/kernel/boot/scheduler_state.rs` | Fix `WorkItem::WakeTask`: guard Blocked→Runnable only |
| `src/kernel/boot/ipc_state.rs` | `notification_waiter_count`, `ipc_deadline_count_for_tid` test helpers |
| `src/kernel/boot/task_core_state.rs` | `task_is_runnable`, `task_is_blocked`, `task_blocked_reason` test helpers |
| `src/kernel/boot/tests.rs` | 20 new Stage 16 tests (TIDs 280–322) |
| `doc/KERNEL_LOCKING.md` | §34 (this section) |
| `doc/KERNEL_TEST_RULES.md` | Rule N+23 (Stage 16 test rules) |

## §35 — Stage 17: cross-CPU work queue audit + scheduler wake-plan centralization

### 35.1 Stage 16 smoke acceptance

x86_64 -smp 1 smoke markers (required after Stage 16 WakeTask path changes):

| Marker | Expected | Notes |
|--------|----------|-------|
| `X86_BOOTSTRAP_TIMER_IRQ_EOI_ONLY` | 0 | full timer path |
| `X86_BOOTSTRAP_SCHEDULER_READY` | 1 | |
| `X86_BOOTSTRAP_TIMER_STARTED` | 1 | |
| `ENTER_USER` | 3 | init + 2 servers |
| `STARTUP_INSTALL_FINAL` | 9 | |
| `PM_ELF_ZC_DONE total` | 3 | zero-copy ELF loads |
| `ZC nonzero pages` | ≥3 | |
| `PM_ELF_ZC_FAIL` | 0 | |
| `INITRAMFS_SRV_ENTRY` | 1 | |
| `DEVFS_SRV_ENTRY` | 1 | |
| `VFS_SRV_ENTRY` | 1 | |
| `DRIVER_MANAGER_READY` | 1 | |
| `BLKCACHE_READY` | 1 | |
| `VIRTIO_READY` | 1 | |
| fallback | 0 | |
| TID mismatch | 0 | |
| fatal-ish | 0 | |
| oom/capacity | 0 | |
| cap/refcount suspicious | 0 | |

### 35.2 Cross-CPU work domain audit results

Audit performed at commit `1e5d96d` (Stage 16 baseline).

#### WorkItem variants

| Variant | Purpose | Stale-safe strategy |
|---------|---------|---------------------|
| `Reschedule` | Trigger context switch on target CPU | CPU-match guard: noop if `current_cpu() != cpu` |
| `TlbShootdown { asid, va_range, requester, sequence }` | Request TLB invalidation | Retired-ASID check + sequence validated on ACK |
| `TlbShootdownAck { sequence, from_cpu }` | Confirm TLB invalidation | Sequence-number guard: `wait.sequence != sequence` → skip |
| `WakeTask { tid }` | Wake a blocked task | Status guard: non-Blocked states → silent no-op (Stage 16 + Stage 17) |

#### Queue characteristics

- **Type**: `CrossCpuWorkQueue` — 64-slot fixed ring buffer per CPU in `SmpMailbox`
- **Lock**: `SpinLockIrq<WorkQueue>` — IRQ-safe spin lock
- **Processing**: One item at a time, dequeued under `ipc_state_lock` (rank 3), then
  lock released before `apply_cross_cpu_work` (allows TlbShootdownAck to re-acquire)
- **Overflow**: `SmpError::QueueFull` returned to caller; no silent drops

#### Lock-rank table for cross-CPU work

| Operation | Locks acquired | Order |
|-----------|---------------|-------|
| `submit_cross_cpu_work` | `ipc_state_lock` (rank 3) → `CrossCpuWorkQueue` spinlock | 3 only |
| `process_cross_cpu_work_for_cpu` dequeue | `ipc_state_lock` (rank 3) | 3 only |
| `apply_cross_cpu_work` WakeTask | `task_state_lock` (rank 2) → scheduler (rank 1) | 2 → 1 |
| `apply_cross_cpu_work` TlbShootdownAck | `ipc_state_lock` (rank 3) | 3 only |
| `apply_cross_cpu_work` Reschedule | scheduler (rank 1) | 1 only |

No lock-order violations found. Dequeue-then-release pattern prevents rank-3
re-entry while holding rank-3.

### 35.3 Production bug fixed (Stage 17)

**`WorkItem::WakeTask` missing-TID propagation in `apply_cross_cpu_work`**:

The Stage 16 `WakeTask` handler called `ok_or(KernelError::TaskMissing)?` when
no TCB was found, propagating `TaskMissing` out of `process_cross_cpu_work_for_cpu`.
A stale cross-CPU WakeTask item for a task whose TID was never registered (or whose
slot was recycled with a different TID) would therefore cause the entire work-drain
loop to fail with an error.

**Fix**: Introduced `apply_cross_cpu_wake_task` (centralized helper) which returns
`CrossCpuWakeApplyResult::SkippedMissing` as `Ok` for a missing TID.  The `WakeTask`
arm now delegates to this helper; all `Skipped*` results are silent no-ops.

### 35.4 CrossCpuWakeApplyResult enum

`pub enum CrossCpuWakeApplyResult` in `src/kernel/smp.rs` makes wake-path semantics
observable and testable:

| Variant | Condition | Action |
|---------|-----------|--------|
| `Applied` | `Blocked(_)` | Status → `Runnable`, task enqueued |
| `SkippedMissing` | TID not in TCB table | No-op (stale item) |
| `SkippedDead` | `Dead` status | No-op (terminated) |
| `SkippedExited` | `Exited(_)` status | No-op (terminated) |
| `SkippedAlreadyRunnable` | `Runnable` status | No-op (duplicate wake) |
| `SkippedRunning` | `Running` status | No-op (already active) |
| `SkippedFaulted` | `Faulted` status | No-op (faulted, not schedulable) |

### 35.5 Wake-plan invariants

1. `apply_cross_cpu_wake_task` is the single canonical Blocked→Runnable transition
   for cross-CPU WakeTask items.
2. All non-`Applied` results are silent no-ops; no error is propagated.
3. Only `Applied` triggers `enqueue_on_cpu`; all other results skip enqueue.
4. A missing TID (`SkippedMissing`) is not an error; it is an expected consequence
   of task termination racing with cross-CPU wake delivery.
5. Faulted tasks are not woken; a WakeTask for a Faulted TID is silently dropped.
6. The helper is `pub(crate)` and directly callable by tests to unit-test each variant.

### 35.6 Test helpers added (Stage 17)

| Helper | Location | Purpose |
|--------|----------|---------|
| `apply_cross_cpu_wake_task(cpu, tid)` | `scheduler_state.rs` | Centralized wake-plan; returns `CrossCpuWakeApplyResult` |
| `cross_cpu_work_count_for_cpu(cpu)` | `ipc_state.rs` | Queue depth probe; returns 0 for out-of-range CPU |

### 35.7 Stage 17 test inventory

| Test | TID(s) | Verifies |
|------|--------|---------|
| `cross_cpu_wake_apply_result_missing_tid` | 330 (unregistered) | `SkippedMissing` returned, no error |
| `cross_cpu_wake_apply_result_dead_task` | 331 | `SkippedDead`, task remains Dead |
| `cross_cpu_wake_apply_result_exited_task` | 332 | `SkippedExited`, task remains Exited |
| `cross_cpu_wake_apply_result_runnable_task` | 333 | `SkippedAlreadyRunnable`, task stays Runnable |
| `cross_cpu_wake_apply_result_blocked_task_becomes_runnable` | 335 | `Applied`, task becomes Runnable |
| `cross_cpu_wake_apply_result_faulted_task` | 353 | `SkippedFaulted`, no state change |
| `cross_cpu_work_count_helper_tracks_submit_and_drain` | — | Helper counts 0→2→0 across submit/drain |
| `cross_cpu_work_count_for_invalid_cpu_returns_zero` | — | Out-of-range CPU → 0, no panic |
| `process_cross_cpu_work_missing_tid_not_an_error` | 336 (unregistered) | **Bug fix test**: missing TID does not cause Err |
| `process_cross_cpu_work_dead_task_no_resurrection` | 337 | Dead task unaffected by WakeTask in queue |
| `process_cross_cpu_work_exited_task_no_resurrection` | 338 | Exited task unaffected by WakeTask in queue |
| `process_cross_cpu_work_blocked_task_becomes_runnable` | 340 | Blocked task woken via queue |
| `process_cross_cpu_work_mixed_stale_fresh_items` | 341 (dead), 342 (blocked) | Stale+fresh in same drain; only fresh wakes |
| `duplicate_wake_task_items_for_same_tid_are_harmless` | 343 | Two WakeTask items: first wakes, second no-op |
| `repeated_wake_task_drain_cycles_no_stale_state` | 344–347 | 4 block→wake→check cycles, no stale state |
| `repeated_exit_before_drain_no_resurrection` | 348–351 | 4 exit-then-drain cycles, no resurrection |
| `work_queue_drains_fully_count_zero_after_drain` | — | 8 items → drain → count=0 |
| `work_queue_full_then_drain_then_refill` | 5000–6063 | Fill→drain→refill×2, no wrap-around corruption |

### 35.8 Deferred items

- TLB shootdown test harness (requires multi-CPU address-space teardown, not testable in
  single-CPU hosted-dev without a second CPU's inbox being serviced).
- Futex timeout (timed futex_wait): not implemented.
- Join timeout (timed join_thread): not implemented.
- Full global-lock-removal: deferred.
- x86_64 SMP (multi-core run-queue balancing): deferred.
- RAMFS/FAT runtime spawning: untouched, deferred.

### 35.9 Stage 17 acceptance

728 tests pass single-threaded (`cargo test --lib -- --test-threads=1`).
18 new Stage 17 tests added (TIDs 330–353 sparse).
`cargo check --no-default-features` clean.
`cargo check --features hosted-dev` clean.
x86_64 -smp 1 smoke not required: no live scheduler/cross-CPU behavior changed for
running tasks (missing-TID fix is a no-op path; no new code executes on the hot path).

### 35.10 Files changed (Stage 17)

| File | Change |
|------|--------|
| `src/kernel/smp.rs` | `CrossCpuWakeApplyResult` enum (7 variants) |
| `src/kernel/boot/scheduler_state.rs` | `apply_cross_cpu_wake_task` centralized helper; `WakeTask` arm delegates to it |
| `src/kernel/boot/ipc_state.rs` | `cross_cpu_work_count_for_cpu` test helper |
| `src/kernel/boot/tests.rs` | 18 new Stage 17 tests (TIDs 330–353 sparse) |
| `doc/KERNEL_LOCKING.md` | §35 (this section) |
| `doc/KERNEL_TEST_RULES.md` | Rule N+24 (Stage 17 test rules) |

### 35.11 Stage 17 acceptance

728 tests pass (`cargo test --lib -- --test-threads=1`).
`cargo check --no-default-features` and `cargo check --features hosted-dev` both clean.
x86_64 -smp 1 smoke was **not required**: the missing-TID fix only converted a
previous `TaskMissing` error path to a stale-safe no-op; no live task was newly
woken or differently handled; the successful WakeTask hot path is unchanged.
x86_64 SMP remains deferred.

## §36 — Stage 18: TLB shootdown + cross-CPU VM cleanup audit and implementation

### 36.1 Stage 17 acceptance record

Accepted at commit `7cdcafb`.  728 tests.  No x86_64 -smp 1 smoke required
(see §35.11).  x86_64 SMP deferred.  Hard invariants from Stages 12–17 preserved.

### 36.2 TLB/VM/reclaim domain audit results

Audit performed at commit `7cdcafb` (Stage 17 baseline).  All production VM
paths examined.

#### 36.2.1 Path classification table

| Path | ASID source | PTE removed? | TLB action | Reclaim gate | Stale ASID behavior | Bugs found |
|------|-------------|-------------|-----------|--------------|---------------------|-----------|
| `handle_vm_brk` shrink | plan-first explicit | Phase 1 | `execute_tlb_shootdown_wait_plan` | After shootdown ACK | N/A (live ASID) | None |
| `handle_vm_anon_map` rollback | plan-first explicit | Phase 1 | `execute_tlb_shootdown_wait_plan` | After shootdown ACK | N/A (live ASID) | None |
| `handle_vm_map` rollback | plan-first explicit | Phase 1 | `execute_tlb_shootdown_wait_plan` | After shootdown ACK | N/A (live ASID) | None |
| `handle_transfer_release` | plan-first explicit | Phase 1 | `execute_tlb_shootdown_wait_plan` | After shootdown ACK | N/A (live ASID) | None |
| `map_shared_region_into_receiver` rollback | plan-first explicit | Phase 1 | `execute_tlb_shootdown_wait_plan` | After shootdown ACK | N/A (live ASID) | None |
| `purge_active_transfer_mappings_for_pid` | explicit (owner ASID) | `unmap_range_two_phase` (phase 1) | `execute_tlb_shootdown_wait_plan` | After shootdown ACK | N/A | None |
| `revoke_active_transfer_mappings_for_cap` | explicit (owner ASID) | `unmap_range_two_phase` (phase 1) | `execute_tlb_shootdown_wait_plan` | After shootdown ACK | N/A | None |
| `try_handle_demand_page_fault` | plan-first explicit | N/A (map) | N/A | N/A | N/A | None |
| `try_handle_cow_fault` (split success) | caller-provided explicit | Yes (old PTE remap) | local + inline | Old frame: map_refcount=0 | N/A | None |
| `try_handle_cow_fault` (split failure) | caller-provided explicit | No | None | Old frame: untouched | N/A | None |
| `destroy_user_address_space_by_asid` | explicit | drain_mappings | Fire-and-forget TlbShootdown (all CPUs) | After shootdown **queued** (Stage 18 fix) | Retired ASID prevents reuse | **Fixed** |
| `WorkItem::TlbShootdown` (apply) | work item carries ASID | N/A (remote invalidation) | `invalidate_asid` or per-page | N/A | Retired-entry check; harmless if not found | None |
| `WorkItem::TlbShootdownAck` (apply) | work item carries sequence | N/A | N/A | N/A | Sequence mismatch → skip | None |
| `unmap_user_page` / `unmap_user_page_in_asid` | explicit | Yes | `request_live_asid_shootdown` (AFTER reclaim) | Inline (old pattern) — **test-only** | N/A | Old pattern; safe under global lock |

#### 36.2.2 Two-phase unmap ordering (canonical)

```
Phase 1 — vm (rank 5) → memory (rank 6) → scheduler+task (rank 1+2)
  unmap_page:       remove PTE
  clear_cow_page:   clear COW record
  note_mapping_removed: map_refcount--
  Return TlbShootdownWaitPlan (carries phys)
  DO NOT reclaim frame here.

Phase 2 — ipc (rank 3) [if target_bitmap != 0]
  request_live_asid_shootdown: busy-wait for all remote CPUs to ACK

Phase 3 — memory (rank 6)
  reclaim_memory_object_for_phys: free frame iff all refcounts == 0
```

All normal production paths follow this ordering.  `unmap_user_page` /
`unmap_user_page_in_asid` use the old inline pattern (reclaim before shootdown)
and are limited to tests; they are safe under the global kernel lock.

#### 36.2.3 ASID destroy shootdown ordering (fire-and-forget)

`destroy_user_address_space_by_asid` uses a fire-and-forget full-ASID
invalidation (`va_range: None, requester: None, sequence: 0`).  No ACK wait.

**Stage 18 ordering fix**: TlbShootdown work items are now submitted to all
online CPUs **before** frame reclaim.  Previously the order was reversed.  Under
the global kernel lock both orderings are safe; the fix aligns with the
two-phase contract and is the correct direction for future lock-free SMP.

Remaining SMP limitation (documented, not fixed): frames are reclaimed before
remote CPUs have processed the TlbShootdown work items, because there is no
ACK-gated reclaim for full-ASID destroy.  This is safe under the global lock
because:
  1. The destroyed ASID's task is dead/exiting — no user code runs for it.
  2. The retired ASID mechanism prevents ASID reuse until all CPUs ACK.
  3. No new task can load the stale ASID entries.

Full ACK-gated reclaim for ASID destroy (deferred-reclaim list tied to the
retired slot) is deferred to a future SMP-hardening stage.

#### 36.2.4 MemoryObject refcount invariants

| Refcount | Role | Gate |
|----------|------|------|
| `cap_refcount` | Number of Capability handles for this object | Frame not reclaimed while > 0 |
| `map_refcount` | Number of active page-table entries | Frame not reclaimed while > 0 |
| `pin_refcount` | Number of DMA/shared-memory pins | Frame not reclaimed while > 0 |

`reclaim_memory_object_if_unreferenced` checks all three before calling
`free_frame`.  All callers either decrement the relevant refcount first or rely
on the check to confirm eligibility.

#### 36.2.5 COW metadata cleanup

- `clear_cow_page(asid, virt)` — called from `unmap_page_phase1` on each page.
- `clear_cow_pages_for_asid(asid)` — called at the START of
  `destroy_user_address_space_by_asid`, before mappings are drained.
- `clone_user_address_space_cow` (fork) — sets COW records for parent and child.
- On fork rollback: `destroy_user_address_space_by_asid(child_asid)` clears
  child COW records and restores parent write permissions.
- ASID not immediately reused (retired array) — no stale COW metadata risk
  from recycled ASIDs.

#### 36.2.6 Active-transfer cleanup ordering

Both `purge_active_transfer_mappings_for_pid` and
`revoke_active_transfer_mappings_for_cap` use `unmap_range_two_phase`:
- Phase 1 per page: PTE removal + `map_refcount--`
- Phase 2 per page: TLB shootdown (fast-path if single CPU) + frame reclaim
- After all pages: revoke transfer capability → `cap_refcount--` → reclaim

This ordering ensures: map_refcount reaches 0 before cap_refcount reaches 0.
`reclaim_memory_object_if_unreferenced` fires at the cap revoke point.

#### 36.2.7 Stale cross-CPU TLB work safety

| Condition | Behavior |
|-----------|---------|
| TlbShootdown for a live ASID | Correct: invalidate PTE range or full ASID |
| TlbShootdown for a retired ASID | `acknowledge_shootdown` called; harmless if not found |
| TlbShootdown for a never-existing ASID | `retired` is false; no ACK queued; no-op |
| TlbShootdownAck with wrong sequence | Sequence guard skips update |
| TlbShootdownAck with wrong requester CPU | CPU guard returns early |
| Duplicate TlbShootdown for same ASID | Second call: ack already cleared → no-op |

### 36.3 Production bug fixed (Stage 18)

**`destroy_user_address_space_by_asid` ordering in `memory_state.rs`**:

Previous code: drain mappings → reclaim frames → submit TlbShootdown work items.
Fixed code: drain mappings → submit TlbShootdown work items → reclaim frames.

The fix aligns ASID destroy with the two-phase-unmap contract (shootdown before
reclaim).  Queue-full errors from `submit_cross_cpu_work` are now silenced
(`let _ = ...`) rather than propagated: the ASID is already retired and frames
must be reclaimed regardless of queue capacity.

### 36.4 No-current-ASID helpers audit

All explicit-ASID VM helpers confirmed explicit:
- `map_user_page_in_asid_with_caps(asid, ...)` ✓
- `map_user_page_in_asid_raw(asid, ...)` ✓
- `unmap_user_page_in_asid(asid, ...)` ✓ (test-only only)
- `is_user_page_mapped_in_asid(asid, ...)` ✓
- `unmap_page_phase1(asid, ...)` ✓
- `unmap_range_two_phase(asid, ...)` ✓
- `destroy_user_address_space_by_asid(asid)` ✓

No remaining implicit current-ASID helpers in production VM paths.
`current_asid` appears only in:
- `FatalTrapReadSnapshot` — fault logging only, not VM operations.
- `x86_64/descriptor_tables.rs` — fault logging only.
- `runtime.rs` tests — verified, test-only.

### 36.5 Test helpers added (Stage 18)

| Helper | Location | Purpose |
|--------|----------|---------|
| `asid_is_live_for_test(asid)` | `memory_state.rs` | True if ASID is in live address-space table |
| `asid_is_retired_for_test(asid)` | `memory_state.rs` | True if ASID is in retired table |
| `mapped_page_count_for_asid(asid)` | `memory_state.rs` | Count pages mapped in ASID |
| `active_transfer_count_for_pid(pid)` | `cnode_state.rs` | Count active transfer slots for PID |

### 36.6 Stage 18 test inventory

| Test | Verifies |
|------|---------|
| `asid_destroy_sends_tlb_shootdown_before_reclaim_ordering` | TlbShootdown work queued to all CPUs before reclaim (Stage 18 fix) |
| `asid_destroy_clears_cow_metadata` | COW records removed from ASID bucket on destroy |
| `asid_destroy_clears_all_mappings` | mapped_page_count → 0 after destroy |
| `asid_destroy_does_not_affect_other_asid` | Other ASID mappings + COW records unaffected |
| `asid_destroy_puts_asid_in_retired_array_when_cpus_online` | Retired entry exists after destroy |
| `asid_not_reused_while_in_retired_array` | New allocations skip retired ASID values |
| `stale_tlb_shootdown_for_retired_asid_is_harmless` | Processing stale TlbShootdown work is safe |
| `duplicate_tlb_shootdown_for_same_asid_harmless` | Two items for same ASID both processed safely |
| `tlb_shootdown_for_never_existing_asid_is_harmless` | Phantom ASID TlbShootdown does not crash |
| `two_phase_unmap_map_refcount_decrements_in_phase1` | map_refcount=0 and frame exists after phase 1 |
| `two_phase_unmap_fast_path_no_cross_cpu_work` | Single CPU, no active task → no work item queued |
| `unmap_phase1_absent_page_returns_none` | Idempotent for absent pages |
| `reclaim_blocked_while_cap_refcount_nonzero` | cap_refcount > 0 prevents frame reclaim |
| `reclaim_blocked_while_map_refcount_nonzero` | map_refcount > 0 prevents frame reclaim |
| `reclaim_blocked_while_pin_refcount_nonzero` | pin_refcount > 0 prevents frame reclaim |
| `reclaim_happens_when_all_refcounts_zero` | All-zero refcounts → frame reclaimed |
| `cow_metadata_cleared_on_asid_destroy_after_fork` | Child COW records gone after child destroy; parent intact |
| `repeated_asid_destroy_by_asid_returns_error_not_panic` | Double-destroy returns Err, not panic |
| `active_transfer_count_helper_tracks_mappings` | Helper counts active transfers correctly |
| `active_transfer_purge_is_idempotent` | Double purge does not panic or underflow |

### 36.7 Deferred items

- Full ACK-gated frame reclaim for ASID destroy (requires deferred-reclaim list
  tied to retired ASID slot; needed for full lock-free SMP safety).
- `tick_retired_shootdowns` timeout mechanism: currently returns 0 always;
  escalation path is dead code.  Retired ASIDs require explicit ACK by design.
  Timeout/escalation mechanism is deferred.
- Futex timeout, join timeout: not implemented.
- Full global-lock-removal: deferred.
- x86_64 SMP (multi-core run-queue balancing): deferred.
- RAMFS/FAT runtime spawning: untouched, deferred.

### 36.8 Stage 18 acceptance

748 tests pass single-threaded (`cargo test --lib -- --test-threads=1`).
20 new Stage 18 tests added.
`cargo check --no-default-features` clean.
`cargo check --features hosted-dev` clean.
x86_64 -smp 1 smoke is **required**: `destroy_user_address_space_by_asid`
ordering changed (shootdown before reclaim).  Live ASID destroy behavior changed
on the cross-CPU work submission path.

### 36.9 Files changed (Stage 18)

| File | Change |
|------|--------|
| `src/kernel/boot/memory_state.rs` | Fix `destroy_user_address_space_by_asid` ordering; add `asid_is_live_for_test`, `asid_is_retired_for_test`, `mapped_page_count_for_asid` test helpers |
| `src/kernel/boot/cnode_state.rs` | `active_transfer_count_for_pid` test helper |
| `src/kernel/boot/tests.rs` | 20 new Stage 18 tests |
| `doc/KERNEL_LOCKING.md` | §36 (this section); §35.11 Stage 17 acceptance |
| `doc/KERNEL_TEST_RULES.md` | Rule N+25 (Stage 18 test rules) |

---

## §37 — Stage 19: capability/cnode lifetime audit + cap_refcount symmetry

### 37.1 Scope

Full audit of the capability/cnode domain focusing on:

- `cap_refcount` increment/decrement symmetry for `MemoryObject` and `DmaRegion` caps.
- Correctness of the `grant_capability_task_to_task_with_rights` rollback path.
- `Reply` cap behavior: no `cap_refcount` side-effects.
- `revoke_reply_caps_for_caller` and task exit cleanup.
- `cnode_teardown` cascade: `revoke_capability_in_cnode` cascades to delegated descendants.
- `fork` cap inheritance: `cap_refcount` incremented per inherited cap.

### 37.2 Confirmed-correct invariants (no bug)

| Path | Invariant | Verified |
|------|-----------|---------|
| `adjust_memory_object_cap_refcount(CapObject::Reply{..}, delta)` | No-op: early return on `_ => return` arm | Test `reply_cap_mint_does_not_increment_memory_object_refcount` |
| `create_reply_cap_for_caller` | Mints `CapObject::Reply` cap; `cap_refcount` unchanged | Test above |
| `revoke_reply_caps_for_caller` | Clears global `ReplyCapRecord` slot; cnode cleanup handled later by `maybe_cleanup_process_cnode_for_pid` | Confirmed correct; no double-free risk |
| `ipc_reply` fast-revoke | Both replier cnode slot and caller cnode slot are cleared via `fast_revoke_reply_cap_in_cnode`; no `cap_refcount` change (Reply caps have none) | Existing test coverage |
| `inherit_parent_capabilities_for_fork` | Uses `grant_capability_task_to_task_with_rights` which calls `mint_capability_in_cnode` → `cap_refcount` incremented per inherited cap | Test `fork_cap_inheritance_increments_refcount` |
| `revoke_capability_in_cnode` cascade | Revoking a source cap cascades to all delegated descendants via `collect_delegated_descendants`, decrementing `cap_refcount` for each | Test `cnode_teardown_releases_all_cap_refcounts` |

### 37.3 Bug confirmed and fixed

**`grant_capability_task_to_task_with_rights` rollback leaks `cap_refcount`**

Location: `src/kernel/boot/capability_state.rs`, rollback path in `grant_capability_task_to_task_with_rights`.

Pre-fix behavior: when `record_delegated_capability_link` returned `CapabilityFull` (delegation link table full), the rollback called `fast_revoke_reply_cap_in_cnode` to clear the cnode slot, but did NOT call `adjust_memory_object_cap_refcount(attenuated.object, -1)`. This left `cap_refcount` permanently inflated by 1 for each failed delegation attempt.

Impact: repeated failed delegation (e.g. during fork with a full link table) would accumulate cap_refcount inflation, preventing `reclaim_memory_object_if_unreferenced` from ever freeing the frame even after all real references were dropped.

Fix: after `fast_revoke_reply_cap_in_cnode` returns `true` (slot was cleared), call `adjust_memory_object_cap_refcount(attenuated.object, -1)` and `reclaim_memory_object_if_unreferenced(attenuated.object)` to maintain symmetry with the earlier `mint_capability_in_cnode` call.

```rust
let revoked = self.fast_revoke_reply_cap_in_cnode(dest_cnode, delegated_cap, attenuated.object);
if revoked {
    self.adjust_memory_object_cap_refcount(attenuated.object, -1);
    self.reclaim_memory_object_if_unreferenced(attenuated.object);
}
```

### 37.4 `exit_task` vs `mark_task_dead` cnode cleanup distinction

`exit_task` sets status to `TaskStatus::Exited(code)` and calls `revoke_reply_caps_for_caller`, but does NOT call `maybe_cleanup_process_cnode_for_pid`.

`mark_task_dead` sets status to `TaskStatus::Dead` and calls `maybe_cleanup_process_cnode_for_pid` directly.

`maybe_cleanup_process_cnode_for_pid` guards on `status != TaskStatus::Dead`; if any thread in the process group has status `Exited` (not `Dead`), the guard triggers and cleanup is skipped. Cnode teardown requires `mark_task_dead` (or the process-level supervisor cleanup path that calls it).

### 37.5 Stage 19 test inventory

| Test | Verifies |
|------|---------|
| `cap_refcount_increment_on_mint` | `mint_capability_in_cnode` increments `cap_refcount` to 1 |
| `cap_refcount_decrement_on_revoke` | `revoke_capability_in_cnode` decrements to 0 and reclaims |
| `reply_cap_mint_does_not_increment_memory_object_refcount` | `adjust_memory_object_cap_refcount` is a no-op for `CapObject::Reply` |
| `revoke_reply_cap_record_clears_global_slot` | `revoke_reply_caps_for_caller` clears global slot; idempotent on second call |
| `task_exit_clears_reply_cap_records` | `exit_task` calls `revoke_reply_caps_for_caller` |
| `cnode_teardown_releases_all_cap_refcounts` | `mark_task_dead` → cnode teardown cascades through delegated descendants; MemoryObject reclaimed |
| `double_revoke_capability_is_safe` | Double revoke returns `Err`, no panic, no underflow |
| `fork_cap_inheritance_increments_refcount` | Fork: child inherits cap → `cap_refcount` = 2 |
| `grant_cap_with_rights_link_fail_decrements_cap_refcount` | Rollback restores `cap_refcount` when delegation link table is full (regression for Stage 19 bug fix) |

### 37.6 Stage 19 acceptance

757 tests pass single-threaded (`cargo test --lib -- --test-threads=1`).
9 new Stage 19 tests added (748 Stage 18 baseline + 9 = 757).
`cargo check --no-default-features` clean.
`cargo check --features hosted-dev` clean.
x86_64 -smp 1 smoke not required: Stage 19 changes are pure capability/refcount
accounting in hosted-dev paths; no cross-CPU or live-boot behavior changed.

### 37.7 Files changed (Stage 19)

| File | Change |
|------|--------|
| `src/kernel/boot/capability_state.rs` | Fix rollback path in `grant_capability_task_to_task_with_rights`: add `adjust_memory_object_cap_refcount` + `reclaim_memory_object_if_unreferenced` after successful fast-revoke |
| `src/kernel/boot/tests.rs` | 9 new Stage 19 cap/cnode domain tests |
| `doc/KERNEL_LOCKING.md` | §37 (this section); §36.8 Stage 18 acceptance recorded above |
| `doc/KERNEL_TEST_RULES.md` | Rule N+26 (Stage 19 test rules) |

### 37.8 Stage 19 acceptance (consolidated record)

Recorded here for the Stage 20 audit trail.

- **Accepted without x86_64 smoke.** Reason: pure `cap_refcount` accounting; no
  cross-CPU path and no live boot/runtime path changed. 757 tests pass.
- **Bug:** `grant_capability_task_to_task_with_rights` rollback leaked
  `cap_refcount` for `MemoryObject`/`DmaRegion` caps when the delegation link
  table was full. `mint_capability_in_cnode` had already incremented
  `cap_refcount`, but the rollback used `fast_revoke_reply_cap_in_cnode` (which
  clears the cnode slot only and intentionally does **not** touch `cap_refcount`),
  leaving a permanent `+1` leak on every link-table-full grant.
- **Fix:** after the successful `fast_revoke_reply_cap_in_cnode` in the rollback
  path, explicitly call `adjust_memory_object_cap_refcount(object, -1)` followed
  by `reclaim_memory_object_if_unreferenced(object)`, guarded on the `bool` return
  of the fast-revoke so the decrement only happens when a slot was actually
  cleared.
- **Deferred (still deferred at Stage 20):** x86_64 SMP / `smp.rs`; RAMFS/FAT
  runtime spawning.

---

## §38 — Stage 20: IPC cap-transfer / reply-cap / transfer-envelope lifetime hardening

Stage 20 audits the IPC-mediated capability lifetime: reply caps, transfer caps,
transfer envelopes, recv-v2 cap materialization, blocked send/call/recv paths,
timeout/error cleanup, exit/death cleanup, cnode-cleanup interaction, and
`MemoryObject cap_refcount` symmetry during transfer.

### 38.1 IPC cap lifetime domain map

| Object | Global registry | Receiver-cnode slot | Lifetime owner | Consumed/cleaned by |
|--------|-----------------|---------------------|----------------|---------------------|
| Reply cap | `ipc.reply_caps[idx]` + `reply_cap_generations[idx]` | minted on recv-materialize (`waiter_cap_id`) and on caller mint (`caller_cap_id`) | caller (record), replier+caller (cnode slots) | `ipc_reply` (consume record + fast-revoke both slots), `revoke_reply_caps_for_caller` (exit/death/restart), `maybe_cleanup_process_cnode_for_pid` (cnode) |
| Transfer envelope | `ipc.transfer_envelopes[idx]` + `transfer_envelope_generations[idx]` | n/a (handle only) | source task (until taken) | `take_transfer_envelope` (recv materialize / error cleanup), `purge_transfer_envelopes_for_pid` (process teardown) |
| Transfer cap (materialized) | n/a | minted into receiver cnode via grant | receiver task | `revoke_capability_in_cnode` (cnode teardown / rollback), Stage 20 `rollback_materialized_recv_cap` on failed copy |
| Active transfer mapping | `ipc.active_transfer_mappings[idx]` | n/a | owner task | `remove_active_transfer_mapping`, `revoke_active_transfer_mappings_for_cap`, `purge_active_transfer_mappings_for_pid` |

### 38.2 Lock-rank table (IPC cap paths)

Rank order (must not invert): scheduler(1) < task(2) < ipc(3) < capability(4) < vm(5) < memory(6).

| Operation | Locks acquired (in order) | Notes |
|-----------|---------------------------|-------|
| `stash_transfer_envelope` | ipc(3) read, memory(6) (validate + pin), ipc(3) write | cap resolve via capability(4) read between |
| `take_transfer_envelope` | ipc(3) read, memory(6) (unpin), ipc(3) write | no cap mint/revoke here |
| `materialize_received_*` (transfer) | ipc(3) take, capability(4) grant (mint + link), memory(6) refcount | mint done **outside** ipc lock |
| `materialize_received_*` (reply) | ipc(3) take, ipc(3) liveness, capability(4) mint, ipc(3) set waiter_cap | mint outside ipc lock |
| `rollback_materialized_recv_cap` | capability(4) resolve, capability(4) revoke/fast-revoke, memory(6) refcount, ipc(3) clear waiter_cap | inverse of materialize; no lock held across mint/revoke |
| `ipc_reply` | task(2), ipc(3) consume record, capability(4) fast-revoke ×2, ipc(3) deliver | record consumed under ipc(3); fast-revoke under capability(4) — never simultaneously held |
| `create_reply_cap_for_caller` | ipc(3) reserve slot, capability(4) mint, ipc(3) persist cap_id | mint outside ipc lock |

**Invariant 8 confirmed:** no cap mint or revoke is performed while `ipc_state_lock`
is held. All mint/revoke calls (`mint_capability_in_cnode`,
`revoke_capability_in_cnode`, `fast_revoke_reply_cap_in_cnode`,
`grant_*`) run outside `with_ipc_state*` closures.

### 38.3 Transfer-envelope lifecycle table

| State | Set by | pin_refcount delta (shared_region) | Transition |
|-------|--------|-----------------------------------|------------|
| Created | `stash_transfer_envelope` | `+1` | → MappedReceiver / Released / Revoked |
| Released | `take_transfer_envelope` (transition guard) | `-1` | terminal (slot cleared) |
| (purge) | `purge_transfer_envelopes_for_pid` | `-1` if shared_region | slot cleared, telemetry revoked++ |

Generation-tagged handle (`(generation << 16) | idx`) makes a stale/replayed
handle return `None` from `take_transfer_envelope` → consumed/cleaned exactly once.

### 38.4 Reply-cap lifecycle table

| Phase | Function | Global record | caller cnode | replier/waiter cnode |
|-------|----------|---------------|--------------|----------------------|
| create | `create_reply_cap_for_caller` | reserve + set `caller_cap_id` | mint (SEND) | — |
| materialize | `materialize_received_message_cap` (reply branch) | set `waiter_cap_id` | — | mint (SEND) |
| reply | `ipc_reply` | consume (`= None`) | fast-revoke `caller_cap_id` | fast-revoke `waiter_cap_id` |
| rollback (Stage 20) | `rollback_materialized_recv_cap` | clear `waiter_cap_id` (record stays live) | — | fast-revoke waiter slot |
| exit/death/restart | `revoke_reply_caps_for_caller` | clear records where task is caller | (cnode at death) | (cnode at death) |

Generation bump on reuse (`reply_cap_generations[idx]`) makes a stale reply CapId
resolve to `StaleCapability`/`InvalidCapability` → reply cap is one-shot.

### 38.5 cap_refcount transition table

| Path | cap delta | envelope delta | reply-cap delta | owner | cleanup trigger | lock order | tests |
|------|-----------|----------------|-----------------|-------|-----------------|------------|-------|
| IpcSend cap transfer success | +1 dest | consumed | none | dest task | — | ipc→cap | `stage20_transfer_cap_materialize_success_sets_cap_refcount_to_two` |
| IpcSend cap transfer failure | 0 | cleaned | none | source task | error return | ipc→cap | `stage20_failed_transfer_send_cleans_envelope_and_keeps_refcount` |
| IpcCall reply cap creation | 0 | none | +1 | caller | ipc_reply/revoke | cap | `stage20_reply_cap_creation_does_not_change_memory_cap_refcount` |
| IpcCall timeout/error | 0 | cleaned | −1 | caller | timeout/error | cap | `reply_cap_record_is_single_use_and_routes_reply_to_bound_endpoint` |
| IpcReply with cap transfer | +1 dest | consumed | −1 | dest task | — | ipc→cap | `stage20_reply_cap_double_revoke_is_idempotent` |
| recv-v2 blocked delivery | +1 dest | consumed | — | dest task | waiter wake | ipc→cap | `stage20_transfer_cap_materialize_success_sets_cap_refcount_to_two` |
| recv copy-fault after materialize (Stage 20 fix) | 0 (rolled back) | consumed | clear waiter_cap | dest task | copy fault | cap→ipc | `stage20_rollback_materialized_transfer_cap_restores_cap_refcount`, `stage20_rollback_materialized_reply_cap_clears_slot_and_waiter_id` |
| send timeout | 0 | cleaned | none | sender | timer | ipc→cap | `process_ipc_timeout_deadlines` (existing) |
| task exit while waiting | 0 | cleaned | −1 | task | exit_task/death | ipc→cap | `process_cleanup_purges_transfer_envelopes_and_unpins_memory` (existing) |
| cnode teardown | 0 | cleaned | −1 | task | mark_task_dead | cap | `process_cleanup_*` (existing) |
| double rollback / double take | 0 (no underflow) | no-op | no-op | — | idempotent | cap/ipc | `stage20_rollback_materialized_transfer_cap_double_call_is_harmless`, `stage20_transfer_envelope_double_take_is_harmless` |

### 38.6 Production bug fixed (Stage 20)

**Bug — recv-v2 cap materialized before failable user-memory copy, with no
rollback.** Both recv-delivery paths materialize the transferred/reply cap into
the receiver's cnode (and consume the transfer envelope) **before** the
metadata/payload `copy_to_user` that can fault:

- `complete_blocked_recv_for_waiter` (blocked-waiter delivery): cap materialized,
  then meta `copy_to_user` could return `Err` with no rollback —
  `src/kernel/syscall.rs` (meta-copy failure branch).
- `handle_ipc_recv_result_with_empty_error` (immediate recv): cap materialized,
  then recv-v2 meta `copy_to_current_user` (`?`) and the undersized-buffer
  (`user_len < app_payload.len()`) branch returned `Err` with no rollback.

When the copy faulted the message was dropped and the receiver stayed blocked,
but the freshly-minted cap leaked in the receiver's cnode — an asymmetric
`cap_refcount`/cnode-slot leak — and for Reply caps a dangling global
`waiter_cap_id` pointed at an orphaned slot (which `ipc_reply` would then attempt
to fast-revoke).

**Fix.** Added `KernelState::rollback_materialized_recv_cap`
(`src/kernel/boot/transfer_state.rs`), the inverse of the materialization mint:

- Reply cap → `fast_revoke_reply_cap_in_cnode` (no `cap_refcount`) + clear the
  global `waiter_cap_id` via the new generation-guarded
  `clear_reply_cap_waiter_cap` (`src/kernel/boot/ipc_state.rs`). The
  `ReplyCapRecord` stays live (the reply was never consumed by the mint), so the
  reply remains re-deliverable.
- Transfer cap → `revoke_capability_in_cnode` (removes delegation link,
  decrements `cap_refcount`, reclaims if unreferenced).

Wired into all three post-materialization copy-failure branches. Double-call is
harmless (returns `false`, never underflows) because the slot lookup fails on the
second call.

### 38.7 Invariants enforced (Stage 20)

1. Transfer envelope consumed exactly once or cleaned exactly once
   (generation-tagged handle; double take → `None`).
2. Reply cap is one-shot — cannot be reused after revoke/materialization
   (record consumed + generation bump).
3. Failed cap transfer restores `MemoryObject cap_refcount` (Stage 19 grant
   rollback + Stage 20 materialize rollback).
4. Failed materialization/copy leaves no cnode slot leak (Stage 20 rollback).
5. Timeout cleanup removes waiter and cap/envelope state
   (`process_ipc_timeout_deadlines` + `take`/`purge`).
6. Exit/death cleanup removes waiter and reply-cap/envelope state
   (`revoke_reply_caps_for_caller` + `maybe_cleanup_process_cnode_for_pid`).
7. No cap delivered to Dead/Exited/Missing task — `rollback_materialized_recv_cap`
   returns `false` when `task_cnode` is missing; `clear_ipc_waiters_for_tid`
   removes dead tasks from waiter slots before delivery.
8. No cap mint/revoke under `ipc_state_lock` (see §38.2).
9. recv-v2 metadata/reply-cap behavior unchanged on the success path; rollback
   only affects the failure (dropped-message) path.
10. Cap-transfer preserves payload bytes and opcode semantics (materialize
    happens after payload validation; rollback does not alter delivered bytes).
11. Active-transfer mappings remain two-phase and refcount-safe (registration does
    not touch `cap_refcount`).
12. Double cleanup is harmless / returns a stable error, never underflows.

### 38.8 Files changed (Stage 20)

| File | Change |
|------|--------|
| `src/kernel/boot/transfer_state.rs` | `rollback_materialized_recv_cap` helper (inverse of recv-materialize mint) |
| `src/kernel/boot/ipc_state.rs` | `clear_reply_cap_waiter_cap` (generation-guarded); `#[cfg(test)]` reply-record accessors |
| `src/kernel/syscall.rs` | rollback wired into `complete_blocked_recv_for_waiter` (meta-copy fault) and `handle_ipc_recv_result_with_empty_error` (meta-copy fault + undersized buffer) |
| `src/kernel/boot/tests.rs` | 10 new Stage 20 tests |
| `doc/KERNEL_LOCKING.md` | §37.8 Stage 19 acceptance (consolidated); §38 (this section) |
| `doc/KERNEL_TEST_RULES.md` | Rule N+27 (Stage 20 test rules) |
| `doc/CAPABILITY_MODEL.md` | IPC cap-transfer lifetime rules |

### 38.9 Stage 20 acceptance

767 tests pass single-threaded (757 Stage 19 baseline + 10 = 767).
`cargo check --no-default-features` clean. `cargo check --features hosted-dev`
clean. `git diff --check` clean.

x86_64 `-smp 1` smoke **not required**: the only behavioral change is on the IPC
recv copy-fault error path (a previously-leaking slot is now revoked); the success
path, syscall ABI, `SYSCALL_COUNT`, and SpawnV5 semantics are unchanged; no
cross-CPU, trap/timer/bootstrap, SMP, or live boot path is touched.

**Deferred (still deferred at Stage 20):** x86_64 SMP / `smp.rs`; RAMFS/FAT
runtime spawning; full global-lock removal.

### 38.10 Stage 20 acceptance (consolidated record)

Recorded here for the Stage 21 audit trail (mirrors §37.8 for Stage 19).

- **Baseline:** 757 tests (Stage 19) → **767 tests** (Stage 20), single-threaded,
  `RUST_MIN_STACK=8388608`.
- **Bug fixed:** Recv-v2 cap materialization rollback — a failed `copy_to_user`
  after materializing a transferred cap left a populated cnode slot, an inflated
  `MemoryObject cap_refcount`, and a stale reply-cap `waiter_cap_id`. The Stage 20
  fix (`rollback_materialized_recv_cap`) clears the cnode slot, restores
  `cap_refcount`, and clears the stale `waiter_cap_id`.
- **Checks:** `cargo check --no-default-features` clean; `cargo check --features
  hosted-dev` clean; `git diff --check` clean.
- **x86_64 smoke:** not required — change is confined to the IPC recv copy-fault
  error path; success path, syscall ABI, `SYSCALL_COUNT`, and SpawnV5 unchanged.
- **Invariants:** all Stage 12–20 VM/COW/TLB/task/wake/cap/IPC behavior preserved;
  recv-v2 / reply-cap / transfer-envelope semantics preserved.

---

## §39 — Stage 21: notification / IRQ-route lifetime audit + waiter/route cleanup hardening

Stage 21 audits the lifetime of **notification objects** and **IRQ routes**: how a
notification is created, signalled, waited on, and (newly) destroyed, and how IRQ
routes that target a notification are bound and torn down. The focus is on
preventing a wake from resurrecting a dead task and preventing an IRQ route from
outliving the notification it points at.

### 39.1 Notification / IRQ data model

| Field (`IpcState`) | Shape | Role |
| --- | --- | --- |
| `notifications[idx]` | `Option<NotificationObject>` | pending-IRQ ring buffer (FIFO) |
| `notification_waiters[idx]` | `Option<ThreadId>` | single registered waiter TID |
| `notification_generations[idx]` | `u64` | liveness epoch; bumped on create-reuse and on destroy |
| `irq_routes[irq_line]` | `Option<usize>` | hardware IRQ line → notification slot index |

All four live under `ipc_state_lock` (rank 3). Capability minting for the
notification's SIGNAL/RECEIVE caps happens **outside** the lock (cap rank 4 > ipc
rank 3), preserving the create-two-phase pattern from §38.

### 39.2 Lifetime paths audited

| Path | Lock domains | Wake under lock? | Stale handling |
| --- | --- | --- | --- |
| `create_notification` | ipc (slot+gen+object), then cap (mint) | n/a | bumps gen; Stage 21 also sanitises stale waiter + routes on slot reuse |
| `bind_irq_notification` | cap (resolve), ipc (route store) | n/a | requires SIGNAL right; generation-checked via `resolve_notification_index` |
| `route_external_irq` → `signal_notification` | ipc (send_irq + waiter snapshot), then task (wake) | **no** — waiter woken after `ipc_state_lock` released | Stage 21: dead/exited/missing waiter is a no-op; destroyed-target IRQ is a swallowed no-op |
| `ipc_recv` (notification) | ipc (immediate recv), task (block), ipc (publish waiter) | no | publishes waiter after TCB marked Blocked |
| `clear_ipc_waiters_for_tid` (exit/death/timeout) | ipc | n/a | clears `notification_waiters` entry for the TID |
| `destroy_notification` (Stage 21 new) | ipc (route teardown + waiter clear + object remove + gen bump) | n/a | atomic teardown; returns snapshotted waiter for out-of-lock unblock |

### 39.3 Bugs fixed (Stage 21)

1. **`signal_notification` resurrected dead waiters** (`ipc_state.rs`,
   the Phase-2 wake block). The waiter TID snapshotted from
   `notification_waiters` was transitioned `→ Runnable` and enqueued
   **unconditionally**, without checking `TaskStatus`. A TID that had since been
   exited/killed (or already woken by a timeout) would be resurrected or
   double-enqueued. **Fix:** only wake when `matches!(tcb.status,
   Blocked(_))`; missing TID and all non-Blocked states are silent no-ops —
   mirroring `apply_cross_cpu_wake_task` and the WakeTask work-item guard.

2. **No notification destroy / IRQ-route teardown path existed.** Notifications
   were created (gen bumped on slot reuse) but never freed, and `irq_routes`
   entries were never torn down. A future free-and-reuse of a slot would let a
   stale route silently retarget the new (different-generation) notification, and
   a leftover waiter could be re-targeted. **Fix:** added
   `destroy_notification`, which under a single `ipc_state_lock` critical section
   (a) drops every route targeting the slot, (b) clears the waiter, (c) removes
   the object and bumps the generation (invalidating any surviving cap via
   `capability_object_live` / `resolve_notification_index`). `create_notification`
   additionally sanitises stale waiter/routes on slot reuse as defence-in-depth,
   and `route_external_irq` treats a destroyed-target (`WrongObject`) as a benign
   no-op rather than a kernel error.

### 39.4 Invariants enforced (Stage 21)

1. A notification signal may only transition a waiter `Blocked(_) → Runnable`;
   it never resurrects `Dead`/`Exited` or double-enqueues `Runnable`/`Running`.
2. An IRQ route never outlives its target notification (`destroy_notification`
   clears routes before removing the object, under one lock).
3. A destroyed notification's caps are non-live (generation bump).
4. A reused notification slot starts with no stale waiter and no stale route.
5. A hardware IRQ to a destroyed/absent route target is a no-op, not an error.
6. The waiter wake happens **outside** `ipc_state_lock` (rank 3 → task rank 2,
   no inversion); the route/object/generation teardown is atomic under it.

### 39.5 Stage 21 test inventory

| Test | Asserts |
| --- | --- |
| `stage21_signal_wakes_waiting_task_exactly_once` | Blocked waiter woken once; slot consumed; second IRQ no double-wake |
| `stage21_signal_skips_dead_waiter_safely` | Dead waiter not resurrected |
| `stage21_signal_skips_exited_waiter_safely` | Exited waiter not resurrected |
| `stage21_exit_task_clears_notification_waiter` | `exit_task` clears waiter slot |
| `stage21_mark_task_dead_clears_notification_waiter` | `mark_task_dead` clears waiter slot |
| `stage21_destroy_notification_clears_waiter_and_invalidates_caps` | destroy clears waiter, removes object, bumps gen, caps non-live |
| `stage21_signal_before_wait_leaves_pending_for_later_recv` | pending IRQ delivered to later recv |
| `stage21_wait_before_signal_registers_then_wakes` | waiter registered then woken by signal |
| `stage21_repeated_signal_accumulates_pending` | repeated signals queue FIFO, drained by recv |
| `stage21_irq_route_registration_and_teardown` | route registered by bind, torn down by destroy |
| `stage21_irq_delivery_to_destroyed_notification_is_safe_noop` | IRQ to destroyed target is a no-op (incl. forced stale route) |

### 39.6 Files changed (Stage 21)

| File | Change |
| --- | --- |
| `src/kernel/boot/ipc_state.rs` | `signal_notification` Blocked-status wake guard; new `destroy_notification`; `create_notification` slot-reuse sanitise; `route_external_irq` destroyed-target no-op |
| `src/kernel/boot/tests.rs` | 11 new Stage 21 notification/IRQ-route lifetime tests + `stage21_block_on_notification` helper |
| `doc/KERNEL_LOCKING.md` | §38.10 Stage 20 acceptance (consolidated); §39 (this section) |
| `doc/KERNEL_TEST_RULES.md` | Rule N+28 (Stage 21 test rules) |

### 39.7 Stage 21 acceptance

778 tests pass single-threaded (767 Stage 20 baseline + 11 = 778),
`RUST_MIN_STACK=8388608`. `cargo check --no-default-features` clean.
`cargo check --features hosted-dev` clean. `git diff --check` clean.

x86_64 `-smp 1` smoke **not required**: the only behavioral change is on the
notification wake/destroy paths — `signal_notification` now refuses to wake a
non-Blocked waiter (strictly safer), and `destroy_notification` is a new
teardown primitive not yet wired into any live boot path. Syscall ABI,
`SYSCALL_COUNT`, SpawnV5, trap/timer/bootstrap, SMP, and the live IRQ-delivery
success path (`route_external_irq` for a live, bound notification) are unchanged.

**Deferred (still deferred at Stage 21):** wiring `destroy_notification` into a
capability-revoke / process-teardown path; x86_64 SMP / `smp.rs`; RAMFS/FAT
runtime spawning; full global-lock removal.

## 40. Stage 22 — wire `destroy_notification` into cap revoke / cnode teardown

### 40.0 Stage 21 acceptance (recorded; confirmation)

Stage 21 was **accepted without x86_64 smoke** — see §39.7. Recap of why:
the only behavioral changes were on the notification wake/destroy paths.
`signal_notification` was made *stricter* (it now wakes only a `Blocked(_)`
waiter — strictly safer, never looser), and `destroy_notification` was added as
a new teardown primitive that, at Stage 21, was **not yet on any live boot
path**. The `create_notification` slot-reuse sanitise and the
`route_external_irq` destroyed-target no-op are defence-in-depth. x86_64 SMP /
`smp.rs` remained deferred; RAMFS/FAT runtime spawning remained deferred.

### 40.1 Goal

Stage 21 left `destroy_notification` unwired: a revoked Notification cap, a
cnode teardown, or a task exit freed the cap slot but **leaked** the underlying
`notifications[idx]` object and left any `irq_routes` entry targeting it intact.
Stage 22 wires the teardown into the capability-revoke path so that destroying a
Notification cap also frees the object, tears down IRQ routes, clears the
waiter, and bumps the generation.

### 40.2 Ownership semantics decision

**Notification caps are single-owner per object: revoke → destroy immediately.**

Audit findings:
- `create_notification` mints **exactly two** caps for one object — a `SIGNAL`
  cap and a `RECEIVE` cap — both into the creator's active cnode. It never mints
  more, and it is never invoked from a live syscall handler (only tests / test
  scenarios / posix-compat sim).
- There is **no** `notification_refcount` field on `IpcSubsystem`
  (`defs.rs:247–250`); MemoryObject/DmaRegion are the only cap objects with
  `cap_refcount`.
- `grant_capability_task_to_task_with_rights` is **not** invoked with a
  Notification cap anywhere in non-test code; Notification caps are not granted
  cross-process.

Because there is no refcount and notifications are not delegated cross-process,
revoking **any** Notification cap destroys the object. The paired second cap (and
any double-revoke) re-enters with the object slot already `None`, which
`destroy_notification` reports as `WrongObject` — swallowed as a benign no-op, so
there is no double-destroy and no spurious second generation bump.

### 40.3 Notification cap lifecycle map

| path | cap Δ | object Δ | waiter Δ | route Δ | gen Δ | lock order | tests |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `create_notification` | mints SIGNAL+RECEIVE | allocates slot | sanitised → None | sanitised → None | bump | ipc (rank 3) then cap (rank 4) | `create_notification_both_domains_visible_after_two_phase_create` |
| Notification cap revoke (`revoke_capability_in_cnode`) | slot cleared + gen-bumped | **freed** | cleared + woken | **torn down** | bump | cap (rank 4) released → ipc (rank 3) | `stage22_notification_cap_revoke_destroys_object` |
| Notification cap release (`revoke_capability_direct_in_process_cnode`) | slot cleared | **freed** (idempotent) | cleared + woken | **torn down** | bump (first only) | cap (rank 4) released → ipc (rank 3) | `stage22_cnode_teardown_destroys_notification_object` |
| cnode teardown (`maybe_cleanup_process_cnode_for_pid`) | every live cap revoked in a loop | **freed** via revoke loop | cleared + woken | **torn down** | bump | per-revoke cap → ipc | `stage22_cnode_teardown_destroys_notification_object` |
| task exit cnode cleanup (`mark_task_dead`/`exit_task` → teardown) | inherits revoke loop | **freed** | cleared + woken | **torn down** | bump | per-revoke cap → ipc | `stage22_cnode_teardown_destroys_notification_object` |
| `destroy_notification` direct | n/a | freed | cleared (returned) | torn down | bump | ipc (rank 3) | `stage21_destroy_notification_clears_waiter_and_invalidates_caps` |
| IRQ route register (`bind_irq_notification`) | requires SIGNAL | unchanged | unchanged | set | n/a | cap (resolve) → ipc (store) | `stage21_irq_route_registration_and_teardown` |
| IRQ route delivery after destroy (`route_external_irq`) | n/a | unchanged (None) | unchanged | benign no-op | n/a | ipc | `stage22_route_after_revoke_is_benign_noop` |
| slot reuse after destroy (`create_notification`) | new SIGNAL+RECEIVE | reallocated | None | None | bump | ipc then cap | `stage22_create_notification_after_revoke_reuses_slot_cleanly` |

### 40.4 Lock-rank separation (cap → ipc)

The hard constraint: `destroy_notification` acquires `ipc_state_lock` (rank 3),
and it must never be reached while `capability_state_lock` (rank 4) is held —
acquiring rank 3 under rank 4 inverts the global ordering
`scheduler(1) < task(2) < ipc(3) < capability(4) < vm(5) < memory(6)`.

| step | lock held | rank |
| --- | --- | --- |
| read source cap from cnode | `capability_state_lock` | 4 |
| revoke cap from cnode (clear slot, bump cnode gen) | `capability_state_lock` | 4 |
| **release** `capability_state_lock` | — | — |
| `adjust_memory_object_cap_refcount` / `reclaim_…` (MemoryObject only) | memory | 6 (separate) |
| `destroy_notification_for_revoked_cap` → `destroy_notification` | `ipc_state_lock` | 3 |
| `wake_destroyed_notification_waiter` (Blocked-only) | task | 2 |

This mirrors exactly the existing MemoryObject teardown: both
`adjust_memory_object_cap_refcount` and the new Notification teardown run
**after** the cap-lock critical section closes, each acquiring only its own
domain lock.

### 40.5 `destroy_notification` callsite table

| caller | when | wake target handling |
| --- | --- | --- |
| `revoke_capability_in_cnode` → `destroy_notification_for_revoked_cap` | a Notification cap is revoked (after cap-lock release) | snapshotted waiter woken via `wake_destroyed_notification_waiter` |
| `revoke_capability_direct_in_process_cnode` → `destroy_notification_for_revoked_cap` | a delegated/direct Notification cap is revoked | same |
| (transitively) `maybe_cleanup_process_cnode_for_pid` | cnode teardown loop calls `revoke_capability_in_cnode` per live cap | same |
| Stage 21 tests | direct primitive exercise | test asserts returned waiter |

### 40.6 Waiter wake decision

When `destroy_notification` returns `Some(waiter_tid)`, Stage 22 **wakes** the
waiter (Option C-ish): `wake_destroyed_notification_waiter` transitions the task
`Blocked(_) → Runnable` and enqueues it — but **only** if it is still
`Blocked(_)`, reusing the exact Blocked-only gate from `signal_notification`
(§39.4 invariant 1). A Dead/Exited/Runnable/Running/Faulted task is a no-op; a
missing TID is a no-op. The woken task re-resolves its RECEIVE cap and observes
the destroyed object (generation mismatch → `StaleCapability`/`WrongObject`), so
no separate cancellation error code is needed. This prevents a waiter from being
left parked forever on an object that no longer exists.

### 40.7 IRQ route teardown table

| event | route state | delivery outcome |
| --- | --- | --- |
| `bind_irq_notification(line, cap)` | `irq_routes[line] = Some(idx)` | live notification signalled |
| Notification cap revoke | `irq_routes[line] = None` (inside `destroy_notification`) | route gone → immediate no-op |
| forced stale route after revoke (defence-in-depth) | `irq_routes[line] = Some(idx)` but object `None` | `signal_notification` → `WrongObject` swallowed → benign no-op |
| slot reuse via `create_notification` | stale routes to slot pre-cleared | no cross-generation retarget |

### 40.8 Stale cap / generation behavior

- After revoke, the pre-revoke generation fails `capability_object_live` and
  `resolve_notification_index` (generation mismatch).
- A `try_ipc_recv` via the stale RECEIVE cap returns a **stable** error
  (`StaleCapability`), repeatable across calls, never a panic.
- A second revoke of the now-empty cnode slot returns `InvalidCapability` (slot
  cleared); `destroy_notification`'s `WrongObject` is swallowed, so the
  generation is **not** bumped a second time (no double-destroy).

### 40.9 Fixes made (Stage 22)

1. **Revoked Notification objects / IRQ routes leaked.**
   `revoke_capability_in_cnode` (`capability_lifecycle_state.rs:332–335`) and
   `revoke_capability_direct_in_process_cnode` (`…:398–402`) handled only
   MemoryObject/DmaRegion refcount teardown; a revoked Notification cap left
   `notifications[idx]`, its `irq_routes` entries, and any parked waiter intact.
   **Fix:** both paths now call the new
   `destroy_notification_for_revoked_cap(object)` after the cap-lock critical
   section closes, which invokes `destroy_notification(index)` for
   `CapObject::Notification` and wakes the returned waiter. Cnode teardown and
   task exit inherit the fix because `maybe_cleanup_process_cnode_for_pid` revokes
   every live cap through `revoke_capability_in_cnode`.

### 40.10 Helpers added (Stage 22)

| helper | file | role |
| --- | --- | --- |
| `destroy_notification_for_revoked_cap` | `capability_lifecycle_state.rs` | match `CapObject::Notification`, call `destroy_notification`, wake waiter; idempotent (swallows `WrongObject`/`None`) |
| `wake_destroyed_notification_waiter` | `ipc_state.rs` | Blocked-only `→ Runnable` + enqueue of a destroyed-notification waiter, outside ipc/cap locks |

### 40.11 Invariants enforced (Stage 22)

1. Revoking any Notification cap frees the object, tears down its routes, clears
   and wakes its waiter, and bumps the generation — atomically per
   `ipc_state_lock` critical section.
2. `destroy_notification` is reached only **after** `capability_state_lock` is
   released, preserving the cap(4) → ipc(3) rank ordering.
3. Teardown is idempotent: the paired cap / double-revoke is a benign no-op, no
   double-destroy, no spurious second generation bump.
4. The destroyed-notification waiter wake reuses the Stage 21 Blocked-only gate;
   no Dead/Exited resurrection, no double-enqueue.
5. An unrelated notification is wholly unaffected by another's revoke.
6. Live IRQ delivery success path (`route_external_irq` to a live, bound
   notification) is unchanged.

### 40.12 Stage 22 test inventory

| Test | Asserts |
| --- | --- |
| `stage22_notification_cap_revoke_destroys_object` | revoke frees object + bumps gen + cap non-live |
| `stage22_notification_cap_revoke_tears_down_irq_route` | revoke clears `irq_routes` entry |
| `stage22_notification_cap_revoke_clears_waiter` | revoke clears waiter + wakes Blocked→Runnable |
| `stage22_cnode_teardown_destroys_notification_object` | `mark_task_dead` → teardown frees object |
| `stage22_signal_after_revoke_is_stable_noop` | post-revoke external IRQ is a repeatable no-op |
| `stage22_wait_after_revoke_is_stable_error` | post-revoke recv via stale cap = stable error |
| `stage22_route_after_revoke_is_benign_noop` | forced stale route to destroyed object = no-op |
| `stage22_create_notification_after_revoke_reuses_slot_cleanly` | reused slot: fresh gen, no route, no waiter, live cap |
| `stage22_double_revoke_notification_cap_is_safe` | second revoke = safe error, no double-destroy |
| `stage22_notification_cap_revoke_does_not_affect_unrelated_notification` | unrelated notification intact + still delivers |

### 40.13 Files changed (Stage 22)

| File | Change |
| --- | --- |
| `src/kernel/boot/ipc_state.rs` | removed `dead_code` attr from `destroy_notification`; added `wake_destroyed_notification_waiter` |
| `src/kernel/boot/capability_lifecycle_state.rs` | added `destroy_notification_for_revoked_cap`; called from both revoke paths after cap-lock release |
| `src/kernel/boot/tests.rs` | 10 new Stage 22 tests |
| `doc/KERNEL_LOCKING.md` | §40 (this section), incl. §40.0 Stage 21 acceptance confirmation |
| `doc/KERNEL_TEST_RULES.md` | Rule N+29 (Stage 22 test rules) |

### 40.14 Stage 22 acceptance

788 tests pass single-threaded (778 Stage 21 baseline + 10 = 788),
`RUST_MIN_STACK=8388608`. `cargo check --no-default-features` clean (no
`dead_code` warning for `destroy_notification` — it is now reachable on the
non-test revoke path). `cargo check --features hosted-dev` clean.
`git diff --check` clean.

x86_64 `-smp 1` smoke **not required**: `destroy_notification` is reached only
from the capability-revoke / cnode-teardown / task-exit cleanup paths. On the
live boot path no service revokes a Notification cap or tears down a cnode that
holds one (`create_notification` is not invoked from any syscall handler;
notifications are exercised by tests and the hosted posix-compat sim only). The
wake is gated to Blocked-only (strictly safer). Syscall ABI, `SYSCALL_COUNT`,
SpawnV5, trap/timer/bootstrap, SMP/`smp.rs`, VFS/syscall27, and the live
IRQ-delivery success path are all unchanged.

**Deferred (still deferred at Stage 22):** a live-syscall `create_notification` /
notification-destroy surface; x86_64 SMP / `smp.rs`; RAMFS/FAT runtime spawning;
full global-lock removal.

### 40.15 Stage 22 acceptance (formal record)

Recorded at Stage 23 entry; the Stage 22 commit (`b14b167`) is **accepted**.

- **Accepted without x86_64 smoke.** Reason: `destroy_notification` /
  `destroy_notification_for_revoked_cap` are **not** on the live boot path. The
  only behavior changed is cap-revoke / cnode-teardown accounting (Notification
  object + IRQ route + waiter teardown). No service revokes a Notification cap or
  tears down a cnode holding one during live boot, because `create_notification`
  has no syscall caller.
- **Wiring recorded.** `revoke_capability_in_cnode`
  (`capability_lifecycle_state.rs:335`) and
  `revoke_capability_direct_in_process_cnode` (`…:433`) both call
  `destroy_notification_for_revoked_cap(capability.object)` **after** the
  `capability_state_lock` (rank 4) critical section closes;
  `destroy_notification` then acquires `ipc_state_lock` (rank 3), preserving the
  cap(4) → ipc(3) ordering.
- **Single-owner semantics decision.** Notification caps are single-owner per
  object (creator mints exactly one SIGNAL + one RECEIVE cap into its own cnode;
  never granted cross-process; no refcount). Revoking **any** Notification cap
  destroys the underlying object immediately.
- **Idempotent double-revoke.** The paired cap / a second revoke re-enters with
  the object slot already `None`; `destroy_notification` returns `WrongObject`,
  swallowed as a benign no-op — no double-destroy, no second generation bump.
- **Teardown completeness.** IRQ routes torn down, waiter cleared + Blocked-only
  woken, object freed, generation bumped.
- **Still deferred at Stage 22:** x86_64 SMP / `smp.rs`; RAMFS/FAT runtime
  spawning; full global-lock removal; a live-syscall notification create/destroy
  surface.
- 788 tests pass single-threaded; no smoke required.

---

## §41 Stage 23 — Notification live revoke/release surface audit

Stage 23 is an **audit-and-prove** stage: it answers "is there a user-facing
capability-release / revoke syscall through which a userspace task can release a
Notification cap, and if so does the Stage 22 teardown fire through it?" — then
locks the answer down with focused tests. No production wiring changed.

### 41.1 Audit method

Searched `src/kernel/syscall.rs` and `src/kernel/boot/` for `CAP_RELEASE`,
`SYS_CAP_RELEASE`, `cap_release`, `release_capability`, `revoke_cap`, every
`revoke_capability_in_cnode` / `revoke_capability_direct_in_process_cnode`
callsite, and the syscall dispatch table + `SYSCALL_COUNT`.

### 41.2 Audit result — syscall surface

`SYSCALL_COUNT == 30`; 22 live `Syscall` variants. **No** generic
capability-release / capability-revoke syscall exists. The only release-shaped
syscall is **`TransferRelease` (NR = 4)** (`handle_transfer_release`).

`revoke_capability_in_cnode` / `revoke_capability_direct_in_process_cnode`
callsite classification:

| callsite | file:line | class | reaches Stage 22 notif teardown? |
| --- | --- | --- | --- |
| `revoke_current_transfer_cap_best_effort` (IPC recv rollback) | `syscall.rs:741` | C internal helper | no — transfer (MemoryObject) cap only |
| `handle_transfer_release` (TransferRelease NR=4) | `syscall.rs:2053` | A user syscall, but **MemoryObject-scoped** | no — gated on `active_transfer_mapping_for`, which a Notification cap never has |
| `rollback_anon_map` (un-mapped cap) | `syscall.rs:3187` | C internal rollback | no |
| `rollback_anon_map` (mapped-page cap) | `syscall.rs:3201` | C internal rollback | no |
| `maybe_cleanup_process_cnode_for_pid` loop | `cnode_state.rs:135` | B task/process-exit teardown | **yes** (revokes every live cap) |
| transfer-mapping purge | `cnode_state.rs:249` | B/C teardown | no (transfer cap) |
| `memory_state.rs` rollback sites | `memory_state.rs:118,490,497,507` | C internal rollback | no |
| `driver_state.rs` rollback sites | `driver_state.rs:527,530,533` | C internal | no |
| `thread_state.rs` child teardown | `thread_state.rs:67` | B thread-exit teardown | **yes** (inherits revoke) |
| `transfer_state.rs:162` | rollback materialized recv cap | C internal | no (transfer cap) |

**Notification-cap creation surface:** `create_notification` (`ipc_state.rs:1305`)
has **no production / syscall caller** — referenced only by `tests.rs` and the
doc. Classification for the live notification user-release surface:
**E — missing/deferred live surface.**

### 41.3 Is Notification teardown reachable from a user syscall?

**No live user *release* syscall reaches it.** `TransferRelease` is the only
user-facing release syscall and is structurally MemoryObject-scoped:
`handle_transfer_release` resolves `active_transfer_mapping_for(owner, cap)` and
returns `InvalidArgs` when there is no transfer mapping — a Notification cap has
none, so the handler errors out **before** `revoke_capability_in_cnode` is ever
called. There is no other userspace-reachable path that revokes a Notification
cap.

Notification teardown **is** reachable from the **task/process-exit cnode
teardown** path (`maybe_cleanup_process_cnode_for_pid` →
`revoke_capability_in_cnode` per live cap, and the per-thread variant in
`thread_state.rs`). That is the closest live-path equivalent, but exit teardown is
not a normal-runtime user request and — because no live service creates a
Notification cap — does not fire for notifications on the live boot path today.

### 41.4 What changed in Stage 23

Nothing in production code. The audit confirmed the Stage 22 wiring is the only
reachable teardown surface and that no user *release* syscall bypasses it (the
only candidate, `TransferRelease`, cannot target a Notification cap by
construction). Stage 23 adds focused tests pinning these facts:

- the direct revoke helper destroys object + routes (helper-level proof of the
  Stage 22 path);
- the task-exit cnode-teardown path destroys a held notification (closest live
  equivalent);
- double-revoke idempotency;
- `TransferRelease` (the live release syscall) does **not** and cannot revoke a
  Notification cap — proven by exercising `handle_transfer_release`'s gate;
- a documentation test recording that no live notification-release syscall exists.

### 41.5 No ABI change

`SYSCALL_COUNT` stays `30`; the 22 `Syscall` variants are unchanged; no syscall
number added; SpawnV5, IPC recv-v2 / reply-cap / transfer-envelope, and the live
IRQ-delivery success path are untouched.

### 41.6 Deferred (Stage 23)

A live-syscall notification create/release surface (would require a new syscall
number — explicitly **out of scope** by the hard ABI invariant); x86_64 SMP /
`smp.rs`; RAMFS/FAT runtime spawning; full global-lock removal.

### 41.7 Stage 23 acceptance

- No production change → no x86_64 `-smp 1` smoke required (no live boot/runtime
  behavior change).
- `cargo check --no-default-features` + `cargo check --features hosted-dev` clean.
- Stage 23 tests + Stage 22 notification tests pass single-threaded
  (`RUST_MIN_STACK=8388608`).
- `git diff --check` clean.

### 41.8 Stage 23 acceptance — re-confirmed at Stage 24 (Part 0)

Re-verified at the start of Stage 24 before any new work:

- **Stage 23 accepted without smoke**: audit-only stage; **no production code was
  changed** in Stage 23 (only docs + tests), so no x86_64 smoke was or is required.
- `SYSCALL_COUNT == 30` confirmed (frozen by
  `syscall_abi_numbers_are_frozen`); 22 live `Syscall` variants; no new syscall
  numbers were added.
- Notification teardown remains reachable **only** via the direct revoke helpers
  (`revoke_capability_in_cnode` / `revoke_capability_direct_in_process_cnode` →
  `destroy_notification_for_revoked_cap`) and cnode teardown
  (`maybe_cleanup_process_cnode_for_pid`). No generic user-facing cap release
  syscall exists; `TransferRelease` (NR=4) is MemoryObject-scoped and returns
  `InvalidArgs` for Notification caps.
- A **live notification create/release syscall surface remains deferred** (would
  require a new syscall number — out of scope by the hard ABI invariant).

## §42 Stage 24 — VFS ELF staging soundness (TakeOnceStagingBuffer) + endpoint/reply-cap revoke audit

### 42.0 Stage 23 acceptance confirmation

See §41.8. Stage 23 accepted, audit-only, no production code changed,
`SYSCALL_COUNT == 30` confirmed, live notification syscall deferred.

### 42.1 Part A — VFS ELF staging static-mut soundness

**Before.** `src/kernel/syscall.rs` defined the ELF staging buffer as a raw
`static mut VFS_ELF_STAGING: [u8; 128 * 1024]`. Two handlers
(`handle_spawn_process_from_user_buf`, `handle_spawn_from_initramfs_file`)
obtained a `&mut` to it via `&raw mut` + `unsafe { &mut * }`. The exclusivity
invariant ("only PM calls this, serialised") lived **only in a comment** — there
was no machine-checked guard against two overlapping `&mut` references, which is
exactly the unsoundness `static mut` invites (and the reason for the
`static_mut_refs` lint).

**After.** The buffer is wrapped in a typed take-once container:

```rust
struct TakeOnceStagingBuffer<const N: usize> {
    claimed: AtomicBool,
    data: UnsafeCell<[u8; N]>,
}
unsafe impl<const N: usize> Sync for TakeOnceStagingBuffer<N> {}
```

- `try_take(&'static self) -> Option<StagingBufferClaim<'static, N>>` flips
  `claimed` from `false → true` with a single `compare_exchange(AcqRel,
  Relaxed)`. The **only** way to obtain a mutable view is through the returned
  guard, so exclusive access is encoded in the type system rather than in a
  comment.
- `StagingBufferClaim::as_mut_slice(&mut self) -> &mut [u8]` is the sole `unsafe`
  deref; its safety argument is local and trivially true (holding the guard ⇒
  `claimed == true` ⇒ no other guard exists).
- The guard is **not** `Clone`/`Copy`: at most one can exist per buffer.

**One-shot vs. reuse semantics (deliberate deviation from the literal design
note).** The Stage 24 design sketch suggested "no Drop release; the buffer stays
claimed after use." That cannot be used here: **both** spawn handlers share this
one buffer and the system spawns many processes over its lifetime (PM calls
`SpawnProcessFromUserBuf` once per process). A permanently-claimed buffer would
make every spawn after the first fail. The real soundness invariant is **mutual
exclusion per call**, not "used exactly once ever". `StagingBufferClaim`
therefore **releases the claim on `Drop`** (a `Release` store), so the next spawn
syscall can reclaim it. This is sound because a syscall handler runs to
completion before the next syscall is dispatched (single in-flight spawn at a
time), so claims never overlap; if they ever did, the second caller gets `None`
and the handler returns a stable `SyscallError::Internal` instead of aliasing
the buffer.

**no_std/freestanding.** `AtomicBool`, `UnsafeCell` come from `core`; no heap,
no `std`. `cargo check --no-default-features` is clean.

### 42.2 Part B — endpoint / reply-cap revoke + cnode-teardown audit

Audit-only (no production code changed in Part B). Findings:

1. **Endpoint ownership: multi-owner (delegable cross-process).** Unlike
   Notification (single-owner, see §40.2), endpoint caps ARE granted across
   processes: the spawn path mints a SEND + RECEIVE pair and then
   `grant_capability_task_to_task_with_rights(..., SEND)` delegates the send cap
   to the parent PID while the service keeps the receive cap. Revoking **one**
   endpoint cap must therefore **not** destroy the shared object.

2. **`destroy_endpoint` exists** (`ipc_state.rs:1238`) and fully tears an
   endpoint down (clears object, receiver waiter, sender-waiter queue, bumps
   generation, clears `fault_handler_endpoint`). It is **not** wired into
   `revoke_capability_in_cnode` — by design (point 1). Endpoints are otherwise
   effectively permanent once created (no user syscall destroys them); this is
   the intended design, not a gap.

3. **`revoke_capability_in_cnode` does nothing endpoint-specific** beyond
   clearing the cnode slot and the usual transfer/delegation bookkeeping. Correct
   for a multi-owner object: only `destroy_notification_for_revoked_cap` and the
   MemoryObject refcount/reclaim paths are special-cased.

4. **`clear_ipc_waiters_for_tid` clears ALL endpoint waiters for the tid** —
   receiver (`endpoint_waiters`), every sender slot
   (`endpoint_sender_waiters[*][*]`), and notification waiters
   (`ipc_state.rs:128`).

5. **No stranded waiter after `exit_task` / `mark_task_dead`.** Both call
   `clear_ipc_waiters_for_tid(tid)`, so a dying task that was blocked as an
   endpoint receiver or sender is removed from the waiter slots. Confirmed by the
   two Stage 24 teardown tests.

6. **Reply caps at cnode teardown.** `mark_task_dead` / `exit_task` call
   `revoke_reply_caps_for_caller(tid)` **before** cnode teardown; that clears the
   global `reply_caps[idx]` record for every record whose `caller_tid == tid`.
   Cnode teardown (`maybe_cleanup_process_cnode_for_pid`) then iterates live caps
   and calls `revoke_capability_in_cnode`, which clears Reply cap **cnode slots**
   (no special global-record handling needed — the global record is already gone
   for the caller, and `ipc_reply` consumes it for the replier).

7. **Stale `waiter_cap_id` cannot cause unsoundness.** Global reply records are
   generation-guarded (`reply_cap_generations[idx]`). A `waiter_cap_id` that
   points into a torn-down replier cnode is only consulted inside `ipc_reply`,
   which first resolves the reply cap in the **current** (replier) cnode — a dead
   replier cannot call it. Any stale Reply CapId resolves to
   `StaleCapability`/`InvalidCapability` once the record is `None` or its
   generation has advanced. Confirmed by
   `stage24_stale_reply_cap_cannot_be_reused_after_cnode_teardown`.

### 42.3 Bugs fixed

- **Part A (soundness hardening):** removed the raw `static mut` +
  `unsafe { &mut * }` aliasing exposure in `src/kernel/syscall.rs` (the two
  spawn handlers, formerly at `syscall.rs:2413` and `syscall.rs:2590`). No
  behavioral change for the legitimate single-in-flight path; an overlapping
  claim now fails closed with `SyscallError::Internal` instead of aliasing.
- **Part B:** no bugs found. The audit confirms the existing teardown paths are
  complete (waiters cleared, reply records cleared, generations guard stale caps).

### 42.4 Helpers / wrappers added

- `TakeOnceStagingBuffer<const N: usize>` (+ `unsafe impl Sync`), `try_take`.
- `StagingBufferClaim<'a, const N: usize>` (+ `as_mut_slice`, `Drop`). All in
  `src/kernel/syscall.rs`.

### 42.5 Ownership semantics summary (Endpoint vs Notification)

| Object | Owners | Cross-process delegation | Revoke-one-cap destroys object? |
|--------|--------|--------------------------|----------------------------------|
| Notification | single-owner | never | yes (`destroy_notification_for_revoked_cap`) |
| Endpoint | multi-owner | yes (spawn delegates SEND to parent) | no (object persists; `destroy_endpoint` is a separate explicit path) |
| Reply | one-shot, caller+replier slots | never | n/a (consumed by `ipc_reply` or cleared by `revoke_reply_caps_for_caller`) |

### 42.6 Deferred (Stage 24)

- No user-facing endpoint **destroy** syscall (endpoints are effectively
  permanent once created; a destroy surface would need a new syscall number —
  out of scope by the ABI invariant).
- Live notification create/release syscall (carried over from §41.6).
- x86_64 SMP / `smp.rs`; RAMFS/FAT runtime spawning; full global-lock removal.

### 42.7 x86_64 smoke decision

Part A changes `syscall.rs` but is **behavior-preserving** for the single
in-flight spawn path (same bytes copied into the same buffer; only the access is
now type-checked). Part B changed no production code. No live boot/runtime
behavior change → **no x86_64 `-smp 1` smoke required**. Confirmed instead by the
full hosted-dev test suite (no regression).

### 42.8 Stage 24 acceptance

- `cargo check --no-default-features` + `cargo check --features hosted-dev` clean.
- 9 new Stage 24 tests pass single-threaded (`RUST_MIN_STACK=8388608`).
- Full hosted-dev suite green; `git diff --check` clean.
- `SYSCALL_COUNT == 30` unchanged; no ABI change.

#### 42.8.1 Stage 24 acceptance (recorded at Stage 25A)

- **Accepted without x86_64 smoke.** Part A (`TakeOnceStagingBuffer`) is a
  behavior-preserving replacement of the raw `static mut VFS_ELF_STAGING`:
  mutual-exclusion-per-call with `Drop`-release, identical bytes copied into the
  identical buffer for the single in-flight spawn path. No live boot/runtime
  behavior change, so no `-smp 1` smoke was required (§42.7).
- **Endpoint / reply-cap audit found no production bugs.** Endpoints are
  multi-owner (delegated cross-process at spawn); reply records are cleared by
  `revoke_reply_caps_for_caller(tid)` on teardown but **can linger** after a
  replier teardown until the caller exits. Generation guards prevent any safety
  bug — the deferral was a lifecycle-hygiene / bounded-capacity item, addressed
  in Stage 25C below.
- **803-test baseline** confirmed at acceptance (latest commit `2b3266d`).
- **Still deferred** at Stage 24 acceptance: x86_64 SMP / `smp.rs`; RAMFS/FAT
  runtime spawning.

## §43 Stage 25 — reply-cap replier-exit cleanup + endpoint permanence certification + deferred live-surface map

### 43.0 Carried-forward context

Stage 24 accepted (see §42.8.1). The one Stage 24 deferral with code impact was:
**reply records involving a torn-down replier linger until the caller exits.**
This is safe (generation guards) but is a lifecycle-hygiene / bounded-capacity
issue. Stage 25C closes it; 25D certifies endpoint permanence; 25E maps the
remaining deferred live cap-release surface.

### 43.1 Reply-cap global record audit (Stage 25B)

`ReplyCapRecord` (`src/kernel/boot/defs.rs:205`) fields:

| Field | Meaning |
|-------|---------|
| `caller_tid: ThreadId` | the task that issued the IpcCall (owns the caller-side Reply cap) |
| `reply_endpoint: CapObject` | endpoint the reply is delivered through |
| `responder_tid: Option<ThreadId>` | **the replier** — the task expected to call `ipc_reply` (`None` = any receiver may reply) |
| `caller_cap_id: CapId` | Reply CapId minted into the caller's cnode |
| `waiter_cap_id: Option<CapId>` | Reply CapId minted into the waiter/replier's cnode on materialization |

Findings:

- The replier is identified by **`responder_tid`**.
- `revoke_reply_caps_for_caller(tid)` (`ipc_state.rs:109`) matches on
  **`record.caller_tid.0 == tid`** and sets the slot to `None`. It does NOT
  match on `responder_tid`.
- **Before Stage 25C: no `revoke_reply_caps_for_replier(tid)` existed** — nothing
  cleared a record keyed on `responder_tid`.
- A record **could linger after the replier's `mark_task_dead`/`exit_task`**:
  teardown only ran the caller-keyed revoke, so a record where
  `responder_tid == dead_replier` but `caller_tid` is still live survived until
  the caller itself exited (or the reply was consumed).
- Lingering records are **bounded** by `MAX_REPLY_CAPS` — the global table is a
  fixed array; the worst case is slot pressure, not unbounded growth.
- A lingering record **holds no cnode slot, no cap_refcount, and cannot wake a
  dead task**: the caller-side cnode slot is owned by the caller (cleared at
  caller teardown); the replier-side `waiter_cap_id` slot is cleared by the
  replier's own cnode teardown; `ipc_reply` resolves the reply cap in the
  **current (replier)** cnode first, so a dead replier can never deliver; and
  `resolve_reply_index` rejects any cap whose slot is `None` or whose generation
  advanced (`StaleCapability`).
- **Classification: I — safe lingering record** (bounded, generation-guarded,
  no slot/refcount/wake hazard). Stage 25C clears it proactively for hygiene and
  to free the slot earlier.

### 43.2 Stage 25C — replier-exit reply-record cleanup (FIX)

Added `revoke_reply_caps_for_replier(&mut self, tid: u64) -> usize`
(`ipc_state.rs`), a mirror of `revoke_reply_caps_for_caller` that filters on
`record.responder_tid == Some(tid)` and sets matching slots to `None` under
`ipc_state_lock` (rank 3). Wired into `exit_task` and `mark_task_dead`
(`restart_state.rs`) immediately after the existing
`revoke_reply_caps_for_caller(tid)` call, so a teardown clears records from
**both** sides.

**Generation handling.** Like `revoke_reply_caps_for_caller`, the helper sets
the slot to `None` rather than bumping the generation. Setting the slot to `None`
is already sufficient to invalidate any outstanding Reply cap:
`resolve_reply_index` returns `StaleCapability` whenever the slot is empty,
**before** the generation is consulted. The generation is bumped on the next
slot *reuse* by `create_reply_cap_for_caller` Phase 1. Mirroring the caller path
exactly keeps the two teardown directions symmetric and behavior-identical.

**Idempotency.** All four orderings are no-ops past the first clear:

- caller exits first → `for_caller` clears the slot → later `for_replier`
  (caller's teardown, or the replier's own) sees `None` → 0 revoked.
- replier exits first → `for_replier` clears the slot → later caller teardown's
  `for_caller` sees `None` → 0 revoked.
- both helpers run in one teardown: the second sees the slot already `None`.
- An unrelated record (different caller and different replier) is untouched.

### 43.3 Reply-cap lifecycle table (Stage 25)

| Event | Global `reply_caps[idx]` record | Caller cnode slot | Replier cnode slot | Generation |
|-------|---------------------------------|-------------------|--------------------|------------|
| IpcCall create (`create_reply_cap_for_caller`) | reserved (slot `None`→`Some`) | Reply cap minted (`caller_cap_id`) | — | bumped on reserve |
| recv-v2 materialization | `waiter_cap_id` set | — | Reply cap minted | unchanged |
| IpcReply success (`ipc_reply`) | consumed (`Some`→`None`) | fast-revoked via `caller_cap_id` | fast-revoked via `waiter_cap_id` | unchanged (bumped on next reuse) |
| Caller exit (`revoke_reply_caps_for_caller`) | cleared if `caller_tid == tid` | cleared by caller cnode teardown | — | bumped on next reuse |
| **Replier exit (NEW, `revoke_reply_caps_for_replier`)** | **cleared if `responder_tid == Some(tid)`** | — | cleared by replier cnode teardown | bumped on next reuse |
| Timeout / error | record NOT touched by `process_ipc_timeout_deadlines`; rollback paths clear `waiter_cap_id` only | per-path | per-path | unchanged |
| Cnode teardown | already-`None` for the torn-down side (cleared above); `revoke_capability_in_cnode` clears the cnode slot only | cleared | cleared | unchanged |
| Stale generation guard | `resolve_reply_index` → `StaleCapability` when slot `None` or generation advanced | n/a | n/a | guards reuse |

### 43.4 Stage 25D — endpoint permanence certification

Audit answers:

1. **Is there a `destroy_endpoint` helper?** Yes (`ipc_state.rs:1238`): clears
   object, receiver waiter, sender-waiter queue, bumps generation, clears
   `fault_handler_endpoint`. Called **only from tests** today, never from
   production teardown.
2. **Does `revoke_capability_in_cnode` do anything for `CapObject::Endpoint`?**
   No. Only `CapObject::Notification` (single-owner) is destroyed on revoke via
   `destroy_notification_for_revoked_cap`; MemoryObject has refcount/reclaim
   handling. Endpoint revoke clears the cnode slot only — correct for a
   multi-owner object.
3. **Endpoint generation for stale detection?** Yes —
   `endpoint_generations[idx]`; `resolve_endpoint_index` returns
   `StaleCapability` on mismatch.
4. **`MAX_ENDPOINTS`?** `256` (`boot/mod.rs:62`). Bounded; current boot
   services use a small fixed number, so practical exhaustion does not occur.
5. **Permanent by design?** Yes — endpoints are kernel-owned IPC rendezvous
   objects, deliberately multi-owner (the spawn path delegates the SEND cap to
   the parent while the service keeps RECEIVE).

**Certification.**

- Endpoints are **permanent-once-created kernel-owned IPC rendezvous objects.**
- A single Endpoint cap revoke does **not** destroy the object (multi-owner by
  design).
- Bounded by `MAX_ENDPOINTS` (256); current boot services use a small fixed
  number.
- Endpoint receiver waiters and sender waiters are cleaned by
  `clear_ipc_waiters_for_tid` on task exit/death (and by
  `process_ipc_timeout_deadlines` on timeout).
- **`destroy_endpoint` was deliberately NOT wired** into cnode teardown:
  ownership is multi-owner, so destroying the object on one owner's teardown
  would break the surviving owner(s). Future endpoint teardown would require
  refcount or owner tracking; deferred.

#### 43.4.1 Endpoint permanence table

| Event | Endpoint object | Generation | Receiver waiter | Sender waiter |
|-------|-----------------|------------|-----------------|---------------|
| create (`create_endpoint`) | allocated | bumped | — | — |
| SEND cap delegation (spawn) | unchanged | unchanged | — | — |
| cap revoke (`revoke_capability_in_cnode`) | **unchanged** (multi-owner) | unchanged | unchanged | unchanged |
| cnode teardown | unchanged | unchanged | cleared for tid | cleared for tid |
| task exit receive-blocked | unchanged | unchanged | **cleared** (`clear_ipc_waiters_for_tid`) | — |
| task exit send-blocked | unchanged | unchanged | — | **cleared** |
| timeout cleanup | unchanged | unchanged | cleared (`process_ipc_timeout_deadlines`) | cleared |
| future destroy blocker | would need `destroy_endpoint` wiring | bumped on destroy | cleared | cleared |

### 43.5 Deferred live cap release/destroy surface (Stage 25E)

- **No generic user-facing cap release syscall today.** `SYSCALL_COUNT == 30`,
  unchanged this stage.
- **Notification create/destroy live surface deferred** — there is no
  `sys_notification_destroy` user syscall; destruction happens implicitly via
  `destroy_notification_for_revoked_cap` on the single-owner cap revoke
  (Stage 22) and on cnode teardown.
- **Endpoint teardown deferred** — endpoints are permanent-once-created for the
  current architecture (§43.4). No user syscall destroys an endpoint.
- **Any future live surface must be folded into an existing control-plane
  opcode, not a new syscall number** (ABI invariant: `SYSCALL_COUNT` frozen).
- **Candidate existing opcodes:** none today is a clean fit. `TransferRelease`
  (transfer NR=4) is **MemoryObject-scoped** and must NOT become a generic cap
  release without an explicit design — its semantics are tied to transfer
  envelope / pin lifecycle, not arbitrary cap destruction. There is no existing
  generic "object control" / "resource management" opcode to extend; **a new
  opcode (in a future ABI version) would be required** for a generic cap-release
  surface.
- **Stage 23 finding carried forward:** the live notification/cap release
  surface is classified **E — missing/deferred** (no production code; surface
  intentionally absent under the current ABI).

### 43.6 Fixes made

- `revoke_reply_caps_for_replier` added (`ipc_state.rs`) and wired into
  `exit_task` and `mark_task_dead` (`restart_state.rs`) so reply records are
  cleared proactively from the replier side, not only the caller side. Closes
  the Stage 24 lifecycle-hygiene deferral (audit classification **I**).
- No safety bug existed beforehand (generation guards); this is a
  bounded-capacity / hygiene improvement, not a soundness fix.
- Endpoint side: no code change — certified permanent-by-design; `destroy_endpoint`
  intentionally left unwired (multi-owner).

### 43.7 Tests added (Stage 25)

`src/kernel/boot/tests.rs`:

- `stage25c_replier_exit_clears_global_reply_record`
- `stage25c_caller_exit_still_clears_global_reply_record`
- `stage25c_caller_exits_first_then_replier_exit_is_idempotent`
- `stage25c_replier_exits_first_then_caller_cleanup_is_idempotent`
- `stage25c_both_exit_no_leak_no_underflow`
- `stage25c_stale_waiter_cap_id_cleared_on_replier_teardown`
- `stage25c_reply_cap_cannot_be_reused_after_replier_teardown`
- `stage25c_unrelated_reply_record_unaffected`
- `stage25c_cnode_teardown_with_reply_cap_remains_safe`
- `stage25d_endpoint_cap_revoke_does_not_destroy_shared_endpoint`
- `stage25d_task_exit_clears_endpoint_receiver_waiter`
- `stage25d_task_exit_clears_endpoint_sender_waiter`
- `stage25d_repeated_waiter_cleanup_idempotent`
- `stage25d_endpoint_remains_after_one_owner_cnode_teardown`
- `stage25d_unrelated_endpoint_unaffected_by_other_endpoint_revoke`

### 43.8 Deferred blockers (Stage 25)

- Generic user-facing cap-release syscall: requires a new ABI opcode (frozen
  `SYSCALL_COUNT`); deferred.
- Endpoint destroy live surface: requires refcount/owner tracking; deferred.
- Notification destroy live syscall: deferred (implicit-only today).
- x86_64 SMP / `smp.rs`: still deferred.
- RAMFS/FAT runtime spawning: still deferred.

### 43.9 x86_64 smoke decision (Stage 25)

The only production change is `revoke_reply_caps_for_replier` + two call sites in
`restart_state.rs`. It runs under the same `ipc_state_lock` (rank 3) as the
existing caller-side revoke and only sets already-bounded global array slots to
`None` (no wake, no scheduler mutation, no new lock acquisition order). It is a
proactive-cleanup superset of behavior that was previously deferred to caller
exit — observable runtime behavior for live boot is unchanged (records are
cleared either way; only the timing is earlier). **No x86_64 `-smp 1` smoke
required**; confirmed instead by the full hosted-dev suite (no regression).

### 43.10 Still deferred

- x86_64 SMP / `smp.rs` — deferred.
- RAMFS/FAT runtime spawning — deferred.
- Full global-lock removal — not claimed.

### 43.11 Stage 25 acceptance (recorded at Stage 26)

- **Accepted without x86_64 smoke.** The only production change was
  `revoke_reply_caps_for_replier(tid)` (added in Stage 25C) plus its two call
  sites in `restart_state.rs`; it runs under `ipc_state_lock` (rank 3) and only
  clears already-bounded reply-record array slots (no wake, no scheduler
  mutation, no new lock acquisition). See §43.9 for the smoke rationale.
- **`revoke_reply_caps_for_replier` added** — proactive replier-exit reply-record
  cleanup; a timing superset of behavior previously deferred to caller exit.
- **Endpoints certified permanent-once-created** (§43.4): no live endpoint destroy
  path exists; endpoint slots/generations are stable for the kernel lifetime once
  created, which is what makes the Stage 26 split-read of `ipc` array slots sound.
- **818-test baseline** carried into Stage 26 (full hosted-dev suite, no
  regression).
- x86_64 SMP / `smp.rs` still deferred; RAMFS/FAT runtime spawning still deferred.

## §44 Stage 26 — global-lock callsite audit + two domain-lock-only extractions

Stage 26 audits every `SharedKernel::with` / `with_cpu` (global-lock) acquisition
site, classifies each, and extracts two further read-only paths to acquire only a
single domain lock — continuing the split-read program established in Stages 2–5.
**No claim of full global-lock removal is made**; this is preparatory
finer-grained locking.

### 44.1 Stage 25 acceptance

Recorded in §43.11 above (accepted without smoke; 818-test baseline carried
forward).

### 44.2 Global-lock architecture summary

- **Type:** the global lock is `SpinLock<KernelState>`, owned by `SharedKernel`
  (`src/runtime.rs`). `KernelState` (`src/kernel/boot/mod.rs`) embeds one
  `SpinLockIrq<()>` (or `SpinLockIrq<SchedulerState>`) per domain — scheduler(1),
  task(2), ipc(3), capability(4), vm(5), memory(6), driver(7), fault(8),
  restart(9), telemetry(10), boot_config(11).
- **Acquisition pattern:** closure-based. `SharedKernel::with(|state| …)` and
  `with_cpu(cpu, |state| …)` lock the whole `KernelState` and hand the closure a
  `&mut KernelState`. Inside, domain methods (`with_ipc_state`,
  `with_capability_state`, `with_tcbs`, `with_scheduler_state`,
  `with_memory_state`, …) re-acquire the per-domain lock.
- **Domain locks inside vs. independent:** historically always *inside* the global
  lock. The split-read/split-mut helpers (`*_split_read`, `*_split_mut` on
  `SharedKernel`, backed by `KernelState::*_from_raw`) are the *only* paths that
  acquire a domain lock *without* the global lock, deriving raw field pointers via
  `core::ptr::addr_of!` so no `&KernelState` whole-struct reference is created.
- **Pre-existing global-lock-free paths:** scheduler tick / current-TID / topology
  reads (Stage 2B–L7A), boot-config reads (L8B), fault/telemetry split mut+read
  (3B/3C/4T+5), task ASID/class/exists, cnode slot capacity, process-id and
  group-leader reads (Stages 4T+7 / 5A / 5B).

### 44.3 Callsite audit table (A–G)

Classification key: **A** already split / domain-only; **B** read-only candidate;
**C** simple 1–2-domain mutation candidate; **D** complex multi-domain deferred;
**E** trap/arch boundary deferred; **F** Spawn/fork/exec deferred; **G** SMP
deferred.

| file:line | handler/context | class | domains | notes |
| --- | --- | --- | --- | --- |
| runtime.rs `*_split_read` / `*_split_mut` (≈30 helpers) | scheduler/task/ipc/cap/fault/telemetry/boot reads & diag muts | A | 1 each | already domain-lock-only; the extraction template |
| runtime.rs:283 `handle_trap_with_cpu` | trap dispatch entry | E | all (via handle_trap) | trap boundary; do not touch |
| runtime.rs:269/273 `ipc_recv_*_split_bridge` | IPC recv timeout bridge | D | scheduler+ipc+task | multi-domain blocking recv; deferred |
| runtime.rs:293 `control_plane_set_process_cnode_slots_via_syscall` | cap control plane | C | task→capability | Stage 27: `_split_mut` helper extracted (task(2)→cap(4), no global lock); live callsite still global-locked via `handle_trap` seam (F+I) |
| arch/trap_entry.rs:149 `.with_cpu` | arch trap dispatch | E | all | trap boundary; do not touch |
| arch/x86_64/descriptor_tables.rs:904/932 `.with_cpu(current_tid)` | x86_64 entering/exiting TID | E | scheduler/task | hard-invariant trap TID logic; do not touch |
| arch/{aarch64,x86_64}/boot.rs `.with` / `borrow_kernel_for_boot` | boot/ERET | E | all | boot single-CPU; do not touch |
| syscall.rs handlers (inside `handle_trap` closure) | all syscalls | D/F | varies | run under the global lock via `with_cpu`; SpawnV5/fork/exec are F; multi-domain are D |
| boot/*_state.rs `with_*_state[_mut]` (domain helpers) | domain accessors | A | 1 each | domain-granular; called inside global lock today |
| smp.rs cross-CPU work | SMP | G | ipc(mailbox) | SMP deferred |
| **NEW** runtime.rs `notification_waiter_count_split_read` | ipc waiter read | **B→A** | ipc(3) | extracted this stage |
| **NEW** runtime.rs `cnode_registered_split_read` | cnode presence read | **B→A** | capability(4) | extracted this stage |

Approximate counts: **A** ≈ 32 (30 pre-existing split helpers + 2 new), **B** 2
(the two extracted this stage, now A), **C** 1, **D** several (IPC recv bridge,
multi-domain syscalls), **E** ~8 (trap/arch/boot entries), **F** Spawn/fork/exec
syscall family, **G** SMP cross-CPU work. The bulk of remaining `with`/`with_cpu`
*call sites* in the codebase are inside `#[cfg(test)]` modules (class A test
scaffolding, not production paths).

### 44.4 Two extracted paths (before / after)

**Extraction 1 — `notification_waiter_count_split_read` (ipc, rank 3):**
- *Before:* the only way to read whether a notification slot has a waiter was the
  `#[cfg(test)]` `KernelState::notification_waiter_count`, reached through
  `SharedKernel::with(|state| state.notification_waiter_count(idx))` — i.e. under
  the **global lock**, which then re-acquires `ipc_state_lock`.
- *After:* `SharedKernel::notification_waiter_count_split_read(idx)` calls
  `KernelState::notification_waiter_count_from_raw`, which derives raw field
  pointers via `addr_of!` and acquires **only** `ipc_state_lock` (rank 3). The
  outer global `SpinLock<KernelState>` is never taken.

**Extraction 2 — `cnode_registered_split_read` (capability, rank 4):**
- *Before:* CNode presence for a pid was only observable through
  `SharedKernel::with(|state| state.cnode_slot_capacity(CNodeId(pid)).is_some())`
  — under the **global lock**, which re-acquires `capability_state_lock`.
- *After:* `SharedKernel::cnode_registered_split_read(pid)` calls
  `KernelState::cnode_registered_from_raw`, acquiring **only**
  `capability_state_lock` (rank 4) (mirrors the existing
  `cnode_slot_capacity_from_raw`). The global lock is never taken.

### 44.5 Lock-rank ordering analysis

- *Extraction 1* takes exactly one lock, ipc (rank 3), and releases it before
  return. No lock of rank ≤ 3 may be held by the caller when invoking it
  (documented in the helper). It is read-only: it never mutates `ipc`, never wakes
  a task, never touches the scheduler. Soundness rests on Stage 25's endpoint/
  notification permanence — slot storage is stable for the kernel lifetime, so the
  raw-pointer read under the ipc lock cannot race a structural move.
- *Extraction 2* takes exactly one lock, capability (rank 4), and releases it
  before return. No lock of rank ≤ 4 may be held by the caller. It is read-only:
  it only scans `capability.cnode_spaces` for a matching `CNodeId`. Same
  single-domain, no-inversion property as the Stage 5A `cnode_slot_capacity`
  split-read it parallels.
- Both helpers acquire a **single** domain lock, so they cannot create a
  rank-inversion by themselves; the documented caller constraint prevents an
  inversion against an already-held lower-rank lock.

### 44.6 What remains under the global lock and why

- **Trap/arch entry** (`handle_trap_with_cpu`, arch `with_cpu`, entering/exiting
  TID reads): hard invariant — must not touch. Class E.
- **SpawnV5 / fork / exec**: multi-domain atomic construction; hard invariant.
  Class F.
- **Multi-domain syscalls and the IPC recv-timeout bridge**: need atomicity across
  3+ domains (scheduler + task + ipc, etc.); extracting them safely requires a
  staged lock-acquisition protocol not in scope here. Class D.
- **SMP cross-CPU work**: SMP still deferred. Class G.

### 44.7 Progress toward finer-grained locking (not completion)

Stage 26 extends the split-read surface from ~30 to ~32 domain-lock-only helpers
and proves two more observable reads do not need the global lock. This is
incremental progress, **not** a global-lock-removal claim: every mutation path and
every trap/Spawn/SMP path still serializes on `SpinLock<KernelState>`.

### 44.8 Deferred (complex / trap / SMP / Spawn)

- Multi-domain syscall handlers and the IPC recv-timeout bridge (D).
- Trap/arch/boot entry and entering/exiting TID logic (E).
- SpawnV5 / fork / exec (F).
- x86_64 SMP / `smp.rs` (G).
- RAMFS/FAT runtime spawning.
- Full global-lock removal — not claimed.

### 44.9 x86_64 smoke decision (Stage 26)

**No x86_64 `-smp 1` smoke required.** Stage 26 adds only two read-only
`SharedKernel` split-read helpers plus their `*_from_raw` backings; nothing on any
live boot/runtime/trap path is modified (no production caller is rewired this
stage — the helpers are additive and currently exercised only by the new unit
tests). No live boot/runtime behavior changes, so the hard-invariant smoke trigger
("x86_64 smoke only if live boot/runtime behavior changes") is not met. Confirmed
instead by the full hosted-dev suite (no regression).

### 44.10 Stage 26 acceptance

**Stage 26 accepted without smoke.** Summary of accepted state:

- Two read-only extractions landed:
  - `notification_waiter_count_split_read` — ipc domain, **rank 3**.
  - `cnode_registered_split_read` — capability domain, **rank 4**.
- Global-lock callsite audit complete; the A–G classification table is recorded
  in §44.3, with the per-class approximate counts in the same section.
- Class **C** candidate identified for the next stage:
  `control_plane_set_process_cnode_slots` (task read → capability mutate).
- Baseline: **821 tests pass** at Stage 26; `SYSCALL_COUNT == 30` unchanged.
- No live boot/runtime/trap path rewired (helpers additive), so no x86_64 smoke.
- SMP and RAMFS/FAT runtime spawning remain deferred (classes G / out-of-scope).

## 45. Stage 27 — first mutating global-lock extraction (`control_plane_set_process_cnode_slots`)

Stage 27 performs the **first mutating** domain-lock extraction: it lifts the
control-plane CNode-slot create/resize out of the global `SpinLock<KernelState>`
into a two-phase **task(read, rank 2) → capability(mutate, rank 4)** protocol that
never acquires the outer global lock and never calls `with` / `with_cpu`.

### 45.1 Audit result — domains touched

`control_plane_set_process_cnode_slots` (`src/kernel/boot/capability_lifecycle_state.rs:32`)
and its Stage-5B plan-first variant `_planned` (`:72`) touch exactly two domains:

- **Task (read, rank 2):** requester class and requester pid (`task_class`,
  `process_id`). In the `_planned` variant these are pre-snapshotted into a
  `ControlPlaneCnodePlan` by the caller so the capability mutation never re-enters
  the task lock.
- **Capability (mutate, rank 4):** the pid→cnode registration table
  (`capability.process_cnodes`) and the CNode-slot table
  (`capability.cnode_spaces` / each `CapabilitySpace`). Create path =
  `ensure_cnode_space_with_slots` + `set_process_cnode_for_pid`; resize path =
  `resize_cnode_slots`.
- **Boot-config (read):** runtime capacity limits (`runtime_capacity_config`,
  reads `boot_config.capacity_profile`) for slot-capacity bounds checks.

It does **not** touch scheduler / IPC / VM / memory. Confirmed by the Stage 27
side-effect tests (`..no_scheduler_wake_side_effect`, `..no_ipc_side_effect`).

Errors returned (all preserved by the extraction): `TaskMissing` (requester TID
has no task), `MissingRight` (non-system-server requester whose pid != target, or
non-system-server target of class `App`), `WrongObject` / `CapabilityFull` (slot
normalization), `CapabilityFull` (global pool exhausted / cspace alloc/grow),
`TaskTableFull` (no free cnode-space slot for a new registration).

### 45.2 Before / after call path

- **Before:** `SharedKernel::control_plane_set_process_cnode_slots_via_syscall`
  (`runtime.rs:288`) → `with(|state| state.…_via_syscall(..))` →
  `KernelState::…_via_syscall` (`fault_state.rs:350`) builds a `TrapFrame` and
  calls `handle_trap()`. The syscall handler `handle_control_plane_set_cnode_slots`
  (`syscall.rs:2062`) snapshots the task plan then calls
  `control_plane_set_process_cnode_slots_planned`. The entire sequence runs under
  the **global lock** via `with`/`with_cpu` (the `via_syscall` wrapper internally
  calls `handle_trap` — the F+I blocker recorded in §44.3 line for runtime.rs:293).
- **After (new path):** `SharedKernel::control_plane_set_process_cnode_slots_split_mut`
  (`runtime.rs`, Stage 27 section):
  1. **Phase 1 — task snapshot (rank 2):** `task_class_from_raw` +
     `process_id_from_raw` each acquire and **release** `task_state_lock`; build a
     `ControlPlaneCnodePlan`. `TaskMissing` if the requester has no task.
  2. **Phase 1b — boot-config snapshot:** `runtime_capacity_config_split_read`
     (boot_config lock only).
  3. **Phase 2 — capability mutation (rank 4):**
     `control_plane_set_process_cnode_slots_apply_from_raw`
     (`orchestrator_state.rs`) acquires **only** `capability_state_lock` and
     applies the create/resize, faithfully mirroring `_planned`.

  The outer global `SpinLock<KernelState>` is never taken; no `with`/`with_cpu`.

### 45.3 Snapshot / apply protocol and lock order

Lock order is **task(2) → boot_config → capability(4)**, never inverted: the
capability lock is acquired only after both reads have released their own locks, so
the capability lock is never held while taking the task lock. Each phase takes a
single domain lock and releases it before the next, so the helper cannot itself
create a rank inversion.

### 45.4 Live-wired or helper-only

**Helper-only this stage (live callsite NOT rewired).** Rationale (documented
blocker): the production callsite reaches the logic through
`…_via_syscall` → `handle_trap()` (class **F+I** in §44.3 / §44.1 — it internally
calls `handle_trap`, so it must keep entering the global lock via `with_cpu`). The
syscall ABI path (`Syscall::ControlPlaneSetCnodeSlots`, `SYSCALL_COUNT == 30`) and
SpawnV5/PM/init/service boot behavior must be preserved exactly, and rewiring the
trap-dispatch wrapper is out of scope and would touch the trap boundary (hard
invariant). The new `_split_mut` helper is therefore proven correct by direct unit
tests and is ready to become the live path once the surrounding trap-dispatch seam
is itself extracted (future stage). No live boot/runtime behavior changes this
stage, so **no x86_64 smoke** is required.

### 45.5 Extraction table

| path | old lock | new lock | domains | mutation? | live-wired? | tests |
|------|----------|----------|---------|-----------|-------------|-------|
| `control_plane_set_process_cnode_slots` | `SharedKernel::with` | task→cap split | task,cap | yes | no (helper-only; trap-seam blocker) | stage27_split_mut_* (7) |
| task snapshot | `SharedKernel::with` | `task_class_from_raw` + `process_id_from_raw` only | task | read | — | stage27_split_mut_missing_process_returns_stable_error |
| capability apply | `SharedKernel::with` | `control_plane_set_process_cnode_slots_apply_from_raw` (capability lock) | capability | yes | — | stage27_split_mut_helper_matches_global_lock_behavior_for_success |

### 45.6 Errors preserved

`TaskMissing`, `MissingRight`, `WrongObject`, `CapabilityFull`, `TaskTableFull` —
all produced at the same decision points as the global `_planned` path. The
`apply_from_raw` helper reuses `normalize_requested_cnode_slots` and the exact
`CapabilityDeriveError → KernelError` mapping from `resize_cnode_slots`.

### 45.7 Tests added (7)

`stage27_split_mut_helper_matches_global_lock_behavior_for_success`,
`stage27_split_mut_missing_process_returns_stable_error`,
`stage27_split_mut_missing_cnode_returns_stable_error`,
`stage27_split_mut_duplicate_update_preserves_existing_behavior`,
`stage27_split_mut_two_processes_isolated`,
`stage27_split_mut_no_scheduler_wake_side_effect`,
`stage27_split_mut_no_ipc_side_effect`
(in `src/kernel/boot/tests.rs`, module `stage27_split_mut_tests`).

### 45.8 Deferred blockers

- Live-wiring the syscall callsite: blocked on the `via_syscall` → `handle_trap`
  trap-dispatch seam (class F+I); deferred to a future trap-seam extraction stage.
- Multi-domain syscalls / IPC recv-timeout bridge (D), trap/arch/boot entry (E),
  SpawnV5/fork/exec (F), x86_64 SMP (G), RAMFS/FAT runtime spawning — all remain
  deferred. Full global-lock removal is **not** claimed.

### 45.9 Stage 27 acceptance (recorded by Stage 28)

Stage 27 is **accepted without x86_64 smoke** (helper-only; no live trap/runtime
behavior changed). Summary of the accepted state:

- `SharedKernel::control_plane_set_process_cnode_slots_split_mut` (`runtime.rs:447`)
  is implemented and proven correct: two-phase task(read, rank 2) → boot-config
  (read) → capability(mutate, rank 4), with no outer global lock and no
  `with`/`with_cpu`.
- **Helper-only**: the live syscall callsite was NOT rewired (§45.4). The blocker
  is the `…_via_syscall` → `handle_trap()` trap-dispatch seam (class **F+I**),
  which must keep entering the global lock via `with_cpu` and owns the trapframe
  result writeback.
- Baseline at acceptance: **828 tests pass** (`cargo test --lib --features
  hosted-dev`); `SYSCALL_COUNT == 30`; SpawnV5 / PM / init / service boot
  preserved.
- SMP / RAMFS-FAT runtime spawning remain **deferred**; no full global-lock
  removal claim.

## §46 Stage 28 — trap/syscall dispatch seam audit + split-dispatch bridge scaffold

Stage 28 audits the full trap/syscall dispatch seam and lands a **whitelist-only**
`try_split_dispatch` bridge (`src/kernel/syscall_split.rs`) that classifies the one
proven-safe mutating syscall (`ControlPlaneSetCnodeSlots`, NR 8) as eligible for
servicing via the Stage 27 split-mut helper without the global lock. The bridge is
**helper-only** (NOT live-wired); the exact arch blocker is documented below.

### 46.1 Trap/syscall seam audit — where the global lock is grabbed

The global `SpinLock<KernelState>` is taken inside `SharedKernel::with` /
`with_cpu` (`runtime.rs:57` / `:62`). The x86_64 trap entry path is:

```
hardware vector → IDT stub (descriptor_tables.rs)
  → save GPRs, read CR2 for #PF, current_cpu_id()                 [arch, untouched]
  → entering_tid = shared.with_cpu(cpu, |k| k.current_tid())      [GLOBAL LOCK, transient]
  → build_trap_frame_from_saved_regs(regs, iret_frame, vector)    [arch, untouched]
  → dispatch_trap_entry_with_shared_kernel(shared, cpu, ctx, &mut frame)
      → handle_trap_entry_shared (trap_entry.rs:105)
          → [PRE-LOCK SEAM] recv-timeout staging (scheduler split-read),
            page-fault diagnostic split-mut bookkeeping                 [no global lock]
          → shared.with_cpu(cpu, |kernel| handle_trap_entry_…(kernel,…))  [GLOBAL LOCK]
              → KernelState::handle_trap_event_… → handle_trap(Trap::Syscall, frame)
                  → syscall::dispatch(kernel, frame)
                      → Syscall::decode(frame.syscall_num())
                      → match → handle_control_plane_set_cnode_slots(kernel, frame)
                          → frame.set_ok(slot_capacity, target_pid, 0)   [TRAPFRAME WRITE]
  → exiting_tid = shared.with_cpu(cpu, |k| k.current_tid())       [GLOBAL LOCK, transient]
  → task_switched = entering_tid != exiting_tid
  → if task_switched: write_task_gprs_to_saved_regs(regs, &frame)
    else if syscall:  write_trap_returns_to_saved_regs(regs, &frame)  [reads frame.ret*]
  → flush_trap_context_to_iret_frame(iret_frame, &frame)              [arch, untouched]
  → iretq                                                             [arch, untouched]
```

- **Before the global lock:** arch register save, CR2 read, `current_cpu_id`,
  the `entering_tid` snapshot (`with_cpu`→`current_tid`), trap-frame construction
  from saved regs, and the `handle_trap_entry_shared` PRE-LOCK SEAM (recv-timeout
  scheduler split-read + page-fault diagnostic split-mut). This seam takes only
  per-domain locks and does **not** write the syscall result.
- **After the global lock exits:** the `exiting_tid` snapshot, `task_switched`
  computation, the GPR/return-register writeback to saved regs
  (`write_task_gprs_to_saved_regs` / `write_trap_returns_to_saved_regs`), the
  iret-frame flush, and `iretq`.
- **What the lock wraps:** the global lock wraps only the *logical* handler
  (`handle_trap` → `dispatch`), which both reads the trap frame args AND writes the
  result registers (`frame.set_ok(..)`). It does not wrap the arch register save
  or the iret-frame flush — those bracket it. `current_tid` is per-CPU scheduler
  state read via `with_cpu` under the global lock (Stage 4T+6R deliberately kept
  this on the global-lock path after the split-read variant broke the x86_64
  service chain in smoke).

### 46.2 Split-dispatch bridge design

`src/kernel/syscall_split.rs`:

- `enum SplitEligibleSyscall` — whitelist of eligible syscalls. Currently the
  single variant `ControlPlaneCnodeSlots { requester_tid, target_pid, slots }`.
- `classify_split_eligible(syscall, requester_tid, args) -> Option<…>` — maps a
  decoded `Syscall` + raw `[u64;6]` args to a descriptor; `_ => None` default-deny.
  For the control-plane syscall it also pre-validates `target_pid != 0 && slots
  != 0`, returning `None` on a precondition miss so the canonical `InvalidArgs`
  encoding is produced by the global-lock fallback.
- `try_split_dispatch(shared, syscall, requester_tid, args) -> Option<Result<(),
  KernelError>>` — returns `Some(result)` only for whitelisted syscalls (serviced
  via `control_plane_set_process_cnode_slots_split_mut`), else `None`. The bridge
  itself never blocks, yields, schedules, or copies user memory.

**Fallback guarantee.** `_ => None` is the default arm. Any syscall not on the
whitelist — all IPC, Spawn/fork/exec, VM, futex — returns `None`, and the caller
falls back to the unchanged global-lock dispatch. Adding the bridge therefore
cannot change the behavior of any non-whitelisted syscall.

### 46.3 Whitelist / classification table

| syscall/path | class | domains | trapframe? | block? | split eligible? | action |
|--------------|-------|---------|------------|--------|-----------------|--------|
| `control_plane_set_cnode_slots` (NR 8) | A (helper-only / B) | task(2)→bootcfg→cap(4) | yes (`set_ok(slots,pid,0)`) | no | **yes (whitelisted)** | classify+`try_split_dispatch` → `…_split_mut`; helper-only |
| `notification_waiter_count` | C (read split done §44/§26) | ipc(3) | n/a (not syscall-visible) | no | n/a (helper, not a syscall) | split-read helper only |
| IPC send (NR 1) | G | ipc(3)+task+sched | yes | yes (can block) | no | global-lock fallback |
| IPC recv (NR 2) | F/G | ipc(3)+task+sched | yes | yes (blocks) | no | global-lock fallback |
| IPC call (NR 6) | F/G | ipc(3)+task+sched | yes | yes (blocks) | no | global-lock fallback |
| IPC reply (NR 7) | G | ipc(3)+task+sched | yes | maybe (wakes) | no | global-lock fallback |
| IPC recv-timeout (NR 5) | F/D | ipc(3)+sched(1) | yes | yes (blocks/deadline) | no | global-lock fallback (recv-timeout staging only, §L4A) |
| SpawnV5 (NR 23/24/26/29) | H | task+cap+vm+mem+ipc | yes | no but heavy | no | global-lock fallback |
| fork/exec (NR 12) | H | task+cap+vm+mem | yes | no but heavy | no | global-lock fallback |
| VM map / anon-map / brk (NR 3/13/14) | I | vm(5)+mem(6)+cap | yes | no (shootdown wait) | no | global-lock fallback |
| futex wait/wake (NR 9/10) | F | sched+task | yes | wait blocks | no | global-lock fallback |
| fault / trap paths | J | varies | n/a | varies | no (arch boundary) | untouched |

### 46.4 Why dangerous classes remain deferred

- **IPC (G/F):** touch ipc(3)+task(2)+scheduler(1), can block/yield/schedule, and
  must run inside the global-lock dispatch so the `dispatch` WouldBlock-reschedule
  epilogue (`syscall.rs:3500`) and the arch `task_switched` writeback stay correct.
- **Spawn/fork/exec (H):** span task+capability+vm+memory(+ipc) domains with
  partial-commit rollback; not expressible as a single ascending-rank split.
- **VM map/unmap/brk (I):** cross vm(5)+memory(6)+capability(4) with TLB-shootdown
  waits; multi-domain and latency-sensitive.
- **futex (F):** blocks/wakes via the scheduler.
- **fault/trap (J):** arch boundary — never touched.

### 46.5 x86_64 entering/exiting TID — untouched, and why

The `entering_tid` / `exiting_tid` snapshots in the x86_64 shared trap path
(`descriptor_tables.rs:903` / `:931`) remain on the global-lock
`with_cpu(cpu, |k| k.current_tid())` path. Stage 4T+6 converted them to
`current_tid_split_read` and that broke the x86_64 service chain in smoke (Stage
4T+6R revert); they are a hard invariant ("do not touch x86_64 entering/exiting
TID logic") and Stage 28 leaves them exactly as-is. `task_switched` detection
drives the GPR-vs-return-register writeback branch, so it must observe the same
scheduler state the dispatch saw.

### 46.6 `handle_trap_with_cpu` — retained, and why

`SharedKernel::handle_trap_with_cpu` (`runtime.rs:277`) is retained unchanged. It
is the global-lock dispatch entry used by non-shared/raw trap paths and is a hard
invariant ("do not remove handle_trap_with_cpu"). The split bridge is purely
additive and does not replace or alter it.

### 46.7 Live-wired or helper-only — decision + blocker

**Helper-only this stage (NOT live-wired).** The whitelisted candidate's
production handler writes a *non-trivial trapframe payload*
(`frame.set_ok(slot_capacity, target_pid, 0)` — two meaningful return registers).
`try_split_dispatch` returns only the logical `Result<(), KernelError>` and
deliberately does not touch the `TrapFrame`.

**Exact missing arch abstraction.** To live-wire safely the x86_64 arch seam would
need, *before* the global lock:
1. A pre-lock result-writeback contract: a seam that has both `&mut TrapFrame` and
   the decoded args and is authorized to call `frame.set_ok(slots, pid, 0)` /
   `set_err(code)` itself. The existing `handle_trap_entry_shared` pre-lock seam
   (`trap_entry.rs:105`) only stages diagnostic fault / recv-timeout data and owns
   no result-writeback contract.
2. Preservation of `entering_tid` / `exiting_tid` / `task_switched`: the
   control-plane syscall never switches tasks, so the split path must still make
   `task_switched == false` observable so the `write_trap_returns_to_saved_regs`
   branch (not the GPR branch) flushes the frame.

Both are arch-sensitive and fall under the "do not touch x86_64 entering/exiting
TID logic" / "do not touch the trap boundary" hard invariants. Until that
result-writeback abstraction exists, the bridge stays helper-only and is proven by
unit tests (§46.8). Because no live trap/runtime dispatch behavior changed, **no
x86_64 smoke is required**.

### 46.8 Tests added (8)

In `src/kernel/syscall_split.rs` (`mod tests`):
`stage28_split_dispatch_whitelist_accepts_cnode_slots_syscall`,
`stage28_split_dispatch_whitelist_rejects_ipc_send`,
`stage28_split_dispatch_whitelist_rejects_ipc_recv`,
`stage28_split_dispatch_whitelist_rejects_spawnv5`,
`stage28_split_dispatch_whitelist_rejects_vm_map`,
`stage28_split_dispatch_fallback_preserved_for_unwhitelisted`,
`stage28_syscall_count_unchanged`,
`stage28_stage27_split_mut_helper_still_works`.

### 46.9 Smoke decision

No live trap/syscall dispatch behavior changed (bridge is helper-only; default-deny
fallback keeps every live syscall on the global-lock path). **x86_64 smoke not
required.** Full suite intentionally not run (helper-only stage); focused Stage 28
+ Stage 27 tests pass.

### 46.10 Stage 28 acceptance record

- **Accepted at commit `0d720b7`** (`Stage 28: split-dispatch bridge scaffold +
  trap/syscall seam audit`) on `claude/ecstatic-feynman-9ZZwC`.
- **Delivered:** the whitelist-only split-dispatch bridge scaffold in
  `src/kernel/syscall_split.rs` (`SplitEligibleSyscall`, `classify_split_eligible`,
  `try_split_dispatch`), default-deny `_ => None`, sole whitelist variant
  `ControlPlaneCnodeSlots` (NR 8). 8 unit tests; `SYSCALL_COUNT == 30` preserved.
- **Helper-only — exact blocker:** there was no *pre-global-lock result-writeback
  contract*. The candidate handler writes a two-register payload
  (`set_ok(slots, pid, 0)`); the bridge returned only `Result<(), KernelError>` and
  did not own `&mut TrapFrame`. The existing `handle_trap_entry_shared` pre-lock
  seam staged only diagnostic fault / recv-timeout data.
- **Stage 29 target (this stage):** introduce that result-writeback contract and
  live-wire ONLY `ControlPlaneSetCnodeSlots` / NR 8.
- **Still deferred after Stage 28:** x86_64 SMP (`smp.rs`) and RAMFS/FAT runtime
  spawning remain out of scope; IPC/Spawn/VM/futex/fault stay on the global lock.

## §47 Stage 29 — live-wire ControlPlaneSetCnodeSlots split dispatch (NR 8)

Stage 29 introduces the smallest safe **pre-global-lock syscall result-writeback
seam** and live-wires exactly one whitelisted syscall —
`ControlPlaneSetCnodeSlots` / NR 8 — through the Stage 28 split-dispatch bridge.
This is the first live syscall extraction from the global `SpinLock<KernelState>`.

### 47.1 Seam audit result

The audit confirmed three facts. (1) The architecture-neutral
`handle_trap_entry_shared` (`src/arch/trap_entry.rs`) already owns `&mut TrapFrame`
*before* the global lock is taken (`shared.with_cpu(...)`), and on x86_64 the
trap frame's `syscall_num()` / `arg(i)` are already populated at this seam (the
existing recv-timeout staging reads them here). (2) `TrapFrame::set_ok` / `set_err`
(`src/kernel/trapframe.rs`) are pure return-register writes with no global-lock
dependency, so they may be called directly from the pre-lock seam. (3) The requester
TID the old handler read via `current_tid(kernel)` is value-equivalent to
`SharedKernel::current_tid_split_read(cpu)` (scheduler lock only; proven in
`runtime.rs` Stage 4T+6 tests), so the split path needs no global lock to obtain it.

### 47.2 Result-writeback contract

`set_ok` / `set_err` are architecture-neutral and safe to call directly from the
split seam, so **no new contract type was added.** Instead Stage 29 adds one
function, `try_split_dispatch_into_frame(shared, cpu, frame)` in
`src/kernel/syscall_split.rs`, which:

1. Default-denies by syscall number (`classify_split_eligible_nr_only`) — fast gate,
   no lock.
2. Reads the requester TID via `current_tid_split_read(cpu)`; on `None` returns
   `None` so the global-lock path produces the canonical `Internal` error.
3. Decodes `(target_pid, slots)` from the frame exactly as
   `handle_control_plane_set_cnode_slots` does (`arg(SYSCALL_ARG_CAP)` /
   `arg(SYSCALL_ARG_PTR)`), then calls `try_split_dispatch` (Stage 28).
4. On `Ok(())` writes `frame.set_ok(slots, target_pid, 0)` — byte-for-byte the old
   handler's encoding — and returns `Some(Ok(()))`.
5. On a domain error returns `Some(Err(TrapHandleError::Syscall(SyscallError::from(
   kernel_err))))` — exactly the value the old `Err(SyscallError)` return became at
   the trap boundary. NR 8's domain errors were never user-recoverable: the old
   handler returned `Err` (never `set_err`), so the arch stub treats them as the
   same propagated/fatal error on both paths.

### 47.3 Live-wired: yes — NR 8 only

`handle_trap_entry_shared` now calls `try_split_dispatch_into_frame` BEFORE
`with_cpu`, gated on `TrapEvent::Syscall`. `Some(result)` ⇒ the seam wrote the
frame and we `return result`, skipping the global lock entirely. `None` ⇒ the
existing global-lock dispatch runs UNCHANGED. The whitelist remains default-deny
with the single member NR 8; `SYSCALL_COUNT == 30` and the syscall ABI are
unchanged. A low-noise serial breadcrumb (`YARM_LOCK_SPLIT_DISPATCH nr=8 ...`) is
emitted on the split path.

### 47.4 Before/after path for NR 8

```
BEFORE (Stage 28, global-lock):
  hw vector → IDT stub → entering_tid (with_cpu) → build frame
    → dispatch_trap_entry_with_shared_kernel → handle_trap_entry_shared
        → shared.with_cpu(cpu, |k| handle_trap_event_… )      [GLOBAL LOCK]
            → handle_trap(Syscall) → dispatch → handle_control_plane_set_cnode_slots
                → frame.set_ok(slots, pid, 0)
    → exiting_tid (with_cpu) → task_switched=false → write_trap_returns → iretq

AFTER (Stage 29, split, NR 8 only):
  hw vector → IDT stub → entering_tid (with_cpu) → build frame
    → dispatch_trap_entry_with_shared_kernel → handle_trap_entry_shared
        → try_split_dispatch_into_frame(shared, cpu, frame)   [NO GLOBAL LOCK]
            → current_tid_split_read (sched lock) + split-mut helper
              (task rank 2 → bootcfg → cap rank 4)
            → frame.set_ok(slots, pid, 0)  → return
    → exiting_tid (with_cpu) → task_switched=false → write_trap_returns → iretq
```

The `entering_tid` / `exiting_tid` snapshots and the `task_switched` writeback
branch are bit-for-bit identical; only the *logical handler* moved off the global
lock. The split path performs no scheduler interaction, so
`entering_tid == exiting_tid` (`task_switched == false`) is preserved and the
`write_trap_returns_to_saved_regs` branch (not the GPR branch) flushes the frame.

### 47.5 Fallback for all other syscalls

Every non-whitelisted syscall number returns `None` from the seam and is dispatched
by the unchanged `shared.with_cpu(...)` → `handle_trap` → `dispatch` global-lock
path. A whitelisted syscall with a failed precondition (`target_pid == 0` /
`slots == 0`) or an absent requester TID also returns `None`, deferring the
canonical error encoding to the global-lock path. Adding the live seam therefore
cannot change the behavior of any syscall other than a fully-valid NR 8.

### 47.6 Why IPC / Spawn / VM / futex / fault remain deferred

Unchanged from §46.4: IPC (G/F) and futex (F) block/yield/schedule and must stay
inside the global-lock dispatch so the `dispatch` WouldBlock-reschedule epilogue and
arch `task_switched` writeback remain correct; Spawn/fork/exec (H) span
task+cap+vm+memory(+ipc) with partial-commit rollback not expressible as a single
ascending-rank split; VM map/unmap/brk (I) cross vm+memory+cap with TLB-shootdown
waits; fault/trap (K) is the arch boundary. None is a single-ascending-rank,
non-blocking, trapframe-encodable mutation, so none is whitelisted.

### 47.7 Why entering/exiting TID untouched

The `entering_tid` / `exiting_tid` snapshots in the x86_64 shared trap path
(`descriptor_tables.rs`) stay on the global-lock `with_cpu(cpu, |k| k.current_tid())`
path (hard invariant; Stage 4T+6R revert rationale). The Stage 29 seam reads
`current_tid_split_read(cpu)` only *internally* to identify the requester; it does
not touch the arch `entering_tid` / `exiting_tid` reads or the `task_switched`
computation that drives the GPR-vs-return-register writeback branch.

### 47.8 Why handle_trap_with_cpu retained

`SharedKernel::handle_trap_with_cpu` is retained unchanged (hard invariant). It is
the global-lock dispatch entry for non-shared/raw trap paths and the fallback for
every non-NR-8 syscall. The split seam is purely additive and does not replace it.

### 47.9 Smoke requirement

Because NR 8 is now live-wired, x86_64 `-smp 1` smoke IS required. The live
bare-metal `kernel_boot` binary builds clean for `targets/x86_64-yarm-none.json`
(`-Z build-std`, `-Z json-target-spec`). QEMU is **not installed in this remote
execution environment** (`qemu-system-x86_64` absent); the smoke run is therefore
**deferred to CI / manual run**. Acceptance markers to verify there:
`X86_BOOTSTRAP_TIMER_IRQ_EOI_ONLY=0`, `X86_BOOTSTRAP_SCHEDULER_READY=1`,
`X86_BOOTSTRAP_TIMER_STARTED=1`, `ENTER_USER=3`, `STARTUP_INSTALL_FINAL=9`,
`PM_ELF_ZC_DONE` (image_id 7/8/9, total=3, zc_pages>0), `PM_ELF_ZC_FAIL=0`,
`INITRAMFS_SRV_ENTRY=1`, `DEVFS_SRV_ENTRY=1`, `VFS_SRV_ENTRY=1`,
`DRIVER_MANAGER_READY=1`, `BLKCACHE_SRV_READY=1`, `VIRTIO_BLK_SRV_READY=1`,
fallback=0 / TID mismatch=0 / fatal-ish=0 / oom/capacity=0. The new split path also
emits `YARM_LOCK_SPLIT_DISPATCH nr=8 cpu=0 result=ok` whenever the control plane
resizes a process cnode.

### 47.10 Still deferred

x86_64 SMP (`src/arch/x86_64/smp.rs`) and RAMFS/FAT runtime spawning remain out of
scope and untouched. No claim of full global-lock removal is made: only NR 8 is
extracted; all other syscalls remain on the global lock.

### 47.11 Tests added (20)

In `src/kernel/syscall_split.rs` (`mod tests`), all prefixed `stage29_`:
behavior-equivalence — `…_ok_return_lanes`, `…_missing_task_error`,
`…_bad_requester_class_error`, `…_missing_cnode_error`, `…_duplicate_update_ok`,
`…_capacity_resize_ok`, `…_error_code_preserved`, `…_no_scheduler_side_effect`,
`…_no_ipc_side_effect`; fallback safety — `stage29_only_nr8_is_split_eligible`,
`stage29_ipc_send_not_eligible`, `stage29_spawnv5_not_eligible`,
`stage29_vm_map_not_eligible`, `stage29_futex_not_eligible`,
`stage29_syscall_count_still_30`, `stage29_whitelist_exhaustive`;
result-writeback — `stage29_split_result_ok_encodes_same_as_old_path`,
`stage29_split_result_err_encodes_same_as_old_path`,
`stage29_split_result_no_task_switch`,
`stage29_split_dispatch_fallback_path_unchanged`.

### 47.12 Path/syscall table

| path/syscall | class | old path | new path | global lock? | result writeback | task_switched | smoke? |
|---|---|---|---|---|---|---|---|
| ControlPlaneSetCnodeSlots / NR 8 | J | global-lock → handle_trap | split-dispatch bridge | no (split) | set_ok(slots,pid,0) | false | yes |
| IPC send/recv/call/reply | G | global-lock | unchanged | yes | unchanged | may block | deferred |
| IPC recv-timeout | F | pre-lock split-read | unchanged | yes for handler | unchanged | may block | deferred |
| SpawnV5 | H | global-lock | unchanged | yes | unchanged | yes | deferred |
| fork/exec | H | global-lock | unchanged | yes | unchanged | yes | deferred |
| VM map/unmap/brk/anon | I | global-lock | unchanged | yes | unchanged | false | deferred |
| futex | F | global-lock | unchanged | yes | unchanged | may block | deferred |
| fault/trap | K | global-lock | unchanged | yes | unchanged | varies | deferred |
| fallback default | L | global-lock | unchanged | yes | unchanged | varies | N/A |

## §47.13 Stage 29 / 29A acceptance record

**Accepted at commit `bf1a1e4`** (Stage 29A, branch `claude/ecstatic-feynman-9ZZwC`).

- `YARM_LOCK_SPLIT_DISPATCH nr=8 result=ok` count = 1 in x86_64 `-smp 1` smoke.
- `PM_NR8_SELF_PROBE_OK pid=3 slots=520` emitted — PM self-probe confirmed NR 8
  went through the real arch syscall trap, not a direct function call.
- All Phase3B / service-entry / fallback=0 / TID-mismatch=0 health markers intact.
- SYSCALL_COUNT == 30; no ABI changes.

**Stale-TID lesson (Stage 29A finding):** `current_tid_split_read(cpu)` reads the
scheduler's per-CPU current slot under the scheduler lock WITHOUT first binding
`current_cpu`. At the pre-global-lock x86_64 trap point this is stale — it can
return tid 0 (the previous occupant) instead of the running requester. Trap-seam
requester identity MUST use `current_tid_authoritative(cpu)`, which takes the
global lock just long enough to set `current_cpu` and read `current_tid()`. The
split-dispatch *mutation* still runs lock-free via the per-domain split-mut helper.

x86_64 SMP still deferred. RAMFS/FAT spawning still deferred.

---

## §48 Stage 30 — borrow_kernel_for_boot debug guard + split-helper validation labels

### 48.1 Purpose

Stage 30 implements two safety guardrails (Review findings C1 and C2) before
expanding live trap-seam split dispatch to more syscalls.

**C1** — `borrow_kernel_for_boot` opens a raw `&mut KernelState` aliasing window
with no debug guard. If a timer ISR or trap entry fired and called `with_cpu`
during that window, two `&mut KernelState` references would exist simultaneously
— undefined behavior.

**C2** — Split-read/split-mut helpers lacked explicit validation-status labels
making it easy to misuse them (e.g., the Stage 29A regression used the
TRAP_FORBIDDEN `current_tid_split_read` from the trap seam).

### 48.2 borrow_kernel_for_boot canonical safety contract

`borrow_kernel_for_boot` must only be called during single-CPU boot:

1. Before the arch trap handler is installed (`TRAP_KERNEL_STATE_PTR` is null while
   the borrow is live, so the trap fallback cannot also yield `&mut KernelState`).
2. Before external interrupts are unmasked for normal operation (LAPIC/timer
   deadline is far beyond the boot window; no timer ISR fires during it).
3. The returned `&mut KernelState` must not be used after the ERET to user space.

If a timer ISR DID fire during the window and reach `with_cpu`, it would build a
second aliasing `&mut KernelState` — UB.

### 48.3 Debug guard design

New items in `src/runtime.rs` under `#[cfg(any(debug_assertions, test))]`:

```
BOOT_RAW_BORROW_ACTIVE: AtomicBool      — global flag, zero release cost
begin_boot_raw_borrow_window()          — sets flag; debug_asserts no double-borrow
end_boot_raw_borrow_window()            — clears flag
boot_raw_borrow_is_active() -> bool     — current flag value
BootRawKernelBorrowGuard                — RAII: Drop clears flag (test/returning paths)
```

`borrow_kernel_for_boot` calls `begin_boot_raw_borrow_window()` unconditionally
(no-op in release). The live boot path is non-returning (ERET), so the window is
intentionally never closed in production — the flag becomes irrelevant after ERET
since all subsequent KernelState access goes through `with` / `with_cpu`.

### 48.4 Arch timer/trap guard wiring

At the **top** of each arch trap/vector entry (before any `SharedKernel` access):

- **x86_64**: `yarm_x86_dispatch_trap_from_stub` (`src/arch/x86_64/descriptor_tables.rs`)
- **AArch64**: `yarm_aarch64_vector_entry` (`src/arch/aarch64/boot.rs`)
- **RISC-V**: not wired — does not call `borrow_kernel_for_boot`

```rust
#[cfg(any(debug_assertions, test))]
debug_assert!(
    !crate::runtime::boot_raw_borrow_is_active(),
    "trap/timer fired during boot raw-borrow window — aliasing &mut KernelState risk"
);
```

Compiles to nothing in release. Zero ISR/vector overhead in production.

### 48.5 Split-helper validation-status labels

Policy: each split helper carries a one-line `# Validation status` doc comment
using this vocabulary:

| Tag | Meaning |
|---|---|
| `UNIT_ONLY` | hosted-dev/test only; never on arch trap path |
| `LIVE_OFF_TRAP` | called from off-trap kernel code (boot, control-plane) |
| `LIVE_TRAP_SMOKE_X86_64` | on pre/post-global-lock trap seam; x86_64 smoke validated |
| `TRAP_FORBIDDEN` | MUST NOT be called from pre-global-lock trap seam (stale data) |
| `HELPER_ONLY` | scaffold/helper; not yet wired to live trap path |
| `REQUIRES_AUTHORITATIVE_TID` | trap-seam callers must use `current_tid_authoritative` |

Applied labels (Stage 30):

| helper | status |
|---|---|
| `current_tid_split_read` | TRAP_FORBIDDEN / REQUIRES_AUTHORITATIVE_TID |
| `current_tid_authoritative` | (existing Stage 29A doc; authoritative for trap seam) |
| `scheduler_tick_now_split_read` | LIVE_TRAP_SMOKE_X86_64 |
| `with_fault_split_mut` | LIVE_TRAP_SMOKE_X86_64 |
| `with_telemetry_split_mut` | LIVE_OFF_TRAP |
| `ipc_recv_with_deadline_split_bridge` | LIVE_OFF_TRAP |
| `notification_waiter_count_split_read` | LIVE_OFF_TRAP |
| `cnode_registered_split_read` | LIVE_OFF_TRAP |
| `control_plane_set_process_cnode_slots_split_mut` | LIVE_TRAP_SMOKE_X86_64 |
| `try_split_dispatch_into_frame` | LIVE_TRAP_SMOKE_X86_64 |
| `online_cpu_count_split_read` | UNIT_ONLY |
| `present_cpu_count_split_read` | UNIT_ONLY |
| `capacity_profile_split_read` | UNIT_ONLY |
| `borrow_kernel_for_boot` | LIVE_OFF_TRAP (raw aliasing window, C1) |

### 48.6 No live syscall expansion in Stage 30

The split-dispatch whitelist remains NR 8 only. IPC, SpawnV5, VM, futex, fault,
and SMP paths remain entirely on the global lock.

### 48.7 Smoke decision

All additions are `#[cfg(any(debug_assertions, test))]` / `debug_assert!` and
compile to nothing in release. No release/live timer/trap/bootstrap behavior
changed. x86_64 smoke is deferred for Stage 30.

### 48.8 Still deferred

x86_64 SMP; RAMFS/FAT runtime spawning; live split-dispatch expansion beyond NR 8.

### 48.9 Tests added (Stage 30)

6 tests in `src/kernel/boot/tests.rs` module `stage30_boot_guard_tests`:
`stage30_boot_raw_borrow_guard_begin_sets_active`,
`stage30_boot_raw_borrow_guard_end_clears_active`,
`stage30_boot_raw_borrow_guard_double_begin_panics` (`#[should_panic]`),
`stage30_raii_guard_clears_on_drop`,
`stage30_timer_guard_detects_active_window_in_test`,
`stage30_syscall_count_still_30`.
All pass single-threaded. Full lib suite: 871 passed, 0 failed.

### 48.10 Stage 30 terminal acceptance record

**Accepted at commit `77af2cb`** (Stage 30, branch `claude/ecstatic-feynman-9ZZwC`).

- **`BOOT_RAW_BORROW_ACTIVE` debug guard:** `borrow_kernel_for_boot` now opens a
  debug-only "raw borrow active" window (`begin_boot_raw_borrow_window` /
  `end_boot_raw_borrow_window`, RAII guard). If a timer ISR reaches `with_cpu`
  while the window is active, a `debug_assert!` fires — surfacing the C1 aliasing
  hazard (a second `&mut KernelState` aliasing the boot borrow) in debug/test
  builds. Compiles to nothing in release; no live timer/trap/bootstrap change.
- **Validation-status labels:** every split helper now carries a
  `# Validation status` doc label (`LIVE_TRAP_SMOKE_X86_64`, `LIVE_OFF_TRAP`,
  `UNIT_ONLY`, `TRAP_FORBIDDEN`, `HELPER_ONLY`), so a reader can tell at a glance
  whether a helper is proven on the live trap seam, off-trap only, unit-only, or
  forbidden at the trap seam.
- **`current_tid_split_read` TRAP_FORBIDDEN lesson (carried forward):** the
  scheduler per-CPU current-slot split read is stale at the pre-global-lock x86_64
  trap seam (Stage 29A returned tid 0 instead of the running requester). Any new
  trap-seam split path MUST read requester identity via
  `current_tid_authoritative(cpu)`. Stage 31 obeys this.
- **Stage 31 target:** the first IPC live fast-path candidate — `IpcRecv` of a
  plain message already queued on a buffered endpoint, non-blocking, no cap
  transfer, no recv-v2 metadata, no sender wake. Default-deny for every other IPC
  case.
- **Still deferred:** x86_64 SMP (`smp.rs`); RAMFS/FAT runtime spawning.

## §49 Stage 31 — IPC queued plain recv fast-path split (helper-only)

### 49.1 Stage 30 acceptance (Part 0)

Recorded in §48.10 above: accepted at `77af2cb`; `BOOT_RAW_BORROW_ACTIVE` guard;
validation-status labels; `current_tid_split_read` TRAP_FORBIDDEN lesson carried
forward.

### 49.2 IPC recv case classification (Part 1 audit)

`IpcRecv` is **NR 2** (`SYSCALL_IPC_RECV_NR == 2`). Args:
`arg(SYSCALL_ARG_CAP)=recv endpoint cap`, `arg(SYSCALL_ARG_PTR)=user payload ptr`,
`arg(SYSCALL_ARG_LEN)=user payload len`, `arg(SYSCALL_ARG_INLINE_PAYLOAD0)=meta
ptr (recv-v2)`, `arg(SYSCALL_ARG_INLINE_PAYLOAD1)=meta len (recv-v2)`. A recv-v2
request is detected as `meta_ptr != 0 && meta_len >= 40`.

The endpoint queue dequeue is already domain-split: `try_endpoint_split_recv` →
`ipc_try_recv_queued_plain_endpoint_only` (Stage 4C/4D scaffolding) mutates only
`ipc_state_lock` (rank 3) and returns `Received(msg)` for a plain queued message,
`ReceivedWithSenderWake(msg, tid)` when a plain sender waiter must be refilled, or
`Ineligible(reason)` otherwise. What is NOT split today is the *writeback*
(`handle_ipc_recv_result_with_empty_error`): for a user-ASID receiver it requires
`copy_to_current_user` (user-memory copy) and possibly shared-memory mapping; for
a kernel task (no user ASID) it is a pure register write
(`set_ok(sender, raw_len, NO_TRANSFER_CAP)` + two inline payload words).

Case audit:

| case | class | disposition |
|---|---|---|
| A queued plain, no cap, no meta, kernel-task | candidate | split (helper) |
| A' queued plain, no cap, no meta, **user-ASID** | needs user-copy | fallback |
| B queued + sender-waiter refill | scheduler interaction | defer (None) |
| C recv-v2 metadata | cap/meta materialization | defer (None) |
| D cap-transfer receive | cap domain | defer (None) |
| E reply-cap materialization | cap domain | defer (None) |
| F empty endpoint / WouldBlock | not an error | fallback (None) |
| G timeout receive | deadline logic | defer (None) |
| H blocking receive | scheduler interaction | defer (None) |
| I invalid cap / fault | error | match old error (Some(Err)) |
| J non-IpcRecv IPC (send/call/reply) | out of scope | reject (None) |

### 49.3 Exact split coverage

Stage 31 splits **only case A**: a plain (no `FLAG_CAP_TRANSFER` /
`FLAG_CAP_TRANSFER_PLAIN` / `FLAG_REPLY_CAP`, no transferred cap) message already
queued on a buffered endpoint, delivered to a **kernel-task receiver (no user
ASID)**, with **no recv-v2 metadata** requested and **no sender-waiter refill**.
Sender wake is **NOT** included — any `ReceivedWithSenderWake` result is rejected
(→ `None`) and falls back to the global path.

### 49.4 Live-wired vs helper-only — DECISION: helper-only

`try_split_recv_queued_plain_into_frame_locked` (in `src/kernel/syscall.rs`) and
the `SharedKernel::try_split_ipc_recv_queued_plain_into_frame` wrapper (in
`src/runtime.rs`), plus the `syscall_split::try_split_ipc_recv_queued_plain_into_frame`
entry point, are added with full tests but are **intentionally NOT wired** into
`try_split_dispatch_into_frame`. IpcRecv stays default-deny in the live seam.

**Blocker (why not live-wired):** the realistic live x86_64 receivers (PM, init,
VFS and other servers) are **user-ASID** tasks. Their plain-recv writeback needs
`copy_to_current_user` (a user-memory copy), which is forbidden under the Stage 31
split lock rules, and endpoint-cap resolution crosses the capability domain
(rank 4) which has no proven split extraction yet. The helper rejects every
user-ASID receiver (case A'), so it can only ever fast-path a kernel-task
receiver — which the live boot path does not exercise. Wiring it live would gate a
fast path that never fires while adding a trap-seam branch, with no smoke
coverage. Helper-only keeps the proven dequeue+writeback equivalence available for
a future stage that first extracts the capability-resolution and user-copy domains.

### 49.5 Fallback / split matrix

| case | split? | reason | lock domains | writeback | fallback? |
|---|---|---|---|---|---|
| queued plain recv (kernel task) | yes (Stage 31, helper-only) | fast-path, no deps | ipc rank 3 | set_ok(sender,len,NO_CAP) + inline words | no |
| queued plain recv (user ASID) | no | needs user-copy | — | — | yes |
| empty endpoint | no | WouldBlock → fallback | — | — | yes |
| blocking recv | no | scheduler interaction | — | — | yes |
| recv-timeout | no | deadline logic | — | — | yes |
| recv-v2 metadata | no | cap/meta materialization | — | — | yes |
| cap-transfer recv | no | cap domain needed | — | — | yes |
| reply-cap materialization | no | cap domain needed | — | — | yes |
| sender-waiter refill | no (Stage 31) | scheduler interaction | — | — | yes |
| invalid cap | no/error | match old error | — | same error | yes |
| non-IpcRecv IPC | no | out of scope | — | — | yes |

### 49.6 Lock order

```
[no lock] → current_tid_authoritative (takes+releases global) →
            ipc_state_lock (rank 3, via ipc_try_recv_queued_plain_endpoint_only) →
            [release] → [no lock]
```

Forbidden under `ipc_state_lock`: scheduler lock, capability lock, VM lock,
user-copy. `task_switched` is always `false` (no dispatch / yield / switch); no
deferred wake plan is produced because the sender-waiter-refill case is rejected.
(NOTE: the helper-only `SharedKernel` wrapper currently performs cap-resolution +
dequeue + writeback under the global `with` lock, because the capability domain is
not yet split-extracted; the *dequeue itself* still touches only the IPC domain.
This is the blocker that keeps Stage 31 helper-only — see §49.4.)

### 49.7 Return / writeback equivalence

The split writeback reproduces the kernel-task (no-user-ASID) branch of
`handle_ipc_recv_result_with_empty_error` for a plain message exactly:
`recv_meta_flags == 0`, `materialize_received_message_cap → None`,
`encode_transfer_cap_ret(frame, None)` ⇒ `ret2 = NO_TRANSFER_CAP`,
`set_ok(sender, msg.as_slice().len(), ret2)`, and the two inline payload words from
`pack_register_payload(msg.as_slice())`. Proven by
`stage31_split_recv_return_lanes_match_old_path`, which drives the unchanged
`syscall::dispatch` recv path on an identical state and asserts ret0/ret1/ret2 +
error lane + both inline payload words are byte-for-byte equal.

### 49.8 Rejected cases and why

recv-v2 (would materialize metadata into the user meta buffer); cap-transfer /
reply-cap (capability domain mutation outside ipc rank 3); user-ASID receiver
(forbidden user copy); sender-waiter refill (scheduler wake); empty queue /
timeout / blocking (no message, scheduler interaction); non-IpcRecv (out of
scope). Invalid cap is NOT rejected to fallback — it returns the same `Some(Err(
TrapHandleError::Syscall(..)))` the old path produced, proven by
`stage31_split_recv_invalid_endpoint_cap_error`.

### 49.9 Smoke requirement

Not required for Stage 31: the path is **helper-only**, not wired into the live
trap seam, so it changes no live trap behavior. The existing NR-8 live split-
dispatch is untouched (`stage31_nr8_split_still_works` regression). x86_64 smoke
is deferred for Stage 31; `YARM_LOCK_SPLIT_DISPATCH nr=8 result=ok` continues to be
the live split marker. No `ipc_recv_queued_plain` live marker is emitted because
the path is not live-wired.

### 49.10 Still deferred

x86_64 SMP (`smp.rs`); RAMFS/FAT runtime spawning; live-wiring the IPC recv split
(blocked on capability-domain + user-copy split extraction); sender-wake refill;
recv-v2 / cap-transfer / timeout / blocking recv splits.

### 49.11 Tests added (Stage 31)

15 tests in `src/kernel/boot/tests.rs` module `stage31_split_recv_tests`:
`stage31_split_recv_queued_plain_succeeds`,
`stage31_split_recv_return_lanes_match_old_path`,
`stage31_split_recv_dequeues_exactly_one_message`,
`stage31_split_recv_empty_queue_falls_back`,
`stage31_split_recv_cap_transfer_flag_falls_back`,
`stage31_split_recv_blocking_flag_falls_back`,
`stage31_split_recv_invalid_endpoint_cap_error`,
`stage31_split_recv_non_ipc_syscalls_rejected`,
`stage31_nr8_split_still_works`,
`stage31_syscall_count_still_30`,
`stage31_split_recv_task_switched_false`,
`stage31_split_recv_no_waiter_leak`,
`stage31_split_recv_sharedkernel_wrapper_succeeds`,
`stage31_split_recv_not_wired_into_live_seam`,
`stage31_split_recv_invalid_cap_matches_dispatch_error`.
All pass single-threaded. Full lib suite: 886 passed, 0 failed.

### 49.12 Stage 31 terminal acceptance record (Part 0 of Stage 32)

**Accepted as helper-only at commit `424162b`** (Stage 31, branch
`claude/ecstatic-feynman-9ZZwC`).

- **What landed:** `try_split_recv_queued_plain_into_frame_locked`
  (`src/kernel/syscall.rs`), the `SharedKernel::try_split_ipc_recv_queued_plain_into_frame`
  wrapper (`src/runtime.rs`), and the `syscall_split::try_split_ipc_recv_queued_plain_into_frame`
  entry point. `IpcRecv` is **NR 2** (`SYSCALL_IPC_RECV_NR == 2`).
- **Why NOT live-wired (both blockers, see §49.4):**
  1. **Capability-domain blocker** — endpoint-cap resolution crosses the
     capability domain (rank 4) and had **no proven split extraction** at Stage 31;
     the helper resolved the cap under the global `with` lock.
  2. **User-copy blocker** — realistic x86_64 receivers (PM/init/VFS) are
     **user-ASID** tasks whose plain-recv writeback needs `copy_to_current_user`
     **outside `ipc_state_lock`**, which had no split-safe writeback plan.
- **Baseline:** 886 tests, 0 failed. x86_64 smoke health: NR 8 live split marker
  (`YARM_LOCK_SPLIT_DISPATCH nr=8 result=ok`) ×1, fallback=0, Phase3B ×3; no IPC
  recv live marker (path not live-wired).
- **Stage 32 target:** extract the capability-domain endpoint-cap resolution into a
  phase-separated split-read helper; integrate it into the Stage 31 helper before
  the IPC dequeue; design/scaffold the user-copy-outside-`ipc_state_lock` recv
  writeback seam. Do NOT live-wire `IpcRecv` unless both blockers are fully solved
  and tested.
- **Still deferred:** x86_64 SMP (`smp.rs`); RAMFS/FAT runtime spawning.

## §50 Stage 32 — endpoint-cap resolution split + IPC recv user-writeback seam

### 50.1 Stage 31 acceptance (Part 0)

Recorded in §49.12 above: accepted as helper-only at `424162b`; both live-wiring
blockers (capability-domain resolution, user-copy-outside-ipc-lock) carried
forward as the Stage 32 work items.

### 50.2 Endpoint-cap resolution audit (Part 1)

The global-lock `IpcRecv` path (`handle_ipc_recv`, `src/kernel/syscall.rs`)
resolves the receive cap in two steps, both inside the global `with` lock:

1. `validate_endpoint_right(kernel, cap, CapRights::RECEIVE)` — looks the cap up in
   the **current task's cnode** (`current_task_cnode` → `with_task_then_capability`,
   which holds task(2) **and** capability(4) **simultaneously**), then checks it is
   an `Endpoint` (`WrongObject` otherwise) carrying `RECEIVE` (`MissingRight`
   otherwise); a missing cnode/slot is `InvalidCapability`. It also calls
   `capability_object_live`, which for an `Endpoint` reads
   `ipc.endpoint_generations[index]` under **`ipc_state_lock` (rank 3)**.
2. A second `capability_for_cnode_local(...).and_then(capability_object_live)`
   re-lookup yields the `CapObject::Endpoint { index, generation }` used for the
   dequeue.

**Fields needed for the dequeue:** the `CapObject::Endpoint { index, generation }`
(the `index` selects the queue; the `generation` revalidates liveness in
`resolve_endpoint_index` under `ipc_state_lock`) plus the requester identity.

**Locks crossed (old path):** task(2)+capability(4) **held together** in
`current_task_cnode`, plus ipc(3) for the generation liveness check
(`capability_object_live`). Stage 32 phase-separates these: pid under task(2)
read+release, cnode+cap+rights under capability(4) read+release, and the generation
liveness check is deferred to the ipc(3) dequeue phase.

**Errors (exact `SyscallError`):** `InvalidCapability` (missing cnode/slot),
`WrongObject` (non-endpoint, also `StaleCapability` via `From<KernelError>`),
`MissingRight` (no RECEIVE). The split helper returns `KernelError`; the
integration maps it through `SyscallError::from` to the same codes.

**Copy ordering (old path, `handle_ipc_recv_result_with_empty_error`):** the
message is **dequeued first** (consumed from the endpoint queue), THEN
`copy_to_current_user` runs for a user-ASID receiver. On `UserMemoryFault` the
path records a user fault and returns `Ok(())` — **the message is NOT requeued; it
is consumed/lost.** For a kernel task (no user ASID) the writeback is a pure
register write (`set_ok(sender, raw_len, NO_TRANSFER_CAP)` + two inline payload
words) with no copy and no failure mode.

### 50.3 Split helper implemented (Part 2)

- **`EndpointRecvCapSnapshot`** (`src/runtime.rs`) — small immutable `Copy` struct:
  `{ endpoint: CapObject, rights: CapRights, requester_tid: u64, requester_pid: u64 }`,
  plus `endpoint_index()`.
- **`KernelState::resolve_endpoint_recv_cap_in_pid_from_raw(state, requester_pid, cap)`**
  (`src/kernel/boot/orchestrator_state.rs`) — capability-domain (rank 4) phase:
  acquires ONLY `capability_state_lock`, finds the requester pid's cnode, looks up
  + validates the cap (`Endpoint` + `RECEIVE`), returns `(CapObject, CapRights)`.
  No mutation, no IPC lock, no task lock.
- **`SharedKernel::resolve_endpoint_recv_cap_split_read(requester_tid, cap)`**
  (`src/runtime.rs`) — orchestrates Phase 1 (`process_id_from_raw`, task(2)
  read+release) → Phase 2 (capability(4) read+release), returns
  `Result<EndpointRecvCapSnapshot, KernelError>`.

**Lock order:** `task(2) [read+release] → capability(4) [read+release]`. No nested
locks. **ipc(3) is acquired only AFTER this function returns** (the dequeue phase).
No global lock required. No capability mutation.

### 50.4 Integration into the Stage 31 helper (Part 3)

`SharedKernel::try_split_ipc_recv_queued_plain_into_frame` (`src/runtime.rs`) now:

1. reads the authoritative requester TID (`current_tid_authoritative`),
2. calls `resolve_endpoint_recv_cap_split_read` — on `Err(e)` returns
   `Some(Err(TrapHandleError::Syscall(SyscallError::from(e))))` (same error as the
   old path; **no fallback**),
3. on `Ok(snapshot)` releases the capability lock and acquires the IPC domain (via
   the global `with` for this helper-only path) to run
   `try_split_recv_queued_plain_with_snapshot_locked`
   (`src/kernel/syscall.rs`), which revalidates recv-v2 default-deny, rejects
   user-ASID receivers, revalidates endpoint liveness + dequeues under
   `ipc_state_lock` (rank 3) via `try_endpoint_split_recv`/`resolve_endpoint_index`,
   and writes the kernel-task lanes byte-for-byte identical to the old path.

**The capability lock and the IPC lock are NEVER held simultaneously.** The
generation-based liveness revalidation happens under `ipc_state_lock` inside
`resolve_endpoint_index`; a stale snapshot (generation mismatch / vanished
endpoint) returns `None` (fallback), never a wrong-endpoint dequeue. The
monolithic Stage 31 helper `try_split_recv_queued_plain_into_frame_locked` is
retained unchanged for Stage 31 regression tests.

### 50.5 Writeback plan (Part 4) — SCAFFOLDED, user-ASID DEFERRED

`IpcRecvQueuedPlainWritebackPlan` + `MAX_PLAIN_PAYLOAD == 128` (`src/runtime.rs`)
captures `{ payload[128], payload_len, sender_tid, ret_cap, user_payload_ptr,
user_payload_len, is_kernel_task, endpoint }`, with a `for_kernel_task`
constructor (rejects payloads > `MAX_PLAIN_PAYLOAD`) and accessors. The intended
protocol: resolve cap (cap lock only) → under `ipc_state_lock` dequeue + copy
payload into `plan.payload[]` → release lock → outside all locks write the
trap-frame lanes (kernel task) or `copy_to_current_user` (user ASID).

**User-ASID path is DEFERRED (not wired).** Matching the old path's
**message-consumed-on-copy-fail** semantics across a post-dequeue,
post-lock-release `copy_to_current_user` is not yet proven safe in this helper
(the dequeue would already have consumed the message before the copy is attempted
outside the lock, and there is no rollback/requeue path here). The integrated
helper therefore continues to reject every user-ASID receiver (returns `None` /
fallback). The plan struct is scaffolded and unit-tested for the kernel-task
branch so a future stage can wire the user copy once the failure semantics are
matched.

**Copy failure semantics (documented):** old path — dequeue, then copy; on copy
fault the message is consumed (lost), a user fault is recorded, syscall returns
`Ok(())`. Split path — for the only enabled (kernel-task) case there is **no
user copy and no copy-failure mode**; the user-ASID case is fallback-only, so the
unsafe post-dequeue copy is never reached on the split path.

### 50.6 Fallback matrix (Part 6)

Default-deny (helper returns `None`, caller falls back to the global path) for:
user-ASID receiver (writeback plan incomplete); `ReceivedWithSenderWake`
(sender-waiter refill needs a scheduler wake); empty endpoint / `WouldBlock`;
cap-transfer / reply-cap message at head; recv-v2 metadata request; timeout /
blocking recv; endpoint generation mismatch (revalidated under `ipc_state_lock`,
mismatch → `None`); all non-`IpcRecv` IPC syscalls; SpawnV5 / VM / futex / fault.
Invalid/wrong-object/missing-right cap is NOT a fallback — it returns
`Some(Err(..))` with the same error the old path produced.

| case | cap resolution | ipc dequeue | user-copy | split status | fallback reason |
|---|---|---|---|---|---|
| kernel-task queued plain recv, valid cap | cap(4) split | ipc(3) split | none | helper-only | user-ASID user-copy blocker remains |
| user-ASID queued plain recv, valid cap | cap(4) split | ipc(3) split | outside ipc lock | deferred | copy failure semantics |
| invalid cap | cap(4) split → Err | N/A | N/A | helper returns Err | match old error |
| wrong object | cap(4) split → Err | N/A | N/A | helper returns Err | match old error |
| missing right | cap(4) split → Err | N/A | N/A | helper returns Err | match old error |
| empty endpoint | cap(4) split + ipc(3) → empty | N/A | N/A | fallback | WouldBlock path |
| cap-transfer | N/A | N/A | N/A | fallback | cap domain interaction |
| recv-v2 | N/A | N/A | N/A | fallback | metadata materialization |
| sender-waiter refill | N/A | ipc(3) → ReceivedWithSenderWake | N/A | fallback | scheduler interaction |
| timeout/blocking | N/A | N/A | N/A | fallback | scheduler interaction |

### 50.7 Tests added (Part 7)

24 tests in `src/kernel/boot/tests.rs` module `stage32_cap_resolution_tests`:

- **Cap resolution (9):** `stage32_cap_resolution_valid_endpoint_recv_right`,
  `stage32_cap_resolution_missing_cap_error`,
  `stage32_cap_resolution_wrong_object_error`,
  `stage32_cap_resolution_missing_recv_right_error`,
  `stage32_cap_resolution_no_ipc_lock_required`,
  `stage32_cap_resolution_no_cap_mutation`,
  `stage32_cap_resolution_two_processes_isolated`,
  `stage32_cap_resolution_invalid_cap_id_error`,
  `stage32_cap_resolution_unknown_requester_error`.
- **Integration (8):** `stage32_integrated_queued_recv_valid_cap_succeeds`,
  `stage32_integrated_invalid_cap_matches_old_path_error`,
  `stage32_integrated_wrong_object_matches_old_path`,
  `stage32_integrated_empty_endpoint_fallback`,
  `stage32_integrated_user_asid_receiver_fallback`,
  `stage32_integrated_cap_transfer_fallback`,
  `stage32_integrated_recv_v2_fallback`,
  `stage32_integrated_lanes_match_old_path`.
- **Writeback plan (4):** `stage32_writeback_plan_stores_payload`,
  `stage32_writeback_plan_bounds_payload_len`,
  `stage32_writeback_plan_kernel_task_writeback`,
  `stage32_writeback_plan_user_asid_disabled`.
- **Regression (3):** `stage32_nr8_split_still_works`,
  `stage32_ipc_recv_not_wired_into_live_seam`, `stage32_syscall_count_still_30`.

All pass single-threaded. Full lib suite: 910 passed, 0 failed, 2 ignored.

### 50.8 x86_64 smoke decision

Not required for Stage 32: the path remains **helper-only**, not wired into the
live trap seam (`try_split_dispatch_into_frame`), so it changes no live trap
behavior. `IpcRecv` stays default-deny in the live seam
(`stage32_ipc_recv_not_wired_into_live_seam`). The NR-8 live split-dispatch is
untouched (`stage32_nr8_split_still_works`); `YARM_LOCK_SPLIT_DISPATCH nr=8
result=ok` remains the live split marker. No `ipc_recv_queued_plain` live marker is
emitted because the path is not live-wired.

### 50.9 Remaining blockers for live-wiring IpcRecv

1. **User-ASID writeback** — match the old path's message-consumed-on-copy-fail
   semantics for a post-dequeue `copy_to_current_user` performed outside
   `ipc_state_lock` (the plan struct exists; the user branch is disabled).
2. **Sender-waiter refill** — `ReceivedWithSenderWake` needs a deferred scheduler
   wake (still fallback).
3. **Live-seam wiring + x86_64 smoke** — only after (1)/(2) so the fast path
   actually fires on the real (user-ASID server) boot path with smoke coverage.

### 50.10 Still deferred

x86_64 SMP (`smp.rs`); RAMFS/FAT runtime spawning; recv-v2 / cap-transfer /
timeout / blocking recv splits; live-wiring the IPC recv split.

### 50.11 Stage 32B — live-wire kernel-task IpcRecv split (decision)

**Decision: LIVE-WIRED for the kernel-task queued-plain case only.** Stage 32's
§50.9 listed the live-wiring blockers as (1) user-ASID writeback semantics and
(2) sender-waiter refill scheduler wake. Both blockers apply ONLY to cases the
helper already rejects with `None`. The kernel-task queued-plain case has **no
remaining blocker**: it performs no user copy (register-only writeback, no
copy-failure mode) and no scheduler wake (sender-waiter refill →
`ReceivedWithSenderWake` is rejected to `None`). It is therefore safe to live-wire
that single case while every other case keeps falling back to the unchanged
global-lock path.

**Wiring (additive, default-deny-preserving):**

- `classify_split_eligible_nr_only` (`src/kernel/syscall_split.rs`) now admits
  `Syscall::IpcRecv` (NR 2) through the cheap NR gate, alongside NR 8.
- `try_split_dispatch_into_frame` routes IpcRecv to
  `try_split_ipc_recv_queued_plain_into_frame` (the thin wrapper →
  `SharedKernel::try_split_ipc_recv_queued_plain_into_frame`) **before** the global
  lock. The helper's `Option<Result<…>>` is returned **directly**:
  - `Some(Ok(()))` — kernel-task queued-plain recv serviced; lanes written.
  - `Some(Err(e))` — cap-resolution error identical to the old path (NOT a
    fallback — the old path raised the same error).
  - `None` — every other case (user-ASID, empty queue, sender-wake, cap-transfer,
    recv-v2) propagates UNCHANGED to the global-lock fallback. The split path never
    converts a would-be-fallback into a `Some(Err)`.
- The arg-only `classify_split_eligible` maps IpcRecv to
  `SplitEligibleSyscall::IpcRecvKernelTask`; the arg-only `try_split_dispatch`
  returns `None` for it (it has no `cpu`/`frame` to service a recv), deferring to
  the frame-level seam. This keeps the arg-only entry point's behavior unchanged.

**ABI/scheduler invariants:** `SYSCALL_COUNT == 30` unchanged; no syscall numbers
added; NR 8 live split untouched; `task_switched == false` on the recv split path
(no dispatch/yield/switch), so `entering_tid == exiting_tid` holds for the arch
return-register writeback exactly as before.

### 50.12 Full IPC receive path classification (Stage 32B)

| case | NR | split status | reason |
|---|---|---|---|
| IpcRecv, kernel-task, queued plain, no sender wake | 2 | **LIVE (32B)** | no user-copy, no scheduler |
| IpcRecv, user-ASID, queued plain | 2 | HELPER_ONLY → fallback | user-copy writeback (post-dequeue copy-fail semantics) |
| IpcRecv, queued plain, sender-wake needed | 2 | FALLBACK | `ReceivedWithSenderWake` → scheduler wake |
| IpcRecv, empty endpoint, would block | 2 | FALLBACK | blocking path (`WouldBlock`) |
| IpcRecv, cap-transfer / reply-cap msg | 2 | FALLBACK | cap domain materialization |
| IpcRecv, recv-v2 flags set | 2 | FALLBACK | meta-buffer user copy |
| IpcRecv, invalid/wrong-object/missing-right cap | 2 | LIVE-Err | `Some(Err)` == old-path error (not a fallback) |
| IpcRecvTimeout, timeout=0, queued plain | 5 | FALLBACK | NR 5 not in split whitelist (`try_ipc_recv` path; no split-seam) |
| IpcRecvTimeout, timeout=0, empty | 5 | FALLBACK | `WouldBlock`/non-blocking poll, global lock |
| IpcRecvTimeout, timeout>0, queued plain | 5 | FALLBACK | global lock (deadline pre-read already staged pre-lock) |
| IpcRecvTimeout, timeout>0, blocking | 5 | FALLBACK | deadline block + timer interaction |
| IpcSend | 1 | FALLBACK | sender path (enqueue + receiver wake) |
| IpcCall | 6 | FALLBACK | sender + reply-cap alloc + recv |
| IpcReply | 7 | FALLBACK | reply-cap consume + sender wake |

Notes from the audit:
- **IpcRecvTimeout (NR 5)** internally *does* attempt `try_endpoint_split_recv`
  (the IPC-domain queued-plain dequeue) inside the global-lock handler
  `handle_ipc_recv_timeout`, including for `timeout == 0`. But NR 5 is **not** put
  on the pre-global-lock split whitelist in Stage 32B: its handler additionally
  consults the per-CPU `SPLIT_RECV_TIMEOUT_DEADLINE`, the `ipc_timeout_fired`
  flag, and (for the empty/blocking case) the deadline scheduler path — none of
  which the recv split-seam models. A unified treatment is the Stage 33 work item.
- The `timeout == 0` "behaves like non-blocking IpcRecv when the queue has a
  message" semantics ARE present (immediate delivery via the in-handler split
  recv), but only on the global-lock path for now.
- **recv-v2** adds a metadata-buffer materialization (`meta_user_ptr` /
  `meta_user_len`, `IPC_RECV_META_V2_ENCODED_LEN`) on top of plain recv — a user
  copy — so it stays fallback for all variants.

### 50.13 Per-phase telemetry markers (Stage 32B)

Emitted only on the LIVE split recv path (kept low-noise — at most one line per
phase per serviced recv; fallbacks and propagated errors stay silent):

- `YARM_LOCK_SPLIT_IPC_RECV nr=2 phase=cap_plan result=ok endpoint_idx={idx}` —
  after the phase-separated cap resolution (task(2)→cap(4), no ipc lock) succeeds.
  Emitted in `SharedKernel::try_split_ipc_recv_queued_plain_into_frame`
  (`src/runtime.rs`).
- `YARM_LOCK_SPLIT_IPC_RECV nr=2 phase=writeback result=ok target=kernel` —
  after a plain message was dequeued under `ipc_state_lock` and the kernel-task
  return lanes were written (only on `Some(Ok(()))`).
- `YARM_LOCK_SPLIT_DISPATCH nr=2 cpu={cpu} result=ok` — emitted by the existing
  `handle_trap_entry_shared` seam (`src/arch/trap_entry.rs`) whenever the routed
  IpcRecv split returns `Some(_)` (shared with the NR-8 marker line).

The `cap_plan` marker can fire on an attempt that later falls back at the dequeue
phase (e.g. empty queue) — it marks only that cap resolution cleared, not a full
delivery. The `phase=writeback` + `nr=2 result=ok` dispatch markers mark a fully
serviced split recv.

### 50.14 Manual x86_64 smoke grep commands

```
grep -c 'YARM_LOCK_SPLIT_DISPATCH nr=8.*result=ok' $LOG               # >= 1 (NR 8 live)
grep -c 'YARM_LOCK_SPLIT_IPC_RECV.*phase=writeback.*result=ok' $LOG   # >= 0 (boot-dependent)
grep -c 'YARM_LOCK_SPLIT_DISPATCH nr=2 .*result=ok' $LOG              # >= 0 (boot-dependent)
grep -c 'YARM_LOCK_SPLIT_STAGE2N_FALLBACK' $LOG                       # 0
grep -c 'YARM_LOCK_SPLIT_CURRENT_TID_MISMATCH' $LOG                   # 0
```

The realistic x86_64 service receivers (PM/init/VFS) are **user-ASID** tasks, so
they keep falling back and the `nr=2` recv markers may legitimately be `0` on a
given boot — the kernel-task split recv only fires when a kernel task receives a
plain queued message. The `nr=8 result=ok` count and all Phase3B / service-entry /
fallback=0 / TID-mismatch=0 health markers remain the load-bearing live assertions.

## §51 Stage 33 — Canonical internal IPC receive engine (plan)

**Goal:** collapse the three receive entry points (IpcRecv, IpcRecvTimeout,
recv-v2) onto **one** internal receive engine, so split-eligibility, dequeue,
writeback, and wake are decided in exactly one place instead of three divergent
per-variant handlers.

**Public ABIs unchanged.** No syscall numbers added; `SYSCALL_COUNT == 30` stays.

**Adapter pattern.** Each syscall variant decodes its frame into a single request
descriptor:

```text
RecvRequest {
    kind:         RecvKind,       // PlainRecv | TimedRecv | RecvV2
    cap:          CapId,          // endpoint receive cap
    timeout:      Option<u64>,    // None = block forever, Some(0) = poll, Some(n) = deadline
    v2_meta_ptr:  u64,            // recv-v2 metadata buffer (0 otherwise)
    v2_meta_len:  usize,
    payload_ptr:  u64,            // user payload dst (user-ASID receiver)
    payload_len:  usize,
}
```

**Internal engine responsibilities (one consistent implementation):**
1. cap resolution — phase-separated split-read (task(2)→cap(4), reuse Stage 32
   `resolve_endpoint_recv_cap_split_read`);
2. dequeue — IPC-domain-only queued-plain attempt (reuse `try_endpoint_split_recv`);
3. writeback — kernel-task register-only (live) OR user-ASID copy (gated on the
   formalized copy-failure semantics, see prerequisite);
4. wake — deferred sender-waiter wake applied AFTER all locks release;
5. block/deadline — only when the dequeue did not deliver.

**This enables:**
- **IpcRecvTimeout `timeout==0` fast path** — same as plain recv when the queue is
  non-empty (today it is serviced only on the global-lock path); the engine lets
  NR 5 share the pre-global-lock recv split-seam.
- **recv-v2 fast path** for kernel-task receivers (no user meta copy needed when
  the receiver is a kernel task), once the engine owns metadata materialization.

**Prerequisite (carried from §50.9):** formalize user-ASID writeback
copy-failure semantics — the old path dequeues then copies, and on
`copy_to_current_user` fault the message is consumed (lost), a user fault is
recorded, and the syscall returns `Ok(())`. A post-lock-release copy in the engine
must reproduce that exactly (or introduce a proven requeue/rollback) before the
user-ASID writeback can move onto the split path. Until then the engine keeps the
user-ASID writeback on the global-lock path while sharing cap-resolution/dequeue.

**Sequencing:** Stage 33 is a refactor-then-extend: first land the engine behind
the current behavior (no live-wire change), prove value-equivalence via the
existing per-variant tests, then incrementally move the timeout=0 and recv-v2
kernel-task fast paths onto the split seam with x86_64 smoke coverage.

## §52 Stage 33+34 — recv_core: canonical receive engine + recv_shared_v3 scaffold

### 52.1 Overview

Stage 33+34 lands the **canonical internal IPC receive engine** (`src/kernel/recv_core.rs`) and the **recv_shared_v3 design scaffold** (helper-only, no public syscall).

The primary goals:
1. Formalize the internal request/outcome model for all IPC receive paths.
2. Route the Stage 32B kernel-task queued plain live path through the canonical core (the only currently-eligible path).
3. Document and freeze current copy-failure semantics for user-ASID receivers.
4. Design and scaffold `recv_shared_v3` as a future ABI — versioned structs, validation functions, test-only adapter — without adding a public syscall.

**Hard invariants preserved:**
- `SYSCALL_COUNT == 30` — no new public syscall.
- No public `recv_shared_v3` syscall dispatch.
- ipc_state_lock NOT held during copies to user memory.
- capability lock NOT held during copies to user memory.
- Public ABIs (ipc_recv, ipc_recv_timeout, recv-v2, IpcSend/IpcCall/IpcReply, SpawnV5, VFS) unchanged.
- Phase2B, Phase3B, startup slots, Stage 30 raw-borrow guard, Stage 29 NR8 behavior, Stage 32B split behavior: all preserved.

### 52.2 Receive-path classification table

| Entry point | NR | Split eligible now? | Core path | Fallback reason (if not eligible) |
|---|---|---|---|---|
| ipc_recv kernel task (queued plain) | 2 | YES | `try_recv_core_kernel_plain` | — |
| ipc_recv user-ASID receiver | 2 | NO | global-lock | UserAsidCopySemantics |
| ipc_recv_timeout (any) | 5 | NO | global-lock | UserAsidCopySemantics or RecvV2MetaUserCopy |
| recv-v2 (meta write) | 2 | NO | global-lock | RecvV2MetaUserCopy |
| mapped recv (MemoryObject) | 2 | NO | global-lock | UserAsidCopySemantics |
| recv_shared_v3 | — | N/A (future) | none (helper-only) | SharedV3HelperOnly |

### 52.3 Canonical request model

```rust
pub(crate) struct RecvRequest {
    pub(crate) kind:           RecvRequestKind,
    pub(crate) requester_tid:  u64,
    pub(crate) recv_cap:       CapId,
    pub(crate) payload_target: RecvPayloadTarget,
    pub(crate) meta_target:    RecvMetaTarget,
    pub(crate) blocking:       RecvBlockingPolicy,
    pub(crate) transfer:       RecvTransferPolicy,
    pub(crate) map_intent:     RecvMapIntent,
}

pub(crate) enum RecvRequestKind    { LegacyRecv, LegacyTimedRecv, SharedV3Future }
pub(crate) enum RecvPayloadTarget  { KernelRegister, UserMemory { ptr, len } }
pub(crate) enum RecvMetaTarget     { None, V2 { ptr, len }, V3Future { ptr, len } }
pub(crate) enum RecvBlockingPolicy { WaitForever, Timed { ticks }, NonBlocking }
pub(crate) enum RecvTransferPolicy { LegacyFull }
pub(crate) enum RecvMapIntent      { None, ReadOnly, ReadWrite }
```

### 52.4 Canonical outcome model

```rust
pub(crate) enum RecvOutcome {
    Delivered(RecvDelivery),
    WouldBlock,
    TimedOut,
    FallbackRequired(FallbackReason),
    Error(KernelError),
}

pub(crate) struct RecvDelivery {
    pub(crate) writeback:  RecvWritebackPlan,
    pub(crate) scheduler:  RecvSchedulerWakePlan,
    pub(crate) msg:        Message,
}

pub(crate) enum RecvWritebackPlan {
    KernelRegister { sender_tid: usize, raw_len: usize },
}

pub(crate) enum RecvSchedulerWakePlan { None }

pub(crate) enum FallbackReason {
    UserAsidCopySemantics,
    RecvV2MetaUserCopy,
    SenderWaiterWake,
    CapTransfer,
    SharedV3HelperOnly,
}
```

### 52.5 Adapter constructor map

| Adapter | Source ABI | Notes |
|---|---|---|
| `RecvRequest::from_legacy_ipc_recv` | NR2 (ipc_recv) | Plain recv; detects kernel vs user-ASID via `is_kernel_task` |
| `RecvRequest::from_ipc_recv_timeout` | NR5 (ipc_recv_timeout) | Encodes timeout ticks; always user-ASID for now |
| `RecvRequest::from_recv_v2` | NR2 with v2 frame | Detects meta ptr/len; encodes V2 meta target |
| `RecvRequest::from_legacy_mapped_recv` | NR2 with map intent | MemoryObject recv; always user-ASID |
| `RecvRequest::future_shared_v3` | (none, `#[cfg(test)]`) | Design scaffold only; always returns SharedV3HelperOnly |

### 52.6 Copy-failure semantics table

The table below documents the **current observable behavior** of the global-lock
receive path when a `copy_to_current_user` fault occurs. This behavior is
**frozen** by Stage 33+34: no live path changes it. The table is the formal
prerequisite for any future user-ASID writeback move onto the split seam.

| Scenario | Dequeue order | On copy fault | Message fate | Syscall return |
|---|---|---|---|---|
| User-ASID ipc_recv, oversized payload | dequeue → size-check | `InvalidArgs` | message LOST | `Err(InvalidArgs)` |
| User-ASID ipc_recv, copy fault | dequeue → copy | `copy_to_current_user` fault | message LOST | `Ok(())` + user fault recorded |
| User-ASID recv-v2, meta copy fault | dequeue → meta copy | fault | message LOST | `Ok(())` + user fault recorded |
| Kernel-task recv (split path) | dequeue under ipc_state_lock | N/A (no user copy) | delivered to registers | `Ok(())` |
| User-ASID recv (split path) | — | N/A (falls back before dequeue) | message PRESERVED in queue | `None` (fallback) |

**Key invariant guaranteed by Stage 33+34:** On the split path, `plan_recv_core`
returns `FallbackRequired(UserAsidCopySemantics)` BEFORE `try_recv_core_kernel_plain`
is called for any user-ASID receiver. The dequeue never runs for user-ASID on the
split path, so no messages can be lost via split-path fallback.

**Next step for enabling user-ASID on split path:** Introduce a
post-lock-release copy path that either (a) proves rollback/requeue on fault, or
(b) reproduces the current "dequeue-then-copy, lose on fault" semantics with the
copy happening outside ipc_state_lock. Stage 35 tracking item.

### 52.7 recv_shared_v3 design scaffold

The `recv_shared_v3` module (`src/kernel/recv_core.rs`, submodule
`recv_shared_v3`) defines versioned request/output structs and validation
functions for a potential future shared-buffer receive ABI.

**Key constants:**

| Constant | Value | Meaning |
|---|---|---|
| `V3_VERSION` | 3 | Required version field in request header |
| `V3_MIN_REQUEST_LEN` | 64 | Minimum validated request record length |
| `V3_MIN_OUTPUT_LEN` | 80 | Minimum validated output record length |
| `MAP_READ` | 0x1 | map_intent flag: read access |
| `MAP_WRITE` | 0x2 | map_intent flag: write access |

**Why no public v3 syscall now:**
- Shared-buffer recv requires stable user-ASID writeback semantics (§52.6).
- The output record format is not finalized — field layout will evolve.
- No driver or service currently requires v3.
- Adding a syscall now would freeze an immature ABI before the design is ready.

**Future metadata not yet available in recv_shared_v3 output:**
- Sender ASID (not yet tracked in message metadata).
- Transfer capability slot — requires capability-lock-safe writeback path.
- Message sequence number — requires endpoint-level sequence tracking.

### 52.8 Telemetry markers (Stage 33+34)

New markers emitted on the canonical core path:

- `YARM_RECV_CORE_ADAPTER kind=legacy` — emitted when `from_legacy_ipc_recv` adapter was used and `plan_recv_core` returned `KernelPlainEligible`. Marks that the request entered the canonical core.
- `YARM_RECV_CORE_LIVE kind=kernel_plain` — emitted on successful delivery through `try_recv_core_kernel_plain`. One marker per delivered message on the split fast path.
- `YARM_RECV_CORE_FALLBACK reason=<FallbackReason>` — emitted when `plan_recv_core` returns `FallbackRequired`. Identifies the specific deferral cause.

### 52.9 Live vs fallback matrix (Stage 33+34)

| Syscall | Receiver type | Path | Markers |
|---|---|---|---|
| ipc_recv (NR2) | kernel task, queued plain | LIVE split + canonical core | `ADAPTER kind=legacy`, `LIVE kind=kernel_plain` |
| ipc_recv (NR2) | user-ASID | fallback to global-lock | `FALLBACK reason=UserAsidCopySemantics` |
| ipc_recv (NR2) | v2 meta frame | fallback to global-lock | `FALLBACK reason=RecvV2MetaUserCopy` |
| ipc_recv_timeout (NR5) | any | not split-eligible (NR gate) | — |

### 52.10 Smoke grep commands (Stage 33+34)

```
grep -c 'YARM_RECV_CORE_LIVE kind=kernel_plain' $LOG     # >= 0 (boot-dependent)
grep -c 'YARM_RECV_CORE_ADAPTER kind=legacy' $LOG        # matches LIVE count
grep -c 'YARM_RECV_CORE_FALLBACK' $LOG                   # >= 0 (user-ASID recvs)
grep -c 'YARM_RECV_CORE_FALLBACK.*SharedV3' $LOG         # 0 (no v3 syscall)
```

## §53 Stage 35 — Receive ABI adapters over canonical RecvRequest

### 53.1 Overview

Stage 35 integrates the canonical `RecvRequest` adapters (defined in Stage 33+34) into the existing full-path receive syscall handlers (`handle_ipc_recv`, `handle_ipc_recv_timeout`).

**No behavior change.** The global-lock full path is authoritative for all execution. The adapters are used only for:
- Structured decode of frame arguments (replaces inline `frame.arg()` checks for v2/timeout detection)
- Telemetry markers at the full-path entry
- Paving the way for future live-routing of eligible cases

**Invariants preserved:**
- `SYSCALL_COUNT == 30`
- Public ABI register layout unchanged
- user-ASID recv falls back before dequeue
- recv-v2 stays on global-lock path
- Stage 32B kernel-plain live split still works

### 53.2 Adapter integration points

| Handler | Adapter used | Change |
|---|---|---|
| `handle_ipc_recv` | `from_legacy_ipc_recv` | v2 detection via `request.meta_target` instead of raw frame arg check |
| `handle_ipc_recv_timeout` | `from_ipc_recv_timeout` | timeout branch via `request.blocking` instead of `timeout_ticks == 0` |
| split path (`try_split_recv_queued_plain_with_snapshot_locked`) | `from_legacy_ipc_recv` | unchanged (already canonical since Stage 33+34) |

### 53.3 Telemetry markers (Stage 35)

- `YARM_RECV_CORE_ADAPTER kind=legacy_full_path is_kernel_task=<bool>` — emitted at entry to `handle_ipc_recv`
- `YARM_RECV_CORE_ADAPTER kind=legacy_timeout is_kernel_task=<bool> blocking=<policy>` — emitted at entry to `handle_ipc_recv_timeout`

### 53.4 Live vs fallback matrix (cumulative Stage 33+34+35)

| Syscall | Receiver type | Path | Stage |
|---|---|---|---|
| ipc_recv (NR2) | kernel task, queued plain | LIVE split + canonical core | 33+34 |
| ipc_recv (NR2) | user-ASID | global-lock (canonical adapter decode) | 35 |
| ipc_recv (NR2) | v2 meta | global-lock (canonical adapter decode) | 35 |
| ipc_recv_timeout (NR5) | any | global-lock (canonical adapter decode) | 35 |

## §54 Stage 36 — Formalize user-ASID receive writeback semantics, live-enable narrow path

### 54.1 Overview

Stage 36 **formally audits** the copy-failure semantics for user-ASID plain receive and **live-enables** the narrow eligible path (no meta, no map_intent) on the canonical split path.

**Changes:**
- `RecvPlan::UserPlainEligible` added to `recv_core.rs` — new plan variant for eligible user-ASID plain recv
- `plan_recv_core` restructured: meta check fires BEFORE the `UserMemory`/`KernelRegister` split, so user-ASID + V2 meta → `RecvV2MetaUserCopy` (not `UserAsidCopySemantics` as in Stage 35)
- `try_recv_core_user_plain` — new function, dequeues under `ipc_state_lock` (rank 3), returns `RecvDelivery` with `UserMemory` writeback plan
- `execute_user_asid_plain_writeback` — new function, performs user-space copy after lock release
- `RecvUserWritebackOutcome` — new enum: `Ok`, `UndersizedBuffer`, `CopyFault`
- `RecvWritebackPlan::UserMemory.user_buf_len` — renamed from `app_payload_len`; stores user buffer capacity
- `try_split_recv_queued_plain_with_snapshot_locked` updated to handle `UserPlainEligible`

**Invariants preserved:**
- `SYSCALL_COUNT == 30` (no new public syscall)
- Public ABI register layout unchanged
- Kernel-plain split path behavior unchanged
- recv-v2 still falls back (meta user-copy required)
- Mapped recv still falls back (map_intent != None → `UserAsidCopySemantics`)
- Cap-transfer messages fall back at dequeue time (`FallbackRequired(CapTransfer)`)

### 54.2 Copy-failure semantics proof table

| Scenario | Full-path (global-lock) | Split-path (Stage 36) | Verdict |
|---|---|---|---|
| Successful copy | `copy_to_current_user` → `Ok` → `frame.set_ok(sender, len, ret2)` | Same: `RecvUserWritebackOutcome::Ok` → `frame.set_ok` | **Equivalent** |
| Undersized buffer | dequeue → `user_len < payload.len()` → rollback cap → `Err(InvalidArgs)` | dequeue → `user_buf_len < payload.len()` → `UndersizedBuffer` → `Err(InvalidArgs)` | **Equivalent** (message consumed in both) |
| UserMemoryFault on copy | dequeue → `copy_to_current_user` → `UserMemoryFault` → `record_user_fault` + `Ok(())` | dequeue → `copy_to_current_user` → `CopyFault` → `record_user_fault` + `Ok(())` | **Equivalent** (message consumed in both) |
| Empty queue | `WouldBlock` → block or return `WouldBlock` to user | `WouldBlock` → `None` → fall back to global-lock path | **Equivalent** (same outcome) |
| Cap-transfer at queue head | `Ineligible(TransferOrReplyCapMessage)` → handle on global-lock path | `FallbackRequired(CapTransfer)` → `None` → global-lock path | **Equivalent** (no dequeue on either path) |
| Sender-waiter refill | `ReceivedWithSenderWake` → handle on global-lock path | `FallbackRequired(SenderWaiterWake)` → `None` → global-lock path | **Equivalent** |

**Key invariant (Stage 36):** For plain messages (no flags), `app_payload = raw_payload` — no cap materialization, no prefix stripping.  The narrow eligible path requires no cap rollback on failure.

### 54.3 Lock order (Stage 36 user-ASID plain path)

```
[no lock]
  → current_tid_authoritative (takes+releases global lock)
  → [no lock]
  → resolve_endpoint_recv_cap_split_read (task(2) read+release → cap(4) read+release; no ipc lock)
  → [no lock]
  → self.with(|state| try_split_recv_queued_plain_with_snapshot_locked):
      → try_recv_core_user_plain:
          → ipc_state_lock (rank 3) acquired
          → dequeue plain message
          → ipc_state_lock (rank 3) released
      → execute_user_asid_plain_writeback:
          → copy_to_current_user (NO ipc_state_lock held ✓, NO capability lock held ✓)
      → frame.set_ok / record_user_fault
  → global lock released
```

**Hard constraints satisfied:**
- `ipc_state_lock` (rank 3): NOT held during `copy_to_current_user` ✓
- Capability lock (rank 4): NOT held during `copy_to_current_user` ✓

### 54.4 Eligibility matrix (Stage 36 additions to §52 table)

| Scenario | Eligible for split? | Reason |
|---|---|---|
| ipc_recv user-ASID plain (no meta, no map) | **YES** (Stage 36) | `UserPlainEligible` |
| ipc_recv user-ASID + V2 meta | NO | `RecvV2MetaUserCopy` |
| ipc_recv user-ASID + mapped recv | NO | `UserAsidCopySemantics` (map_intent) |
| ipc_recv kernel task plain | YES (Stage 33+34) | `KernelPlainEligible` |
| ipc_recv_timeout (any) | NO | NR5 not in split-dispatch table |
| recv-v2 (any) | NO | `RecvV2MetaUserCopy` |

### 54.5 Telemetry markers (Stage 36)

- `YARM_RECV_CORE_PLAN plan=UserPlainEligible` — emitted when user-ASID plain recv is shape-eligible
- `YARM_RECV_CORE_ADAPTER kind=user_plain` — emitted when try_recv_core_user_plain is called
- `YARM_RECV_CORE_LIVE kind=user_plain` — emitted on successful user-ASID plain delivery

### 54.6 Live vs fallback matrix (cumulative Stage 33+34+35+36)

| Syscall | Receiver type | Path | Stage |
|---|---|---|---|
| ipc_recv (NR2) | kernel task, queued plain | LIVE split + canonical core | 33+34 |
| ipc_recv (NR2) | user-ASID plain (no meta, no map) | **LIVE split + canonical core** | **36** |
| ipc_recv (NR2) | user-ASID + V2 meta | global-lock | 35 |
| ipc_recv (NR2) | user-ASID mapped recv | global-lock | 35 |
| ipc_recv (NR5) | any | global-lock | 35 |

## §55 Stage 37 — recv-v2 metadata writeback semantics audit and live-enable

### 55.1 Overview

Stage 37 **formally audits** the metadata writeback semantics for recv-v2 plain queued
messages and **live-enables** the narrow eligible path (user-ASID + V2 meta + no
map_intent) on the canonical split path.

**Changes:**
- `RecvPlan::UserPlainV2Eligible` added to `recv_core.rs` — new plan variant for
  user-ASID + V2 meta + no map_intent
- `plan_recv_core` restructured: meta check now per-payload-target; for
  `UserMemory` + V2 meta + no map_intent → `UserPlainV2Eligible` (Stage 37)
- `try_recv_core_user_plain_v2` — dequeues under `ipc_state_lock` (rank 3), extracts
  both `UserMemory` and `V2` targets, returns `RecvDelivery` with `UserMemoryV2` plan
- `execute_user_asid_plain_v2_writeback` — builds 40-byte meta struct, copies meta
  FIRST, then payload; returns `RecvV2WritebackOutcome`
- `RecvV2WritebackOutcome` — new enum: `Ok`, `PayloadUndersized`, `MetaCopyFault`,
  `PayloadCopyFault`
- `RecvWritebackPlan::UserMemoryV2` — new variant carrying meta_ptr, meta_len,
  payload ptr, payload buf_len, sender_tid

**Invariants preserved:**
- `SYSCALL_COUNT == 30` (no new public syscall)
- Public ABI register layout unchanged
- Kernel-plain and user-plain split path behaviors unchanged
- Cap-transfer/reply-cap/sender-waiter-refill/shared receive still fall back
- recv-v3 still helper-only
- Mapped recv still falls back (map_intent != None → `UserAsidCopySemantics`)

### 55.2 recv-v2 plain metadata writeback semantics proof table

Meta struct layout (40 bytes, written to `meta_ptr`):

| Offset | Field | Value for plain message |
|---|---|---|
| [0..8] | sender_tid | `msg.sender_tid.0` (little-endian u64) |
| [8..10] | opcode | `msg.opcode` (little-endian u16) |
| [10..12] | flags | `msg.flags` (little-endian u16) |
| [12..16] | payload_len | `app_payload.len() as u32` (little-endian u32) |
| [16..24] | transfer_cap | `Message::NO_TRANSFER_CAP` = `u64::MAX` |
| [24..32] | recv_meta_flags | `0u64` (no FLAG_REPLY_CAP, no FLAG_CAP_TRANSFER for plain) |
| [32..40] | sender_tid2 | `msg.sender_tid.0` (little-endian u64, duplicate slot) |

Copy ordering: meta FIRST, payload SECOND. This matches the full-path order in
`handle_ipc_recv_result_with_empty_error` which writes `out_meta_ptr` before `payload_ptr`.

Equivalence proof for each outcome:

| Scenario | Full-path (global-lock) | Split-path (Stage 37) | Verdict |
|---|---|---|---|
| Meta copy succeeds, payload fits | write meta → copy payload → `set_ok(0, len, ret2)` | write meta → copy payload → `RecvV2WritebackOutcome::Ok` → `set_ok(0, len, ret2)` | **Equivalent** |
| Meta copy fault | write meta → `UserMemoryFault` → `Err(PageFault)` (no rollback for plain) | `execute_user_asid_plain_v2_writeback` → `MetaCopyFault` → `Err(PageFault)` | **Equivalent** (message consumed in both) |
| Meta ok, payload undersized | write meta → `user_len < payload.len()` → `Err(InvalidArgs)` | meta ok → `PayloadUndersized` → `Err(InvalidArgs)` | **Equivalent** (message consumed in both) |
| Meta ok, payload copy fault | write meta → payload `UserMemoryFault` → `record_user_fault` + `Ok(())` | meta ok → `PayloadCopyFault` → `record_user_fault` + `Ok(())` | **Equivalent** (message consumed in both) |
| Empty queue | `WouldBlock` → block or return `WouldBlock` | `WouldBlock` → `None` → global-lock fallback | **Equivalent** |
| `ret0` value on success | `0` (recv_v2_meta_written = true branch) | `frame.set_ok(0, payload_len, frame.ret2())` | **Equivalent** |
| `transfer_cap` field | `frame.ret2() as u64` after `encode_transfer_cap_ret(frame, None)` = `SYSCALL_NO_TRANSFER_CAP` | `Message::NO_TRANSFER_CAP` = `u64::MAX` = `SYSCALL_NO_TRANSFER_CAP` | **Equivalent** |
| `recv_meta_flags` field | `0u64` for plain (no FLAG_REPLY_CAP, no FLAG_CAP_TRANSFER) | `0u64.to_le_bytes()` hardcoded | **Equivalent** |

**Key invariant (Stage 37):** For plain messages, `recv_meta_flags = 0`,
`recv_local_transfer = None`, no cap materialization needed. The split path requires
no cap rollback on any failure path.

### 55.3 Lock order (Stage 37 user-ASID + V2 meta plain path)

```
[no lock]
  → current_tid_authoritative (takes+releases global lock)
  → [no lock]
  → resolve_endpoint_recv_cap_split_read (task(2) read+release → cap(4) read+release; no ipc lock)
  → [no lock]
  → self.with(|state| try_split_recv_queued_plain_with_snapshot_locked):
      → try_recv_core_user_plain_v2:
          → ipc_state_lock (rank 3) acquired
          → dequeue plain message
          → ipc_state_lock (rank 3) released
      → execute_user_asid_plain_v2_writeback:
          → build 40-byte meta struct (stack-local, no lock)
          → copy_to_current_user (meta)   [NO ipc_state_lock held ✓, NO capability lock held ✓]
          → copy_to_current_user (payload) [NO ipc_state_lock held ✓, NO capability lock held ✓]
      → frame.set_ok / Err(PageFault) / Err(InvalidArgs) / record_user_fault
  → global lock released
```

**Hard constraints satisfied:**
- `ipc_state_lock` (rank 3): NOT held during either `copy_to_current_user` call ✓
- Capability lock (rank 4): NOT held during either `copy_to_current_user` call ✓
- Meta struct built on stack — no allocation, no lock ✓

### 55.4 Eligibility matrix (Stage 37 additions to §54 table)

| Scenario | Eligible for split? | Plan | Reason |
|---|---|---|---|
| ipc_recv user-ASID plain (no meta, no map) | YES (Stage 36) | `UserPlainEligible` | plain writeback |
| ipc_recv user-ASID + V2 meta (no map) | **YES (Stage 37)** | **`UserPlainV2Eligible`** | meta-first + payload writeback |
| ipc_recv user-ASID + V2 meta + map_intent | NO | `UserAsidCopySemantics` | map_intent != None |
| ipc_recv user-ASID + V3Future meta | NO | `RecvV2MetaUserCopy` | V3Future helper-only |
| ipc_recv kernel task + V2 meta | NO | `RecvV2MetaUserCopy` | kernel-register target |
| ipc_recv user-ASID mapped recv | NO | `UserAsidCopySemantics` | map_intent |
| ipc_recv kernel task plain | YES (Stage 33+34) | `KernelPlainEligible` | register writeback |
| ipc_recv_timeout (any) | NO | — | NR5 not in split-dispatch table |
| recv-v2 (ipc_recv NR2 from non-user-ASID) | NO | `RecvV2MetaUserCopy` | meta user-copy |

### 55.5 Telemetry markers (Stage 37)

- `YARM_RECV_CORE_ADAPTER kind=user_plain_v2` — emitted when `UserPlainV2Eligible` plan is dispatched
- `YARM_RECV_CORE_LIVE kind=user_plain_v2` — emitted on successful user-ASID V2 delivery
- `YARM_RECV_CORE_V2_WRITEBACK result=ok` — meta+payload copy succeeded
- `YARM_RECV_CORE_V2_WRITEBACK result=meta_fault` — meta copy faulted (→ `PageFault`)
- `YARM_RECV_CORE_V2_WRITEBACK result=payload_fault` — payload copy faulted (→ `record_user_fault`)
- `YARM_RECV_CORE_V2_WRITEBACK result=payload_undersized` — payload buffer too small (→ `InvalidArgs`)

### 55.6 Live vs fallback matrix (cumulative Stage 33+34+35+36+37)

| Syscall | Receiver type | Path | Stage |
|---|---|---|---|
| ipc_recv (NR2) | kernel task, queued plain | LIVE split + canonical core | 33+34 |
| ipc_recv (NR2) | user-ASID plain (no meta, no map) | LIVE split + canonical core | 36 |
| ipc_recv (NR2) | user-ASID + V2 meta (no map) | **LIVE split + canonical core** | **37** |
| ipc_recv (NR2) | user-ASID + V2 meta + map_intent | global-lock | 37 |
| ipc_recv (NR2) | user-ASID + V3Future meta | global-lock | 37 |
| ipc_recv (NR2) | user-ASID mapped recv | global-lock | 35 |
| ipc_recv (NR5) | any | global-lock | 35 |

## §56 Stage 38+39 — recv-core transfer/reply/shared audit + sender-waiter fix

### 56.1 Overview

Stage 38+39 audits the full-path semantics for cap-transfer, reply-cap, mapped/shared
receive, and sender-waiter refill. Based on the audit, it live-enables the plain
sender-waiter refill case on the canonical split path (blocking the latent message-loss
bug). Cap-transfer, reply-cap, and mapped/shared receive remain on the global-lock
fallback path with documented blockers.

**Changes:**
- `try_recv_core_kernel_plain`, `try_recv_core_user_plain`, `try_recv_core_user_plain_v2`:
  `ReceivedWithSenderWake(msg, wake_tid)` now returns `Delivered` with
  `RecvSchedulerWakePlan::WakeSender(wake_tid)` instead of `FallbackRequired(SenderWaiterWake)`
- `try_split_recv_queued_plain_with_snapshot_locked`: applies sender-wake plan BEFORE
  writeback when `delivery.scheduler == WakeSender(wake_tid)`, matching full-path order
- `FallbackReason::CapTransfer` doc updated to describe cap-transfer + reply-cap blockers
- `FallbackReason::SenderWaiterWake` doc updated to reflect narrowed semantics (only
  cap-transfer sender-waiter or sparse queue still falls back; plain now promoted)

**Invariants preserved:**
- `SYSCALL_COUNT == 30`
- cap-transfer/reply-cap/mapped/shared receive still fall back
- recv-v3 still helper-only

### 56.2 Audit: cap-transfer receive semantics

**Full-path order for cap-transfer messages:**
1. `ipc_recv(cap)` — dequeues the message (including cap-transfer)
2. `materialize_received_message_cap` — acquires transfer envelope + capability lock (rank 4) + grants/mints cap
3. If materialization fails → `return Err(InvalidCapability)` — message consumed, no rollback possible
4. Meta copy (if recv-v2): write 40-byte meta struct; if fault → `rollback_materialized_recv_cap` (re-takes cap lock) + `return Err(PageFault)`
5. Undersized buffer: `rollback_materialized_recv_cap` + `return Err(InvalidArgs)`
6. Payload copy fault: `record_user_fault + Ok()` — cap **not** rolled back by design

**Split path:** `ipc_try_recv_queued_plain_endpoint_only` rejects cap-flagged messages
(`FLAG_CAP_TRANSFER`, `FLAG_CAP_TRANSFER_PLAIN`, `FLAG_REPLY_CAP`) with
`TransferOrReplyCapMessage` **before dequeuing**. Message stays in queue for the full path.
This is correct: no message loss, no lock violation.

**Blocker for split-path cap-transfer (Stage 38+39 conclusion — NOT live):**
- Materialization requires capability lock (rank 4) after ipc_state_lock (rank 3) released: allowed by lock rank.
- Rollback on copy failure (meta fault or undersized buffer) requires re-taking capability lock after copy attempt.
- Proving exact rollback semantics and tests covering all failure paths is deferred.
- **This path is explicitly NOT live-enabled.**

### 56.3 Audit: reply-cap materialization semantics

**Additional steps vs cap-transfer:**
1. `take_transfer_envelope` — retrieves underlying Reply object handle
2. `capability_object_live(reply_object)` — generation check
3. `mint_capability_in_cnode` — direct mint (bypasses delegation-link table)
4. `set_reply_cap_waiter_cap` — records CapId in global ReplyCapRecord for `ipc_reply` fast-revoke

**Rollback on copy failure:** same as cap-transfer — `rollback_materialized_recv_cap` with `is_reply=true`.

**Blocker for split-path reply-cap:** same as cap-transfer + generation liveness check + ReplyCapRecord mutation.
**NOT live-enabled.**

### 56.4 Audit: mapped/shared receive (OPCODE_SHARED_MEM) semantics

Messages with `msg.opcode == OPCODE_SHARED_MEM` carry `FLAG_CAP_TRANSFER` (the
memory object capability is the transferred cap). They are already rejected by the
`TransferOrReplyCapMessage` check before any opcode inspection.

Even if cap-transfer were handled: the shared-memory path requires:
1. Decode `SharedMemoryRegion` from message payload (offset + len)
2. Validate transfer_cap presence and rights
3. `attenuate_transfer_cap_for_recv_intent` — capability domain mutation
4. `map_shared_region_into_receiver` — VM page-table operations (vm_state_lock rank 5)
5. `register_active_transfer_mapping` — bookkeeping
6. Multi-phase rollback: TLB shootdown + cap revoke on any failure

**Missing from IPC model:** `exact_region_len` (v3 FUTURE field) and
`exact_object_size` (v3 FUTURE field) cannot be populated from `SharedMemoryRegion.len`
alone (len is application-provided, not kernel-verified against object bounds).

**NOT live-enabled.**

### 56.5 Sender-waiter refill: semantics proof table

| Scenario | Full path (handle_ipc_recv) | Split path (Stage 38+39) | Verdict |
|---|---|---|---|
| Plain message at head, plain sender-waiter | dequeue first + refill second; wake sender BEFORE `handle_ipc_recv_result` | dequeue first + refill second; `RecvSchedulerWakePlan::WakeSender`; wake BEFORE writeback | **Equivalent** |
| Plain message at head, cap-transfer sender-waiter | dequeue first; sender-waiter stays (not handled by split) | `Ineligible(SenderWaiterPresent)` → `FallbackRequired(SenderWaiterWake)` → global path | **Equivalent** (fallback) |
| Cap-transfer at queue head | dequeue; materialize; copy | `TransferOrReplyCapMessage` → `FallbackRequired(CapTransfer)` → global path | **Equivalent** (no dequeue on split) |
| Wake ordering: plain sender-waiter | wake BEFORE copy (apply_split_sender_wake_plan at line 1165) | wake BEFORE writeback (`delivery.scheduler` applied before `match delivery.writeback`) | **Equivalent** |

### 56.6 Lock order (Stage 38+39 sender-waiter fix)

```
[no lock]
  → current_tid_authoritative (takes+releases global lock)
  → resolve_endpoint_recv_cap_split_read (task(2)→cap(4), no ipc lock)
  → self.with(|state| try_split_recv_queued_plain_with_snapshot_locked):
      → try_recv_core_*:
          → ipc_state_lock (rank 3) acquired
          → dequeue first message + refill sender's message (two-phase)
          → ipc_state_lock (rank 3) released
      → apply_split_sender_wake_plan(wake_tid):
          → scheduler_state (rank 1) acquired
          → wake sender (set Runnable)
          → scheduler_state released
          (NO ipc_state_lock held ✓, NO capability lock held ✓)
      → writeback (user copy / register write):
          (NO ipc_state_lock held ✓, NO capability lock held ✓)
```

### 56.7 FallbackReason update: SenderWaiterWake narrowed

`FallbackReason::SenderWaiterWake` is now produced only when:
- Sender-waiter's message has cap-transfer/reply-cap flags, OR
- Sender-waiter queue is sparse (gap at position 0 indicates timed-out sender)

Plain sender-waiter + plain queued message is now handled via `Delivered + WakeSender`.

### 56.8 Eligibility matrix (Stage 38+39 additions)

| Scenario | Eligible? | Plan | Reason |
|---|---|---|---|
| kernel-task + plain + plain sender-waiter | **YES (Stage 38+39)** | `KernelPlainEligible` + `WakeSender` | sender wake deferred via delivery |
| user-ASID + plain + plain sender-waiter | **YES (Stage 38+39)** | `UserPlainEligible` + `WakeSender` | same |
| user-ASID + V2 meta + plain sender-waiter | **YES (Stage 38+39)** | `UserPlainV2Eligible` + `WakeSender` | same |
| any + cap-transfer sender-waiter | NO | `SenderWaiterWake` fallback | cap materialization required |
| cap-transfer at queue head | NO | `CapTransfer` fallback | not dequeued on split |
| reply-cap at queue head | NO | `CapTransfer` fallback | not dequeued on split |
| OPCODE_SHARED_MEM | NO | `CapTransfer` fallback (via FLAG_CAP_TRANSFER) | VM mapping required |

### 56.9 v3 known vs missing metadata table

| v3 output field | Available from kernel | Source | Blocker |
|---|---|---|---|
| `sender_tid` | YES | `msg.sender_tid.0` | None |
| `message_len` | YES | `app_payload.len()` | None |
| `message_flags` | YES | `msg.flags` | None |
| `transferred_cap` | PARTIAL | local CapId after materialization | cap materialization not yet on split path |
| `object_kind` | NO | FUTURE | kernel doesn't surface cap type in IPC msg |
| `object_generation` | NO | FUTURE | not in transfer envelope today |
| `effective_rights` | NO | FUTURE | not in transfer envelope today |
| `exact_object_size` | NO | FUTURE | not in IPC message |
| `region_offset` | YES (OPCODE_SHARED_MEM only) | `SharedMemoryRegion.offset` | only for OPCODE_SHARED_MEM messages |
| `exact_region_len` | NO | FUTURE | `SharedMemoryRegion.len` is app-provided, not kernel-verified |
| `mapped_base` | YES (after map) | returned by `map_shared_region_into_receiver` | requires VM mapping |
| `page_rounded_mapped_len` | YES (after map) | `mapped_len` from mapper | requires VM mapping |
| `actual_mapping_perm` | YES (after map) | `recv_map_flags` | requires VM mapping |
| `cleanup_token` | NO | FUTURE | no cleanup token identity in kernel today |
| `request_id` | NO | FUTURE | VFS shared I/O request ID not implemented |

### 56.10 Blockers before public v3 syscall

1. **Cap-transfer materialization on split path** — rollback semantics not yet proven for all failure paths
2. **Object kind/rights/size surfaced in transfer envelope** — kernel does not yet include these in the transfer envelope
3. **Exact region length** — `SharedMemoryRegion.len` is application-provided; kernel does not verify against object bounds
4. **Cleanup token identity** — no cleanup token concept in kernel today
5. **VM mapping on split path** — vm_state_lock (rank 5) acquisition and TLB shootdown rollback not yet split-extracted

### 56.11 Live vs fallback matrix (cumulative Stage 33+34+35+36+37+38+39)

| Syscall | Receiver type | Sender-waiter? | Path | Stage |
|---|---|---|---|---|
| ipc_recv (NR2) | kernel task, queued plain | none | LIVE split | 33+34 |
| ipc_recv (NR2) | kernel task, queued plain | plain | **LIVE split + wake** | **38+39** |
| ipc_recv (NR2) | user-ASID plain (no meta, no map) | none | LIVE split | 36 |
| ipc_recv (NR2) | user-ASID plain (no meta, no map) | plain | **LIVE split + wake** | **38+39** |
| ipc_recv (NR2) | user-ASID + V2 meta (no map) | none | LIVE split | 37 |
| ipc_recv (NR2) | user-ASID + V2 meta (no map) | plain | **LIVE split + wake** | **38+39** |
| ipc_recv (NR2) | any + cap-transfer msg | any | global-lock | 38+39 blocker |
| ipc_recv (NR2) | any + reply-cap msg | any | global-lock | 38+39 blocker |
| ipc_recv (NR2) | any + OPCODE_SHARED_MEM | any | global-lock | 38+39 blocker |
| ipc_recv (NR2) | user-ASID + V2 meta + map_intent | any | global-lock | 37 |
| ipc_recv (NR5) | any | any | global-lock | 35 |

---

## §57 Stage 40+41: recv_shared_v3 ABI contract + object metadata audit + disabled dispatch scaffold

### 57.1 Overview

Stage 40+41 finalises the internal `recv_shared_v3` metadata contract against
the §56.9 gap table, defines stable ABI structs/constants in `yarm-ipc-abi`,
adds a non-test kernel adapter (`RecvRequest::from_v3_abi_request`), and adds
draft helpers in `yarm-user-rt`.  No public syscall dispatch is added; `SYSCALL_COUNT`
remains 30.

**Files changed:**
- `crates/yarm-ipc-abi/src/recv_shared_v3_abi.rs` — new: stable ABI structs, constants, validation
- `crates/yarm-ipc-abi/src/lib.rs` — add `pub mod recv_shared_v3_abi`
- `src/kernel/recv_core.rs` — add `RecvRequest::from_v3_abi_request` (non-test adapter)
- `crates/yarm-user-rt/src/recv_v3_draft.rs` — new: draft builder + output helpers
- `crates/yarm-user-rt/src/lib.rs` — add `pub mod recv_v3_draft`
- `src/kernel/boot/tests.rs` — add `mod stage40` (31 tests)

### 57.2 Metadata availability table (Stage 40+41)

This table refines §56.9 with full authoritative classification.

| Output field | Status | Source | Blocker before population |
|---|---|---|---|
| `sender_tid` | **Authoritative** | `msg.sender_tid.0` | None |
| `message_len` | **Authoritative** | `msg.as_slice().len()` | None |
| `message_flags` | **Authoritative** | `msg.flags` | None |
| `result_status` | **Authoritative** | kernel result | None |
| `abi_version` | **Authoritative** | `SYSCALL_ABI_VERSION = 10` | None |
| `transferred_cap` | Available, but not on split path | CapId after `take_transfer_envelope` + `grant_task_to_task_with_rights` | Cap materialization on split path (§56.2 blockers) |
| `region_offset` | Available (OPCODE_SHARED_MEM only) | `SharedMemoryRegion.offset` | Requires OPCODE_SHARED_MEM message path |
| `mapped_base` | Available after VM mapping | result of `map_shared_region_into_receiver` | vm_state_lock (rank 5) on split path |
| `page_rounded_mapped_len` | Available after VM mapping | `mapped_len` from mapper | Same as above |
| `actual_mapping_perm` | Available after VM mapping | `recv_map_flags` | Same as above |
| `object_kind` | **FUTURE (unavailable)** | Kernel does not surface cap type in IPC transfer envelope | Object introspection API needed |
| `object_generation` | **FUTURE (unavailable)** | Not in transfer envelope today | Same |
| `effective_rights` | **FUTURE (unavailable)** | Not in transfer envelope today | Same |
| `exact_object_size` | **FUTURE (unavailable)** | Not in IPC message | Same |
| `exact_region_len` | **FUTURE (unavailable)** | `SharedMemoryRegion.len` is app-provided, not kernel-verified | Kernel must verify against object size |
| `cleanup_token` | **FUTURE (unavailable)** | No cleanup token concept in kernel | New object type needed |
| `request_id` | **FUTURE (unavailable)** | VFS shared I/O not implemented | VFS_SHARED_IO stage |

**Summary:** 4 fields are authoritative now (`sender_tid`, `message_len`, `message_flags`, result metadata).
3 fields are available only on specific transfer paths (cap, mapping).
7 fields are genuinely unavailable and would require new kernel object model work.

### 57.3 ABI contract (yarm-ipc-abi/src/recv_shared_v3_abi.rs)

**Request record** (`RecvSharedV3Request`, 64 bytes minimum):

| Offset | Field | Type | Notes |
|---|---|---|---|
| 0 | `version` | u32 | Must equal `RECV_V3_VERSION` = 3 |
| 4 | `record_len` | u32 | Must be ≥ `RECV_V3_MIN_REQUEST_LEN` = 64 |
| 8 | `endpoint_cap` | u64 | Endpoint capability ID |
| 16 | `payload_ptr` | u64 | User payload buffer pointer |
| 24 | `payload_len` | u64 | User payload buffer capacity |
| 32 | `metadata_ptr` | u64 | Output record pointer (0 if not needed) |
| 40 | `metadata_len` | u64 | Output record capacity |
| 48 | `map_intent` | u32 | `MAP_READ=0x1`, `MAP_WRITE=0x2`, or 0 |
| 52 | `flags` | u32 | Reserved; must be 0 |
| 56 | `timeout_ticks` | u64 | 0=no-wait, `u64::MAX`=forever |
| 64 | `reserved[0..2]` | [u64;2] | Must be 0 |

**Output record** (`RecvSharedV3Output`, 80 bytes minimum):

| Offset | Field | Type | Populated now? |
|---|---|---|---|
| 0 | `version` | u32 | Yes (= 3) |
| 4 | `record_len` | u32 | Yes (≥ 80) |
| 8 | `abi_version` | u32 | Yes (= 10) |
| 12 | `result_status` | u32 | Yes |
| 16 | `sender_tid` | u64 | Yes |
| 24 | `message_len` | u32 | Yes |
| 28 | `message_flags` | u32 | Yes |
| 32 | `transferred_cap` | u64 | Partial (after cap-transfer stage) |
| 40 | `object_kind` | u32 | No (0 = Unknown) |
| 44 | _pad_ | u32 | — |
| 48 | `object_generation` | u64 | No (0) |
| 56 | `effective_rights` | u32 | No (0) |
| 60 | _pad_ | u32 | — |
| 64 | `exact_object_size` | u64 | No (0) |
| (see struct) | … remaining fields | … | See §57.2 |

**Constants defined:**
- `RECV_V3_VERSION = 3`, `RECV_V3_MIN_REQUEST_LEN = 64`, `RECV_V3_MIN_OUTPUT_LEN = 80`
- `RECV_V3_MAP_READ = 0x1`, `RECV_V3_MAP_WRITE = 0x2`
- `RECV_V3_NO_TRANSFER_CAP = u64::MAX` (sentinel)
- `RECV_V3_FIELD_UNAVAILABLE = 0` (sentinel for FUTURE fields)
- `RECV_V3_ABI_VERSION = 10`
- Status constants: `OK=0`, `WOULD_BLOCK=1`, `TIMED_OUT=2`, `INVALID_CAP=3`, `BAD_REQUEST=4`
- `RecvSharedV3ObjectKind` enum: `Unknown=0`, `MemoryObject=1`, `Endpoint=2`, `ReplyCap=3`, `Notification=4`, `Other=0xFF`

### 57.4 Kernel adapter: RecvRequest::from_v3_abi_request

```
RecvRequest::from_v3_abi_request(requester_tid: u64, abi: &RecvSharedV3Request)
  -> Result<RecvRequest, RecvSharedV3Error>
```

- Calls `validate_v3_request` first (rejects bad version/length/reserved/flags/map_intent)
- Maps `map_intent` u32 → `RecvMapIntent` enum
- Maps `timeout_ticks` → `RecvBlockingPolicy` (0=NoWait, MAX=WaitForever, else Deadline)
- Sets `payload_target = UserMemory { ptr, len }`
- Sets `meta_target = V3Future { ptr, len }` if `metadata_ptr != 0` and `metadata_len >= 80`, else `None`
- Returns `RecvRequest { kind: SharedV3Future, ... }`
- `plan_recv_core` will return `FallbackRequired(SharedV3HelperOnly)` for any `SharedV3Future` request — no live dispatch

### 57.5 user-rt draft module: recv_v3_draft

`crates/yarm-user-rt/src/recv_v3_draft.rs` provides:
- Re-exports of all `recv_shared_v3_abi` types and constants
- `RecvSharedV3Builder` — builder pattern for constructing request records
- `alloc_output()` — zeroed output record
- `output_is_ok()`, `output_has_transfer_cap()`, `output_has_mapping()` — status helpers

**No syscall invocation.** Draft; expect breaking changes before Stage 42.

### 57.6 Disabled dispatch invariants

1. `SYSCALL_COUNT == 30` — no new syscall number assigned
2. `plan_recv_core` returns `SharedV3HelperOnly` for ALL `SharedV3Future` requests
3. No NR (syscall number) allocated for `recv_shared_v3`
4. The split-dispatch table does not contain any v3 routing
5. `RecvRequest::future_shared_v3` (test-only constructor) continues to produce the same plan result

### 57.7 Blockers before live public recv_shared_v3

1. **Cap materialization on split path** — cap lock (rank 4) after ipc_state_lock (rank 3) released; rollback semantics unproven (§56.2)
2. **Object introspection** — `object_kind`, `object_generation`, `effective_rights`, `exact_object_size` not in transfer envelope
3. **Exact region length verification** — `SharedMemoryRegion.len` is app-provided
4. **VM mapping on split path** — vm_state_lock (rank 5) + TLB shootdown rollback
5. **Cleanup token** — no cleanup token object model in kernel today
6. **VFS shared I/O** — `request_id` field requires VFS_SHARED_IO stage
7. **Wire layout lock** — `#[repr(C)]` not yet applied; must be added before live syscall with guarantee of no layout change

### 57.8 Relation to old ipc_recv / recv-v2 / VFS_SHARED_IO

- `ipc_recv` (NR2) and `recv-v2` (via NR2 with meta args) are unchanged; their live paths
  and fallback semantics are preserved (§55, §56).
- `recv_shared_v3` is a distinct future syscall — the old NR2 register ABI is not reused.
  When live, it will receive a new NR.
- `VFS_SHARED_IO`: `request_id` / `region_offset` / `exact_region_len` fields in
  `RecvSharedV3Output` are reserved for VFS shared I/O integration.  They are
  always zero until that stage is implemented.

### 57.9 Next recommended stage

**Stage 42 options (choose one):**

A. **Cap-transfer materialization on split path** — prove rollback semantics, add
   `take_transfer_envelope` / `grant_task_to_task_with_rights` under correct lock order;
   then `transferred_cap` becomes authoritative in output.

B. **Object introspection API** — extend transfer envelope to carry object kind,
   generation, effective rights; populate FUTURE fields in output.

C. **Live recv_shared_v3 dispatch (if A complete)** — assign NR, add `#[repr(C)]`,
   wire split-dispatch table to v3 adapter, live-enable with `SYSCALL_COUNT = 31`.

---

## §58 Stage 42+43: cap-transfer split path + live recv_shared_v3 dispatch (NR 30)

Stage 42+43 implements options A+C from §57.9 in a single stage:

### 58.1 Cap-transfer split-path proof

**Lock order (all three split-core functions):**

1. `ipc_state_lock` (rank 3) — acquired for dequeue via `ipc_try_recv_queued_with_cap_transfer`; released before any cap operation.
2. `capability_lock` (rank 4) — acquired inside `materialize_received_message_cap` / `rollback_materialized_recv_cap`; no ipc lock held.
3. `scheduler_lock` (rank 1) — acquired inside `apply_split_sender_wake_plan`; after ipc lock release, before or after cap lock (sender wake does not touch capability domain).

This satisfies the canonical lock order: scheduler(1) → task(2) → ipc_state(3) → capability(4) → vm(5) → memory(6). No lock is ever held across a user-memory copy.

**Operation order per delivery:**
1. Dequeue message (ipc lock acquired and released).
2. Materialize cap from `RecvCapTransferPlan` — `take_transfer_envelope` + `grant_task_to_task_with_rights` (capability lock only).
3. Wake sender if `RecvSchedulerWakePlan::WakeSender` (scheduler lock only).
4. User-space writeback (payload/meta copy; no kernel lock held).
5. On writeback failure (meta fault or undersized buffer): `rollback_materialized_recv_cap` (capability lock only).

**No rollback on payload copy fault**: payload copy failure is defined as a silent truncation with `Ok()` return (dequeue-before-copy semantics, §54); the cap has already been transferred and the message dequeued.

### 58.2 Rollback semantics table

| Writeback outcome | Cap rollback? | Reason |
|---|---|---|
| `Ok` | No | Message delivered successfully |
| `UndersizedBuffer` | Yes | Meta-first: meta buffer too small; message not useful to receiver |
| `CopyFault` (meta) | Yes | Meta copy fault; receiver cannot read message |
| `CopyFault` (payload) | No | Payload copy fault treated as silent truncation (§54); message "delivered" |

### 58.3 `ipc_try_recv_queued_with_cap_transfer`

New `ipc_state.rs` helper, identical to `ipc_try_recv_queued_plain_endpoint_only` except:
- **Does NOT call `split_unsafe_flags`** on the receiver's message — cap-flagged messages are dequeued.
- Still rejects a **sender-waiter** whose **refill message** has cap-transfer/reply-cap flags (`SenderWaiterPresent` fallback); that path requires cap materialization under the ipc lock which the split path cannot do.

### 58.4 `RecvCapTransferPlan` and `extract_cap_transfer_plan`

```rust
pub struct RecvCapTransferPlan {
    pub raw_handle: u64,    // raw value from msg.transferred_cap()
    pub is_reply_cap: bool, // true iff FLAG_REPLY_CAP set
}
fn extract_cap_transfer_plan(msg: &Message) -> Option<RecvCapTransferPlan>
```

All three `try_recv_core_*` functions populate `RecvDelivery.cap_transfer` via `extract_cap_transfer_plan`.

### 58.5 `handle_recv_shared_v3` (NR 30 dispatch)

- **Non-blocking only**: `timeout_ticks != 0` → `WouldBlock` (blocking requires task-state changes).
- **No mapped receive**: `map_intent != 0` → `InvalidArgs` (vm lock on split path not proven).
- Uses `try_recv_core_user_plain` with the canonical request built from the v3 ABI record.
- Writes `RecvSharedV3Output` (80 bytes) to `metadata_ptr` on success.
- `transferred_cap` field in output is populated when cap materialization succeeds; `RECV_V3_NO_TRANSFER_CAP` (u64::MAX) when no cap transfer.
- `object_kind`, `object_generation`, `effective_rights`, `exact_object_size` remain 0 (FUTURE fields; object introspection not yet implemented — §57.2 rows B/C).

### 58.6 Invariants

- `SYSCALL_COUNT == 31`: confirmed by internal compile-time assertion and 12 stage42 tests.
- `SYSCALL_RECV_SHARED_V3_NR == 30`: added to syscall.rs and asserted in tests.
- compile-time: `assert!(SYSCALL_RECV_SHARED_V3_NR < SYSCALL_COUNT)` ensures NR is in-range.
- Old NR2 (`ipc_recv` / recv-v2 / recv-timeout) ABI: unchanged; split path behavior identical to pre-stage-42 for all non-cap-transfer messages.
- `FallbackReason::CapTransfer`: retained for external callers and the sender-waiter-with-cap-transfer case (still produces `SenderWaiterWake` fallback, deferred).
- `#[repr(C)]` applied to `RecvSharedV3Request` and `RecvSharedV3Output` in `yarm-ipc-abi` to lock wire layout.


## §59 Stage 44+45: user-rt wrapper and first userspace proof

### 59.1 Stage 44 corrections

- `SYSCALL_RECV_SHARED_V3_NR` was 31 in the Stage 42+43 commit; corrected to 30.
  The exhaustive whitelist loop `for nr in 0..SYSCALL_COUNT` only covers 0..=30,
  so NR 31 would never have been tested or dispatched.
- compile-time assert `assert!(SYSCALL_RECV_SHARED_V3_NR < SYSCALL_COUNT)` added.
- 9 hosted-dev dispatch tests added (`mod stage44`) covering WouldBlock, deliver, field guards.

### 59.2 Stage 45 output metadata contract

Stage 45 proves the kernel writes correct authoritative fields to the user-supplied
`metadata_ptr` buffer:

```
@0  version (u32)         = RECV_V3_VERSION (3)
@4  record_len (u32)      = 80 (RECV_V3_MIN_OUTPUT_LEN)
@8  abi_version (u32)     = RECV_V3_ABI_VERSION (10)
@12 result_status (u32)   = 0 (OK) or 1 (WouldBlock)
@16 sender_tid (u64)      = authoritative sender thread ID
@24 message_len (u32)     = authoritative payload byte count
@28 message_flags (u32)   = raw message flags
@32 transferred_cap (u64) = local cap ID or u64::MAX (RECV_V3_NO_TRANSFER_CAP)
@40 ... (80 bytes total)  = zeros (FUTURE fields in Stage 42+43)
```

### 59.3 user-rt decoder (`RecvSharedV3Delivery::from_output`)

- `from_output(&RecvSharedV3Output)` added to decode the kernel-written buffer.
- Returns `Some(delivery)` only when `result_status == RECV_V3_STATUS_OK`.
- Returns `None` for any other status (WouldBlock, timed-out, etc.).
- `transferred_cap` field: `None` when `output.transferred_cap == RECV_V3_NO_TRANSFER_CAP`.

### 59.4 Cap-transfer blocker

Cap-transfer proof through `dispatch()` in hosted-dev requires `stash_transfer_envelope`
to be set up before `ipc_send`. The boot-level `ipc_send` helper does NOT set up the
envelope (that is done by the syscall handler path in `handle_ipc_send`). The decoder
contract (from_output with a non-sentinel `transferred_cap`) is proven in user-rt unit
tests; the full kernel-dispatch cap-transfer proof is deferred to a future stage.

### 59.5 Production isolation

- No production service loop uses recv_shared_v3.
- No new syscall numbers. `SYSCALL_COUNT == 31` unchanged.
- Blocking, map_intent, and object metadata remain disabled.


## §60 Stage 46: cap-transfer recv_shared_v3 proof via real send path

### 60.1 Blocker resolved

Stage 45 documented (§59.4) that cap-transfer proof through `dispatch()` was blocked
because the boot-level `ipc_send` helper does not call `stash_transfer_envelope`.
Stage 46 resolves this by using `dispatch(IpcSend)` which calls the real
`handle_ipc_send` → `stash_transfer_handle` → `stash_transfer_envelope` path.

### 60.2 Proof fixture (mod stage46 in src/kernel/boot/tests.rs)

**Send phase (no user ASID):**
- `dispatch(IpcSend)` with `arg5 = mem_cap` (capability to transfer).
- `handle_ipc_send`: `sender_has_user_asid = false` → inline payload path.
- `stash_transfer_handle` → `stash_transfer_envelope` stashes the envelope.
- Message enqueued with `FLAG_CAP_TRANSFER` and stash handle embedded.
- Telemetry: `cap_transfer_stage4e_enqueued` incremented (observable proof of stash call).

**Receive phase (user ASID set up after send):**
- `dispatch(RecvSharedV3)` on same task/endpoint.
- `try_recv_core_user_plain` dequeues the `FLAG_CAP_TRANSFER` message.
- `extract_cap_transfer_plan` produces `Some(plan)` from the message flags.
- `materialize_received_message_cap` → `materialize_received_transfer_cap` →
  `take_transfer_envelope(stash_handle, endpoint, receiver_tid)` → succeeds.
- `grant_task_to_task_with_rights` mints cap into receiver cnode.
- `write_v3_output_to_user` writes non-sentinel `transferred_cap` to metadata buffer.
- `frame.set_ok(..., xfer_cap_out)` also carries the cap in `ret2`.

### 60.3 Proven assertions

- `result_status == RECV_V3_STATUS_OK`
- `transferred_cap != SYSCALL_NO_TRANSFER_CAP` (u64::MAX)
- `message_flags & FLAG_CAP_TRANSFER != 0`
- `frame.ret2() == transferred_cap` (register path and metadata path agree)
- `resolve_current_task_capability(CapId(transferred_cap))` succeeds and returns
  `CapObject::MemoryObject` (cap is live in receiver cnode)

### 60.4 Negative proof (stage46_direct_enqueue_phony_cap_transfer_fails_materialization)

Boot-level `ipc_send` with a phony `FLAG_CAP_TRANSFER` handle (not stashed) causes
`take_transfer_envelope` to return `None`, resulting in `Err(SyscallError::InvalidCapability)`
from `dispatch(RecvSharedV3)`. This proves `stash_transfer_envelope` is required.

### 60.5 FUTURE / remaining blockers (resolved in Stage 47+48)

- `object_kind`, `object_generation`, `effective_rights` implemented in Stage 47+48 (see §61).
- `exact_object_size` remains 0 (FUTURE; Stage 49).
- No production service loop uses recv_shared_v3.
- `SYSCALL_COUNT == 31` unchanged. No new syscall numbers.

## §61 Stage 47+48: object metadata for transferred caps in recv_shared_v3

### 61.1 Goal

Fill the three object-introspection fields in the `RecvSharedV3Output` 80-byte buffer
that §60.5 identified as remaining FUTURE/zero.  `exact_object_size` stays 0.

### 61.2 ABI layout (frozen since Stage 42+43)

| Offset | Width | Field | Stage 47+48 status |
|--------|-------|-------|--------------------|
| 40 | 4 | `object_kind` (u32) | **Authoritative** — `RecvSharedV3ObjectKind` discriminant |
| 44 | 4 | C-layout padding | Always zero |
| 48 | 8 | `object_generation` (u64) | **Authoritative** for Endpoint/Notification/Reply; 0 for MemoryObject |
| 56 | 4 | `effective_rights` (u32) | **Authoritative** — `CapRights::bits() as u32` on receiver-local cap |
| 60 | 4 | C-layout padding | Always zero |
| 64 | 8 | `exact_object_size` (u64) | 0 (FUTURE, Stage 49) |
| 72 | 8 | `region_offset` (u64) | 0 (FUTURE) |

### 61.3 Kernel implementation

**`write_v3_output_to_user`** extended with three new parameters:
- `object_kind: u32` → `out[40..44]`
- `object_generation: u64` → `out[48..56]`
- `effective_rights: u32` → `out[56..60]`

**`recv_v3_object_kind(obj: CapObject) -> u32`:** maps CapObject variant to RecvSharedV3ObjectKind discriminant (MemoryObject→1, Endpoint→2, Reply→3, Notification→4, other→0xFF).

**`recv_v3_object_generation(obj: CapObject) -> u64`:** returns the `generation` field for Endpoint/Notification/Reply; 0 for all other variants (MemoryObject has no generation).

**Call sites in `handle_recv_shared_v3`:**
- WouldBlock path: passes `0, 0, 0` (no cap).
- OK/Delivered path: resolves the materialized cap via `kernel.capability_service().resolve_current_task_capability(CapId(cap_id_raw))`, extracts `recv_v3_object_kind(cap.object)`, `recv_v3_object_generation(cap.object)`, `u32::from(cap.rights_bits())`.

### 61.4 Proven assertions (mod stage47 in src/kernel/boot/tests.rs)

**Primary proof (MemoryObject transfer):**
- `object_kind @40 == 1` (RecvSharedV3ObjectKind::MemoryObject)
- `padding @44 == 0`
- `object_generation @48 == 0` (MemoryObject has no generation field)
- `effective_rights @56 == 0x07` (READ|WRITE|MAP on anonymous MemoryObject)
- `padding @60 == 0`
- `exact_object_size @64 == PAGE_SIZE` (authoritative since Stage 49; updated from 0)

**Negative proof (plain message, no cap):**
- All five introspection fields are 0.

### 61.5 FUTURE / remaining blockers (resolved in Stage 49)

- `exact_object_size` for MemoryObject implemented in Stage 49 (see §62).
- No production service loop uses recv_shared_v3.
- `SYSCALL_COUNT == 31` unchanged. No new syscall numbers.

## §62 Stage 49: exact_object_size for MemoryObject transfers in recv_shared_v3

### 62.1 Goal

Fill `exact_object_size @64` in the `RecvSharedV3Output` 80-byte buffer when the
transferred cap resolves to a `CapObject::MemoryObject`. All other cap kinds and
plain messages continue to receive 0.

### 62.2 Authoritative size source

`MemoryObject.len: usize` in `MemorySubsystem.memory_objects` (a `[Option<MemoryObject>; 512]`
array). The `len` field is always > 0 and PAGE_SIZE-aligned (enforced at creation in
`create_memory_object_with_len_and_kind`). Looked up by iterating the array and matching
`entry.id == id` (where `id` is extracted from `CapObject::MemoryObject { id }`).

`CapObject::MemoryObject` stores only `id: u64` — no size. The kernel registry is
the sole authority.

### 62.3 Kernel implementation

**`recv_v3_exact_object_size(kernel: &KernelState, obj: CapObject) -> u64`:**
- Pattern-matches on `CapObject::MemoryObject { id }` — returns 0 for all other variants.
- Calls `kernel.with_memory_state(|memory| ...)` and linear-searches `memory_objects` by `id`.
- Returns `entry.len as u64` or 0 if the object is not found (should not happen after materialization).

**`write_v3_output_to_user`:** New `exact_object_size: u64` parameter fills `out[64..72]`.

**`handle_recv_shared_v3` OK path:** Metadata computation block refactored to call
`capability_service().resolve_current_task_capability()` first (releasing the borrow),
then calls `recv_v3_exact_object_size(kernel, cap.object)` separately.

### 62.4 Proven assertions (mod stage49 in src/kernel/boot/tests.rs)

**Primary proof (1-page MemoryObject):**
- `exact_object_size @64 == PAGE_SIZE` (create_memory_object defaults to 1 page)
- `region_offset @72 == 0` (FUTURE)

**Negative proof (plain message, no cap):**
- `exact_object_size == 0`

### 62.5 ABI semantics (frozen)

- `exact_object_size` is authoritative for `object_kind == MemoryObject (1)` only.
- For all other cap kinds (Endpoint, Notification, ReplyCap, Other) → 0.
- For plain messages (no cap transferred) → 0.
- The value is always a non-zero multiple of PAGE_SIZE when a MemoryObject was transferred.

### 62.6 FUTURE / remaining blockers (resolved in Stage 50+51)

- `exact_region_len` for DmaRegion implemented in Stage 50+51 (see §63).
- No production service loop uses recv_shared_v3.
- `SYSCALL_COUNT == 31` unchanged. No new syscall numbers.

## §63 Stage 50+51: exact_region_len for DmaRegion transfers + map_intent audit

### 63.1 Goal

Fill `exact_region_len @80..88` in the `RecvSharedV3Output` extended buffer when the
transferred cap resolves to a `CapObject::DmaRegion`.  All other cap kinds and plain
messages get 0.  The field is outside the 80-byte minimum buffer — it is written only
when the caller provides at least 88 bytes in `metadata_len`.

Document map_intent blockers; keep the `map_intent != 0 → InvalidArgs` gate unchanged.

### 63.2 Authoritative size source

`DmaRegion.len: u64` is embedded directly in `CapObject::DmaRegion { id, offset, len }`.
No registry lookup is needed — the length is the authoritative sub-region extent stored at
`mint_dma_region_cap` time.  The value is always a non-zero PAGE_SIZE-multiple (enforced
in `mint_dma_region_cap_for_task`: `!len.is_multiple_of(PAGE_SIZE) || len == 0 → Misaligned`).

DmaRegion caps can be transferred via plain IPC cap-transfer (no shared region):
`validate_transfer_record_metadata` returns `Ok(())` immediately when `shared_region = None`.

### 63.3 Kernel implementation

**`recv_v3_exact_region_len(obj: CapObject) -> u64`:**
- Pattern-matches on `CapObject::DmaRegion { len, .. }` — returns 0 for all other variants.
- No lock needed — `len` is stored inline in the CapObject.

**`write_v3_output_to_user`:**
- New `exact_region_len: u64` parameter.
- Buffer extended from `[0u8; 80]` to `[0u8; 88]`.
- Writes `out[80..88]` with `exact_region_len`.
- Write length: `min(out_len as usize, 88)` so 80-byte callers receive only 80 bytes
  (unchanged behaviour); 88-byte callers additionally receive `exact_region_len`.
- `RECV_V3_EXTENDED_OUTPUT_LEN = 88` added to `yarm-ipc-abi` ABI crate.

**`handle_recv_shared_v3` OK path:**
Metadata computation tuple extended from 4 elements to 5:
`(obj_kind, obj_gen, eff_rights, exact_obj_size, exact_reg_len)`.

### 63.4 map_intent audit (blockers, gate unchanged)

The gate `if req.map_intent != 0 { return Err(SyscallError::InvalidArgs); }` at
`syscall.rs` ~line 4206 remains in place.  Full mapping requires:

1. VA selection field missing from current ABI.
2. Per-mapping cleanup on cap revoke — unaudited.
3. Output fields beyond @88 need further buffer extension.
4. Atomicity/rollback of mapping vs cap transfer undefined.

No production service loop uses `map_intent`.

### 63.5 Proven assertions (mod stage50 in src/kernel/boot/tests.rs)

**A. Primary proof (DmaRegion, 88-byte buffer):**
- `object_kind @40 == 0xFF` (DmaRegion has no dedicated kind discriminant — falls through to Other)
- `exact_region_len @80..88 == PAGE_SIZE` (1-page DmaRegion via `mint_dma_region_cap`)

**B. Negative proof (MemoryObject, 88-byte buffer):**
- `exact_object_size @64 == PAGE_SIZE` (MemoryObject has exact_object_size)
- `exact_region_len @80..88 == 0`

**C. Negative proof (plain message, 88-byte buffer):**
- `exact_region_len == 0`

**D+E. map_intent gate still enforced:**
- `map_intent = RECV_V3_MAP_READ → InvalidArgs`
- `map_intent = RECV_V3_MAP_READ | RECV_V3_MAP_WRITE → InvalidArgs`

### 63.6 ABI semantics (frozen)

- `exact_region_len` is authoritative for `CapObject::DmaRegion` only.
- For MemoryObject, Endpoint, Notification, ReplyCap, Other → 0.
- For plain messages (no cap transferred) → 0.
- Only present when `metadata_len >= RECV_V3_EXTENDED_OUTPUT_LEN (88)`.
- The value is always a non-zero multiple of PAGE_SIZE when a DmaRegion was transferred.
- DmaRegion maps to `object_kind = 0xFF (Other)` in the current ABI.

### 63.7 FUTURE / remaining blockers

- `map_intent` / shared-memory mapping — blocked (see §63.4).
- `cleanup_token` — future.
- No production service loop uses recv_shared_v3.
- `SYSCALL_COUNT == 31` unchanged. No new syscall numbers.

## §64 Stage 52+53 — DmaRegion first-class object kind + cleanup-token scaffold

### 64.1 Overview

Make `CapObject::DmaRegion` a first-class `RecvSharedV3ObjectKind` (discriminant 5).
Design and document the cleanup-token semantics; add a helper-only scaffold struct
(`RecvSharedV3CleanupIdentity`) with no live allocation.  Keep `map_intent` disabled.
`SYSCALL_COUNT == 31` unchanged; no syscall numbers changed.

### 64.2 DmaRegion object kind promotion

`recv_v3_object_kind` previously fell through the catch-all `_ => 0xFF` for
`CapObject::DmaRegion`.  A dedicated arm is now added:

```rust
CapObject::DmaRegion { .. } => 5,
```

`RecvSharedV3ObjectKind::DmaRegion = 5` is added to the ABI enum in
`yarm-ipc-abi`.  Discriminant 5 is beyond the previous maximum (Notification=4) and
does not conflict with any existing variant.

The stage50 test `stage50_exact_region_len_for_dma_region_transfer` previously asserted
`object_kind == 0xFF`; it has been updated to `object_kind == 5` to match the new
canonical discriminant.

### 64.3 Cleanup-token design audit

The `cleanup_token` ABI field lives at offset @112 in `RecvSharedV3Output`.
The kernel's write window is `min(out_len, 88)` bytes — `cleanup_token` is
**never written by the kernel** in the current implementation.

Allocation of a live cleanup token requires:

1. A per-transfer kernel-allocated handle table entry.
2. A corresponding release syscall (not yet designed).
3. Token revocation semantics when the receiving task exits.
4. Atomicity between token creation and cap transfer.

None of the above are implemented.  `RECV_V3_CLEANUP_TOKEN_NONE = 0` is the sentinel
for "no live cleanup token."

### 64.4 RecvSharedV3CleanupIdentity scaffold (helper-only)

`RecvSharedV3CleanupIdentity` is a **helper-only** struct in `yarm-ipc-abi`.
It is never created by the kernel and never participates in live transfers:

```rust
pub struct RecvSharedV3CleanupIdentity {
    pub receiver_cap:   u64,
    pub object_kind:    u32,
    pub region_len:     u64,
    pub transfer_token: u64,
}
```

- `none()` — returns a sentinel (receiver_cap = u64::MAX, others = 0).
- `is_active()` — true iff `transfer_token != 0`.
- `is_structurally_valid(page_size)` — precondition helper for future allocation code.

No kernel path allocates or consumes a `RecvSharedV3CleanupIdentity`.

### 64.5 map_intent gate unchanged

The gate at `syscall.rs` ~line 4206 (`if req.map_intent != 0 → InvalidArgs`) remains
in place.  Blockers are identical to those documented in §63.4.  No new blocker has
been resolved.

### 64.6 user-rt additions

`RecvSharedV3Delivery` gains:

- `cleanup_token: u64` field (always 0 — decoded from output but kernel never writes it).
- `is_dma_region() -> bool` — true iff `object_kind == 5`.
- `cleanup_token() -> u64` — const accessor.
- `has_cleanup_token() -> bool` — true iff `cleanup_token != RECV_V3_CLEANUP_TOKEN_NONE`.

### 64.7 Proven assertions (mod stage52 in src/kernel/boot/tests.rs)

- `stage52_dma_region_object_kind_is_five` — object_kind @40 == 5 for DmaRegion transfer.
- `stage52_dma_region_full_metadata_output` — all DmaRegion metadata fields correct
  (kind=5, gen, rights, exact_region_len=PAGE_SIZE).
- `stage52_memory_object_still_kind_one_with_exact_object_size` — promotion of DmaRegion
  does not regress MemoryObject (kind=1, exact_object_size=PAGE_SIZE).
- `stage52_recv_writes_exactly_88_bytes_for_dma_region` — write window is exactly 88 bytes;
  no write beyond @88 occurs.
- `stage52_recv_writes_exactly_88_bytes_for_plain_message` — plain message: same 88-byte
  write window; no cap metadata.

### 64.8 ABI semantics (frozen)

- `object_kind = 5` is canonical for `CapObject::DmaRegion`; replaces `0xFF` (Other).
- `cleanup_token @112` is never written; callers always read 0 for this field.
- `RECV_V3_CLEANUP_TOKEN_NONE = 0` is the stable sentinel.
- `RECV_V3_EXTENDED_OUTPUT_LEN = 88` remains the extended write boundary.
- `SYSCALL_COUNT == 31` unchanged.

### 64.9 FUTURE / remaining blockers

- Cleanup-token live allocation — blocked (see §64.3).
- `map_intent` / shared-memory mapping — blocked (see §63.4).
- Release syscall for cleanup tokens — not yet designed.
- No production service loop uses recv_shared_v3.

---

## §65 Stage 54+55 — recv_shared_v3 map_intent/shared mapping audit + Option B helper implementation

### 65.1 Audit scope

Stage 54+55 performed a comprehensive audit of all infrastructure required to perform
receive-time shared-memory mapping via `recv_shared_v3` (SYSCALL_RECV_SHARED_V3_NR = 30).

All blocking infrastructure was confirmed present:

| Requirement | Status | Location |
|---|---|---|
| Task-exit cleanup | Confirmed | `purge_active_transfer_mappings_for_pid` at `src/kernel/boot/cnode_state.rs:226` |
| Two-phase rollback | Confirmed | `map_shared_region_into_receiver` (internal rollback on failure) |
| Release syscall | Confirmed | `handle_transfer_release` (NR 4) |
| TLB two-phase unmap | Confirmed | `unmap_page_phase1` + `execute_tlb_shootdown_wait_plan` |
| Transfer map tracking | Confirmed | `register_active_transfer_mapping` / `remove_active_transfer_mapping` / `active_transfer_mapping_for` |
| Cap attenuation | Confirmed | `attenuate_transfer_cap_for_recv_intent` |

### 65.2 Implementation decision: Option B (helper-only mapping plan)

Despite all infrastructure being confirmed available, three unresolved semantic ambiguities
blocked live mapping (Option C):

1. **map_intent != 0 with non-OPCODE_SHARED_MEM message**: unclear whether to fail, skip,
   or return a partial result.
2. **Output write failure after mapping**: if the kernel maps memory but then fails to write
   the output record, the rollback order and error reporting semantics are undefined.
3. **`attenuate_transfer_cap_for_recv_intent` + already-materialized cap interaction**:
   the interaction between cap attenuation and receiver-side cap materialization is not
   yet specified.

Option B was chosen: implement a pure, side-effect-free planning function with no live VM
mutation.

### 65.3 compute_recv_v3_mapping_plan (pure function)

Location: `src/kernel/recv_core.rs`, `mod recv_shared_v3`.

```rust
pub fn compute_recv_v3_mapping_plan(
    msg_opcode: u16,
    map_intent: u32,
    payload_ptr: u64,
    payload_len: u64,
    cap_rights_bits: u8,
    region_len: u64,
    page_size: u64,
) -> RecvV3MappingPlan
```

Rules:
- Returns `Skip` if `map_intent == 0` or `msg_opcode != OPCODE_SHARED_MEM_VALUE`.
- Returns `InvalidRegion` if `payload_ptr == 0`, `region_len == 0`, `page_size == 0`, or
  `payload_len < page_aligned(region_len)`.
- Returns `InsufficientRights` if the `MAP` bit is absent, or `WRITE` is requested but the
  `WRITE` bit is absent from cap_rights.
- Returns `Map { map_va, mapped_len, read_only }` otherwise.

No lock is held during this call. No memory is mapped. No side effects.

### 65.4 RecvV3MappingPlan enum

```rust
pub enum RecvV3MappingPlan {
    Skip,
    Map { map_va: u64, mapped_len: u64, read_only: bool },
    InsufficientRights,
    InvalidRegion,
}
```

### 65.5 map_intent gate unchanged

The gate at `syscall.rs` that returns `InvalidArgs` for any `map_intent != 0` request
remains in place.  `compute_recv_v3_mapping_plan` is not called from the live dispatch path.

### 65.6 ABI additions

`RECV_V3_MAPPED_OUTPUT_LEN: u32 = 108` — output length constant covering the mapping output
fields:
- `mapped_base @88` (u64)
- `page_rounded_mapped_len @96` (u64)
- `actual_mapping_perm @104` (u32)

The kernel never writes these fields.  They remain 0 in all current responses.

### 65.7 user-rt additions

`RecvSharedV3Delivery` gains:
- `mapped_base: u64` field
- `page_rounded_mapped_len: u64` field
- `actual_mapping_perm: u32` field
- `mapped_base() -> u64` — const accessor
- `page_rounded_mapped_len() -> u64` — const accessor
- `actual_mapping_perm() -> u32` — const accessor
- `has_mapping() -> bool` — true iff `mapped_base != 0`

These are always zero in current responses (kernel gate not lifted).

### 65.8 Proven assertions (mod stage54 in src/kernel/boot/tests.rs)

14 tests added:

**Gate (A):**
- `stage54_map_intent_read_only_still_invalid_args`
- `stage54_map_intent_read_write_still_invalid_args`

**Regression (B):**
- `stage54_syscall_count_is_31_and_nr_is_30`
- `stage54_plain_receive_unchanged`

**Plan: Skip (C):**
- `stage54_mapping_plan_skip_when_map_intent_zero`
- `stage54_mapping_plan_skip_when_opcode_not_shared_mem`

**Plan: Map (D):**
- `stage54_mapping_plan_read_only_for_map_read_intent`
- `stage54_mapping_plan_read_write_for_map_readwrite_intent`
- `stage54_mapping_plan_region_len_rounds_up_to_page`

**Plan: InsufficientRights (E):**
- `stage54_mapping_plan_insufficient_rights_when_map_bit_missing`
- `stage54_mapping_plan_insufficient_rights_when_write_requested_but_cap_read_only`

**Plan: InvalidRegion (F):**
- `stage54_mapping_plan_invalid_region_when_payload_ptr_zero`
- `stage54_mapping_plan_invalid_region_when_region_len_zero`
- `stage54_mapping_plan_invalid_region_when_payload_buf_too_small`

### 65.9 FUTURE / remaining blockers for Option C

- Resolve map_intent != 0 semantics for non-OPCODE_SHARED_MEM messages.
- Define output write failure rollback order.
- Specify `attenuate_transfer_cap_for_recv_intent` + materialized cap interaction.
- No production service loop uses recv_shared_v3.

---

## §66 Stage 56+57 — recv_shared_v3 cleanup-token lifecycle design + helper-only registry

### 66.1 Scope

Stage 56+57 designs and scaffolds the cleanup-token lifecycle for future
`recv_shared_v3` map_intent / shared-memory mapping.  No live mapping is
enabled; no VM is mutated; the map_intent gate remains disabled.

### 66.2 Cleanup identity audit

The following table classifies every field required to perform a correct
cleanup of a receive-time shared-memory mapping.

| Field | Classification |
|---|---|
| `receiver_tid` | Available now |
| `receiver_asid` | Available now |
| `receiver_local_cap` | Available now (after cap materialisation) |
| `object_kind` | Available now |
| `object_generation` | Available now |
| `exact_region_len` | Available now (embedded in DmaRegion cap) |
| `map_intent` | Available now (from request) |
| `mapped_base` | Available after live mapping |
| `mapped_len` | Available after live mapping |
| `actual_mapping_perm` | Available after live mapping |
| request/output generation | Not needed (cleanup token serves this role) |

### 66.3 Helper-only types (src/kernel/recv_core.rs, mod recv_shared_v3)

All types below are **helper-only**: no live syscall path creates, reads, or
releases any of them.

#### RecvV3CleanupIdentity

Full kernel-internal cleanup identity with 10 fields covering both
"available now" and "available after live mapping" categories.  Two methods:
- `zeroed()` — `const fn` sentinel with `receiver_local_cap = u64::MAX`.
- `is_mapped()` — `true` iff `mapped_base != 0 && mapped_len != 0`.

#### RecvV3CleanupToken

Opaque `u64` wrapper.  Encoding: `(slot_index + 1) | (generation << 16)`.

- Bits 0..15: `slot_index + 1` (1-based; 0 = NONE sentinel).
- Bits 16..47: per-slot generation counter (starts at 1 on first allocation;
  skips 0 on wrapping).
- `NONE` (0) is the sentinel for "no active mapping".
- `is_valid()` — true iff raw != 0.
- `raw()` — u64 for writing to `RecvSharedV3Output::cleanup_token`.

#### RecvV3CleanupReleaseResult

```
Released        — slot freed successfully
AlreadyReleased — same token, same generation, slot already free
InvalidToken    — zero slot index or index ≥ capacity
StaleGeneration — slot was recycled (generation advanced) since this token was issued
```

#### RecvV3CleanupRegistry

Fixed-capacity (`RECV_V3_CLEANUP_REGISTRY_CAPACITY = 16`) no-heap registry.

- `new()` — `const fn`, all slots empty (generation=0).
- `allocate(identity)` — increments per-slot generation, sets `occupied`,
  returns `Some(token)`; `None` when full.
- `release(token)` — checks index, generation, occupied; returns
  `RecvV3CleanupReleaseResult`.
- `lookup(token)` — returns `Option<&RecvV3CleanupIdentity>`.
- `count_occupied()` — number of currently occupied slots.

### 66.4 Generation/stale semantics

- Generation is incremented **on allocation** (not on release).
- Duplicate release of an unrecycled slot → `AlreadyReleased` (generation
  matches, slot not occupied).
- After a new allocation on the same slot, old tokens → `StaleGeneration`
  (generation no longer matches).
- Generation never reaches 0 (wraps to 1), ensuring the token encoding
  `slot_index_plus_one | (generation << 16)` is always nonzero for valid
  tokens.

### 66.5 ABI crate expansion (crates/yarm-ipc-abi/src/recv_shared_v3_abi.rs)

`RecvSharedV3CleanupIdentity` expanded with four post-mapping fields:
- `mapped_base: u64` — mapped VA (0 until live mapping).
- `mapped_len: u64` — page-rounded length (0 until live mapping).
- `actual_mapping_perm: u32` — permissions granted (0 until live mapping).
- `map_intent: u32` — flags from the request (available now).

New method: `is_mapped()` — `true` iff `mapped_base != 0 && mapped_len != 0`.

`none()` zeros all new fields.  `is_structurally_valid()` is unchanged (it
tests pre-mapping fields only).  `is_active()` is unchanged (tests
`transfer_token != 0`).

### 66.6 map_intent gate unchanged

The gate at `syscall.rs` (`if req.map_intent != 0 → InvalidArgs`) remains in
place.  No `RecvV3CleanupRegistry` slot is ever occupied in the live path.

### 66.7 Kernel write window unchanged

`write_v3_output_to_user` writes a `[0u8; 88]` local array capped at
`min(out_len, 88)` bytes.  Fields at offsets ≥ 88 (`mapped_base`, `cleanup_token`)
are provably never written in the current stage.

### 66.8 Proven assertions

**(A) mod stage56 — 14 kernel tests (src/kernel/boot/tests.rs):**

Token invariants: `stage56_token_none_is_invalid`

Allocation: `stage56_allocate_gives_nonzero_token`,
`stage56_token_encodes_slot_and_generation`

Release: `stage56_release_valid_token_gives_released`,
`stage56_duplicate_release_gives_already_released`,
`stage56_stale_token_after_realloc_gives_stale_generation`,
`stage56_none_token_gives_invalid_token`

Capacity: `stage56_registry_full_returns_none`, `stage56_fill_release_refill`

Lookup: `stage56_lookup_returns_correct_identity`,
`stage56_lookup_after_release_returns_none`

Independence: `stage56_two_slots_are_independent`

Integration: `stage56_integration_map_plan_to_identity_and_token`,
`stage56_rw_plan_to_identity_is_not_read_only`

**(B) mod stage57 — 8 kernel tests (src/kernel/boot/tests.rs):**

Syscall numbering: `stage57_syscall_count_still_31_and_nr_still_30`

Gate: `stage57_map_intent_read_only_gate_still_disabled`,
`stage57_map_intent_read_write_gate_still_disabled`

Write window / no mapping: `stage57_plain_receive_write_window_is_88_no_mapping_fields`,
`stage57_memory_object_transfer_no_mapping_output`,
`stage57_dma_region_transfer_no_mapping_output`

Token sentinel: `stage57_cleanup_token_none_matches_abi_sentinel`,
`stage57_new_registry_has_zero_occupied_slots`

**(C) ABI crate — 5 new tests (crates/yarm-ipc-abi/src/recv_shared_v3_abi.rs):**

`abi_cleanup_identity_none_has_all_new_fields_zero`,
`abi_cleanup_identity_is_mapped_false_in_none`,
`abi_cleanup_identity_is_mapped_requires_both_base_and_len`,
`abi_cleanup_identity_is_active_and_is_mapped_are_independent`,
`abi_cleanup_identity_full_round_trip`

### 66.9 FUTURE / remaining blockers before map_intent live enablement

1. Resolve `map_intent != 0` semantics for non-OPCODE_SHARED_MEM messages.
2. Define output write failure rollback order (mapping established but
   writeback fails).
3. Specify `attenuate_transfer_cap_for_recv_intent` + materialized-cap
   interaction.
4. Extend `write_v3_output_to_user` write window to cover `mapped_base @88`,
   `page_rounded_mapped_len @96`, `actual_mapping_perm @104`, and
   `cleanup_token @112`.
5. Wire `RecvV3CleanupRegistry` into the live dispatch path under a per-CPU
   or per-process lock.
6. Implement process-exit hook to call `purge_active_transfer_mappings_for_pid`
   on every occupied slot for the exiting process.
7. No production service loop uses recv_shared_v3.

## §67 Stage 58+59 — recv_shared_v3 live map_intent audit + DmaRegion RO mapping

### 67.1 Scope

Stage 58+59 enables live `map_intent` mapping for `recv_shared_v3` (NR 30) with
DmaRegion read-only as the primary candidate.  SYSCALL_COUNT=31 is unchanged.
All hard constraints from §65/§66 remain in force.

### 67.2 Audit classification

- **A** (can implement): DmaRegion read-only mapping via
  `map_user_page_in_asid_raw` using `PhysAddr(mo.phys.0 + dma.offset)`.
- **B** (needs phys+offset resolution): Standard ASID lookup for the receiver
  task; phys = MemoryObject base + DmaRegion offset.
- **D** (VA selection): caller-provided via `payload_ptr` in the request struct.
  No kernel VA allocator needed.

### 67.3 Locking invariants

- Physical address resolution for DmaRegion: sequential borrows.
  `capability_service().resolve_current_task_capability(cap_id)` (first borrow,
  released after extracting `(mo_id, dma_offset)`), then
  `with_memory_state(|m| ...)` (second borrow, separate call).  No nested
  lock-holding across both.
- `map_user_page_in_asid_raw` is called with no IPC/cap lock held (both borrows
  are released before the mapping loop begins).
- `register_active_transfer_mapping` is called after all pages are mapped and
  before any user writeback.  Rollback via `unmap_range_two_phase` is called on
  failure before cap rollback.

### 67.4 map_intent gate (metadata_len check)

The old `if req.map_intent != 0 { return Err(InvalidArgs); }` gate is replaced
by a minimum-metadata-length check:

```
if req.map_intent != 0 && req.metadata_len < V3_LIVE_OUTPUT_LEN (= 120) {
    return Err(SyscallError::InvalidArgs);
}
```

Callers must provide at least 120 bytes of metadata buffer when requesting live
mapping; this guarantees `cleanup_token @112` can always be written.

### 67.5 New constants (src/kernel/recv_core.rs, mod recv_shared_v3)

| Constant | Value | Meaning |
|---|---|---|
| `V3_LIVE_OUTPUT_LEN` | 120 | Minimum metadata_len for live mapping |
| `MAP_PERM_READ_ONLY` | 1 | `actual_mapping_perm` for RO mappings |
| `MAP_PERM_READ_WRITE` | 3 | `actual_mapping_perm` for RW mappings |

### 67.6 write_v3_output_to_user extension

`write_v3_output_to_user` now writes up to 120 bytes (previously 88).
Four new trailing parameters carry the live-mapping fields:

```
mapped_base @88..96      (u64, VA of first mapped page)
page_rounded_mapped_len @96..104 (u64)
actual_mapping_perm @104..108    (u32; 1=RO, 3=RW)
cleanup_token @112..120          (u64; cap ID as opaque token; 0 = no mapping)
```

Fields @108..112 are padding (reserved, written as 0).

When `map_intent == 0`, all four trailing fields are 0 and `write_len =
min(out_len, 120)`.  Callers with 88-byte buffers receive unchanged behaviour
because `write_len = min(88, 120) = 88`.

### 67.7 skip_payload flag

When live mapping succeeds, `execute_user_asid_plain_writeback` is **skipped**
(`skip_payload = true`) because `payload_ptr` is the mapping target VA, not a
copy buffer.  Writing the `SharedMemoryRegion` encoding there would corrupt the
mapped page (and fault for RO mappings in production).

The frame's `ret1` (payload_len_copied) is set to 0 in the skip path.

### 67.8 cleanup_token semantics

`cleanup_token = xfer_cap_out` (the receiver-local cap ID as an opaque u64).
This is the key used in `active_transfer_mappings` and matches the token
returned by `register_active_transfer_mapping`.  Userspace passes this back
to a future `release_shared_mapping` syscall.

### 67.9 Proven assertions (mod stage58, mod stage59, src/kernel/boot/tests.rs)

**(A) mod stage58 — 12 kernel tests:**

Gate: `stage58_map_intent_requires_metadata_len_120`,
`stage58_map_intent_read_write_also_requires_metadata_len_120`

Mapping success: `stage58_dma_region_ro_mapping_result_status_ok`,
`stage58_dma_region_ro_mapped_base_equals_payload_ptr`,
`stage58_dma_region_ro_mapped_len_equals_page_size`,
`stage58_dma_region_ro_actual_perm_is_1`

Token: `stage58_dma_region_ro_cleanup_token_nonzero`,
`stage58_dma_region_ro_cleanup_token_equals_transferred_cap`

Registry: `stage58_active_transfer_count_increments_after_mapping`

skip_payload: `stage58_mapping_skips_payload_copy_frame_payload_len_is_zero`

Rejection: `stage58_map_intent_without_cap_message_rejected`,
`stage58_map_intent_with_non_shared_mem_message_rejected`

**(B) mod stage59 — 6 kernel tests:**

Invariants: `stage59_syscall_count_still_31_and_nr_still_30`

Regression: `stage59_plain_receive_write_window_still_88_bytes`,
`stage59_dma_region_transfer_without_map_intent_unchanged`,
`stage59_map_intent_small_buffer_still_invalid_args`,
`stage59_vfs_shared_io_disabled`,
`stage59_legacy_ipc_recv_unaffected_by_mapping`

### 67.10 Test-ordering constraint

All `send`/`ipc_send` calls in stage58/stage59 tests are issued **before**
`setup_receiver`/`setup_recv_asid` (which binds task 0 to a user ASID).  After
ASID binding, `IpcSend` from task 0 takes the user-ASID path and calls
`copy_from_current_user`, which faults on `VA=0`.  The send must use the
kernel-task path (no ASID bound) to avoid silent message-loss.

---

## §68 Stage 60 — recv_shared_v3 cleanup-token hardening + rollback audit

### 68.1 Motivation

Stage 58+59 delivered live DmaRegion read-only mapping through `recv_shared_v3`
(NR 30).  Stage 60 hardens the two residual gaps identified in the audit:

1. **Metadata writeback gap** — `write_v3_output_to_user` ignored the return
   value of `copy_to_current_user`.  If the metadata buffer VA was unmapped, the
   mapping was created and registered but the caller never received the
   `cleanup_token`, making the mapping un-releasable (permanent leak).

2. **RW gate gap** — `map_intent = READ|WRITE (0x3)` was not explicitly rejected;
   RW mapping is not yet supported.

### 68.2 cleanup_token generation safety

`CapId.0 = (generation << 16) | slot_index` (INDEX_BITS = 16, defined in
`crates/yarm-kernel/src/capability.rs`).  The cleanup_token returned to the
caller is `xfer_cap_out = minted.0`, which encodes both slot and generation.

`ActiveTransferMapping { transfer_cap: CapId, … }` stores the full CapId.
`remove_active_transfer_mapping` matches on `(owner_tid, transfer_cap)` equality,
so a stale token with a different generation cannot match a live entry.
`CapabilitySpace::get` validates generation on lookup:
`if slot.generation != id.generation() { return None; }`.

No additional generation field in `ActiveTransferMapping` is needed; CapId
already carries it.

### 68.3 Changes to `write_v3_output_to_user`

Return type changed from `()` to `bool`:

- Returns `false` when `out_ptr == 0` or `out_len < V3_MIN_OUTPUT_LEN`.
- Returns `kernel.copy_to_current_user(…).is_ok()`.

Call sites:

| Call site | Action |
|---|---|
| `WouldBlock` branch | `let _ = write_v3_output_to_user(…);` — no mapping to rollback |
| `skip_payload` (live-mapping) branch | Checks `bool`; on `false` rolls back mapping+registry+cap |
| `RecvUserWritebackOutcome::Ok` branch | `let _ = write_v3_output_to_user(…);` — payload already written |

### 68.4 Rollback in the skip_payload branch

After `write_v3_output_to_user` returns `false`:

1. `kernel.unmap_range_two_phase(rb_asid, mapped_base, mapped_len_out)` — remove
   page table entries.
2. `kernel.remove_active_transfer_mapping(ThreadId(caller_tid), rb_cap)` — clear
   registry entry so no dangling slot exists.
3. `kernel.rollback_materialized_recv_cap(caller_tid, rb_cap, is_reply_cap)` —
   revoke the materialized cap from receiver cnode.
4. Return `Err(SyscallError::InvalidArgs)`.

The `map_rollback: Option<(Asid, CapId)>` tuple is threaded through the mapping
arm so the rollback path does not need a second cap lookup.

### 68.5 RW gate

Added immediately after the `metadata_len` gate:

```rust
if req.map_intent & SYSCALL_RECV_MAP_INTENT_WRITE as u32 != 0 {
    return Err(SyscallError::InvalidArgs);
}
```

This rejects `MAP_READ|MAP_WRITE (0x3)` and `MAP_WRITE (0x2)` before any
mapping code is reached.

### 68.6 Audit classifications

| Classification | Assessment |
|---|---|
| A — can implement in hosted-dev | All Stage 60 changes |
| B — new phys helper needed | None required |
| C — not expressible in hosted-dev | Partial-page-fault rollback (documented only) |
| D — locking | No new lock ordering; all changes within existing single-lock scope |

### 68.7 Stage 60 tests (mod stage60, 10 tests)

**(A) Token generation safety:**
`stage60_cleanup_token_encodes_generation`

**(B) Duplicate release rejected:**
`stage60_duplicate_release_rejected`

**(C) Stale token rejected:**
`stage60_stale_token_release_rejected`

**(D) Writeback rollback:**
`stage60_output_writeback_fail_rolls_back_mapping`

**(E) TransferRelease removes mapping:**
`stage60_transfer_release_removes_active_mapping`

**(F) RW gate:**
`stage60_map_intent_rw_rejected`,
`stage60_map_intent_write_only_rejected`

**(G) Invariants:**
`stage60_syscall_count_still_31`,
`stage60_vfs_shared_io_disabled`,
`stage60_legacy_ipc_recv_unaffected`

---

## §69 Stage 61+62 — recv_shared_v3 read-only mapped receive user-rt integration

### 69.1 Scope

Stage 61 proves that the kernel's read-only mapping path (NR 30 with
`map_intent=1`) is reachable end-to-end from hosted-dev tests. Stage 62
proves the cleanup path via the `TransferRelease` syscall (NR 4).

No new kernel code was added. All changes are:
- `crates/yarm-user-rt` — new public functions (`ipc_recv_shared_v3_mapped_readonly_nonblocking`, `release_v3_cleanup_token`)
- `src/kernel/boot/tests.rs` — `mod stage61_62` (14 dispatch proof tests)

### 69.2 hosted-dev read window

`read_user_memory_for_asid(asid, ptr, len)` reads exactly `len` bytes from the
hashmap-backed user memory. The kernel's `write_v3_output_to_user` writes at
most `RECV_V3_TOKEN_OUTPUT_LEN = 120` bytes. Bytes 120-127 (`request_id`) are
never written. Tests therefore read only 120 bytes; the returned `[u8; 128]`
array has zeros in positions 120-127.

### 69.3 Invariants preserved

- SYSCALL_COUNT = 31 (unchanged)
- NR 30 = RecvSharedV3, NR 4 = TransferRelease (unchanged)
- VFS_SHARED_IO disabled (unchanged)
- No read-write mapping enabled
- No Drop-based cleanup
- Old ipc_recv / recv-v2 / recv-timeout behavior unchanged
- No SpawnV5, VFS, Phase2B/3B, startup slot changes

### 69.4 Stage 61+62 tests (mod stage61_62, 14 tests)

**(A) Kernel dispatch — mapping fields:**
`stage61_kernel_dispatch_map_intent_one_populates_mapped_base`,
`stage61_kernel_dispatch_map_intent_one_populates_mapped_len`,
`stage61_kernel_dispatch_map_intent_one_actual_perm_read_only`,
`stage61_kernel_dispatch_map_intent_one_cleanup_token_nonzero`,
`stage61_kernel_dispatch_map_intent_one_result_status_ok`,
`stage61_kernel_dispatch_map_intent_one_registers_active_mapping`

**(B) ABI struct layout:**
`stage61_v3_output_struct_size_is_128`,
`stage61_v3_output_parses_via_abi_struct`

**(C) Token encoding:**
`stage61_cleanup_token_generation_in_bits_63_16`

**(D) Release removes mapping:**
`stage62_release_via_cleanup_token_removes_active_mapping`,
`stage62_duplicate_release_rejected_via_v3_path`

**(E) Invariants:**
`stage61_syscall_count_still_31`,
`stage61_vfs_shared_io_disabled`,
`stage61_legacy_ipc_recv_unaffected`

---

## §70 Stage 63+64 — plain-receive adoption proof and VFS readiness audit

### 70.1 Scope

Stage 63 proves the recv_shared_v3 plain-receive kernel dispatch path (map_intent=0)
end-to-end in hosted-dev. Stage 64 audits VFS shared-IO readiness against the
recv_shared_v3 primitives now available.

No new kernel code was added. All changes are:
- `src/kernel/boot/tests.rs` — `mod stage63` (8 proof tests)
- `crates/yarm-user-rt/src/syscall/recv_v3.rs` — 4 stage63 adoption tests
- `doc/VFS.md` — Stage 64 readiness audit (see VFS shared-I/O section)

### 70.2 Plain-receive output field invariants

When recv_shared_v3 is dispatched with map_intent=0 (no mapping):
- `result_status = RECV_V3_STATUS_OK`
- `transferred_cap` is materialised (DmaRegion object_kind=5)
- `mapped_base = 0`, `page_rounded_mapped_len = 0`, `actual_mapping_perm = 0`
- `cleanup_token = RECV_V3_CLEANUP_TOKEN_NONE (0)`
- No active transfer mapping is registered (`active_transfer_count == 0`)

These are complementary to stage61_62 invariants (map_intent=1 path).

### 70.3 VFS readiness conclusion (Stage 64)

| Capability | Status |
|---|---|
| MAP_READ plain receive | Ready |
| MAP_READ mapped receive + cleanup | Ready (Stage 61+62) |
| MAP_WRITE | Blocked — RW gate |
| WRITE_SHARED_REQUEST | Helper-only — descriptor↔token binding missing |
| READ_SHARED_REPLY | Blocked — requires MAP_WRITE which is gated |
| Process-exit/timeout/cancel signals | Blocked |

VFS_SHARED_IO_ENABLED remains disabled. Full rationale in doc/VFS.md (Stage 64 shared-I/O readiness).

### 70.4 Invariants preserved

- SYSCALL_COUNT = 31, NR 30 = RecvSharedV3, NR 4 = TransferRelease
- VFS_SHARED_IO disabled
- No MAP_WRITE enabled
- No production service loops changed
- No Drop-based cleanup added

---

## §71 Stage 65 — VfsWriteSharedBinding contract and WRITE_SHARED_REQUEST helper bridge

### 71.1 Scope

Stage 65 defines and proves the binding contract between a `recv_shared_v3` MAP_READ delivery
and a VFS `WRITE_SHARED_REQUEST` descriptor. No kernel code changed. No syscall numbers changed.
No production service paths changed.

All changes are in `crates/yarm-fs-servers/src/fs/common/shared_io_adapter.rs` (helper-only
`VfsWriteSharedBinding` type + 21 stage65 tests).

### 71.2 Binding contract (cross-reference invariants)

When `recv_shared_v3` returns a MAP_READ delivery, the kernel populates `cleanup_token` as a
full `u64` CapId: `(generation << 16) | slot_index`. The VFS requestor must encode:

```
descriptor.object_handle     = cleanup_token          // full u64 CapId
descriptor.object_generation = cleanup_token >> 16    // generation only
```

`VfsWriteSharedBinding::validate()` checks both fields. A single-field match is insufficient:
handle can collide across generations; generation alone does not uniquely identify a slot.

### 71.3 Mapping permission invariant

`actual_mapping_perm` from `recv_shared_v3` must equal `MAP_PERM_READ_ONLY = 1` for a
WRITE_SHARED_REQUEST binding. The validate() function rejects any non-read-only permission.
`BorrowedSharedIoTestMapper` enforces direction safety: `with_write_request_buffer` returns an
immutable `&[u8]`; requesting mutable access via `with_read_reply_buffer` returns
`WrongAccessDirection`.

### 71.4 Invariants preserved

- SYSCALL_COUNT = 31 (no change)
- NR 30 = RecvSharedV3 (no change)
- NR 4 = TransferRelease (no change)
- All existing recv_shared_v3 ABI field offsets unchanged
- MAP_WRITE not enabled (Stage 60 RW gate intact)
- READ_SHARED_REPLY still blocked
- VFS_SHARED_IO_ENABLED still disabled
- No Drop-based cleanup added
- No live VFS dispatch path changed
- No FAT/ext4/blkcache production write behavior changed

---

## §72 Stage 66+67+68 — Gated WRITE_SHARED_REQUEST live route in VfsService

### 72.1 Scope

Stages 66+67+68 add a gated live route `VfsService::dispatch_write_shared_request` and split
shared-I/O feature flags into independently gated constants. No kernel code changed.

Changes:
- `crates/yarm-srv-common/src/vfs_core.rs` — `write_shared_bytes` default method on `VfsBackend`
- `crates/yarm-fs-servers/src/fs/ramfs/tree.rs` — `RamFsBackend` override of `write_shared_bytes`
- `crates/yarm-fs-servers/src/fs/common/shared_io_adapter.rs` — 3 feature flag constants
- `crates/yarm-fs-servers/src/fs/common/vfs_service.rs` — `dispatch_write_shared_request` + 17 tests

### 72.2 Feature gate model

```
VFS_WRITE_SHARED_REQUEST_ENABLED = false  // only WRITE direction gate
VFS_READ_SHARED_REPLY_ENABLED    = false  // only READ direction gate (blocked by MAP_WRITE)
VFS_SHARED_IO_ENABLED            = false  // = WRITE && READ → aggregate; always false currently
```

### 72.3 dispatch_write_shared_request invariants

- `handle_request` still returns `Unsupported` for `VFS_OP_WRITE_SHARED_REQUEST` (unchanged).
- `dispatch_write_shared_request` validates via `VfsWriteSharedBinding` before backend access.
- `mapper.release(descriptor)` is called unconditionally after the access attempt.
- `op_sequence` advances only on success.
- `VFS_READ_SHARED_REPLY_ENABLED = false` and `VFS_SHARED_IO_ENABLED = false` remain.

### 72.4 Production invariants preserved

- SYSCALL_COUNT = 31 (no change)
- recv_shared_v3 ABI field offsets unchanged
- MAP_WRITE not enabled
- READ_SHARED_REPLY still blocked
- VFS_SHARED_IO_ENABLED = false
- No Drop-based cleanup
- FAT/ext4/blkcache production write behavior unchanged

---

## §73 Stage 69+70 — MAP_WRITE audit + READ_SHARED_REPLY helper/gated path

### 73.1 Scope

No kernel code changed. This section records the audit findings for MAP_WRITE and the
implementation of the helper-only READ_SHARED_REPLY path in userspace.

### 73.2 MAP_WRITE audit findings

| Gap | Status |
|---|---|
| Stage 60 gate location | `syscall.rs` ~4266 — single line, intact |
| MAP_PERM_READ_WRITE = 3 | Defined in `recv_core.rs`; unreachable via live delivery |
| Writeback rollback | Present: unmap → remove registry → revoke cap (ordered) |
| TransferRelease | Present: two-phase unmap + cap revocation |
| Process-exit cleanup | **Not confirmed** — critical safety blocker |
| NX enforcement | Hardcoded `execute: false`; unconditional |
| Rights check | `cap_rights & CAP_RIGHT_WRITE` in planning; prevents escalation |

**Verdict:** Stage 60 MAP_WRITE gate remains intact. The process-exit cleanup gap means a
writable shared mapping could outlive a dead process. Gate removal requires:
- Kernel sends `VfsSharedIoTerminalReason::RequesterExit` signal to VFS server on process exit.
- VFS server calls `dispatch_read_shared_reply` / `mapper.release` on receiving the signal.

### 73.3 New userspace symbols (no kernel change)

- `VfsReadSharedBinding` (12 constraints, symmetric to `VfsWriteSharedBinding`)
- `VfsReadSharedBindingError` (12 variants)
- `VfsService::dispatch_read_shared_reply<M: VfsSharedIoMapper>` — helper-only, gated
- `VfsBackend::read_shared_bytes` — new default method (returns `Unsupported`)
- `RamFsBackend::read_shared_bytes` — overrides to delegate to `read_bytes` + metrics

### 73.4 Invariants preserved

- SYSCALL_COUNT = 31 (no change)
- NR 30 = RecvSharedV3 (no change)
- NR 4 = TransferRelease (no change)
- All recv_shared_v3 ABI field offsets unchanged
- MAP_WRITE not enabled — Stage 60 gate intact
- `VFS_READ_SHARED_REPLY_ENABLED = false`
- `VFS_SHARED_IO_ENABLED = false`
- `handle_request` still rejects `VFS_OP_READ_SHARED_REPLY` with `Unsupported`
- `handle_request` still rejects `VFS_OP_WRITE_SHARED_REQUEST` with `Unsupported`
- No Drop-based cleanup added
- FAT/ext4/blkcache production read/write behavior unchanged
- No SpawnV5/Phase2B/Phase3B/startup slot changes

---

## §74 Stage 71 — active recv_shared_v3 mapping cleanup on task/process exit + timeout/cancel

### 74.1 Scope

Stage 71 audits and proves that every active MAP_READ mapping created by `recv_shared_v3`
is cleaned up regardless of how the receiver's lifetime ends.  No new production kernel code
was added; only tests and documentation.

### 74.2 Cleanup paths confirmed

| Path | Code location | Status |
|---|---|---|
| Explicit `TransferRelease` | `syscall.rs` `handle_transfer_release` | Confirmed — removes registry entry, unmaps, revokes cap |
| Writeback rollback | `syscall.rs` ~4559-4604 | Confirmed — removes registry entry, unmaps, revokes cap |
| Task/process exit | `restart_state.rs` `mark_task_dead` → `cnode_state.rs` `maybe_cleanup_process_cnode_for_pid` → `purge_active_transfer_mappings_for_pid` | **Confirmed** (Stage 71 audit) |
| Cap revocation | `cnode_state.rs` `revoke_active_transfer_mappings_for_cap` | Confirmed |
| Timeout (`timeout_ticks != 0`) | `syscall.rs` ~4252-4254 | Confirmed — `WouldBlock` returned before endpoint/mapping work; no mapping created |
| Cancel / `RECV_V3_MAP_WRITE` gate | `syscall.rs` ~4266-4269 | Confirmed — `InvalidArgs` returned before mapping; no entry created |

### 74.3 Call chain: process-exit cleanup

```
mark_task_dead(pid)
  └─ maybe_cleanup_process_cnode_for_pid(pid)          [cnode_state.rs:67]
       ├─ destroy ASIDs                                 [cnode_state.rs:108-114]
       ├─ purge_transfer_envelopes_for_pid(pid)         [cnode_state.rs:115]
       └─ purge_active_transfer_mappings_for_pid(pid)  [cnode_state.rs:116]
            ├─ for each slot: find entries where owner_pid == pid
            ├─ unmap_range_two_phase(asid, base, len)  tolerates absent pages/ASIDs
            ├─ active_transfer_mappings[idx] = None
            ├─ note_shared_mem_released(len)
            ├─ revoke_capability_in_cnode(cnode, transfer_cap)
            └─ note_transfer_record_revoked()
```

### 74.4 Lock ordering

`purge_active_transfer_mappings_for_pid` holds ipc_state (rank 3) only transiently via
`with_ipc_state` / `with_ipc_state_mut` closures; the lock is released before any
rank-4 (capability_state) acquisition in `revoke_capability_in_cnode`.  No nested
rank-3 + rank-4 hold occurs.  Lock ordering is not violated.

### 74.5 Idempotency

Calling `purge_active_transfer_mappings_for_pid` twice for the same pid is safe:
the second call finds no entries and is a no-op.  `unmap_range_two_phase` also
tolerates already-unmapped pages (silently skips them).

### 74.6 Invariants preserved

- SYSCALL_COUNT = 31 (no change)
- NR 30 = `RecvSharedV3` (no change)
- NR 4 = `TransferRelease` (no change)
- All `recv_shared_v3` ABI field offsets unchanged
- MAP_WRITE not enabled — Stage 60 gate intact (removed by Stage 72)
- `VFS_SHARED_IO_ENABLED = false`
- No Drop-based cleanup added
- No SpawnV5/Phase2B/Phase3B/startup slot changes

---

## §75 Stage 72 — narrow recv_shared_v3 MAP_WRITE enablement

### 75.1 Change summary

Stage 72 removes the Stage 60 blanket MAP_WRITE gate from `syscall.rs` (lines 4266-4269
of the pre-Stage-72 file).  MAP_READ|MAP_WRITE (`map_intent = 0x3`) is now a valid
request for the READ_SHARED_REPLY profile.

**Single code change:**
```rust
// Before (Stage 60 gate — removed):
if req.map_intent & SYSCALL_RECV_MAP_INTENT_WRITE as u32 != 0 {
    return Err(SyscallError::InvalidArgs);
}

// After (Stage 72 comment — no gate):
// Stage 72: MAP_READ|MAP_WRITE (0x3) is permitted for the READ_SHARED_REPLY profile.
// Rights enforcement: compute_recv_v3_mapping_plan checks CAP_RIGHT_MAP + CAP_RIGHT_WRITE;
// InsufficientRights → rollback + InvalidArgs below.
// WRITE-only (0x2) is already rejected: validate_v3_request requires READ bit.
```

**Additional change in `recv_core.rs`:** `validate_v3_request` now explicitly rejects
WRITE-only (`map_intent = 0x2`): WRITE without READ is not a valid mapping mode.

### 75.2 Rights enforcement

All rights enforcement pre-existed in `compute_recv_v3_mapping_plan` (`recv_core.rs`
lines 1363-1397):

| Cap rights | map_intent | Result |
|---|---|---|
| MAP + READ + WRITE | 0x3 | `Map { read_only: false }` → writable mapping |
| MAP + READ (no WRITE) | 0x3 | `InsufficientRights` → rollback → `InvalidArgs` |
| MAP + READ + WRITE | 0x1 | `Map { read_only: true }` → read-only mapping |
| any | 0x2 | `BadMapIntent` from `validate_v3_request` → `InvalidArgs` |

### 75.3 Page mapping

`syscall.rs` line 4465 (unchanged): `write: !read_only`.  `execute: false` is hardcoded
for all recv_shared_v3 mappings.

### 75.4 Cleanup and rollback (identical to MAP_READ)

`ActiveTransferMapping` carries `owner_tid, transfer_cap, base, len` — no permission
field.  `purge_active_transfer_mappings_for_pid` cleans both RO and RW mappings via the
same code path.  The rollback path (writeback failure, InsufficientRights) unmaps pages
and removes the registry entry regardless of permission.

### 75.5 Lock ordering

No change to lock ordering.  The MAP_WRITE path acquires the same locks in the same
order as MAP_READ (rank 4 capability_state → rank 3 ipc_state → rank 2 task_domain →
rank 1 memory_state).

### 75.6 Invariants preserved

- SYSCALL_COUNT = 31 (no change)
- NR 30 = `RecvSharedV3` (no change)
- NR 4 = `TransferRelease` (no change)
- All `recv_shared_v3` ABI field offsets unchanged
- MAP_WRITE now enabled for caps with write rights
- `VFS_SHARED_IO_ENABLED = false` (production VFS route still gated)
- No Drop-based cleanup added
- No SpawnV5/Phase2B/Phase3B/startup slot changes
- WRITE-only (0x2) remains invalid — rejected by `validate_v3_request`

### 75.7 Remaining work

`VfsSharedIoTerminalReason::RequesterExit` signal delivery: when a process holding an
active MAP_WRITE receive exits, the kernel cleans the mapping via
`purge_active_transfer_mappings_for_pid` but does not yet notify the VFS server.  Until
this notification path is implemented, `VFS_READ_SHARED_REPLY_ENABLED` remains `false`.

---

## §76 Stage 73+74 — RequesterExit helper model + VFS_READ_SHARED_REPLY_ENABLED

### 76.1 Change summary

Stage 73 adds the VFS-side entry point for requester-exit cleanup and enables the
`VFS_READ_SHARED_REPLY_ENABLED` production gate.  No kernel changes are made.

**New VFS method (`shared_io_lifecycle.rs`):**
```rust
pub fn deliver_requester_exit<const N: usize>(
    &mut self,
    handles: &mut VfsSharedIoHandleTable<N>,
) -> Result<VfsSharedIoCleanupResult, VfsSharedIoLifecycleError> {
    self.cleanup(handles, VfsSharedIoTerminalReason::RequesterExit)
}
```
This is a thin wrapper over the existing `cleanup` path.  It models the VFS-side handler
that will eventually be called when the supervisor delivers `SUPERVISOR_OP_TASK_EXITED`.

**Flag change (`shared_io_adapter.rs`):**
```rust
pub const VFS_READ_SHARED_REPLY_ENABLED: bool = true;  // Stage 73
```

**VfsService dispatch tests (`vfs_service.rs`, `mod stage73_74_tests`, 9 tests):**
confirm that `dispatch_read_shared_reply` correctly handles RW-perm buffers end-to-end,
rejects RO-perm buffers with `PermissionDenied`, delivers short-EOF, and calls cleanup
exactly once.

### 76.2 Signal delivery classification

The RequesterExit delivery model is classified **helper-only** (class C + D):

- **Class C**: `deliver_requester_exit` is implemented and lifecycle invariants are proven
  by 7 unit tests (`mod stage73` in `shared_io_lifecycle.rs`).
- **Class D**: No live kernel→VFS notification path exists.  `mark_task_dead` in
  `src/kernel/task.rs` calls `purge_active_transfer_mappings_for_pid` (kernel mapping
  cleanup) but performs no userspace notification.  The supervisor
  `SUPERVISOR_OP_TASK_EXITED` message path is not yet wired to call
  `deliver_requester_exit`.

Until Class D is resolved, `VFS_SHARED_IO_ENABLED` remains `false`.

### 76.3 Lifecycle invariants proven (7 tests)

| Test | Invariant |
|---|---|
| `stage73_requester_exit_before_completion_wins` | RequesterExit during Active state returns `Cleaned` |
| `stage73_duplicate_requester_exit_is_idempotent` | Double RequesterExit returns `AlreadyCleaned` |
| `stage73_success_cleanup_beats_requester_exit` | Success cleanup blocks subsequent RequesterExit |
| `stage73_backend_error_beats_requester_exit` | BackendError cleanup blocks subsequent RequesterExit |
| `stage73_requester_exit_blocks_inline_fallback` | RequesterExit prevents inline fallback transition |
| `stage73_requester_exit_from_reserved_state` | RequesterExit from Reserved state is safe no-op |
| `stage73_handle_generation_advances_after_requester_exit` | Generation counter increments after exit |

### 76.4 Lock ordering

No change to lock ordering.  `deliver_requester_exit` operates entirely within
`VfsSharedIoLifecycle` state (no kernel locks held; VFS-side only).

### 76.5 Invariants preserved

- SYSCALL_COUNT = 31 (no change)
- NR 30 = `RecvSharedV3` (no change)
- NR 4 = `TransferRelease` (no change)
- All `recv_shared_v3` ABI field offsets unchanged
- `VFS_READ_SHARED_REPLY_ENABLED = true` (Stage 73 — dispatch path enabled)
- `VFS_SHARED_IO_ENABLED = false` (production umbrella still gated)
- `VFS_WRITE_SHARED_REQUEST_ENABLED = false` (WRITE direction still disabled)
- No Drop-based cleanup added
- No SpawnV5/Phase2B/Phase3B/startup slot changes

### 76.6 Remaining work

Live RequesterExit delivery via PM (Stage 77+78 RESOLVED): PM notification endpoint wired;
`VFS_SHARED_IO_ENABLED = true` enabled at Stage 78 after full gate matrix audit.
Supervisor path (`VFS_SUPERVISOR_TASK_EXIT_NOTIFICATION_ENABLED`) remains disabled — PM-owned
model is the production path.

---

## §77 Stage 75 — TID-matched RequesterExit identity model

### 77.1 Change summary

Stage 75 adds the VFS-side identity field and TID-matched dispatch helper that enable
future live wiring of `SUPERVISOR_OP_TASK_EXITED` → `deliver_requester_exit`.

No kernel changes are made.  No new caps or startup slots are added.

**New field in `VfsSharedIoLifecycle`:**
```rust
requester_tid: u64  // TID of the requesting task; correlates to TaskExitedEvent.tid
```

**New method:**
```rust
pub fn deliver_requester_exit_if_tid_matches<const N: usize>(
    &mut self,
    tid: u64,
    handles: &mut VfsSharedIoHandleTable<N>,
) -> Result<VfsSharedIoRequesterExitAction, VfsSharedIoLifecycleError>
```
Returns `NotMatched` (safe no-op) when `tid != self.requester_tid`.
Returns `Matched(result)` when TID matches — identical to `deliver_requester_exit`.

**New constant:**
```rust
pub const VFS_SUPERVISOR_TASK_EXIT_NOTIFICATION_ENABLED: bool = false;
```
Documents and machine-checks that the notification channel is absent.

### 77.2 Signal flow (current state)

```
Kernel: exit_task(tid) → report_task_exit_to_supervisor(tid, code, token)
  → Message(0xEE, TaskExitedEvent{tid, exit_code, restart_token})
  → supervisor_fault_recv_cap endpoint

Supervisor: service_step() → handle_task_exit()
  → ScheduledRestart / MarkedDead / Ignored
  [NO forwarding to VFS]

VFS: no notification endpoint, no lifecycle store
```

### 77.3 Missing production infrastructure

Two blocking pieces before `VFS_SUPERVISOR_TASK_EXIT_NOTIFICATION_ENABLED = true`:

**A. Supervisor→VFS notification cap**
`InitFaultHandoff` needs `vfs_task_exit_send_cap: Option<CapId>`.  The supervisor's
`handle_task_exit` must send `SUPERVISOR_OP_TASK_EXITED(tid)` to that cap when a
non-supervisor-managed task exits with an active shared-I/O lifecycle.

**B. VFS-side lifecycle store**
`VfsService` needs a bounded `[Option<VfsSharedIoLifecycle>; N]` keyed by `requester_tid`.
On `SUPERVISOR_OP_TASK_EXITED(tid)` the service scans the store and calls
`deliver_requester_exit_if_tid_matches(tid, handles)` on each entry.

### 77.4 Lock ordering

No change to lock ordering.  `deliver_requester_exit_if_tid_matches` is entirely
within VFS-space (no kernel locks held).

### 77.5 Invariants preserved

- SYSCALL_COUNT = 31 (no change)
- NR 30 = `RecvSharedV3` (no change)
- NR 4 = `TransferRelease` (no change)
- `VFS_READ_SHARED_REPLY_ENABLED = true` (unchanged from Stage 73)
- `VFS_SHARED_IO_ENABLED = false` (unchanged)
- `VFS_SUPERVISOR_TASK_EXIT_NOTIFICATION_ENABLED = false` (Stage 75 documents gap)
- No Drop-based cleanup added
- No startup slots changed
- 295 yarm-fs-servers tests pass (up from 277 after Stage 73+74)

### 77.6 Remaining work

Wire `SUPERVISOR_OP_TASK_EXITED` → VFS by adding the two missing pieces (§77.3).
After both are in place, enable `VFS_SUPERVISOR_TASK_EXIT_NOTIFICATION_ENABLED = true`

---

## §78 Stage 76+77+78 — PM-owned TaskExited/ProcessExited notification ABI + full wiring

### 78.1 Architectural decision

PM should own lifecycle notifications; supervisor should own fault/restart policy only.
Stage 76 defined the ABI. Stage 77+78 resolved both blockers and enabled the gate.

### 78.2 ABI contracts (all stages)

```rust
// crates/yarm-ipc-abi/src/process_abi.rs

// Stage 77+78: Kernel → PM push
pub const KERNEL_OP_PM_TASK_EXITED: u16 = 0xDC;

pub struct KernelPmTaskExitedPayload { pub tid: u64, pub exit_code: u64 }
// Wire: [0..8] tid LE, [8..16] exit_code LE (16 bytes)

// Stage 76: PM → VFS push
pub const PROC_OP_TASK_EXITED:   u16 = 13;
pub const PROC_OP_PROCESS_EXITED: u16 = 14;

pub struct PmTaskExitedEvent { pub tid: u64, pub exit_code: u64 }
pub struct PmProcessExitedEvent { pub process_tid: u64, pub exit_code: u64 }
// Both: [0..8] tid LE, [8..16] exit_code LE (16 bytes each)
```

Kernel-side additions (Stage 77+78):
```rust
// src/kernel/boot/defs.rs — FaultSubsystem
pub(crate) pm_task_exit_endpoint: Option<usize>,

// src/kernel/boot/fault_endpoint_state.rs
pub fn set_pm_task_exit_endpoint_for_task(tid: u64, recv_cap: CapId) -> Result<(), KernelError>

// src/kernel/boot/restart_state.rs
pub fn report_task_exit_to_pm(tid: u64, code: u64) -> Result<(), KernelError>
// Called from exit_task() after report_task_exit_to_supervisor()
```

VFS entry points:
```rust
// crates/yarm-fs-servers/src/fs/common/shared_io_adapter.rs
pub fn handle_pm_task_exited<const N: usize>(
    tid: u64, lifecycle: &mut VfsSharedIoLifecycle, handles: &mut VfsSharedIoHandleTable<N>,
) -> Result<VfsSharedIoRequesterExitAction, VfsSharedIoLifecycleError>

pub fn dispatch_pm_task_exited_push<const N: usize>(
    opcode: u16, payload: &[u8],
    lifecycle: &mut VfsSharedIoLifecycle, handles: &mut VfsSharedIoHandleTable<N>,
) -> Result<VfsSharedIoRequesterExitAction, VfsPmPushDispatchError>

pub fn decode_kernel_pm_task_exited(opcode: u16, payload: &[u8]) -> Result<(u64, u64), VfsPmPushDispatchError>
```

### 78.3 Blockers resolved (Stage 77+78)

**Blocker A — PM→VFS send cap: RESOLVED**
PM already has `vfs_send_cap` via `lifecycle_table.get_by_image_id(6).pm_service_send_cap`
(image_id=6 = VFS). No new startup slot needed.

**Blocker B — Kernel→PM task-exit delivery: RESOLVED (Stage 77+78)**
`FaultSubsystem::pm_task_exit_endpoint: Option<usize>` added.
`exit_task()` calls `report_task_exit_to_pm(tid, code)` after `report_task_exit_to_supervisor()`.
Kernel sends `KERNEL_OP_PM_TASK_EXITED = 0xDC` with 16-byte LE payload to PM's endpoint.
Tests prove end-to-end delivery.

Gate constant:
```rust
pub const VFS_PM_TASK_EXIT_NOTIFICATION_ENABLED: bool = true;  // Stage 77+78: enabled
```

### 78.4 Signal flow (implemented)

```
exit_task(tid, code)
  → report_task_exit_to_supervisor(tid, code, token)    [kernel]
  → report_task_exit_to_pm(tid, code)                   [kernel, Stage 77+78]
    → KERNEL_OP_PM_TASK_EXITED delivered to pm_task_exit_endpoint
    → PM: decode_kernel_pm_task_exited(opcode, payload) → (tid, code)
    → PM: encode PmTaskExitedEvent, send PROC_OP_TASK_EXITED to VFS
      → VFS: dispatch_pm_task_exited_push(opcode, payload, lifecycle, handles)
        → handle_pm_task_exited(tid, lifecycle, handles)
          → deliver_requester_exit_if_tid_matches(tid, handles)
```

### 78.5 Lock ordering

No change to lock ordering. `handle_pm_task_exited` and `dispatch_pm_task_exited_push`
are entirely within VFS-space (no kernel locks held).
`report_task_exit_to_pm` holds `fault_state_lock` to read `pm_task_exit_endpoint`,
then calls `send_message_to_endpoint_and_wake` under `ipc_state_lock` — same ordering
as `report_task_exit_to_supervisor`. No new lock ordering introduced.

### 78.6 Invariants preserved

- SYSCALL_COUNT = 31 (no change)
- NR 30 = `RecvSharedV3` (no change)
- `PROC_OP_TASK_EXITED = 13` (unchanged from Stage 76)
- `PROC_OP_PROCESS_EXITED = 14` (unchanged from Stage 76)
- `KERNEL_OP_PM_TASK_EXITED = 0xDC` (new, does not collide with `SUPERVISOR_OP_TASK_EXITED = 0xEE`)
- `VFS_WRITE_SHARED_REQUEST_ENABLED = true` (Stage 78: all prerequisites met)
- `VFS_READ_SHARED_REPLY_ENABLED = true` (unchanged from Stage 73)
- `VFS_SHARED_IO_ENABLED = true` (Stage 78: WRITE && READ && PM all proven)
- `VFS_SUPERVISOR_TASK_EXIT_NOTIFICATION_ENABLED = false` (unchanged from Stage 75)
- `VFS_PM_TASK_EXIT_NOTIFICATION_ENABLED = true` (Stage 77+78: enabled)
- `handle_request` still rejects shared opcodes — `UnsupportedSharedIoMapper` is production default
- No startup slots added or changed; STARTUP_SLOT_COUNT = 18 (unchanged)
- 340 yarm-fs-servers tests pass (325 Stage 77+78 + 15 Stage 78)
- 15 kernel-side Stage 77+78 tests in `mod stage77` in `tests.rs`

## Stage 83: `RecvV3SharedIoMapper` RAMFS byte-access proof

No kernel-side locking changes.  All work is in `yarm-fs-servers`.

### 83.1 Mapper status

`RecvV3SharedIoMapper` (Stage 79) is the production `VfsSharedIoMapper`.  It holds:
- `cleanup_token: u64` — from recv_shared_v3 delivery
- `mapped_base: u64` — kernel VA (or heap buffer in tests) of the shared region
- `page_rounded_mapped_len: u64` — mapping length
- `actual_mapping_perm: u32` — `1` = RO, `3` = RW
- `released: bool` — at-most-once release guard

`release` sets the flag before calling `release_v3_cleanup_token` (NR 4); the syscall
failing in hosted-dev does not prevent `is_released()` from returning `true`.

### 83.2 Unsafe boundary

`with_write_request_buffer` calls `core::slice::from_raw_parts(ptr, len)`.
`with_read_reply_buffer` calls `core::slice::from_raw_parts_mut(ptr as *mut u8, len)`.
Preconditions checked before pointer arithmetic:
- `released == false`
- descriptor direction and handle/generation match cleanup_token
- `actual_mapping_perm` has the required bit(s)
- `buffer_offset + requested_len <= page_rounded_mapped_len`

Stage 83 tests use a real heap `Vec<u8>` as `mapped_base`; the slice creation is defined.

### 83.3 Production routing status

`handle_request` still rejects `VFS_OP_WRITE_SHARED_REQUEST` and `VFS_OP_READ_SHARED_REPLY`
with `VfsError::Unsupported`.  `UnsupportedSharedIoMapper` remains the production default.
FAT, ext4, and blkcache are unaffected.

### 83.4 Invariants preserved

- SYSCALL_COUNT = 31 (no change)
- `VFS_WRITE_SHARED_REQUEST_ENABLED = true` (unchanged)
- `VFS_READ_SHARED_REPLY_ENABLED = true` (unchanged)
- `VFS_SHARED_IO_ENABLED = true` (unchanged)
- `VFS_SUPERVISOR_TASK_EXIT_NOTIFICATION_ENABLED = false` (unchanged)
- `handle_request` still rejects shared opcodes
- No startup slots changed; STARTUP_SLOT_COUNT = 18
- 392 yarm-fs-servers tests pass (368 prior + 24 Stage 83)

## 84. Stage 84: RAMFS-only shared-I/O service-loop bridge

### 84.1 Locking model

`VfsService` gains two new fields:
- `shared_io_handles: VfsSharedIoHandleTable<4>` — internal handle allocator; no kernel lock.
- `shared_io_requests: [Option<VfsSharedIoLifecycle>; 4]` — per-request lifecycle slots.

Both fields are owned by `VfsService` and accessed only from the VFS service loop thread.
No new kernel locks are introduced.  Lifecycle operations (`reserve/map/begin/complete/cleanup`)
and handle-table operations (`allocate/validate/release`) are single-threaded with no interior
mutability.

`deliver_requester_exit_all` is called from the PM task-exit notification path (same thread);
it takes each lifecycle by value, calls `deliver_requester_exit_if_tid_matches`, and
restores unmatched lifecycles in place.  No concurrent access is possible.

### 84.2 Lifecycle slot ownership

Each slot in `shared_io_requests` is either `None` (free) or `Some(VfsSharedIoLifecycle)`.
The gated methods take ownership via `.take()` before any mutable operation to avoid
split borrows against `shared_io_handles`.  On success and on all error paths, the slot is
left as `None` (lifecycle consumed via `cleanup`).

### 84.3 Invariants preserved

- SYSCALL_COUNT = 31 (no change)
- STARTUP_SLOT_COUNT = 18 (no change)
- `handle_request` still rejects shared opcodes
- `VFS_SUPERVISOR_TASK_EXIT_NOTIFICATION_ENABLED = false` (unchanged)
- 412 yarm-fs-servers tests pass (392 prior + 20 Stage 84)

## Stage 85 — dispatch_shared_delivery locking model

### 85.1 Threading and ownership

`dispatch_shared_delivery` is a `&mut self` method on `VfsService<B>`.  It delegates to
`handle_write_shared_request_gated` or `handle_read_shared_reply_gated` (Stage 84 gated methods),
inheriting their ownership model:

- The `Message` argument is consumed by value (decoded before the dispatch call).
- The `RecvSharedV3Delivery` is borrowed immutably; no mutable delivery state is held.
- All lifecycle slot operations continue to be single-threaded under `&mut self`.
- `delivery.mapped_base == 0` is rejected before reaching the mapper (null-pointer guard).

No new kernel locks, interior mutability, or concurrent access paths are introduced.

### 85.2 Invariants preserved

- SYSCALL_COUNT = 31 (no change)
- STARTUP_SLOT_COUNT = 18 (no change)
- `handle_request` still rejects shared opcodes
- `VFS_SUPERVISOR_TASK_EXIT_NOTIFICATION_ENABLED = false` (unchanged)
- 436 yarm-fs-servers tests pass (412 prior + 24 Stage 85)

---

# Stage 106 / Kernel Unlocking Pass 3 addendum

Live split paths as of Stage 106 (full record:
`doc/KERNEL_UNLOCKING.md` §13/§14/§18 and
`doc/KERNEL_UNLOCKING.md`):

- **D1 (Stage 104):** recv-side transfer-cap materialization routed through
  `cap_transfer_split` Phase A (ipc 3 → cap 4 read) / Phase B (cap 4 mutate).
- **D5 (Stage 105):** reply-cap materialization Phase A/B/B' with fallible
  `try_set_reply_cap_waiter_cap` (ipc 3) + mint rollback on stale.
- **D2 (Stage 106):** endpoint blocking-recv waiter publish via
  `publish_recv_waiter_live` — atomic queue-recheck + publish in one ipc(3)
  critical section; phase order block(1) → TCB(2) → publish(3) → dispatch;
  `QueueNonEmpty` drives the no-lost-wakeup unwind.

Gates still in force: D3 structural shootdown-before-reclaim order is
UAF-load-bearing (`execute_tlb_shootdown_wait_plan`); D6 per-CPU scheduler
locking not started — `entering_tid`/`exiting_tid` remain Class F; x86_64
smoke pinned `-smp 1`.

Milestone 1 status: DECLARED (2026-06-12; all three smoke runs passed — see
the milestone doc acceptance record).

---

# Stage 108 / Milestone 2 Pass 1 addendum

Per-domain split-mut seams now exist for scheduler (rank 1), task/TCB
(rank 2), VM/user-spaces (rank 5), and memory/frames (rank 6) in
`runtime.rs` (M2_SEAM_HELPER_ONLY — no live callers yet; equivalence-tested).
Together with the pre-existing fault/telemetry seams this completes the seam
set the D2/D3/D6 lock-window conversions need. The x86_64 AP trampoline is
split into `arch/x86_64/smp_trampoline.rs` (mechanical, zero behavior
change); the AP still parks in assembly — the per-CPU AP environment is the
remaining SMP blocker (`doc/KERNEL_UNLOCKING.md` §3).
