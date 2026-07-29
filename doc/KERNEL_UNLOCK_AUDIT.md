<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Umut Deniz Balkan -->

# YARM Kernel-Unlock Audit

**Evidence-based audit of the broad `SpinLock<KernelState>` ("global lock") and the
canonical roadmap to full kernel unlocking.**

This document is generated from the source tree, not from prior stage reports. Where a
prior report and the source disagree, the source wins and the disagreement is recorded.

**The canonical stage definitions for 199C–205D are owner-supplied and authoritative.**
They live in `doc/KERNEL_UNLOCKING.md` §0 and are reproduced in status form in §3 here.
Historical stage numbering (`199A2D2C2B3`, `200D-2B1D5B`, and the historical `200A`/`200B`/
`200C`) is **not** a progress measure and is retained only as commit-evidence. **A
historical stage carrying the same number does not complete the canonical stage unless it
satisfies the canonical definition.**

> **Correction notice.** The first revision of this document (Pass 6, commit `f8a3c04`)
> invented a stage ladder by grouping recent branch history under the 199C–205D numbers.
> That was wrong in a specific and consequential way: it mapped the historical
> reply-timeout stages onto canonical `200A`/`200B`/`200C` and reported them **COMPLETE**,
> when canonical 200A–200C are the **capability** subsystem stages and have essentially no
> production wiring. It also placed defect retirement in `205A`, which is a reporting
> stage. Every status in §3 has been recalculated from source against the owner-supplied
> definitions.

---

## 0. Audited baseline

| Item | Value |
|------|-------|
| Audited branch | `claude/kernel-unlock-audit-i7nt1t` |
| Audited commit | `757993b699b309dafdb3d17c428380c08d7fc9f7` |
| Audited tree | `1118b61b74588e73b0dc235dc96086ec7488257c` |
| `main` | `757993b699b309dafdb3d17c428380c08d7fc9f7` |
| `origin/main` | `757993b699b309dafdb3d17c428380c08d7fc9f7` |
| `main == origin/main` | **verified** |
| Working tree | clean at audit time |
| QEMU runs performed | **none** (out of scope for this audit) |

> **Preflight correction.** At session start the local `main` ref was stale at
> `1b100cc1540e7f3282b67bc7ec3833b7cf71cdea` (tree `d49beacd`) — 46 commits behind the
> remote. `git ls-remote --heads origin` reported `refs/heads/main = 757993b`, and an
> explicit `git fetch origin main` advanced `origin/main` `1b100cc..757993b`. Local `main`
> was then fast-forwarded to `origin/main`. **Every statement in this document describes
> `757993b`.** An audit taken against the stale `1b100cc` would have missed all of Stage
> 200C2C (the three-architecture reply-timeout matrix), all of Stage 200D (server death and
> `ExitCurrentTask`), and the entire `OwnerRevalidation` contract.

### Hosted validation actually executed for this audit

| Command | Result |
|---------|--------|
| `cargo test --lib -- --test-threads=1` | **ok — 3729 passed, 0 failed, 2 ignored** |
| `cargo test --tests -- --test-threads=1` | **ok — 3729 lib + 146 integration, 0 failed** |
| `cargo test --lib` (default parallel harness) | **completes — no abort in 5 of 5 runs**; 58–71 logical assertion failures remain, count varying per run |
| `bash scripts/check-contract-doc-enforcement.sh` | **ok — gate passes** |
| `cargo check` for x86_64 / AArch64 / RISC-V bare-metal `kernel_boot` | **clean** |

**The parallel abort is fixed.** The first revision of this audit recorded
`cargo test --lib` aborting with `double free or corruption` / SIGABRT on 3 of 3 attempts
and listed it as a blocker. That was a real memory-safety bug with three independent
cross-test aliasing causes; the fixes were transplanted from `889026f3` and every parallel
run now reaches a normal test-result line. See `doc/KERNEL_TEST_RULES.md` Rule H1 for the
three causes and the surviving constraint.

What remains under a parallel harness is **logical** shared-machine contention, not memory
unsafety: modules that share process-global counters and one-shot latches
(`stage200d1_server_death` 11, `stage200c_reply_timeout_transaction` 8,
`stage198e3b2b_drain_switch` 8, `stage200d2a_deferred_death` 7, and ~15 more) fail
non-deterministically when run concurrently. Every "hosted evidence" claim in this document
is therefore still a **single-threaded** claim, and removing that contention is part of
canonical Stage **205C**.

---

## 1. Broad-lock census

### 1.1 What counts as the broad lock

`SharedKernel` (`src/runtime.rs:231`) owns exactly one broad lock:

```rust
pub struct SharedKernel {
    state: SpinLock<KernelState>,
}
```

Three methods acquire it, all in `src/runtime.rs`:

| Method | Line | Acquires | Notes |
|--------|------|----------|-------|
| `SharedKernel::lock` | `runtime.rs:341` | `self.state.lock()` | `#[cfg(test)]` only — not production |
| `SharedKernel::with` | `runtime.rs:345` | `self.state.lock()` | broad `&mut KernelState`, no CPU binding |
| `SharedKernel::with_cpu` | `runtime.rs:350` | `self.state.lock()` + `set_current_cpu(cpu)` | broad `&mut KernelState`, CPU-bound |

Everything else that reads or mutates kernel state does so through the **per-domain
split seams**, which derive raw field pointers from `self.state.data_ptr()` and take only
one rank-ordered domain lock (`scheduler_state`, `ipc_state_lock`, `task_state_lock`,
`vm_state_lock`, `capability_state_lock`, `memory_state_lock`, `fault_state_lock`,
`driver_state_lock`, `restart_state_lock`, `telemetry_state_lock`,
`boot_config_state_lock` — declared at `src/kernel/boot/mod.rs:6791`).

"Unlocking" therefore has one measurable definition: **the number of production
callsites of `with` / `with_cpu`, driven to zero, after which the `SpinLock<KernelState>`
field itself can be deleted.**

### 1.2 Production callsite census (test code excluded)

Method: every `.rs` file under `src/` and `crates/`, excluding whole-file test modules
(`tests.rs`) and everything after a `#[cfg(test)] mod tests {` boundary; comment-only
lines excluded.

| Category | Production callsites |
|----------|---------------------|
| `SharedKernel::with_cpu` | **41** |
| `SharedKernel::with` (broad `&mut KernelState`) | **10** |
| Raw `self.state.lock()` | **3** (all inside the three definitions above) |
| **Total broad-lock acquisition sites** | **51** |

### 1.3 `with_cpu` — 41 production callsites

| File | Count | Lines |
|------|-------|-------|
| `src/runtime.rs` | 13 | 389, 670, 1350, 1450, 1484, 1533, 1701, 1714, 1846, 2190, 2368, 2402, 2644 |
| `src/arch/trap_entry.rs` | 12 | 299, 423, 499, 558, 644, 674, 747, 805, 1055, 1202, 1295, 1432 |
| `src/arch/riscv64/trap.rs` | 8 | 563, 659, 727, 825, 870, 958, 1063, 1194 |
| `src/arch/x86_64/smp.rs` | 4 | 2179, 2455, 2571, 2664 |
| `src/arch/x86_64/descriptor_tables.rs` | 2 | 1249, 1305 |
| `src/arch/riscv64/boot.rs` | 1 | 1048 |
| `src/kernel/boot/thread_state.rs` | 1 | 232 |

Structural reading of those 41:

* **1 is the authoritative trap dispatch** — `trap_entry.rs:299` (`handle_trap_entry_shared`)
  and its RISC-V twin `riscv64/trap.rs:563`. These are *the* global lock of the system: every
  syscall that is not on the split whitelist, plus every timer IRQ, external IRQ and page
  fault, runs its entire handler inside this closure.
* **~20 are post-lock re-acquisitions** — the D2/D6/FutexWait/Yield drains re-enter
  `with_cpu` briefly to perform the arch thread-state restore after the authoritative
  dispatch already ran off-lock. These are short and bounded, but they are still broad
  acquisitions and still count.
* **2 are identity snapshots** — `descriptor_tables.rs:1249/1305` read `current_tid()` under
  the broad lock purely to compute `entering_tid`/`exiting_tid`.
* **1 is the AArch64 split return path** — `trap_entry.rs:1432`, see §2.4.
* The remainder are SMP bring-up (`x86_64/smp.rs`), RISC-V resume
  (`riscv64/boot.rs:1048`) and thread creation (`thread_state.rs:232`).

### 1.4 Broad `.with(|state| …)` — 10 production callsites

| File | Line | Purpose |
|------|------|---------|
| `src/runtime.rs` | 1244 | `try_ipc_recv` fallback (hosted/non-deadline path) |
| `src/runtime.rs` | 1248 | `ipc_recv_until_deadline` |
| `src/runtime.rs` | 2654 | trap handling helper |
| `src/runtime.rs` | 3696 | `task_home_cpu` read |
| `src/runtime.rs` | 3725 | `run_reply_timeout_completion_locked` — **broad-lock completion fallback** |
| `src/runtime.rs` | 4013 | `reply_timeout_token_for_caller` read |
| `src/runtime.rs` | 4017 | `disarm_deadline_after_terminal_completion` |
| `src/runtime.rs` | 4025 | `set_task_home_cpu` |
| `src/arch/x86_64/smp.rs` | 2442 | `ap_saved_resume_context` read |
| `src/arch/x86_64/smp.rs` | 2582 | `ap_saved_resume_context` read |

`src/kernel/boot/orchestrator_state.rs:47` matches the same textual pattern but is
`LOCK_ORDER_LAST_RANK.with(|last| …)` — a `thread_local!` accessor, **not** a broad-lock
acquisition. It is excluded from the count.

`runtime.rs:3725` (`run_reply_timeout_completion_locked`) is the surviving **legacy
global-lock fallback handler** of the reply-timeout path. Its off-lock replacement —
`OffLockReplyTimeout` (`runtime.rs:247`), which composes the same transaction from
`with_ipc_split_mut` / `with_task_tcbs_split_mut` / `enqueue_reply_timeout_wake_split` —
is the production path on x86_64. The broad-lock variant is retained as the fallback and
has not been deleted.

### 1.4a Per-callsite classification — the Stage 204A deliverable

Canonical Stage 204A requires every runtime callsite classified **boot-only / test-only /
runtime-required / obsolete fallback**, with no undocumented runtime callsite remaining.
Enclosing functions were resolved mechanically from source.

| Class | Count |
|-------|-------|
| boot-only | **0** |
| test-only | **3** |
| obsolete | **2** |
| runtime-required | **46** |
| undocumented | **0** |

#### test-only (3)

| Site | Enclosing fn | Why |
|------|--------------|-----|
| `runtime.rs:1244` | `ipc_recv_with_deadline_split_bridge` | only callers are in `src/kernel/boot/tests.rs`; the helper's own doc says "falls back to global lock for recv; **not a standalone trap-seam path**" |
| `runtime.rs:1248` | `ipc_recv_with_deadline_split_bridge` | same helper, deadline arm |
| `runtime.rs:2654` | `control_plane_set_process_cnode_slots_via_syscall` | the `SharedKernel` wrapper's only callers are in the `runtime.rs` test module (lines 4962, 4986, 5495, 5682); production NR 8 goes through `control_plane_set_process_cnode_slots_split_mut` |

#### obsolete (2)

| Site | Enclosing fn | Why |
|------|--------------|-----|
| `runtime.rs:2644` | `SharedKernel::handle_trap_with_cpu` | **no in-tree caller at all** — production, tests or otherwise. Only source-grep guards reference the name. Deletable. |
| `runtime.rs:3725` | `run_reply_timeout_completion` | no production caller; superseded by the `OffLockReplyTimeout` composition (`runtime.rs:247`). Retained only until the AArch64/RISC-V reply-timeout scans are ported (canonical 199E). |

#### runtime-required (46)

| Group | Sites | Enclosing fn |
|-------|-------|--------------|
| Authoritative trap dispatch | `trap_entry.rs:299`; `riscv64/trap.rs:563` | `handle_trap_entry_shared`, `handle_riscv_trap_entry_shared` |
| Post-lock drain re-acquisitions | `trap_entry.rs:423, 499, 558, 644, 674, 747, 805`; `riscv64/trap.rs:659, 727, 825, 870, 958, 1063, 1194` | same two functions |
| First-resume trampoline | `trap_entry.rs:1055, 1202, 1295` | `yarm_kernel_thread_switch_trampoline` |
| AArch64 split return path | `trap_entry.rs:1432` | `finalize_split_handled_syscall` |
| Identity snapshots | `descriptor_tables.rs:1249, 1305` | `yarm_x86_dispatch_trap_from_stub` |
| x86_64 AP paths (knob-gated at runtime, still compiled in) | `smp.rs:2179, 2442, 2455, 2571, 2582, 2664` | `ap_seal_return_to_idle`, `c2c_bsp_saved_frame_resume`, `ap_saved_frame_resume`, `ap_sched_next_or_idle` |
| RISC-V resume | `riscv64/boot.rs:1048` | `yarm_riscv64_trap_bridge` |
| Thread creation | `thread_state.rs:232` | `yarm_kernel_thread_switch_trampoline_rust_real` |
| Recv / delivery boundary | `runtime.rs:1350, 1450, 1484, 1533, 1701, 1714, 1846, 2190, 2368, 2402` | `try_split_ipc_recv_queued_plain_into_frame`, `complete_recv_boundary_user_copy`, `complete_recv_boundary_ordinary_cap`, `execute_dispatch_post_work`, `execute_blocked_waiter_reply_cap_delivery`, `execute_blocked_waiter_ordinary_cap_delivery` |
| Identity / SMP / deadline helpers | `runtime.rs:389, 670, 3696, 4013, 4017, 4025` | `current_tid_authoritative`, `revalidate_idle_owner_after_drains`, `smp_request_wake_target_split_read`, `disarm_reply_deadline_on_reply_win`, `smp_assign_task_home_cpu` |

> **Naming hazard found during classification.** `runtime.rs:3696` sits inside
> `smp_request_wake_target_split_read` — a function whose name ends in `_split_read` but
> which takes the **broad** lock via `self.with(|k| k.task_home_cpu(tid))`. It is the only
> `*_split_read`-named function in the tree that does so. Do not treat the naming
> convention as a guarantee; canonical 204B should either rename it or give it a real
> task-domain seam.

### 1.5 Raw / global `KernelState` mutation

There is **no** production path that mutates `KernelState` through a raw global other
than the three `SharedKernel` methods. Specifically:

* No `static mut KernelState`, no `static KERNEL: …<KernelState>` exists. The only
  kernel-wide `static` of that shape is `KERNEL_GLOBAL_ALLOCATOR`
  (`src/kernel/global_allocator.rs:679/685`), which is an allocator, not kernel state.
* `KernelState` is reachable only via `SharedKernel::state`, either through the broad
  lock or through the `*_split_mut` / `*_split_read` seams that go via
  `self.state.data_ptr()`.
* The split seams *do* use `unsafe` raw-pointer derivation, but each one takes the
  matching domain lock before touching the storage, and the pointers are recomputed fresh
  on every call (the Stage 114 staleness fix documented at `runtime.rs:325`).

The residual risk is therefore **not** uncontrolled global mutation; it is **lock-domain
ordering** and **the size of the remaining broad-lock closures**.

### 1.6 Legacy global-lock fallback handlers

| Fallback | Site | Trigger | Status |
|----------|------|---------|--------|
| Default-deny split-dispatch fallback | `syscall_split.rs:885` `classify_split_eligible_nr_only` → `_ => None` | every non-whitelisted syscall | **by design, permanent until §4 retires each class** |
| In-helper decline → broad lock | `try_split_ipc_recv_queued_plain_into_frame`, `try_split_vm_brk_shrink_into_frame`, `try_split_debug_log_into_frame`, `try_split_futex_wake_into_frame` all return `None` for cases they cannot service | narrow-case miss | active |
| Reply-timeout broad-lock completion | `runtime.rs:3725` | non-x86_64, or off the off-lock collector | active |
| D2/D6 drain `reason=state_changed` fallback | `trap_entry.rs:445, 521, 876` | re-verify failed after the broad guard dropped | active |
| `d6_genuine_enabled()` compile-time false | `src/kernel/boot/mod.rs:766` | **AArch64 and RISC-V always** | active — see B4 |

`d6_genuine_enabled()` is `cfg!(target_arch = "x86_64") && !d6_controlled_switch_proof_enabled()
&& !d6_switch_a_enabled()`. There is no production opt-out on x86_64, and no opt-**in** on
the other two architectures: **the off-lock authoritative dispatch is an x86_64-only
mechanism today.**

---

## 2. Syscall / path matrix per architecture

### 2.1 Split-dispatch admission, by architecture

`try_split_dispatch_into_frame` (`src/kernel/syscall_split.rs:206`) is the single
pre-broad-lock seam. What reaches it differs per architecture:

| NR | Syscall | x86_64 | AArch64 | RISC-V | Gate |
|----|---------|--------|---------|--------|------|
| 15 | `DebugLog` | ✅ off-lock | ✅ off-lock¹ | ✅ off-lock | ungated |
| 10 | `FutexWake` | ✅ off-lock | ✅ off-lock¹ | ✅ off-lock | ungated |
| 8 | `ControlPlaneSetCnodeSlots` | ✅ off-lock | ❌ not imported | ❌ excluded by NR gate | ungated |
| 2 | `IpcRecv` (kernel-task queued-plain only) | ✅ narrow | ❌ not imported | ❌ excluded | ungated |
| 14 | `VmBrk` (page-crossing shrink only) | ✅ narrow | ❌ not imported | ❌ excluded | ungated |
| 6 | `IpcCall` direct | 🧪 gated | 🧪 gated | 🧪 gated | `ipccall_direct_proof_enabled()` — **default OFF** |
| 7 | `IpcReply` direct | 🧪 gated | 🧪 gated | 🧪 gated | `ipccall_direct_proof_enabled()` — **default OFF** |
| 0,1,3,4,5,9,11,12,13,16,23,24,26,28,29,30,31 | all others | ❌ | ❌ | ❌ | broad lock only |

¹ AArch64 reaches the seam only for NRs its selective ABI import admits
(`trap_entry.rs:1384` `pre_split_import_syscall_abi`): NR 15, NR 10, plus NR 6/7 under the
proof gate, plus everything when `ipc_recv_oracle_proof_enabled()`. Every other syscall
keeps `nr = 0` in the frame and is declined at the NR gate.

RISC-V does **not** use `handle_trap_entry_shared`. It has a purpose-built bridge,
`handle_riscv_trap_entry_shared` (`src/arch/riscv64/trap.rs:417`), whose own gate
(`riscv64/trap.rs:452`) admits exactly `DebugLog | FutexWake | (IpcCall|IpcReply if gated)`
— so NR 8 / NR 2 / NR 14 can never be split-serviced on RISC-V even though the shared
dispatcher knows them.

**Production off-lock syscall classes, ungated: 5 on x86_64, 2 on AArch64, 2 on RISC-V.**
Everything else — including all of `IpcSend`, `IpcRecvTimeout`, `FutexWait`, `Yield`,
`Fork`, `SpawnThread`, all VM and all spawn syscalls, and `ExitCurrentTask` — enters the
broad lock.

### 2.2 Per-path matrix

Legend — **Lock**: `broad` = inside `with_cpu`; `split` = per-domain seams only;
`split+broad` = off-lock body with a broad re-acquisition somewhere on the path.

| Path | Arch | Implementation | Locks acquired | Blocking / wake | Post-lock work | Rollback | Hosted evidence | Live evidence |
|------|------|----------------|----------------|-----------------|----------------|----------|-----------------|---------------|
| `DebugLog` NR 15 | x86_64 | `try_split_debug_log_into_frame` (`syscall_split.rs:363`) | **split** — task read + VM user-copy split-read | none; caller stays `current` | none | n/a (pure read) | 3725-test suite | `FIRST_COHORT_LIVE_MATRIX arches=3 classes=4 live_cells=12 result=ok` |
| `DebugLog` NR 15 | AArch64 | same helper | **split+broad** — `finalize_split_handled_syscall` re-takes `with_cpu` (`trap_entry.rs:1432`) | none | none | n/a | same | same |
| `DebugLog` NR 15 | RISC-V | same helper via RISC-V bridge | **split** — early return, broad phase never entered | none | none | n/a | same | `RISCV_DEBUGLOG_SPLIT_USER_RETURN_OK` |
| `FutexWake` NR 10 | x86_64 | `try_split_futex_wake_into_frame` | **split** — task split-mut wake scan + scheduler split-mut enqueue | wakes waiters; caller never switches | none | waiter scan is single-pass, no partial state | 3725-test suite | `X86_FUTEX_WAKE_LIVE_ORACLE_DONE result=ok first_wake=1 second_wake=0 waiter_resumes=1` |
| `FutexWake` NR 10 | AArch64 | same | **split+broad** (return path) | as above | none | as above | same | oracle |
| `FutexWake` NR 10 | RISC-V | same via bridge | **split** | as above | none | as above | same | `RISCV_FUTEX_WAKE_LIVE_ORACLE_DONE result=ok first_wake=1 second_wake=0` |
| `FutexWait` NR 9 | x86_64 | in-lock `futex_wait_current`, dispatch deferred | **broad** + post-lock drain | **blocks caller**, queue-advancing dispatch | Stage 192A drain (`trap_entry.rs:537`) runs the authoritative dispatch off-lock | drain re-verifies `Blocked`; `reason=state_changed` → broad fallback | suite | first-cohort matrix |
| `FutexWait` NR 9 | AArch64 | in-lock + Stage 195E drain (`trap_entry.rs:612`) | **broad** + drain | blocks caller | 195E drain | as above | suite | `AARCH64_FUTEX_WAIT_IDLE_ORACLE_DONE result=ok` |
| `FutexWait` NR 9 | RISC-V | in-lock, typed idle outcome | **broad** | blocks caller | typed `EnterKernelIdle` | typed outcome, not an `Err` sentinel | suite | `RISCV_FUTEX_WAIT_IDLE_ORACLE_DONE result=ok lock_dropped=1 current_none=1 outgoing_blocked=1` |
| `Yield` NR 0 | x86_64 | in-lock `yield_current`, deferred | **broad** + Stage 192B drain (`trap_entry.rs:792`) | preempts caller | 192B drain | re-verify → broad fallback | suite | first-cohort matrix |
| `Yield` NR 0 | AArch64 | Stage 195G drain (`trap_entry.rs:717`) | **broad** + drain | preempts caller | 195G drain | as above | suite | first-cohort matrix |
| `Yield` NR 0 | RISC-V | Stage 196G post-lock retirement drain | **broad** + drain | preempts caller | full drain (~13 console lines/yield) | as above | suite | first-cohort matrix |
| `ControlPlaneSetCnodeSlots` NR 8 | x86_64 | `control_plane_set_process_cnode_slots_split_mut` | **split** — task read r2 → boot-config read → cap mutate r4 | none | none | precondition miss → `None` → broad path emits canonical `InvalidArgs` | suite | `YARM_LOCK_SPLIT_DISPATCH nr=8 result=ok` |
| `ControlPlaneSetCnodeSlots` NR 8 | AArch64 / RISC-V | — | **broad** | none | none | n/a | suite | — |
| `IpcRecv` NR 2 (kernel-task queued-plain) | x86_64 | `try_split_ipc_recv_queued_plain_into_frame` | **split** | non-blocking case only | none | recv-v2 queued-split rollback re-rolls the materialized cap, then `set_err` | suite | Stage 32B markers |
| `IpcRecv` NR 2 (all other cases) | all | broad handler | **broad** | **blocks receiver** | D2-recv drain (x86_64 only) | drain re-verify | suite | — |
| `IpcSend` NR 1 plain | all 3 | broad handler + off-lock blocked-receiver delivery | **broad** | blocks sender on full endpoint | Stage 188B delivery | synchronous error, no stash | suite | `SECOND_COHORT_PLAIN_SEAL arches=3 classes=2 live_cells=6 result=ok` |
| `IpcSend` NR 1 ordinary-cap | all 3 | as above | **broad** | as above | Stage 188C delivery | as above | suite | `SECOND_COHORT_ORDINARY_CAP` 3×2 matrix |
| `IpcSend` NR 1 shared-region direct | all 3 | post-lock transaction + origin-neutral executor | **broad** + post-lock txn | blocked receiver | off-lock mapping / finalization | executor-owned cleanup, generation-bearing teardown, cancellation fuse | suite | `SECOND_COHORT_SHARED_REGION_DIRECT_MATRIX_SEAL arches=3 classes=1 live_cells=3 fuse_trips=0 result=ok` |
| `IpcCall` NR 6 direct | x86_64 | `try_split_ipccall_direct_into_frame` | **split** (gate on) | server wake, cross-CPU IPI | reply-authority substrate | reserve → commit → cancel; `commit=GoneDead` on peer death | suite | `STAGE_199_IPCCALL_DIRECT_SMP_REQUEST_USER_SEAL … cross_cpu=1 result=ok` |
| `IpcReply` NR 7 direct | x86_64 | `try_split_ipcreply_direct_into_frame` | **split** (gate on) | caller wake, reverse IPI | one-shot consumed barrier | `IPCREPLY_DIRECT_SMP_DUPLICATE_REFUSED reason=consumed_barrier` | suite | `IPCREPLY_DIRECT_SMP_REPLY_OK … cross_cpu=1 result=ok` |
| `IpcCall`/`IpcReply` NR 6/7 | AArch64 / RISC-V | same helpers, arch gates 199A2C1/199A2C2 | **split** (gate on) | as above | as above | as above | suite | `qemu-ipccall-reply-direct-matrix-seal.sh` |
| `IpcRecvTimeout` NR 5 | x86_64 | deadline pre-read split; completion via `OffLockReplyTimeout` | **broad** entry + **split** completion | blocks receiver until deadline | `drain_reply_timeout_post_work` (`trap_entry.rs:1120`) | terminal-claim + generation-bearing deadline token; reply-wins disarms | suite | `IPC_REPLY_TIMEOUT_LOCK_STATUS arch=x86_64 scan_broad_lock=0 completion_transaction_narrow=1 result=ok` |
| `IpcRecvTimeout` NR 5 | AArch64 | port landed 200C2C1B | **broad** scan + split completion | as above | shared drain | as above | suite | matrix cell |
| `IpcRecvTimeout` NR 5 | RISC-V | port landed 200C2C2C-R2A/B | **broad** scan + split completion | as above | RISC-V-local collector/drain | causal reply-win reservation | suite | matrix cell |
| Reply-timeout matrix | all 3 | — | — | — | — | — | suite | **`STAGE_200_IPC_REPLY_TIMEOUT_MATRIX_SEAL` — 6 live cells (timeout-wins + reply-wins × 3 arches), commit `72a4ebf`** |
| Server death (`ServerDies`) | all 3 | `drain_server_death_post_work` (`trap_entry.rs:1129`) | **broad** entry + **split** completion | wakes stranded caller | dedicated post-lock drain | terminal claim, `ServerReplyLink` detach | suite (200D-1/2A/2B1B) | **none — 0 live cells** |
| `ExitCurrentTask` NR 16 | x86_64 | non-returning disposition, in-lock consumer | **broad** + full post-lock drain chain | terminates caller, never returns | `EXIT_TASK_POST_LOCK_DRAIN_DONE`, then owner revalidation | `OwnerRevalidation` / `OwnerCommit` fail-closed | suite | live cell sealed at `0b5e98f` |
| `ExitCurrentTask` NR 16 | AArch64 | as above, disposition consumed **after** drains | **broad** + drains | as above | as above | as above | suite | live cell sealed (`EXIT_TASK_BROAD_LOCK_RELEASED arch=aarch64 … result=ok`) |
| `ExitCurrentTask` NR 16 | RISC-V | consumer + oracle prepared | **broad** + drains | as above | as above | as above | suite | **not earned** — first run at `fb5f040` produced a complete correct chain but timed out on the survivor loop; runner fixed at `5488d8e`, **re-run pending** |
| Timer IRQ / external IRQ / page fault | all 3 | `handle_trap_entry_with_fault_bookkeeping_mode` | **broad** — whole handler | may preempt | fault bookkeeping pre-recorded off-lock (`record_fault_split_mut`) | n/a | suite | core smokes |

### 2.3 Post-lock drain chain (order is load-bearing)

`handle_trap_entry_shared` after `with_cpu` returns (`trap_entry.rs:359`–`1129`):

1. `drain_dispatch_post_work` — blocked-waiter delivery (Stage 188A channel)
2. D2-send drain *(x86_64)*
3. D2-recv drain *(x86_64)*
4. FutexWait drain — Stage 192A *(x86_64)* / Stage 195E *(AArch64)*
5. Yield drain — Stage 192B *(x86_64)* / Stage 195G *(AArch64)*
6. D6-genuine mutating dispatch *(x86_64)*
7. Stage 117 switch-plan stash drain
8. `drain_reply_timeout_post_work`
9. `drain_server_death_post_work`

RISC-V runs its own equivalent chain inside `handle_riscv_trap_entry_shared`
(`riscv64/trap.rs:417`+), including a RISC-V-local reply-timeout collector — it does not
flow through step 8 above (noted explicitly at `trap_entry.rs:1109`).

### 2.4 Confirmed architecture asymmetries

| # | Asymmetry | Evidence | Consequence |
|---|-----------|----------|-------------|
| A1 | AArch64 split classes re-acquire the broad lock on return | `trap_entry.rs:1432` inside `finalize_split_handled_syscall` | AArch64 `DebugLog`/`FutexWake` are **not** broad-lock-free end to end; the seal language "off-lock" overstates AArch64 |
| A2 | Off-lock authoritative dispatch is x86_64-only | `mod.rs:766` `d6_genuine_enabled()` is `cfg!(target_arch = "x86_64") && …` | AArch64/RISC-V take every queue-advancing dispatch in-lock |
| A3 | RISC-V admits only 2 split classes | `riscv64/trap.rs:452` | NR 8 / NR 2 / NR 14 unreachable off-lock on RISC-V |
| A4 | x86_64 resolves the restore owner **in-lock**; AArch64 and RISC-V resolve it **after** the drains | Stage 200D-0B3 vs 200D-2B1C guard `a07` | produced the Stage 200D-2B1D4 live hang; patched by `revalidate_idle_owner_after_drains` (`runtime.rs`, x86_64 only) — **the patch itself is x86_64-only and live-unproven** |
| A5 | RISC-V `Yield` is ~13 console lines per call | Stage 200D-0D2 commit body | RISC-V live oracles need architecture-specific loop bounds; a copied x86_64 bound times out the boot |

---

## 3. Canonical stage status — 199C … 205D

**The definitions below are owner-supplied and authoritative.** Full definition text lives
in `doc/KERNEL_UNLOCKING.md` §0.3; this section carries the recalculated status and the
source evidence for each verdict. Three levels are distinguished, and only the third
counts as done:

* **hosted foundation** — seams exist and are hosted-tested. May be `HELPER_ONLY`, i.e.
  wired into nothing.
* **live proof** — proven on a clean QEMU boot. May be knob-gated, in which case it proves
  the mechanism and **not** the production path.
* **stage complete** — the definition's full scope is retired on the production path, on
  every required architecture, with no broad-lock fallback.

### 3.1 Phase 2 — IPC subsystem unlocking

| Stage | Scope (abbreviated) | Status | Evidence and gap |
|-------|--------------------|--------|------------------|
| **199C** | Blocking `IpcSend` — sender-waiter publication retired, sparse-queue + timeout parity | **OPEN** | `handle_ipc_send` and its waiter publication run inside the broad `with_cpu`. The only off-lock element is the x86_64 D2-send drain (`trap_entry.rs:388`), which relocates the queue-advancing **dispatch**, not the publication, and is compile-time absent on the other two architectures. The 15 `IpcSend` live cells prove **delivery**, not blocking-sender retirement. |
| **199D** | `IpcCall` + reply-object lifecycle as one transaction, incl. **server crash cleanup** | **OPEN** — hosted foundation + knob-gated live proof | Off-lock transaction exists (`ipccall_direct_txn.rs`; `syscall_split.rs:295/307`), reserve→commit→cancel, incarnation-safe records, one-shot consumed barrier. Live-proven x86_64 SMP=2 both directions (`STAGE_199_X86_DIRECT_IPC_FINAL_SEAL … result=ok`) but **default-OFF** (`mod.rs:3095`), so no production boot takes it. **Server-crash cleanup unproven: 0 ServerDies live cells and `IPC_SERVER_DEATH_LINK_LEAK created=54 detached=1 result=fail`.** |
| **199E** | IPC timeout + cancellation: recv, **send**, **call**, reply | **OPEN** — one quarter retired | Reply timeout only: narrow completion on 3 arches, scan off-lock **x86_64 only**, 6 live cells (`STAGE_200_IPC_REPLY_TIMEOUT_MATRIX_SEAL`, `72a4ebf`). `IpcRecvTimeout` pre-reads its deadline off-lock; the receive is broad. **`IpcSend` timeout and `IpcCall` timeout are not retired at all.** Broad fallback `runtime.rs:3725` present. |
| **199F** | Notification signal / wait / wait-timeout / multi-signal; no global lock in IRQ-originated delivery | **OPEN** | Only `notification_waiter_count_split_read` exists, and it is a read helper. `signal_notification` (`ipc_state.rs:5196`) takes `&mut self` under the broad lock. IRQ-originated delivery runs inside the broad trap closure. |
| **199G** | Full IPC seal — zero IPC broad-lock acquisitions | **OPEN** | Blocked on 199C–199F. |

### 3.2 Phase 3 — Capability subsystem unlocking

| Stage | Scope | Status | Evidence and gap |
|-------|-------|--------|------------------|
| **200A** | CNode slot lookup / reservation / generation / install-remove / rollback | **OPEN** — narrow foundation | `control_plane_set_process_cnode_slots_split_mut` retires one control-plane op (NR 8); read helpers `cnode_slot_capacity_split_read`, `cnode_registered_split_read`, `process_cnode_for_identity_split_read`. The five-part decomposition does not exist. |
| **200B** | Cap copy / mint / move / release / revoke, identity+refcount separated from slot mutation | **OPEN** — hosted foundation, **zero production wiring** | `cap_transfer_split.rs`, `cap_memory_mint_split.rs`, `cap_transfer_materialize_split.rs`, `cap_transfer_delegation_split.rs` are each marked **`M2_SEAM_HELPER_ONLY`** and documented as **"NOT wired into"** `ipc_reply` / `ipc_send` / `recv` / `call` / `materialize_received_message_cap_routed`. |
| **200C** | Object lifetime + transfer-envelope cleanup for Endpoint, Notification, Reply, MemoryObject, AddressSpace, Task, IRQ | **OPEN** — 1 of 7 object classes | Shared-region transaction cleanup is real and 3-arch live for the direct class (executor-owned protocol A, generation-bearing teardown, cancellation fuse; `orphan_pages=0`, `duplicate_unmaps=0`, `leaked_transactions=0`). `with_capability_state_split_mut` has exactly **one** production caller (`capability_lifecycle_state.rs:864`). |
| **200D** | Capability subsystem seal + failure injection | **OPEN** | Blocked on 200A–200C. |

> The historical stages labelled `200A` / `200B` / `200C` (terminal ownership, deadline
> token store, reply-timeout transaction) are **IPC timeout work and belong to canonical
> 199E**. They contribute nothing to canonical 200A–200C.

### 3.3 Phase 4 — VM and memory-object unlocking

| Stage | Scope | Status | Evidence and gap |
|-------|-------|--------|------------------|
| **201A** | Anon map + MemoryObject creation, initial page ownership, AS reservation | **OPEN** | `VmAnonMap` (NR 13) broad-lock only. |
| **201B** | `VM_MAP` / `VM_UNMAP` transactional model | **OPEN** — narrow foundation | Only `VmBrk` (NR 14) page-crossing shrink is split, x86_64 only, single-CPU-online gated. `with_vm_user_spaces_split_mut` / `with_memory_split_mut` exist. |
| **201C** | ZC grant + file-slice MemoryObjects (`FILE_GRANT_RO`, NR 28) with complete explicit failure handling | **OPEN** | NR 28 / NR 29 broad-lock only. NR 27 confirmed removed (Stage 197A), so the no-fallback premise holds. |
| **201D** | Page fault + demand mapping off the broad lock | **OPEN** | Only diagnostic bookkeeping is off-lock (`record_fault_split_mut`). Classification, AS lookup, MO lookup, page allocation, mapping commit and disposition all run in `handle_trap_event` (`fault_state.rs:1127`) under `with_cpu`. |
| **201E** | COW + fork memory lifetime | **OPEN** | `Fork` (NR 12) broad-lock only. |
| **201F** | Cross-arch TLB seal | **OPEN** — x86_64 mechanism landed | x86_64 has a real cross-CPU shootdown ACK coordinator (`tlb_shootdown.rs`, Stage 189A); the ack is produced by the target AP's mailbox handler and never fabricated. The **AArch64/RISC-V wake-only-AP rationale the stage explicitly requires is not written.** |
| **201G** | VM subsystem seal | **OPEN** | Blocked on 201A–201F. |

### 3.4 Phase 5 — Task and process lifecycle unlocking

| Stage | Scope | Status | Evidence and gap |
|-------|-------|--------|------------------|
| **202A** | Thread creation + admission; x86 naked-trampoline fix generalized as an explicit **entry ABI contract** | **OPEN** | `thread_state.rs:232` still takes `with_cpu`. The trampoline fix exists but has not been generalized into a stated contract. |
| **202B** | Process spawn with rollback; no partially visible process | **OPEN** | NR 23 / 24 / 26 / 29 broad-lock only. |
| **202C** | Fork/COW transaction; child published only after commit | **OPEN** | Blocked on 201E and 202B. |
| **202D** | Exit + normal reap: thread/process exit, scheduler removal, IPC waiter cancellation, **reply-object cleanup**, VM teardown, cap teardown, parent notification | **OPEN** — partial foundation, 2/3 live cells for one sub-path | `ExitCurrentTask` (NR 16) ABI + non-returning disposition landed; live cells x86_64 (`0b5e98f`) and AArch64; **RISC-V unearned** (runner bound corrected `5488d8e`, re-run never executed). NR 16 still runs **inside** the broad lock with post-lock drains; the other seven elements are not retired. The ServerDies link-accounting defect is this stage's **reply-object-cleanup** element, overlapping 199D's server-crash cleanup. |
| **202E** | `ReapFaultedTask` (NR 31) out of the broad-lock-only path | **OPEN** | `handle_reap_faulted_task` (`syscall/process.rs:933`) dispatches under the broad lock; `reap_faulted_task_noalloc_cleanup` (`restart_state.rs:361`) takes `&mut self`. |
| **202F** | Lifecycle subsystem seal | **OPEN** | Blocked on 202A–202E. |

### 3.5 Phase 6 — Timer, IRQ, and scheduler hot paths

| Stage | Scope | Status | Evidence and gap |
|-------|-------|--------|------------------|
| **203A** | Timer tick + deadline processing; **no broad lock from timer interrupt context** | **OPEN** — partial foundation | The tick is handled inside `handle_trap_entry_with_fault_bookkeeping_mode`, i.e. inside `with_cpu`. `scheduler_tick_now_split_read` and the post-lock reply-timeout drain are partial foundations. |
| **203B** | IRQ delivery — ack, notification delivery, waiter wake, mask/unmask; fast paths never take the broad lock | **OPEN** | External IRQs enter the same broad trap closure. |
| **203C** | Scheduler core; rank-1/rank-2 seams become **authoritative, not compatibility helpers** | **OPEN** — partial | The rank-1 seam is authoritative for queue-advancing dispatch on **x86_64 only**: `d6_genuine_enabled()` (`mod.rs:766`) is compile-time **false** on AArch64 and RISC-V. ~20 of the 46 runtime-required callsites are drain re-acquisitions that exist precisely because the seams are not authoritative end to end. |
| **203D** | Cross-CPU work; AArch64/RISC-V may stay BSP-only **provided APs are explicitly wake-only and no runnable task can be stranded** | **OPEN** — x86_64 live-proven, knob-gated | x86_64: shootdown mailboxes, reschedule IPIs both directions, remote wake, cross-CPU placement, per-CPU current — all live at SMP=2 under default-off knobs; production scheduler still BSP-only. The AArch64/RISC-V wake-only + no-stranding argument the stage requires is **not documented**. |

### 3.6 Phase 7 — Remove the monolithic runtime path

| Stage | Scope | Status | Evidence and gap |
|-------|-------|--------|------------------|
| **204A** | Broad-lock callsite census, every runtime use classified boot-only / test-only / runtime-required / obsolete fallback; **no undocumented runtime callsite** | **COMPLETE** | §1.4a: all 51 callsites enumerated with file, line and enclosing function. **0 boot-only, 3 test-only, 2 obsolete, 46 runtime-required, 0 undocumented.** Raw/global `KernelState` mutation outside the three `SharedKernel` methods: none exists (§1.5). |
| **204B** | Decompose `KernelState` ownership; `SharedKernel` may remain a container but must not serialize the kernel | **OPEN** — partial foundation | 11 ranked domain locks and a full seam set already exist, but `with_cpu` still forms a broad `&mut KernelState`. |
| **204C** | Remove fallback-to-global handlers | **OPEN** | Five families live: default-deny `_ => None` (`syscall_split.rs:885`), four in-helper `None` declines, drain `reason=state_changed` re-acquires, the reply-timeout broad completion, and `d6_genuine_enabled()` being compile-time false on two architectures. |
| **204D** | Remove retirement scaffolding | **OPEN** | `GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE`, one-shot class logging and the foundation oracles are all live. |
| **204E** | Delete the runtime `SpinLock` + anti-reintroduction guard | **OPEN** | `state: SpinLock<KernelState>` (`runtime.rs:232`) present; no guard exists. |

### 3.7 Phase 8 — Full unlocking seal

| Stage | Scope | Status | Evidence and gap |
|-------|-------|--------|------------------|
| **205A** | Complete syscall matrix — arch, class, locks, blocking, post-lock work, rollback, **address-space restore**, live proof. **Every runtime cell localized.** | **OPEN** — matrix drafted, localization false | §2 supplies the matrix across all three architectures with locks / blocking / post-lock / rollback / hosted / live columns. Two gaps: the **address-space restore** column is not populated per cell, and the exit condition (every runtime cell localized) is false while 46 runtime-required broad callsites remain. **205A reports cells; it is not where defects are retired.** |
| **205B** | Fault-injection matrix at every transactional boundary | **OPEN** — isolated precedents | Shared-region 12-case race seal, reply-cap 18-case negative seal, 24 deterministic ServerDies races. No unified matrix; no coverage of allocation failure, slot exhaustion, queue full, or shootdown failure. |
| **205C** | Long-running concurrency torture with all anomaly counters zero | **OPEN** | No sustained harness exists. The hosted suite cannot currently serve as one: under a parallel harness it produces 58–71 shared-state assertion failures (§0). |
| **205D** | Cross-arch full-unlock seal — `KERNEL_RUNTIME_GLOBAL_LOCK_CALLS … count=0` ×3, `KERNEL_FULL_UNLOCK_SEAL … result=ok` ×3, `KERNEL_FULL_UNLOCK_CROSS_ARCH_SEAL arches=3 result=ok` | **OPEN** | **None of the three marker families exists anywhere in the tree** (grep over `src`, `crates`, `tests`, `scripts`). |

### 3.8 Summary

| Phase | Complete | Partial foundation | Open |
|-------|----------|--------------------|------|
| 2 — IPC | 0 of 5 | 199D, 199E | 199C, 199F, 199G |
| 3 — Capability | 0 of 4 | 200A, 200C | 200B, 200D |
| 4 — VM | 0 of 7 | 201B, 201F | 201A, 201C, 201D, 201E, 201G |
| 5 — Lifecycle | 0 of 6 | 202D | 202A, 202B, 202C, 202E, 202F |
| 6 — Timer/IRQ/sched | 0 of 4 | 203A, 203C, 203D | 203B |
| 7 — Monolith removal | **204A** | 204B | 204C, 204D, 204E |
| 8 — Seal | 0 of 4 | 205A | 205B, 205C, 205D |
| **Total** | **1 of 34** | 11 | 22 |

**No canonical stage in Phases 2–6 or 8 is complete.** The one completed stage, 204A, is
documentation rather than lock retirement.

### 3.9 Documentation defects found during the stage mapping

* **Stage 200C2C was entirely undocumented.** The three-architecture reply-timeout matrix
  (6 live cells, canonical 199E evidence) existed only in commit messages. Migrated into
  `doc/IPC.md` §8.4 and `doc/PROJECT_HISTORY.md`.
* **`doc/SYSCALL_ABI.md` was stale**: it declared "Public syscall count: 16 (`0..=15`)"
  while NR 16 `ExitCurrentTask` had landed, and its slot matrix still listed `16..=22` as a
  reserved gap. Both corrected.
* **`doc/STATUS.md` was roughly seventy stages stale**, describing Stage 129–132 as the
  frontier.
* **Three doc references were broken and all are now repaired** — see
  `doc/DOCUMENTATION_MAP.md` "Broken references found in Pass 6 — all repaired".
  `doc/ABI_CONTRACT_FREEZE.md` was deleted in `3c86f362` with no migration while
  `scripts/check-contract-doc-enforcement.sh` still grepped it, so that gate could not
  pass; its content is recovered, re-verified and folded into `doc/SYSCALL_ABI.md`.

### 3.10 Gate status

All nine documentation / contract gates pass:

| Gate | Result |
|------|--------|
| `check-contract-doc-enforcement` | ✅ (was failing — repaired, see §3.9) |
| `check-current-contracts` | ✅ |
| `check-roadmap-readiness` | ✅ |
| `check-boundary-milestone-freeze` | ✅ |
| `check-proc-vfs-codec-freeze` | ✅ |
| `check-service-domain-ownership` | ✅ |
| `check-tid-allocation-policy` | ✅ |
| `check-ci-workflow-enforcement` | ✅ |
| `check-hal-conformance-targets` | ✅ |
| `check-crate-graph-boundary` | ✅ |
| `check-pr-scope-and-message` | ✅ |

**Two source-boundary gates fail, and both pre-date this work.** Verified failing at
`origin/main` (`757993b`) and at the Pass 6 commit (`f8a3c04`), i.e. not introduced here.
They are production-source violations, not documentation, and repairing them is unrelated
production work that this audit deliberately did not perform:

| Gate | Failure |
|------|---------|
| `check-kernel-arch-boundary` | `architecture boundary violation pattern 'target_arch = "x86_64"' in src/bin/kernel_boot.rs` — lines 91 and 93 carry `#[cfg(target_arch = "x86_64")]` / `#[cfg(not(target_arch = "x86_64"))]` |
| `check-service-arch-boundary` | `crates/yarm-control-plane-servers/src/bin/crash_test_srv.rs:1: missing delegation to service/runtime entrypoint` |

Because these two are red, **the branch is not gate-clean overall** even though every
documentation gate passes. That distinction is stated rather than papered over.

---

## 4. Roadmap to full unlock

The ordering is the canonical phase order (`doc/KERNEL_UNLOCKING.md` §0). Phases 2–6
retire subsystems; Phase 7 removes the monolith; Phase 8 seals. Within a phase, a seal
stage cannot precede the stages it seals.

Because **no** Phase 2–6 stage is complete, the useful near-term sequencing is:

| # | Work | Canonical stage | Why now |
|---|------|-----------------|---------|
| 1 | ServerDies reply-link accounting repair | **199D** increment (+ 202D cleanup) | the only `result=fail` in the tree; gates every ServerDies live cell — see §6 |
| 2 | x86_64 ServerDies live cell | 199D | first exercise of `revalidate_idle_owner_after_drains`, which has never run in QEMU |
| 3 | RISC-V `ExitCurrentTask` runner re-run | 202D | pure execution debt; the kernel chain is already proven correct |
| 4 | AArch64 + RISC-V ServerDies live cells | 199D | completes server-crash cleanup proof across architectures |
| 5 | Flip `ipccall_direct_proof_enabled()` to production default | 199D | until this lands, the entire off-lock direct-IPC transaction benefits nothing |
| 6 | AArch64 + RISC-V reply-timeout scan off-lock; delete `run_reply_timeout_completion` | 199E | removes one obsolete broad callsite (51 → 50) |
| 7 | `IpcSend` timeout + `IpcCall` timeout retirement | 199E | the two untouched quarters of 199E |
| 8 | Blocking `IpcSend` sender-waiter publication | 199C | largest remaining Phase-2 item |
| 9 | Notification signal/wait/timeout seams | 199F | last Phase-2 subsystem before the 199G seal |
| 10 | Wire the `HELPER_ONLY` capability seams into production | 200A–200C | Phase 3 currently has zero production wiring |

Phases 4–6 follow. Phase 7 (204B–204E) is the actual broad-lock removal and cannot start
meaningfully until Phases 2–6 have localized their paths; 204A is already complete and its
census is the input to 204B.

---

## 5. Ten highest-priority blockers

| # | Blocker | Where | Blocks |
|---|---------|-------|--------|
| **B1** | `IPC_SERVER_DEATH_LINK_LEAK created=54 detached=1 result=fail` — `LinkCreated` counts every bound `IpcCall` system-wide while `LinkDetached` counts only the exit path; `audit_success_path` also demands every class `== 1`, and `reset_instance()` has no live caller | `mod.rs:4116`; `ipc_state.rs:1250` vs `1383` | 199D server-crash cleanup, 202D reply-object cleanup, all ServerDies live cells |
| **B2** | `revalidate_idle_owner_after_drains` has never run in QEMU | `runtime.rs:665`, wired `descriptor_tables.rs:1324` | the ServerDies live programme rests on an unexercised repair |
| **B3** | NR 6 / NR 7 off-lock direct IPC is default-OFF | `mod.rs:3095` | 199D — the landed transaction delivers no production benefit |
| **B4** | `d6_genuine_enabled()` is compile-time x86_64-only | `mod.rs:766` | 203C; AArch64/RISC-V cannot retire any queue-advancing class |
| **B5** | Every capability seam is `M2_SEAM_HELPER_ONLY` | `cap_transfer_split.rs`, `cap_memory_mint_split.rs`, `cap_transfer_materialize_split.rs`, `cap_transfer_delegation_split.rs` | all of Phase 3 (200A–200D) |
| **B6** | `FutexWait` off-lock seams landed helper-only and were never wired | `syscall_split.rs:786`–`803` | 203C; the largest blocking class stays broad-lock-only |
| **B7** | Reply-timeout scan off-lock on x86_64 only; `IpcSend`/`IpcCall` timeouts untouched; broad fallback survives | `runtime.rs:3725`; `IPC_REPLY_TIMEOUT_LOCK_STATUS scan_broad_lock=1` on AArch64/RISC-V | 199E |
| **B8** | RISC-V `ExitCurrentTask` live cell never earned — kernel chain proven correct, runner bound corrected, re-run not executed | `5488d8e` | 202D |
| **B9** | Parallel `cargo test --lib` produces 58–71 shared-state assertion failures from process-global counters and one-shot latches | `stage200d1_server_death`, `stage200c_reply_timeout_transaction`, `stage198e3b2b_drain_switch`, `stage200d2a_deferred_death`, ~15 more | 205C; and every hosted claim remains single-threaded-only |
| **B10** | AArch64 re-acquires the broad lock on its split return path | `trap_entry.rs:1432` | 204B/204E must localize it; **205A reports the cell, it does not retire it** |

The memory-corruption blocker recorded in the first revision of this document is
**resolved** — see §0 and `doc/KERNEL_TEST_RULES.md` Rule H1.

---

## 6. Smallest next production stage

### ServerDies reply-link accounting repair — a **199D** increment that also cleans up state **202D** will own

**One production path:** `server_dies_counters::audit_success_path`
(`src/kernel/boot/mod.rs:4116`), the `LinkCreated` record at
`src/kernel/boot/ipc_state.rs:1250`, and the `LinkDetached` record at
`src/kernel/boot/ipc_state.rs:1383`.

**This is not a complete canonical stage and must not be reported as one.** 199D
additionally requires the NR 6 / NR 7 gate flipped to production default and the whole
call/reply transaction proven with no broad-lock fallback. This increment removes the one
hard failure standing in front of that work.

**Why this one:**

1. It is the only `result=fail` in the tree. B2–B8 are incompleteness; B1 is a live
   failing assertion.
2. It gates every ServerDies live cell, and therefore both 199D's server-crash cleanup and
   202D's reply-object cleanup.
3. It is genuinely one turn: a counter-scoping decision plus hosted tests. **No QEMU, no
   live cell, no scheduler change.**
4. It cannot regress anything — the counters are diagnostic, and the underlying link
   lifecycle is already correct (`ipc_reply` closes links via
   `finalize_server_reply_link_for_record`). The bug is in the *audit*, not the mechanism.

**Recommended resolution:** scope the nine-counter vector to the death currently being
audited rather than adding `unregister_server_reply_link` as a second close site. Per-death
scoping preserves the "every class == 1" expectation the fifteen hard-fail literals were
written against; counting both close sites would force those literals to be re-specified.

**Exit criteria:**
* `IPC_SERVER_DEATH_LINK_LEAK` cannot fire on a boot containing ≥1 unrelated bound `IpcCall`.
* `IPC_SERVER_DEATH_TRANSITION_AUDIT` reports the audited death only.
* `reset_instance()` either gains a live caller or is removed with its rationale recorded.
* The fifteen hard-fail literals and nine wiring guards still hold.

**Hosted tests (≥8):** multi-call boot with one death; reply-closed link; exit-closed link;
both in one boot; counter scoping across two deaths; reset semantics; the nine-class vector
shape; a negative case where a genuine leak **must** still be reported.

**Mutation guards (≥4):** counters revert to global scope; the genuine-leak detector is
weakened to always-pass; the audited-death filter is dropped; `== 1` is removed without
per-death scoping.

**Live cells: none.** State this explicitly in the stage report.

**Expected broad-lock callsite reduction: 0.** It unblocks reduction; it does not perform
one. Claiming otherwise would be false.

---

## 7. Method and limits

* The census is textual over the source tree at `757993b`, with whole-file test modules
  (`tests.rs`) and post-`#[cfg(test)] mod tests {` regions excluded, and comment-only lines
  excluded. `LOCK_ORDER_LAST_RANK.with(…)` was manually excluded as a `thread_local!`
  false positive.
* Live-evidence rows cite seals recorded in commit messages and stage reports; **no QEMU
  was run for this audit**, so every live claim is inherited, not re-verified. Rows marked
  "not earned" / "0 live cells" are the ones where the inherited record explicitly refuses
  the seal.
* Hosted evidence was re-executed for this audit (§0) and is first-hand.
* Canonical stage identities in §3 are assigned by this audit and are **not** the
  historical labels. The mapping is one-directional: canonical → historical evidence.

---

## 8. Related canonical documents

| Topic | Owner |
|-------|-------|
| Canonical stages + roadmap | `doc/KERNEL_UNLOCKING.md` |
| Lock architecture + broad-lock census | `doc/KERNEL_LOCKING.md` |
| IPC / reply / timeout / ServerDies state | `doc/IPC.md` |
| Active syscall contracts | `doc/SYSCALL_ABI.md` |
| Hosted / live validation rules | `doc/KERNEL_TEST_RULES.md` |
| Current verified state + blockers | `doc/STATUS.md` |
| Accepted seals + milestones | `doc/PROJECT_HISTORY.md` |
| Per-architecture return / scheduling / live proof | `doc/ARCH_X86_64.md`, `doc/ARCH_AARCH64.md`, `doc/ARCH_RISCV64.md` |
| Document ownership | `doc/DOCUMENTATION_MAP.md` |
