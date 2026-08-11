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
| `cargo test --tests -- --test-threads=1` | **ok — 3881 passed, 0 failed** (3729 lib + 152 integration, default features) |
| `cargo test --lib` (default parallel harness) | **completes — no abort in 5 of 5 runs**; 58–71 logical assertion failures remain, count varying per run |
| `cargo test --tests --features ipc-reply-timeout-oracle-core -- --test-threads=1` | **ok — 4045 passed, 0 failed** |
| all 13 repository gate scripts | **13 of 13 pass** (§3.10) |
| `cargo check` — x86_64 / AArch64 / RISC-V bare-metal `kernel_boot` | **clean** |
| `cargo check` — x86_64 / AArch64 / RISC-V freestanding `crash_test_srv` | **clean** |
| `cargo fmt --check`, `git diff --check` | **clean** |

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
is therefore still a **single-threaded** claim.

**This is test-infrastructure debt, not canonical Stage 205C work.** 205C is a
*long-running concurrency torture* of the running kernel — sustained IPC / spawn / exit /
reap / fork / VM / cap / futex / timeout / IRQ / restart load, with lock-rank violations,
duplicate current task, duplicate queue membership, cap-refcount anomalies and the various
leak counters all required to be zero. What fails here is the *hosted test corpus* sharing
process-global fixtures with itself. Removing that contention is a prerequisite for using
the hosted suite as a concurrency harness, and it may therefore **precede or support** 205C,
but it proves nothing about the kernel and closes no part of the stage. Do not report it as
205C progress.

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
| `SharedKernel::with_cpu` | **36** |
| `SharedKernel::with` (broad `&mut KernelState`) | **6** |
| Raw `self.state.lock()` | **3** (all inside the three definitions above) |
| **Total broad-lock acquisition sites** | **42** |

### 1.3 `with_cpu` — 40 production callsites

| File | Count | Lines |
|------|-------|-------|
| `src/runtime.rs` | 12 | U1 deleted the obsolete `handle_trap_with_cpu` acquisition (13 → 12) |
| `src/arch/trap_entry.rs` | 11 | 305, 429, 505, 701, 787, 817, 890, 948, 1198, 1345, 1438 |
| `src/arch/riscv64/trap.rs` | 6 | U3 retired two read-only post-lock current-TID re-acquisitions: the foundation-oracle drain (8 → 7) and the FutexWait no-incoming idle branch (7 → 6). The FutexWait SATP/frame-restore switch branch still re-acquires. |
| `src/arch/x86_64/smp.rs` | 4 | 2179, 2455, 2571, 2664 |
| `src/arch/x86_64/descriptor_tables.rs` | 2 | 1249, 1305 |
| `src/arch/riscv64/boot.rs` | 1 | 1048 |
| `src/kernel/boot/thread_state.rs` | 1 | 232 |

Structural reading of those 40:

* **1 is the authoritative trap dispatch** — `trap_entry.rs:305` (`handle_trap_entry_shared`)
  and its RISC-V twin `riscv64/trap.rs:563`. These are *the* global lock of the system: every
  syscall that is not on the split whitelist, plus every timer IRQ, external IRQ and page
  fault, runs its entire handler inside this closure.
* **~20 are post-lock re-acquisitions** — the D2/D6/FutexWait/Yield drains re-enter
  `with_cpu` briefly to perform the arch thread-state restore after the authoritative
  dispatch already ran off-lock. These are short and bounded, but they are still broad
  acquisitions and still count.
* **2 are identity snapshots** — `descriptor_tables.rs:1249/1305` read `current_tid()` under
  the broad lock purely to compute `entering_tid`/`exiting_tid`.
* The remainder are SMP bring-up (`x86_64/smp.rs`), RISC-V resume
  (`riscv64/boot.rs:1048`) and thread creation (`thread_state.rs:232`).

> The AArch64 split-return site that used to sit at `trap_entry.rs:1432` is **gone**: Stage 199D
> removed it when readiness blocker 2 was closed (§6.1.12), taking `trap_entry.rs` from 12 to 11
> and the tree from 51 to 50. Blocker 3's post-lock direct dispatch drain (§6.1.13) added **no**
> replacement — unlike the drains above it takes no `with_cpu` at all — so this table is
> unchanged at 40 across that increment.

### 1.4 Broad `.with(|state| …)` — 6 production callsites

| File | Line | Purpose |
|------|------|---------|
| `src/runtime.rs` | 3696 | `task_home_cpu` read |
| `src/runtime.rs` | 4013 | `reply_timeout_token_for_caller` read |
| `src/runtime.rs` | 4017 | `disarm_deadline_after_terminal_completion` |
| `src/runtime.rs` | 4025 | `set_task_home_cpu` |
| `src/arch/x86_64/smp.rs` | 2442 | `ap_saved_resume_context` read |
| `src/arch/x86_64/smp.rs` | 2582 | `ap_saved_resume_context` read |

`src/kernel/boot/orchestrator_state.rs:47` matches the same textual pattern but is
`LOCK_ORDER_LAST_RANK.with(|last| …)` — a `thread_local!` accessor, **not** a broad-lock
acquisition. It is excluded from the count.

The `SharedKernel::run_reply_timeout_completion` broad-lock wrapper that used to occupy
this table was **deleted by U1**: it had no production caller. Its off-lock counterpart,
`OffLockReplyTimeout` (`runtime.rs:247`), which composes the same transaction from
`with_ipc_split_mut` / `with_task_tcbs_split_mut` / `enqueue_reply_timeout_wake_split`, is
the production path on x86_64 and is unchanged. **Broad reply-timeout processing did not
disappear:** the single completion body `KernelState::run_reply_timeout_completion_locked`
(`kernel/boot/ipc_state.rs`) is untouched, the in-lock timer scan still calls it directly on
every architecture, and off-lock scanning remains x86_64-only. Canonical **199E** stays
**OPEN**.

### 1.4a Per-callsite classification — the Stage 204A deliverable

Canonical Stage 204A requires every runtime callsite classified **boot-only / test-only /
runtime-required / obsolete fallback**, with no undocumented runtime callsite remaining.
Enclosing functions were resolved mechanically from source.

| Class | Count |
|-------|-------|
| boot-only | **0** |
| test-only | **0** |
| obsolete | **0** |
| runtime-required | **42** |
| undocumented | **0** |

#### test-only (0)

**None.** U2 relocated all three into test-only modules the census excludes:
`ipc_recv_with_deadline_split_bridge` (2 acquisitions — never a trap-seam path, only hosted
callers) moved to `src/kernel/boot/tests.rs`, and the
`SharedKernel::control_plane_set_process_cnode_slots_via_syscall` wrapper (1 acquisition —
only callers were `runtime.rs`'s own test module) became a helper inside that
`#[cfg(test)] mod tests`. The tests keep their exact coverage; production NR 8 is unchanged
on `control_plane_set_process_cnode_slots_split_mut`, and the `KernelState` method of the
same name is untouched.

#### obsolete (0)

**None.** U1 deleted both: `SharedKernel::handle_trap_with_cpu` (no in-tree caller at all)
and `SharedKernel::run_reply_timeout_completion` (no production caller; superseded by the
`OffLockReplyTimeout` composition). Neither deletion changed runtime behavior, and the
reply-timeout completion body itself was not touched.

#### runtime-required (42)

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
| **199D** | `IpcCall` + reply-object lifecycle as one transaction, incl. **server crash cleanup** | **OPEN** — hosted foundation + knob-gated live proof | Off-lock transaction exists (`ipccall_direct_txn.rs`; `syscall_split.rs:295/307`), reserve→commit→cancel, incarnation-safe records, one-shot consumed barrier. Live-proven x86_64 SMP=2 both directions (`STAGE_199_X86_DIRECT_IPC_FINAL_SEAL … result=ok`) but **default-OFF** (`mod.rs:3095`), so no production boot takes it. Server-crash cleanup: accounting **repaired** and the **first live cell EARNED on x86_64** — `STAGE_200D2B1C_X86_64_SERVER_DIES_SEAL` at `f5669cb5`/`e2fd0b5c`, scoped vector `[1;9]`, quiescent `created=54 closed=54 live_links=0`, `EXIT_TASK_OWNER_REVALIDATED … committed=replacement`, zero `result=fail` (`doc/IPC.md` §8.5). **Still open as a stage: 1 of 3 architectures, and the NR6/NR7 gate is default-OFF.** |
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
| **202D** | Exit + normal reap: thread/process exit, scheduler removal, IPC waiter cancellation, **reply-object cleanup**, VM teardown, cap teardown, parent notification | **OPEN** — partial foundation, 2/3 live cells for one sub-path | `ExitCurrentTask` (NR 16) ABI + non-returning disposition landed; live cells x86_64 (`0b5e98f`) and AArch64; **RISC-V unearned** (runner bound corrected `5488d8e`, re-run never executed). NR 16 still runs **inside** the broad lock with post-lock drains; the other seven elements are not retired. The ServerDies link-accounting defect — this stage's **reply-object-cleanup** element, overlapping 199D's server-crash cleanup — is **repaired** (`doc/IPC.md` §8.5); the rest of the stage is untouched. |
| **202E** | `ReapFaultedTask` (NR 31) out of the broad-lock-only path | **OPEN** | `handle_reap_faulted_task` (`syscall/process.rs:933`) dispatches under the broad lock; `reap_faulted_task_noalloc_cleanup` (`restart_state.rs:361`) takes `&mut self`. |
| **202F** | Lifecycle subsystem seal | **OPEN** | Blocked on 202A–202E. |

### 3.5 Phase 6 — Timer, IRQ, and scheduler hot paths

| Stage | Scope | Status | Evidence and gap |
|-------|-------|--------|------------------|
| **203A** | Timer tick + deadline processing; **no broad lock from timer interrupt context** | **OPEN** — partial foundation | The tick is handled inside `handle_trap_entry_with_fault_bookkeeping_mode`, i.e. inside `with_cpu`. `scheduler_tick_now_split_read` and the post-lock reply-timeout drain are partial foundations. |
| **203B** | IRQ delivery — ack, notification delivery, waiter wake, mask/unmask; fast paths never take the broad lock | **OPEN** | External IRQs enter the same broad trap closure. |
| **203C** | Scheduler core; rank-1/rank-2 seams become **authoritative, not compatibility helpers** | **OPEN** — partial | The rank-1 seam is authoritative for queue-advancing dispatch on **x86_64 only**: `d6_genuine_enabled()` (`mod.rs:766`) is compile-time **false** on AArch64 and RISC-V. ~20 of the 45 runtime-required callsites are drain re-acquisitions that exist precisely because the seams are not authoritative end to end. |
| **203D** | Cross-CPU work; AArch64/RISC-V may stay BSP-only **provided APs are explicitly wake-only and no runnable task can be stranded** | **OPEN** — x86_64 live-proven, knob-gated | x86_64: shootdown mailboxes, reschedule IPIs both directions, remote wake, cross-CPU placement, per-CPU current — all live at SMP=2 under default-off knobs; production scheduler still BSP-only. The AArch64/RISC-V wake-only + no-stranding argument the stage requires is **not documented**. |

### 3.6 Phase 7 — Remove the monolithic runtime path

| Stage | Scope | Status | Evidence and gap |
|-------|-------|--------|------------------|
| **204A** | Broad-lock callsite census, every runtime use classified boot-only / test-only / runtime-required / obsolete fallback; **no undocumented runtime callsite** | **COMPLETE** | §1.4a: all 42 callsites enumerated with file, line and enclosing function. **0 boot-only, 0 test-only, 0 obsolete, 42 runtime-required, 0 undocumented.** (Stage 199D retired the AArch64 handled-split syscall return; U1 deleted the two obsolete acquisitions, 49 → 47; U2 relocated the three test-only ones, 47 → 44; U3 has retired two read-only RISC-V drains onto an authoritative seam, 44 → 43 → 42. The census is exactly the runtime-required set.) Raw/global `KernelState` mutation outside the three `SharedKernel` methods: none exists (§1.5). Kept honest by `tests/broad_lock_census_guard.rs` (6 tests), which recomputes the census from source and fails on any added or removed production callsite. |
| **204B** | Decompose `KernelState` ownership; `SharedKernel` may remain a container but must not serialize the kernel | **OPEN** — partial foundation | 11 ranked domain locks and a full seam set already exist, but `with_cpu` still forms a broad `&mut KernelState`. |
| **204C** | Remove fallback-to-global handlers | **OPEN** | Five families live: default-deny `_ => None` (`syscall_split.rs:885`), four in-helper `None` declines, drain `reason=state_changed` re-acquires, the reply-timeout broad completion, and `d6_genuine_enabled()` being compile-time false on two architectures. |
| **204D** | Remove retirement scaffolding | **OPEN** | `GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE`, one-shot class logging and the foundation oracles are all live. |
| **204E** | Delete the runtime `SpinLock` + anti-reintroduction guard | **OPEN** | `state: SpinLock<KernelState>` (`runtime.rs:232`) present; no guard exists. |

### 3.7 Phase 8 — Full unlocking seal

| Stage | Scope | Status | Evidence and gap |
|-------|-------|--------|------------------|
| **205A** | Complete syscall matrix — arch, class, locks, blocking, post-lock work, rollback, **address-space restore**, live proof. **Every runtime cell localized.** | **OPEN** — matrix drafted, localization false | §2 supplies the matrix across all three architectures with locks / blocking / post-lock / rollback / hosted / live columns. Two gaps: the **address-space restore** column is not populated per cell, and the exit condition (every runtime cell localized) is false while 45 runtime-required broad callsites remain. **205A reports cells; it is not where defects are retired.** |
| **205B** | Fault-injection matrix at every transactional boundary | **OPEN** — isolated precedents | Shared-region 12-case race seal, reply-cap 18-case negative seal, 24 deterministic ServerDies races. No unified matrix; no coverage of allocation failure, slot exhaustion, queue full, or shootdown failure. |
| **205C** | Long-running concurrency torture with all anomaly counters zero | **OPEN** | No sustained harness exists, and no torture load has been run. Separately, the hosted suite cannot currently *serve* as such a harness because its own fixtures contend (§0) — that is test-infrastructure debt, a prerequisite rather than a part of this stage. |
| **205D** | Cross-arch full-unlock seal — `KERNEL_RUNTIME_GLOBAL_LOCK_CALLS … count=0` ×3, `KERNEL_FULL_UNLOCK_SEAL … result=ok` ×3, `KERNEL_FULL_UNLOCK_CROSS_ARCH_SEAL arches=3 result=ok` | **OPEN** | **None of the three marker families exists anywhere in the tree** (grep over `src`, `crates`, `tests`, `scripts`). |

### 3.8 Summary

| Phase | Complete | Partial foundation | Open |
|-------|----------|--------------------|------|
| 2 — IPC | 0 of 5 | 199D, 199E | 199C, 199F, 199G |
| 3 — Capability | 0 of 4 | 200A, 200C | 200B, 200D |
| 4 — VM | 0 of 7 | 201B, 201F | 201A, 201C, 201D, 201E, 201G |
| 5 — Lifecycle | 0 of 6 | 202D | 202A, 202B, 202C, 202E, 202F |
| 6 — Timer/IRQ/sched | 0 of 4 | 203A, 203C, 203D | 203B |
| 7 — Monolith removal | **1 of 5** (204A) | 204B | 204C, 204D, 204E |
| 8 — Seal | 0 of 4 | 205A | 205B, 205C, 205D |
| **Total** | **1 of 35** | 12 | 22 |

**No canonical stage in Phases 2–6 or 8 is complete.** The one completed stage, 204A, is
documentation rather than lock retirement.

> **Arithmetic correction.** An earlier revision reported *1 of 34* with 11 partials. Phase 7
> was the only row written without an `N of M` denominator, and the totals silently counted it
> as four stages. **The dropped stage was `204B` (decompose `KernelState` ownership), the sole
> Phase-7 partial**, which is why both the total (34 → **35**) and the partial count
> (11 → **12**) were low by exactly one. All 35 stages were, and remain, individually
> documented and classified; only the summary arithmetic was wrong. `204B` is classified
> **partial foundation**: the eleven ranked domain locks and the `*_split_mut` / `*_split_read`
> seam set already exist, but `with_cpu` still forms a broad `&mut KernelState`, so the
> container still serializes the kernel.


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

**Every repository gate passes — 13 of 13.**

| Gate | Result |
|------|--------|
| `check-boundary-milestone-freeze` | ✅ |
| `check-ci-workflow-enforcement` | ✅ |
| `check-contract-doc-enforcement` | ✅ (was failing — repaired, see §3.9) |
| `check-crate-graph-boundary` | ✅ |
| `check-current-contracts` | ✅ |
| `check-hal-conformance-targets` | ✅ |
| `check-kernel-arch-boundary` | ✅ (was failing — repaired, see below) |
| `check-pr-scope-and-message` | ✅ |
| `check-proc-vfs-codec-freeze` | ✅ |
| `check-roadmap-readiness` | ✅ |
| `check-service-arch-boundary` | ✅ (was failing — repaired, see below) |
| `check-service-domain-ownership` | ✅ |
| `check-tid-allocation-policy` | ✅ |

Two source-boundary gates were red on `origin/main` (`757993b`) and remained red through
the first two revisions of this branch. Both are now repaired, minimally and
architecturally — no behaviour changed in either case.

**`check-kernel-arch-boundary`** rejected `target_arch = "x86_64"` in
`src/bin/kernel_boot.rs`: the freestanding entry point chose between calling `run()`
directly (x86_64, where `prepare_arch_boot` has already consumed the IRQ-controller
description) and `run_kernel_boot(run)` (everything else). The rule is that the bin routes
ISA details through `src/arch/*`, and that predicate is genuinely an arch-layer decision.
It moved into `arch::boot_entry::enter_kernel_run_loop`, which the bin now calls
unconditionally. The two-arm `cfg` is byte-identical; only its location changed.

**`check-service-arch-boundary`** reported
`crates/yarm-control-plane-servers/src/bin/crash_test_srv.rs:1: missing delegation to
service/runtime entrypoint`. Every other control-plane bin is entry glue that calls
`yarm_control_plane_servers::run_<name>()`; `crash_test_srv` alone carried its whole
service body inline. The body moved to `control_plane::crash_test`, exported as
`run_crash_test_srv()`, and the bin now matches its five siblings. The marker sequence
(`CRASH_TEST_SRV_ENTRY` → `_READY` → `_DELAY_BEGIN` → `_DELAY_DONE` → `_FAULT_NOW`), the
128-yield delay and the deterministic null-write fault that SUP-L5B depends on are all
preserved; the delay bound is now a shared constant so the hosted and freestanding paths
cannot drift.

---

## 4. Roadmap to full unlock

The ordering is the canonical phase order (`doc/KERNEL_UNLOCKING.md` §0). Phases 2–6
retire subsystems; Phase 7 removes the monolith; Phase 8 seals. Within a phase, a seal
stage cannot precede the stages it seals.

Because **no** Phase 2–6 stage is complete, the useful near-term sequencing is:

| # | Work | Canonical stage | Why now |
|---|------|-----------------|---------|
| — | ServerDies reply-link accounting repair | **199D** increment (+ 202D cleanup) | **LANDED** — removed the tree's only `result=fail` and unblocked every ServerDies live cell (`doc/IPC.md` §8.5) |
| 2 | x86_64 ServerDies live cell | 199D | first exercise of `revalidate_idle_owner_after_drains`, which has never run in QEMU |
| 3 | RISC-V `ExitCurrentTask` runner re-run | 202D | pure execution debt; the kernel chain is already proven correct |
| 4 | AArch64 + RISC-V ServerDies live cells | 199D | completes server-crash cleanup proof across architectures |
| 5 | Flip `ipccall_direct_proof_enabled()` to production default | 199D | until this lands, the entire off-lock direct-IPC transaction benefits nothing |
| 6 | AArch64 + RISC-V reply-timeout scan off-lock | 199E | the wrapper deletion half is **delivered by U1** (49 → 47); the remaining, and larger, half is porting the AArch64/RISC-V scans off-lock, which U1 did **not** do |
| 7 | `IpcSend` timeout + `IpcCall` timeout retirement | 199E | the two untouched quarters of 199E |
| 8 | Blocking `IpcSend` sender-waiter publication | 199C | largest remaining Phase-2 item |
| 9 | Notification signal/wait/timeout seams | 199F | last Phase-2 subsystem before the 199G seal |
| 10 | Wire the `HELPER_ONLY` capability seams into production | 200A–200C | Phase 3 currently has zero production wiring |

Phases 4–6 follow. Phase 7 (204B–204E) is the actual broad-lock removal and cannot start
meaningfully until Phases 2–6 have localized their paths; 204A is already complete and its
census is the input to 204B.

---

## 5. Highest-priority blockers

| # | Blocker | Where | Blocks |
|---|---------|-------|--------|
| **B1** | `revalidate_idle_owner_after_drains` has never run in QEMU | `runtime.rs:665`, wired `descriptor_tables.rs:1324` | the ServerDies live programme rests on an unexercised repair. **Now the leading blocker** — the link-accounting `result=fail` that used to head this list is resolved (§3.1, `doc/IPC.md` §8.5), leaving no hard failure in the tree |
| **B2** | NR 6 / NR 7 off-lock direct IPC is default-OFF | `mod.rs:3095` | 199D — the landed transaction delivers no production benefit |
| **B3** | `d6_genuine_enabled()` is compile-time x86_64-only | `mod.rs:766` | 203C; AArch64/RISC-V cannot retire any queue-advancing class |
| **B4** | Every capability seam is `M2_SEAM_HELPER_ONLY` | `cap_transfer_split.rs`, `cap_memory_mint_split.rs`, `cap_transfer_materialize_split.rs`, `cap_transfer_delegation_split.rs` | all of Phase 3 (200A–200D) |
| **B5** | `FutexWait` off-lock seams landed helper-only and were never wired | `syscall_split.rs:786`–`803` | 203C; the largest blocking class stays broad-lock-only |
| **B6** | Reply-timeout scan off-lock on x86_64 only; `IpcSend`/`IpcCall` timeouts untouched; broad fallback survives | `runtime.rs:3725`; `IPC_REPLY_TIMEOUT_LOCK_STATUS scan_broad_lock=1` on AArch64/RISC-V | 199E |
| **B7** | RISC-V `ExitCurrentTask` live cell never earned — kernel chain proven correct, runner bound corrected, re-run not executed | `5488d8e` | 202D |
| **B8** | Parallel `cargo test --lib` produces 58–71 shared-state assertion failures from process-global counters and one-shot latches — **test-infrastructure debt**, not a kernel defect and not 205C completion work | `stage200d1_server_death`, `stage200c_reply_timeout_transaction`, `stage198e3b2b_drain_switch`, `stage200d2a_deferred_death`, ~15 more | keeps every hosted claim single-threaded-only; a prerequisite for using the hosted suite as a 205C harness |
| **B9** | AArch64 re-acquires the broad lock on its split return path | `trap_entry.rs:1432` | 204B/204E must localize it; **205A reports the cell, it does not retire it** |

The memory-corruption blocker recorded in the first revision of this document is
**resolved** — see §0 and `doc/KERNEL_TEST_RULES.md` Rule H1.

---

## 6. Smallest next production stage

### Flip `ipccall_direct_proof_enabled()` to a production default on x86_64 — a **199D** increment

The previous recommendation (the ServerDies reply-link accounting repair) has **landed**;
see §3.1, §3.4 and `doc/IPC.md` §8.5. There is no longer a hard `result=fail` in the tree.

Two things are now unblocked, and they are different kinds of work:

* **Live proof, no production code** — the x86_64 ServerDies live cell. The kernel chain is
  complete, the accounting is repaired, and `revalidate_idle_owner_after_drains`
  (blocker **B1**) has still never executed in QEMU. One clean boot both earns the first
  ServerDies cell and exercises that repair. This is the highest-value next act, but it is
  a runner act, not a production change.
* **Smallest production change** — flip the NR 6 / NR 7 direct-IPC gate
  (`ipccall_direct_proof_enabled()`, `src/kernel/boot/mod.rs:3095`) from a default-OFF
  proof knob to the production default on x86_64.

**Why the gate flip is the right production increment:** the entire off-lock direct
request/reply transaction is implemented, hosted-tested and live-proven at SMP=2 in both
directions (`STAGE_199_X86_DIRECT_IPC_FINAL_SEAL … result=ok`), yet **no production boot
takes it**, so it delivers nothing today. It is one production path, the seals that must
re-run green already exist, and it converts landed-but-dormant work into the first genuine
production benefit of canonical 199D.

**Exit criteria:** a normal x86_64 boot services NR 6 and NR 7 through the off-lock
transaction with no knob set; every Stage 199 seal re-runs green **ungated**; the feature-off
image stays marker-clean; AArch64 and RISC-V are untouched (their flip is 199D's next
increment).

**Hosted tests:** ≥12, including gate-removal guards proving no production path still reads
the knob, and the fallback still being reachable for the cases the helpers decline.

**Live cells:** the existing Stage 199 cells re-earned **ungated**. **Expected broad-lock
callsite reduction: 0** — the off-lock path already exists; this makes it reachable.

Neither act completes canonical 199D. The stage additionally requires the full call/reply
transaction proven with no broad-lock fallback, and server-crash cleanup proven live on all
three architectures.

---

## 6.1 Why the NR6/NR7 production-default flip is NOT the smallest next increment

Recorded because the flip was attempted and stopped, and the reason is a design constraint
rather than an oversight.

Making the off-lock NR 6 / NR 7 path production-default on x86_64 was expected to be a
one-predicate change to `ipccall_direct_proof_enabled()`. It is not: **enablement is
two-layered**, and the proof gate is only the outer layer.

| Layer | Site | Effect |
|-------|------|--------|
| 1 — proof gate | `ipccall_direct_proof_enabled()` (`mod.rs:3095`), consumed at `syscall_split.rs:243/298/310`, `trap_entry.rs:1403/1430`, `riscv64/trap.rs:451` | admits NR 6 / NR 7 to the split dispatcher at all |
| 2 — **oracle endpoint confinement** | `ipccall_direct_oracle_request_endpoint_is` / `..._reply_endpoint_is` (`mod.rs:3496/3502`), enforced inside `try_split_ipccall_direct_into_frame` (`syscall_split.rs:648`) and its NR 7 twin | services **only the oracle's** request/reply endpoint; every other endpoint returns `None` → legacy broad-lock path |

Layer 2's own comment states the intent: *"confine the off-lock request path to the oracle's
request endpoint so a NORMAL system IpcCall (the live service chain) stays byte-identical on
its legacy path **even while the proof gate is armed**."*

So removing only the proof-gate dependency makes the path *reachable* but leaves it
declining every ordinary `IpcCall`. The two proof obligations — "normal feature-off x86 boots
use the off-lock NR6/NR7 path" and "no broad-lock fallback for eligible x86 NR6/NR7" — are
**unachievable** by that change alone.

Removing layer 2 as well is not a small increment. It used to collide with a second
constraint as well: the blocked-server / blocked-caller acknowledgement was a **single
slot** classified ORACLE-ONLY / SINGLE-OUTSTANDING-PAIR, whose real-build `publish` refused
a second simultaneous pair. The off-lock request path requires a committed ack before it
will service a call, and a production service chain runs many concurrent bound `IpcCall`s —
the x86_64 ServerDies cell measured **54 reverse links in a single boot** — so unconfining
the path would have driven multiple outstanding pairs through a one-pair slot.

**That prerequisite is now met.** `src/kernel/direct_ack_store.rs` implements the bounded,
endpoint-indexed, generation-bearing multi-pair store the Stage 199A2D1 race model named:
`DIRECT_ACK_STORE_CAPACITY` independent `(endpoint_index, endpoint_generation)` pairs
coexist under a reserve → commit → consume/cancel lifecycle, with exactly-once
acknowledgement, incarnation-exact identity, fail-closed rejection of stale/duplicate/
foreign consumption, capacity refused *before* any irreversible publication, and
leak-free rollback. Both ack modules are now endpoint-keyed views over it, and both
split-dispatch consumers name the exact endpoint incarnation they are entitled to. Proved
by `stage199d_multi_pair_races` (deterministic barrier-aligned races over 2 and
`DIRECT_ACK_STORE_CAPACITY` simultaneous pairs, contended consumption, capacity
exhaustion, same-endpoint reservation, stale/foreign consumption, reserve→cancel rollback),
`stage199d_multi_pair_boundary`, and the store's own unit tests. See `doc/IPC.md` §8.6.

**Still blocking the flip.** With the acknowledgement prerequisite met, the flip was
attempted again and stopped a second time. Removing the two gates is *mechanically* small,
but the direct transaction body itself was written against the **oracle's** message
contract, not the production one. Three defects were found by audit; each would be a
correctness regression on a normal boot, and none is a gating question.

### 6.1.1 HARD-STOP A — RESOLVED: the NR6 direct delivery now conforms

**The defect.** Production `IpcCall` frames a request as `[app_opcode_le(2)] ++ data`
(`crates/yarm-user-rt/src/lib.rs:986`). Because the kernel message is built with
`opcode = OPCODE_INLINE` and `flags = FLAG_REPLY_CAP`, the inline-prefix framing predicate
is **true**, so every legacy delivery path stripped the 2-byte prefix and reported the
application opcode in the metadata. The direct NR6 transaction did neither: it copied the
snapshot payload verbatim and encoded `OPCODE_INLINE` with the unstripped length. A
receiver observed:

| Metadata field | Legacy (blocked recv-v2 reply-cap delivery) | Direct NR6 (before) |
|---|---|---|
| `meta.opcode` | application opcode (payload bytes 0..2) | `OPCODE_INLINE` (0) |
| `meta.payload_len` | `data.len()` | `2 + data.len()` |
| payload buffer | `data` | `[opcode_le] ++ data` |
| `meta.flags` | `0` | `FLAG_REPLY_CAP` |

Userspace decodes **exclusively** from the metadata — `ipc_recv_v2` states it outright
(`crates/yarm-user-rt/src/lib.rs:425`) and takes `opcode = meta.opcode` (`:469`) with
`payload[..meta.payload_len]`. Production servers dispatch on that opcode (e.g.
`crates/yarm-fs-servers/src/fs/initramfs/service.rs:212`). The oracle did not catch it
because its server reparsed the prefix itself and asserted the unstripped framing.

**The fix.** One canonical delivery projection now serves every path:
`src/kernel/syscall/ipc_recv_core.rs` holds `RecvDelivery`,
`project_recv_delivery(&Message)`, `project_recv_delivery_parts(opcode, flags, sender, raw)`
and `should_strip_inline_opcode_prefix_parts`. It determines the receiver-visible
application opcode, the payload offset and length, the reply-cap `recv_meta_flags`, and the
malformed/too-short disposition (a framed message whose raw payload is shorter than the
2-byte prefix falls back to the sender's own opcode and the verbatim payload — the frozen
historical behaviour, preserved deliberately rather than "fixed" into a rejection).

Converged onto it:

| Site | What changed |
|---|---|
| `src/kernel/syscall.rs` × 4 (blocked-waiter completions) | four identical inline copies replaced by `project_recv_delivery` |
| `src/kernel/syscall/ipc.rs` (immediate full-recv) | inline copy replaced by `project_recv_delivery` |
| `src/kernel/syscall/ipc_abi.rs` | `should_strip_inline_opcode_prefix` delegates to the canonical rule |
| `src/kernel/ipccall_direct_txn.rs` (direct NR6) | projects the header words it *would* have framed, copies `delivery.app_payload`, encodes via the shared blocked-waiter encoder |
| `src/kernel/syscall.rs`, `src/runtime.rs`, direct NR6 | all three blocked-waiter producers share `encode_blocked_waiter_meta` |

The oracle server was rewritten to consume the **ordinary production `ipc_recv_v2`
contract** — `msg.opcode` from the metadata and `msg.as_slice()` as the data, with no manual
prefix reparse. Its marker changed from `framed_ok=…` to `opcode_ok=… data_ok=…`, and the
three per-arch runner greps were updated to match (the oracle core is arch-neutral).

**Proof.** `stage199d_delivery_projection_differential` feeds the same message through BOTH
deliveries and compares every receiver-visible field. Neither side is computed by the test:
the legacy observation comes from a real `IpcCall` trap
(`handle_ipc_call` → `complete_blocked_recv_for_waiter`, which writes the receiver's payload
and metadata through `copy_to_user`; the deferred reply-cap producer declines in a hosted
build because `GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE` is clear), and the direct observation
comes from `drain_direct_request_post_work`. Compared: `status`, `opcode`, `flags`,
`payload_len`, `cap_id`, `recv_meta_flags`, `sender_tid`, the payload bytes read back from
the receiver's own address space, and the reply-cap identity (both resolve to a `Reply`
object in the server's cnode). Cases: empty application data, nonzero application opcode,
zero application opcode, the maximum framed inline payload (126 bytes of data in a 128-byte
frame), and the malformed too-short prefix.

**Live proof.** Re-earned round-trip cell at commit
`458bb3d4505e1aac3747d4c653463bf1a07eb1b2` (tree
`97513076e09248c5fad86b1fd9ce3e3ca71da81f`):
`STAGE_199_IPCCALL_REPLY_DIRECT_LIVE_SEAL arch=x86_64 classes=2 live_cells=2
duplicate_replies=0 duplicate_wakes=0 result=ok`. The boot log carries the two numbers that
were wrong before — `opcode=1543` (`0x0607`, the application opcode; previously `0`) and
`plen=8` (the stripped length; previously `10`) — with `framed_ok=` absent and
`YARM_BOOT_OK` present. This **supersedes** the identically-named seal earned at `2c07ac96`
for the NR6 delivery path. Recorded in `doc/IPC.md` §8.6.1.

**Scope kept.** The proof gate stays proof-only, the oracle endpoint confinement is
unchanged, the error disposition (B) and return lanes (C) are untouched, and no endpoint-mode
policy was added.

### 6.1.2 HARD-STOP B — RESOLVED: typed disposition contract

**The defect.** Both split helpers discarded the transaction result
(`let _ = shared.drain_direct_request_post_work(&work);` and its NR7 twin), each followed
unconditionally by `frame.set_ok(0, 0, 0)`. Every failure the transaction classifies became
**success with no message delivered** — silent loss, no error, no fallback. Invisible on the
oracle path, which never fails.

**The fix.** `src/kernel/direct_disposition.rs` holds one pure, exhaustive mapping per
direction onto three dispositions — `Completed`, `DeclinedBeforeMutation`,
`Failed(SyscallError)`. **No wildcard arm**: adding a variant to either error enum is a
compile error until its disposition is decided. Neither drain's `Result` is discarded.

`DeclinedBeforeMutation` is admissible only when all six conditions hold of the state the
transaction *leaves*: no reply record reserved or committed, no capability minted or
installed, no user payload/meta copied, no waiter or run-queue change, no acknowledgement
committed as a delivery, no wake published. Two clarifications are load-bearing and are
recorded in the module: a completed rollback of a `Reserved` (never-invokable) record and a
revoked provisional mint genuinely leave nothing; and a claimed-then-discarded
acknowledgement published nothing — it only forfeits the direct path's own retry, which
fails safe. Any *attempted* user copy disqualifies fallback, because a faulted copy may have
written a prefix.

`Failed` encodes the canonical error with `frame.set_err(err.code())` — byte-for-byte how the
global-lock handler encodes a `SyscallError` (`fault_state.rs`), so frame parity reduces to
error-code equality.

#### Request disposition table (14 variants, exhaustive)

| Variant | Disposition | State left behind | Legacy equivalent |
|---|---|---|---|
| `WouldBlock` | Declined | pristine; ack restored | legacy queues or blocks the caller |
| `LeaseNotClaimed` | Declined | pristine (duplicate drain) | legacy re-runs the send |
| `CallerGone` | Declined | pristine; no cap resolved | legacy raises the canonical cap error |
| `SendEndpoint(_)` | Declined | pristine | `validate_endpoint_right(cap, SEND)` |
| `ReplyEndpoint(_)` | Declined | pristine | `validate_endpoint_right(reply, RECEIVE)` |
| `EndpointGenerationChanged` | Declined | pristine | legacy re-resolves the endpoint |
| `RecordFull` | Declined | pristine (probe/reserve refused) | `create_reply_cap_for_caller` table-full error |
| `ServerCnodeMissing` | Declined | reservation cancelled | legacy fails later in materialization |
| `MintFailed` | Declined | reservation cancelled, mint revoked | `materialize_received_message_cap` → `CapabilityFull` |
| `PayloadCopyFault` | **Failed(`InvalidArgs`)** | a user copy was attempted | `complete_blocked_recv_for_waiter` → `InvalidArgs` |
| `MetaCopyFault` | **Failed(`InvalidArgs`)** | a user copy was attempted | same, after rolling the cap back |
| `WaiterLost` | **Failed(`WrongObject`)** | payload **and** meta already in the server's address space | none — legacy would have *queued*; see below |
| `ServerGone` | **Failed(`ServerDied`)** | waiter claimed, deliberately not restored | the request will never be answered by that incarnation |
| `RecordCommitFailed` | **Failed(`Internal`)** | server committed `Runnable` | defensive; unreachable |

#### Reply disposition table (10 variants, exhaustive)

| Variant | Disposition | State left behind | Legacy equivalent |
|---|---|---|---|
| `WouldBlock` | Declined | pristine | legacy `ipc_reply` runs |
| `LeaseNotClaimed` | Declined | pristine (duplicate drain) | legacy re-runs the reply |
| `ReplyCapResolve(_)` | Declined | pristine | legacy raises the canonical cap error |
| `ReservePreconditionFailed` | Declined | pristine (reservation refused) | legacy one-shot/stale-record error |
| `WaiterLost` | Declined | pristine — the **pre-reserve, pre-copy** check | legacy re-resolves the caller waiter |
| `PayloadCopyFault` | **Failed(`InvalidArgs`)** | a user copy was attempted | legacy reply copy fault → `InvalidArgs` |
| `MetaCopyFault` | **Failed(`InvalidArgs`)** | a user copy was attempted | same |
| `WaiterLostAfterCopy` | **Failed(`WrongObject`)** | reply already in the caller's address space | stale reply authority |
| `CallerGone` | **Failed(`WrongObject`)** | waiter claimed, record discarded | the reply cap names a caller that no longer exists |
| `RecordConsumeFailed` | **Failed(`Internal`)** | caller committed `Runnable` | defensive; unreachable |

**One enum change was required.** The reply `WaiterLost` was returned from *two* positions
with **different** post-states — step (2), before any reservation or copy, and step (5),
after both copies had landed in the caller's address space. One variant cannot carry two
dispositions, so step (5) is now `WaiterLostAfterCopy`.

**Where honest parity does not exist.** For request `WaiterLost`, legacy would find no
endpoint waiter and *queue* the message, returning success. The direct path cannot queue —
the request bytes are already in the server's buffer — so it reports the stale-authority
error rather than inventing parity or risking a duplicate delivery. This is recorded rather
than papered over.

**Proof.** `stage199d_delivery_projection_differential::injection` injects each
deterministically reachable variant through the real drain and asserts the exact error, its
disposition, zero reply-record/cap/waiter/wake leak, and zero duplicate delivery. For both
copy faults the frame error code is compared against the code the **legacy path actually
produces** for the same injected condition (an unmapped destination), measured in the same
test. The pure mapping is covered exhaustively by `direct_disposition`'s own tests, whose
per-variant tables are the mutation guards: every arm is pinned individually, the tables are
asserted duplicate-free and count-exact, and `failure()`/`permits_legacy_fallback()` are
asserted mutually exclusive.

**Race-only variants.** `ServerGone`, reply `CallerGone`, `WaiterLostAfterCopy`,
`RecordCommitFailed` and `RecordConsumeFailed` cannot be injected deterministically in a
single-threaded hosted build: the prevalidate and commit steps evaluate the *same* predicate
with no yield between them, the two waiter reads hit the same slot, and the record-commit
branches are defensive. No production test hook was added to force them (a guard asserts the
transaction carries none). All five are `Failed`, pinned by test.

### 6.1.3 HARD-STOP C — RESOLVED: the successful return lanes match legacy

**The defect.** Legacy `handle_ipc_call` ends its success path with
`frame.set_ok(0, 0, 0)` followed by `encode_transfer_cap_ret(frame, None)?`
(`src/kernel/syscall/ipc.rs`), which writes `ret2 = SYSCALL_NO_TRANSFER_CAP` (`u64::MAX`).
The direct path wrote `set_ok(0, 0, 0)` alone, leaving `ret2 = 0` — a silent divergence from
the legacy ABI on the **successful** path.

**The audit did not confirm NR7 was already correct.** `handle_ipc_reply` ends with the
*identical* two-call encoding, so the direct reply path's `ret2` diverged exactly as NR6's
did. Both are fixed by one shared encoder, which is precisely why sharing it is correct
rather than merely convenient.

**The fix.** `direct_disposition::apply_direct_disposition` is the single frame-encoding site
for both directions:

| Disposition | Frame effect |
|---|---|
| `Completed` | `set_ok(0, 0, SYSCALL_NO_TRANSFER_CAP as usize)` — the sentinel comes from the same constant `encode_transfer_cap_ret` writes, not a duplicated literal |
| `Failed(err)` | `set_err(err.code())`, which zeroes `ret0`/`ret1`/`ret2` first, so no stale success value — including the sentinel — can survive |
| `DeclinedBeforeMutation` | nothing written; the legacy fallback receives a pristine frame |

Neither split helper writes a return lane after the drain (asserted structurally).

**Proof.**

* `direct_request_success_lanes_match_legacy_byte_for_byte` — runs a SUCCESSFUL legacy
  `IpcCall` trap, captures `error`/`ret0`/`ret1`/`ret2`, and compares every lane against the
  production encoder's output. The encoder is fed a **poisoned** frame first, so parity
  cannot be an accident of a zeroed frame.
* `direct_reply_success_lanes_match_legacy_byte_for_byte` — pins at source level that both
  legacy handlers end with the same `set_ok` + `encode_transfer_cap_ret` pair (adjacent
  lines), and that `encode_transfer_cap_ret(_, None)` writes `SYSCALL_NO_TRANSFER_CAP`, then
  asserts the encoder's lanes. Driving a successful legacy `IpcReply` end-to-end
  additionally requires the caller committed-blocked on its reply endpoint, which the
  NR6-oriented fixture does not arrange; the lane *values* are pinned empirically by the
  `IpcCall` baseline and the *encoding* is pinned for both handlers.
* `failed_dispositions_leave_no_stale_success_lane` — starts from a full success frame
  (sentinel present) and asserts every `Failed` arm clears all three lanes.
* `declined_disposition_leaves_the_frame_untouched` — asserts no return lane, no syscall
  argument and not the syscall number changes before fallback.
* `successful_drain_encodes_the_legacy_success_lanes` — end-to-end through the real drain.

**Live attestation.** `yarm-user-rt` gained `ipc_call_with_transfer_ret`, which `ipc_call`
delegates to (one syscall sequence, identical behaviour), returning the transfer-cap lane.
The oracle client compares it against `SYSCALL_NO_TRANSFER_CAP`, emits
`IPCCALL_DIRECT_ORACLE_CLIENT_CALL_RET2`, and **gates** its round-trip completion on it; the
runner requires that marker with the exact numeric sentinel. Observed on the sealed boot:

```
IPCCALL_DIRECT_ORACLE_CLIENT_CALL_RET2 ret2=18446744073709551615
    expected=18446744073709551615 ret2_ok=1 result=ok
```

Replacement seal at commit `a4bb63e3e83e93ecc3e9e33582e493c8b37c33fe` (tree
`2f0fddddfccd2b5018c761a32967804d8f64ea95`), recorded in `doc/IPC.md` §8.6.

**One thing the live run caught that hosted tests could not.** The first attempt also added
`call_ret2_ok=1` to the round-trip completion marker. `user_log` truncates at 192 bytes and
that line was already at the cap, so the boot silently clipped ` result=ok` and the runner
failed closed. The evidence now rides in the dedicated marker and the completion literal is
unchanged; the completion is still *gated* on the attestation. A hosted test could not have
found this — only a real boot through the real logging path.

### 6.1.4 Non-blocking gaps found in the same audit

* **No endpoint-mode check.** The direct transaction never inspects `EndpointMode`. Generic
  production eligibility must restrict it to the mode it actually implements (`Buffered`);
  `Synchronous` endpoints carry rendezvous semantics (`src/kernel/boot/ipc_state.rs:5510`,
  `:6092`) the direct path does not reproduce. Rights *are* already enforced —
  `resolve_endpoint_send_cap_in_pid_from_raw` requires `SEND`
  (`src/kernel/boot/orchestrator_state.rs:2084`) and the reply-endpoint resolver requires
  `RECEIVE`.
* **Observability.** The legacy path emits `IPC_CALL_BEGIN`, `IPC_CALL_WAKE_RECEIVER`,
  `IPC_CALL_SPLIT_DELIVERY` and `IPC_CALL_SENT_OR_QUEUED`; the direct path emits none of
  them. Any log-based check that assumes those markers on a normal boot would change
  meaning under the flip.

### 6.1.5 Status

**All three correctness defects are resolved.** The flip is no longer blocked by the two
gates, the acknowledgement store, delivery conformance (A), error disposition (B), or
return-lane parity (C). What remains is not defect repair but the enablement work itself:

~~A (delivery conformance)~~ **done** → ~~B (error disposition)~~ **done** →
~~C (return-lane parity)~~ **done** → **mode eligibility + production counters** →
**gate removal** → **live flip proof**.

**Mode eligibility and the production counters have landed.**
`src/kernel/direct_eligibility.rs` is the pure, exhaustive contract: NR6 requires `SEND`
rights, a current endpoint incarnation, `Buffered` mode and a supported message shape, and
`Synchronous` endpoints decline before any mutation to the legacy rendezvous path; NR7 stays
tied to a live one-shot `Reply` object with no invented mode requirement.
`src/kernel/direct_ipc_counters.rs` adds per-direction attempts / eligible /
declined_ineligible_mode / declined_preflight / declined_pre_transaction / completed /
failed_by_error_code / legacy_fallback_after_decline, plus the ack lifecycle and every
fail-closed fuse, with a balance invariant proved live (`balanced=1`, both directions). The
non-blocking gap §6.1.4 recorded is closed. See `doc/IPC.md` §8.6.4.

### 6.1.6 The gates are removed; the flip is HELD OFF on four new live blockers

**The gate removal has landed.** The outer proof-gate admission, the oracle request/reply
endpoint confinement in eligibility, and the oracle-only filtering at both acknowledgement
publication sites are all gone, replaced by arch-split predicates
(`ipccall_direct_admission_enabled`, `ipccall_direct_publication_enabled`,
`ipccall_direct_{request,reply}_endpoint_admitted`) that consult one compile-time constant.
Oracle selectors survive for scenario setup and diagnostics only; structural guards pin that
the split dispatcher contains no proof-gate and no oracle-endpoint reference, and that
endpoint admission short-circuits on the production constant. AArch64 and RISC-V still
resolve to the proof gate and the oracle confinement, so their boots are byte-identical.

**The flip itself is held off.** `ipccall_direct_production_enabled()` returns `false`;
changing it to `cfg!(target_arch = "x86_64")` is the whole enablement. It was flipped, built
and run on a normal feature-off x86_64 boot, and hard-stopped:

| constant | `scripts/qemu-x86_64-core-smoke.sh` |
| --- | --- |
| `false` | `all 6 service entries present exactly once` |
| `cfg!(target_arch = "x86_64")` | 6 entries **missing**, `PM_ELF_ZC_FAIL`, boot times out |

1. **Capability transfer is silently dropped** (fatal). The direct NR7 path never reads
   `SYSCALL_ARG_TRANSFER_CAP`; legacy `ipc_reply` does, and stashes a transfer handle. A
   cap-bearing reply taken by the direct path loses the capability, so VFS's read-only grant
   never reaches PM and the blkcache / virtio-blk / driver-manager chain never spawns.
2. **The acknowledgement store has no production release path.** Publication is driven by
   blocking, consumption only by direct delivery; any legacy-satisfied recv orphans a
   `Committed` slot forever.
3. **Orphans trip the overwrite fuse** — 17 trips on one short boot.
4. **Capacity pressure is structural** — capacity 8, more than 8 servers blocked at once.

Blockers 2–4 are one unpaired-lifecycle defect; blocker 1 is independent and is the one that
breaks the boot. All four were invisible under the confinement, which is exactly what the
confinement was hiding. Full evidence, live markers and fix directions: `doc/IPC.md` §8.6.5.

**No production-default seal is issued.** The oracle regression still passes
(`live_cells=2 result=ok`) and the feature-off boot is healthy with the flip held off. The
remaining sequence is: **transfer-cap handling** → **ack lifecycle release** → **capacity
re-derivation** → **live flip proof**.

### 6.1.7 Blockers 1–3 closed; capacity is the only one left

**Transfer-cap safety.** `DirectReplyFacts::transfer_cap_present` +
`DirectReplyEligibility::TransferCapUnsupported`: a cap-bearing reply declines before any
mutation and the legacy path does the transfer. The presence question is asked through the one
canonical `ipc_abi::transfer_cap_arg_present` predicate the legacy decode is itself built on, so
the two cannot disagree — including that a raw `0` *is* a capability id. The check precedes every
capability resolution, so a cap-bearing reply can never reach an acknowledgement claim or the
transaction, and the transaction contains no transfer machinery at all. Direct capability
transfer remains unimplemented.

**The acknowledgement lease is owned by the endpoint waiter lifecycle.**
`DirectAckStore::release` is a fourth slot state (`Released`) and the non-direct terminal edge,
exact in endpoint index, endpoint generation, waiter TID and waiter ASID. It is called from
`IpcSubsystem::release_direct_ack_lease`, which is called from exactly the three waiter-removal
primitives every canonical closing edge funnels through — `take_endpoint_waiter`,
`clear_endpoint_waiter_if_identity`, `clear_endpoint_waiters_for_identity` — and nowhere else.
Direct consume and non-direct release are mutually exclusive terminals, proved by two 200-run
deterministic races (edge-vs-edge, and release-vs-slot-recycle).

**Live, feature-off x86_64 boot with the flip temporarily enabled:** the service chain is fully
healthy (**all 6 service entries present exactly once**, `PM_ELF_ZC_FAIL count=0` — blocker 1
gone), the overwrite fuse went from **17 trips to 0**, and **52 NR6 / 64 NR7** leases were
retired by their departing waiters. **10** cap-bearing replies declined to legacy.

**The only remaining blocker is capacity.** The genuine post-release high-watermark is **8 —
full capacity** — with one `CAPACITY_REFUSED` per store. The eight live leases are not orphans:
`reserve == consume + release + cancel + live` is exact in both directions (113 == 53+52+0+8 and
113 == 41+64+0+8). Ten distinct servers blocked over the boot, and at `INIT_IDLE_PARK_BEGIN` the
resident services are all parked holding legitimate leases. `DIRECT_ACK_STORE_CAPACITY = 8` is
simply smaller than the steady-state parked-server count, so a ninth endpoint gets no lease and
falls back to legacy. Resizing was explicitly out of scope for this increment.

Two measurement corrections the live boot forced: the quiescent trigger moved to
`INIT_IDLE_PARK_BEGIN` (the previous trigger read `high_watermark=2` before the store went on to
saturate), and **`live == 0` is not a valid quiescence requirement for a running microkernel** —
`QuiescentVerdict::ok` now requires `no_orphaned_lease` instead. Full evidence: `doc/IPC.md`
§8.6.6.

### 6.1.8 Blocker 4 closed — x86_64 NR6/NR7 is the production default

**Capacity is derived, not chosen.** `DIRECT_ACK_STORE_CAPACITY` is
`crate::kernel::boot::ENDPOINT_WAITER_SLOTS` — the length of the authoritative endpoint
receive-waiter table — with compile-time assertions pinning `>=` and `==` against it. A lease
exists exactly while an endpoint receive-waiter does, so the bound is exact rather than merely
sufficient. The store is one slot per endpoint index, which makes endpoint uniqueness and the
absence of capacity exhaustion structural, turns reservation into a single compare-exchange,
removes the leaf admission spinlock entirely (every store operation is now lock-free), and
removes the release-vs-recycle race with it.

**An independent waiter census** (`src/kernel/direct_ack_census.rs`) measures the same
population from the endpoint receive-waiter table, deliberately unbounded by the store's
capacity — that is what lets it detect an under-sized store rather than agreeing with one. It
reports current and high-watermark waiter counts maintained by the waiter table's own mutators,
and a final two-pass bijection matching live leases against eligible waiters on the complete
`{endpoint_index, endpoint_generation, waiter_tid, waiter_asid}` identity. It runs on the IPC
(rank 3) and task (rank 2) split seams, never simultaneously, so it adds no broad-lock
acquisition; `tests/broad_lock_census_guard.rs` enforces that.

**The flip is healthy — and held off on a fifth blocker.** With the constant temporarily
enabled, the normal feature-off x86_64 boot is fully healthy: `YARM_BOOT_OK`, all 6 service
entries exactly once, `PM_ELF_ZC_FAIL count=0`, **53 NR6 and 41 NR7 ordinary syscalls completed
on the direct path with zero broad-lock entries**, zero capacity refusals, zero overwrite-fuse
trips, zero stale/foreign/duplicate/crossed terminals, and an exact lease/waiter bijection in
both directions (`IPC_DIRECT_PRODUCTION_QUIESCENT_SEAL nr6_ok=1 nr7_ok=1 census_ok=1
result=ok`). Ten cap-bearing NR7 replies correctly stayed on the mutation-free legacy fallback.
The x86 direct NR6/NR7 oracle regression passes with the flip on (`live_cells=2 result=ok`).

`live=9` and `terminal_edges=0` are reported, not gated: they are nine resident services
legitimately parked in recv-v2, each holding a valid lease. `no_orphan=1` —
`reserve == consume + release + cancel + live`, 114 == 53+52+0+9 and 114 == 41+64+0+9 — is the
leak invariant that holds on a running system, and the census confirms it independently.

**No seal is issued and the constant is restored to `false`: the ServerDies regression fails.**
`SharedKernel::register_server_reply_link_split` — the direct NR6 transaction's reverse-link
installation — writes `tcb.server_reply_link` but does not stamp
`server_dies_counters::note_link_created`, while its legacy twin
`KernelState::register_server_reply_link` does. With the direct path as the default, every
request installs a link the system-wide leak accounting never counts as created while the close
edge still counts: `IPC_SERVER_DEATH_LINK_LEAK created=0 closed=13 scope=system result=fail`.
The links are installed and closed correctly — this is an instrumentation gap in the split twin,
not a link leak — but while it is open the attestation that would detect a *real* reverse-link
leak is blind on the production path. Fix: stamp the creation edge in the split twin as the
legacy one does, then re-run ServerDies.

AArch64 and RISC-V are untouched and remain proof-gated. Full evidence: `doc/IPC.md` §8.6.7.

### 6.1.9 Creation parity closed; the close edge mirrors it

**Reverse-link CREATION accounting is now identical on both installation paths.** The legacy and
split seams carried independent copies of the same decision and drifted — the split twin
installed the link without stamping `note_link_created`. Both now delegate to
`boot::install_server_reply_link`: one status gate, one set of match arms, one stamp, with the
stamp unreachable from every refusal arm. Live, `created` went from 0 to 54.

**That exposed the mirror defect on the CLOSE edge.** Of four close sites only two stamp
`note_link_closed`; `SharedKernel::unregister_server_reply_link_split` (the direct NR7 close) and
`rt_detach_server_reply_link` (reply-timeout domain) do not. With the direct path as the
production default the totals read `created=54 closed=13` — the 41 missing closes being exactly
the 41 direct NR7 completions — and the ServerDies seal fails. As with creation the links are
correct and this is an accounting gap, but the leak attestation is now wrong in the permissive
direction on the production path.

**Everything else about the flip is proven healthy** at commit `c94cd304`: `YARM_BOOT_OK`, all 6
service entries exactly once, `PM_ELF_ZC_FAIL count=0`, 53 NR6 and 41 NR7 ordinary syscalls
completed off-lock with zero broad-lock entries, zero capacity refusals, zero fuse trips, exact
lease/waiter bijection (`IPC_DIRECT_PRODUCTION_QUIESCENT_SEAL nr6_ok=1 nr7_ok=1 census_ok=1
result=ok`), and the oracle regression passing with the flip on (`live_cells=2 result=ok`).

The constant is restored to `false`.

### 6.1.10 Close parity closed; terminal-arbitrated replies declined; **x86_64 production default ON**

**Reverse-link CLOSE accounting is identical on all four closing paths**, which delegate to
`boot::close_server_reply_link` with an `Exact`/`Any` selector. Exactly one close mutation and
one `note_link_closed` call remain in the tree, mirroring creation.

**Terminal-arbitrated replies are explicitly ineligible for direct NR7.** A reply whose record
is arbitrated by an armed terminal-ownership / reply-timeout cell must reserve its terminal
before the caller copy and commit it after; that lease lives only on the legacy path, so
servicing one off-lock made the reply reserve, roll back and lose to the timeout's deferred
path. `DirectReplyFacts::terminal_arbitrated` is read from the authoritative
`reply_terminal_ownership` cell, exact in record index AND generation, under one rank-3
acquisition — and arming provably precedes reply deliverability, so it is not a racy test.
Porting the lease into the direct transaction is recorded as future canonical **199E** work.

**FIRST x86_64 NR6/NR7 PRODUCTION-DEFAULT LIVE SEAL — exact commit `0b5ec254`.**

* **Core boot:** `YARM_BOOT_OK`, all 6 service entries exactly once, `PM_ELF_ZC_FAIL count=0`,
  **53 NR6 + 41 NR7 ordinary syscalls off-lock with zero broad-lock entries**,
  `IPC_DIRECT_PRODUCTION_QUIESCENT_SEAL nr6_ok=1 nr7_ok=1 census_ok=1 result=ok`, waiter/lease
  bijection `result=ok` both directions.
* **Oracle regression:** `live_cells=2 result=ok`.
* **ServerDies:** vector `[1;9]`, `created=54 closed=54 live_links=0`, one PeerDeath winner, one
  caller wake, `result=ok`.
* **x86 reply-timeout matrix, both cells, zero `[fail]` lines:** reply-wins with
  `IPC_REPLY_WIN_RESERVE=1`, `IPC_REPLY_BEATS_TIMEOUT_OK=1`, `IPC_REPLY_WIN_ROLLBACK=0`,
  `IPC_REPLY_TIMEOUT_DEFERRED=0` and `arbitrated=1`; timeout-wins unchanged with
  `late_reply=rejected`.

Zero `result=fail`, leak, duplicate, stale or fatal markers in any run. The AArch64 and RISC-V
matrix cells could not be executed — `qemu-system-aarch64`/`qemu-system-riscv64` are not
installed in this environment — and neither architecture was changed. Canonical 199D remains
open. Full evidence: `doc/IPC.md` §8.6.9.

### 6.1.11 AArch64 NR6/NR7 readiness — NOT READY, three blockers

**The canonical contract stack is already AArch64-ready.** Eligibility, disposition, the
acknowledgement store, the waiter census, the counters, the projection and `ipccall_direct.rs`
contain **zero** `target_arch` references; the transaction body has two, both selector-gated
x86 SMP-oracle IPI sends. Neither the transaction nor the split helper takes a broad lock. No
AArch64 semantic copy is needed.

**Three blockers, all in the AArch64 arch bracketing:**

1. **The syscall-ABI import is proof-gated.** `pre_split_import_syscall_abi` admits NR6/NR7
   only under `ipccall_direct_proof_enabled()`, so with the proof gate off `nr` stays 0 and the
   split dispatcher declines — flipping the production predicate alone would be a silent no-op
   on AArch64. *Fix:* ask the canonical `ipccall_direct_admission_enabled()`.
2. **The split return path reacquires the broad lock — decisive.**
   `finalize_split_handled_syscall` calls `shared.with_cpu(...)` to save the user context,
   restore arch thread state and export x0..x5. Every HANDLED AArch64 split syscall, NR6/NR7
   included, takes the broad lock on the way out. x86_64's finalize is an empty no-op because
   its trap stub returns from the ret lanes. *Fix:* move those three steps onto task-domain
   (rank 2) seams, or prove they need no kernel state. Real work, not a config flip.
3. **Off-lock authoritative dispatch is x86_64-only.** `d6_genuine_enabled()` is
   `cfg!(target_arch = "x86_64")`, so an AArch64 wake's downstream dispatch and saved-frame
   resume run under the broad lock. The transaction still completes (it only enqueues), but the
   end-to-end wake is not off-lock.

Nothing was staged live; the production default is unchanged (x86_64 only).
`stage199d_aarch64_readiness_audit` pins all three blockers and the two ready properties, so the
map is executable. `qemu-system-aarch64` is also absent here, so no AArch64 live suite could
run regardless. Full evidence: `doc/IPC.md` §8.6.10.

### 6.1.12 AArch64 blockers 1 and 2 CLOSED; blocker 3 still open

Structural preparation only — **the AArch64 production default remains OFF**, no live AArch64
seal was issued, no RISC-V work, no dispatch retirement.

1. **CLOSED — the syscall-ABI import uses the canonical predicate.** Both
   `pre_split_import_syscall_abi` and its return-path twin `finalize_split_handled_syscall`
   admit NR6/NR7 through `ipccall_direct_admission_enabled()`, the same predicate the split
   dispatcher uses; **no `ipccall_direct_proof_enabled()` call survives in
   `src/arch/trap_entry.rs`**, so AArch64 has no architecture-specific admission rule. The
   predicate is still `production || proof` with production `cfg!(target_arch = "x86_64")`, so
   AArch64 still resolves to the armed proof gate and a normal boot is byte-identical.
2. **CLOSED — the handled split return takes no broad lock.** The `shared.with_cpu(...)`
   wrapper is gone. `split_finalize_handled_syscall` is driven by an exact entering identity
   `SplitReturnIdentity { tid, asid }` captured *before* `try_split_dispatch_into_frame` and
   threaded to both finalize call sites, so nothing after the direct transaction re-discovers an
   unqualified "current task". Work is split into frame-only steps outside every lock (resume PC
   from `last_vector_raw_elr()` with no extra `+4`, `export_syscall_result_to_user_gprs`, the
   `args[0..2]` resync, diagnostics) and **two bounded rank-2 task-domain transactions**:
   `split_return_take_tls_split(id)` and `split_return_commit_context_split(id, ctx)`, both
   exact-incarnation validated and both reaching storage through
   `KernelState::task_return_split_mut_ptrs_from_raw` — one rank-2 lock over two same-domain
   storages. **The pre-export save → restore → read-back round trip was proved redundant and
   removed**, not relocated: `apply_user_context(capture_user_context())` is an exact nine-field
   identity, the TCB setter/getter are verbatim, the read-back was the save's only consumer, and
   the post-export save overwrites it before anything observes it. The TLS take and the
   stale-incarnation bail are kept. Byte-for-byte preserved: success and error lanes, ELR/SPSR/SP
   and all user GPRs, x18 TLS, stale-identity behaviour, and every existing AArch64 split class.
   No fallback to the broad path after a handled direct transaction.
   *Census effect:* `src/arch/trap_entry.rs` 12 → 11; tree total 51 → **50**;
   `AUDITED_WITH_CPU_TOTAL` 41 → **40**; `CLASS_RUNTIME_REQUIRED` 46 → **45**. No new
   broad-lock acquisition site was introduced.
3. **STILL OPEN — off-lock authoritative dispatch is x86_64-only.** `d6_genuine_enabled()` is
   unchanged. The sole remaining gating item for an AArch64 production flip.

`stage199d_aarch64_readiness_audit` now pins 1 and 2 closed and 3 open;
`stage199d_split_return_without_broad_lock` adds 11 differential and structural tests against
the legacy AArch64 non-task-switched return. Full evidence: `doc/IPC.md` §8.6.11.

### 6.1.13 AArch64 blocker 3 CLOSED structurally; live acceptance pending

The authoritative queue-advancing dispatch — the step that picks the next runnable task and
actually resumes it — was reachable only through `d6_genuine_enabled()`, a compile-time x86_64
constant, so an AArch64 wake finished under the broad lock even when the direct transaction had
not taken it. **The AArch64 production default remains OFF; no flip, no live seal, no RISC-V
work, no dispatch retirement.**

**Classification.** NR6 publishes post-lock dispatch work exactly at the reply-blocked commit —
after Phase A removed the caller from `current`, Phase B committed
`Blocked(EndpointReceive(reply_cap))` and Phase C published the waiter. NR7 publishes **nothing**:
the replying server stays `current`, the caller is woken/enqueued once inside the transaction,
and the replier returns through §6.1.12's narrow handled-return finalizer. Enforced twice — a
reply never reaches the publishing commit, and `try_publish` independently refuses the
`IpcReply` class.

**The work item** is typed and generation-bearing:
`DirectDispatchWork { outgoing_tid, outgoing_asid, blocked_generation, cpu, class }`. `{tid,
asid}` identifies the incarnation, `blocked_generation` the block, `cpu` confines it to its
publisher, `class` makes the direction a type. Publication is a per-CPU compare-exchange
(single-shot; a second declines rather than overwrites) and the drain **takes** it destructively,
so one item drives at most one dispatch.

**The drain**, with the broad guard already dropped: (1) revalidate the exact outgoing
incarnation and committed blocked state (rank 2); (2) **one** authoritative dequeue through the
rank-1 scheduler seam; (3) mark Running (rank 2) and confirm the authoritative `current` slot
agrees with the selection; (4) activate ASID/TTBR0 through the same arch primitive and ordering
the in-lock path uses; (5) restore the complete EL0 frame, x18 TLS and any parked blocked-syscall
completion; (6) return through the existing eret model, or the existing `idle_no_eret_loop()`
primitive when nothing is runnable.

Steps 2–3 and the idle outcome **reuse the existing machinery** — the same rank-1 dequeue and
rank-2 mark-Running seam FutexWait/Yield use, and the same idle loop — so there is one scheduler
policy in the tree, not two. What differs is that this drain takes **no broad lock**: what those
drains obtain from a brief `with_cpu` re-acquire, this obtains from bounded rank-2 seams, each
acquired and released before the next (no nested rank acquisition). **AArch64 FutexWait/Yield
behaviour is unchanged.**

To make step 4 possible without a `KernelState` mutation, the HAL's active-ASID record moved out
of `SelectedIsaHal` into a lock-free cell that `SelectedIsaHal::active_asid()` now reads — one
authority, not two, so on- and off-lock activations are observed identically.

**Races are exhaustive and fail closed.** `DrainOutcome` has no wildcard arm: `NoWork`,
`Dispatched`, `Idle`, `StateChangedBeforeDrain`, `OutgoingAlreadyRunnable`, `StaleIdentity`,
`DispatchCurrentDisagreement`. Exactly one advances the queue; every other resumes nothing and
mutates nothing. **No broad-lock fallback exists after a direct transaction has committed.**

**Admission.** `d6_genuine_enabled()` is unchanged and still x86_64-only (it gates the D6
queue-neutral slice and three `exec_state` decisions). AArch64 is admitted by the canonical
replacement `offlock_authoritative_dispatch_enabled()`, which resolves to
`ipccall_direct_admission_enabled()` there — and because production is x86_64-only, that is the
armed proof/oracle gate. An ordinary AArch64 boot publishes nothing and drains nothing.

**Census: unchanged at 50**, with a new guard
(`stage199d_blocker3_added_no_broad_lock_acquisition_site`) asserting it stays at 50 or
decreases and that the drain acquires no broad lock.

`stage199d_aarch64_offlock_dispatch` (17 tests) and `kernel::direct_dispatch` (7 tests) carry the
proof obligations. Live acceptance is pending: `qemu-system-aarch64` is absent here. Full
evidence: `doc/IPC.md` §8.6.12.


### 6.1.14 Correctness repair of the post-lock dispatch

The §6.1.13 landing had four defects. All are repaired; the AArch64 production default is still
OFF and no live seal was issued.

1. **Publication protocol.** The `PENDING` boolean conflated *being written*, *readable* and
   *being read*, so it was correct only under an unstated non-reentrancy assumption. Replaced by
   an explicit per-CPU state machine `EMPTY → WRITING → READY → READING → EMPTY`: a publisher
   claims only `EMPTY`, a taker only `READY`, and the slot recycles only after the payload has
   been copied out. A second publisher can never overwrite `WRITING`/`READY`/`READING`, and a
   publisher can never overwrite a payload a reader still holds.

2. **The `current`-clear is a DEBT (the serious defect).** The drain treated its pre-mutation
   revalidation as a verdict: a caller a reply or timeout had made `Runnable` produced
   `OutgoingAlreadyRunnable`, the drain returned "declined", and the CPU `eret`-ed through a
   parked task's frame with `current` still `None` and the woken task still queued. The
   revalidation is now an **observation** used for diagnostics only, and settlement is
   unconditional: every taken debt ends in `Dispatched` or `Idle`. A wake before the drain does
   not cancel the debt — the woken caller is a normal dequeue candidate. After the dequeue has
   mutated scheduler state, a later failure rolls back **exactly** (status, `current`, queue —
   via the existing `preempt_reenqueue_only_on` inverse) and takes an **explicit fatal path**
   that never returns to userspace. The only remaining no-debt exit is a superseded lease.

3. **The generation claim.** `tcb.blocked_recv_generation` is never incremented anywhere in the
   tree — always 0, so it could not distinguish any two cycles. The claim is withdrawn and
   replaced by a per-CPU **dispatch lease**: a monotonic epoch opened at exactly one site (the
   `current`-clear commit) and carried in the item, so `lease_is_current` decides staleness for
   real. Changing the reply-timeout arbitration that also reads that field stays out of scope.

4. **`ACTIVE_ASID` was one global cell.** `TTBR0_EL1`/`CR3` are per-core registers, so a single
   cell let one core's activation overwrite another's. The record is now a per-CPU table keyed
   by `CpuId`; `Hal::switch_address_space` takes the `CpuId` explicitly; `active_asid_on(cpu)`
   replaces `active_asid()` and all five consumers pass `self.current_cpu()`.

Census unchanged at **50 / 40 / 45** — the repair added no broad-lock acquisition. Because the
HAL authority changed globally, the x86_64 live core-boot and ServerDies regressions were re-run
alongside the hosted battery. Full evidence: `doc/IPC.md` §8.6.13.

### 6.1.15 Canonical Stage 199D closure audit — `CANONICAL_199D_CLOSABLE=no`

An audit increment: **no runtime semantics, production predicate or target-spec changed**. It
reconciles the live-evidence ledger and classifies every coordinate of canonical 199D against the
tree.

**Ledger reconciliation.** The accepted pre-production subtotal was **39** (30 Stage 198F + 6
reply-timeout matrix + 2 `ExitCurrentTask` + 1 x86_64 ServerDies). The x86_64 NR6/NR7
production-default seal at `0b5ec254` is **one** production-path increment — the first live
evidence of NR6/NR7 running off-lock with `ipccall_direct_production_enabled()` true rather than
under a proof knob — so the production-path total is **40**. The six x86 SMP direct-IPC cells
frozen by `STAGE_199_X86_DIRECT_IPC_FINAL_SEAL` are knob-gated **mechanism** evidence; their
re-earning at `7d5a22c9` (§0.1 of `doc/STATUS.md`) preserves historical evidence and **adds no
new cell**, so the combined total is **46**. The superseded pair "39 / 45" and the never-coherent
"43" are retired. The arithmetic is recomputed from its constituent seals by
`stage199d_live_evidence_ledger` (6 tests), and `PROJECT_HISTORY.md` carries the previously
missing `0b5ec254` row.

| Production / proof-gated / total | Figure | Arithmetic |
|---|---|---|
| Production-path | **40** | 30 + 6 + 2 + 1 + 1 |
| Proof-gated (knob-gated mechanism) | **6** | `STAGE_199_X86_DIRECT_IPC_FINAL_SEAL`, counted once |
| Total | **46** | 40 + 6 |

#### Taxonomy

Four in-scope classifications, plus one exclusion:

| Classification | Meaning |
|---|---|
| `COMPLETE` | Landed, exercised hosted, and proven by live evidence on every architecture the coordinate claims. |
| `STRUCTURALLY_COMPLETE` | Landed and exercised hosted; live evidence is not obtainable here. Carries a blocker kind. |
| `PARTIAL` | Landed and sealed for some architectures / sub-paths only. |
| `OPEN` | Not retired. |
| `DEFERRED_TO_CANONICAL_199E` | **Not a 199D coordinate at all.** Excluded from the tally and from `CANONICAL_199D_CLOSABLE` — it can neither close 199D nor block it. |

Blocker kinds are distinguished because a missing emulator, a missing production flip and missing
code are not the same debt:

* `LIVE_EVIDENCE_PENDING` — code landed and enabled; only the live run is missing.
* `LIVE_EVIDENCE_PENDING_AND_CONDITIONAL_PRODUCTION_ENABLEMENT` — live evidence is unobtainable
  here **and** the architecture's production predicate must be enabled first, in a stated order.
  Still not a code blocker.
* `CODE_THEN_ENABLEMENT_THEN_EVIDENCE` — the code does not exist yet, and reaching live evidence
  needs an ordered chain beyond writing it.
* `OUT_OF_SCOPE_199E` — tracked against canonical 199E.

#### Evidence is bound to the coordinate it proves

The previous revision of this audit checked only that a marker literal existed **somewhere** in
the tree. That is not evidence: it let `IPC_DIRECT_TRANSFER_CAP` — a transfer-cap counter dump,
emitted by `emit_direction` — stand as the proof for the reply-vs-timeout terminal race, a
proposition it says nothing about. Every evidence entry now names the **file and the emitting
function** whose body must contain the literal, plus the exact observation (a field the emitter
actually prints, or a live count this audit actually records). A marker emitted by an unrelated
reporter can no longer be borrowed. `stage199d_closure_matrix` enforces this in
`every_evidence_marker_is_emitted_by_the_function_that_claims_it` and
`every_evidence_observation_is_supported_by_emitter_and_audit`.

#### The closure matrix

23 in-scope coordinates plus 1 excluded deferral. `stage199d_closure_matrix` (12 tests) verifies
that every named seam and test module exists, that every evidence marker is emitted by the
function that claims it with the observation it claims, that no live-pending or open cell claims
evidence, that blocker kinds agree with status, and that the verdict is **computed** from the
in-scope matrix rather than asserted beside it.

| # | Coordinate | Status | Seam | Test | Live evidence (emitter → observation) |
|---|---|---|---|---|---|
| 1 | NR6 reply-record creation | COMPLETE | `reserve_direct_reply_record_split` | `stage199d_ack_lease_lifecycle` | `IPC_DIRECT_ACK_COUNTERS` (`emit_direction`) → `reserve=` |
| 2 | NR6 provisional reply-cap mint | COMPLETE | `sr_mint_split` | `stage199d_ack_lease_lifecycle` | `IPCCALL_DIRECT_REQUEST_OK` (`emit_ipccall_direct_request_live_markers`) |
| 3 | NR6 reverse-link installation accounting | COMPLETE | `install_server_reply_link` | `stage199d_link_creation_parity` | `IPC_SERVER_DEATH_LINK_BALANCE_QUIESCENT` (`maybe_emit_server_dies_link_balance`) → `created=` |
| 4 | NR6 server wake (enqueue LAST, non-fallible) | COMPLETE | `sr_enqueue_committed_receiver_split` | `stage199d_remote_wake_authority` | `IPCCALL_DIRECT_REQUEST_OK` (`emit_ipccall_direct_request_live_markers`) |
| 5 | NR6 delivery projection (inline-opcode framing parity) | COMPLETE | `project_recv_delivery` | `stage199d_delivery_projection_differential` | `X86_AP_RECV_V2_USER_VALIDATED` (`build_ap_workload`) |
| 6 | NR7 one-shot record consumption (duplicate barrier) | COMPLETE | `consume_reply_record_split` | `stage199d_link_close_parity` | `IPCREPLY_DIRECT_SMP_DUPLICATE_REFUSED` (`try_split_ipcreply_direct_into_frame`) |
| 7 | NR7 reverse-link close accounting | COMPLETE | `close_server_reply_link` | `stage199d_link_close_parity` | `IPC_SERVER_DEATH_LINK_BALANCE_QUIESCENT` (`maybe_emit_server_dies_link_balance`) → `closed=` |
| 8 | NR7 caller wake (enqueue LAST, non-fallible) | COMPLETE | `sr_claim_endpoint_waiter_split` | `stage199d_remote_wake_authority` | `IPCREPLY_DIRECT_OK` (`emit_ipcreply_direct_live_markers`) |
| 9 | Local enqueue authority — no IPI | COMPLETE | `current_cpu_split_read` | `stage199d_remote_wake_authority` | `IPC_DIRECT_PRODUCTION_QUIESCENT_SEAL` (`maybe_emit_quiescent_attestation`) → `result=` |
| 10 | Remote enqueue authority — one IPI at the home CPU | COMPLETE | `send_reschedule_ipi_to` | `stage199d_remote_wake_authority` | `X86_AP_RESCHEDULE_IPI_SENT` (`send_reschedule_ipi_to`) |
| 11 | Reverse (NR7) remote enqueue authority | COMPLETE | `c2c_send_reschedule_ipi_to` | `stage199d_remote_wake_authority` | `X86_BSP_RESCHEDULE_IPI_SENT` (`c2c_send_reschedule_ipi_to`) |
| 12 | Transfer-cap decline before mutation → legacy | COMPLETE | `transfer_cap_arg_present` | `stage199d_transfer_cap_safety` | `IPC_DIRECT_TRANSFER_CAP` (`emit_direction`) → `declined_transfer_cap=` |
| 13 | **Terminal-arbitrated NR7 declines before mutation; legacy wins the causal reply-vs-timeout race** | **COMPLETE** | `reply_record_terminal_arbitrated_split_read` | `stage199d_terminal_arbitration_safety` | **the causal set — see below** |
| 14 | *Terminal-lease port into the direct transaction (direct-path arbitration)* | **`DEFERRED_TO_CANONICAL_199E`** *(excluded from the tally)* | `reserve_reply_win_before_copy` | `stage199d_terminal_arbitration_safety` | — |
| 15 | Caller exit / replacement terminal race | COMPLETE | `direct_caller_exact_still_blocked` | `stage199d_ack_lease_lifecycle` | `IPC_DIRECT_ACK_FUSES` (`emit_direction`) → `stale=` |
| 16 | **Server death terminal race (reply-link cleanup) — x86_64 only** | **PARTIAL** · `LIVE_EVIDENCE_PENDING` | `close_server_reply_link` | `stage199d_link_close_parity` | `STAGE_200D2B1C_X86_64_SERVER_DIES_SEAL` → `live_cells=1` |
| 17 | Stale generation / foreign / duplicate release | COMPLETE | `release_endpoint_index` | `stage199d_ack_lease_lifecycle` | `IPC_DIRECT_ACK_FUSES` (`emit_direction`) → `dup_release=` |
| 18 | Recycled-slot behaviour (positional, endpoint-keyed) | COMPLETE | `direct_ack_store` | `stage199d_ack_lease_lifecycle` | `IPC_DIRECT_ACK_COUNTERS` (`emit_direction`) → `spent_released=` |
| 19 | Waiter census / lease bijection | COMPLETE | `direct_ack_census` | `stage199d_waiter_census` | `IPC_DIRECT_WAITER_BIJECTION` (`emit_census`) → `waiters_without_lease=` |
| 20 | x86_64 production default ON + live sealed | COMPLETE | `ipccall_direct_production_enabled` | `stage199d_production_default_guards` | `IPC_DIRECT_PRODUCTION_QUIESCENT_SEAL` (`maybe_emit_quiescent_attestation`) → `nr6_ok=` |
| 21 | **AArch64 off-lock NR6/NR7 + authoritative dispatch** | **`STRUCTURALLY_COMPLETE`** · `LIVE_EVIDENCE_PENDING_AND_CONDITIONAL_PRODUCTION_ENABLEMENT` | `offlock_authoritative_dispatch_enabled` | `stage199d_aarch64_offlock_dispatch` | — |
| 22 | **AArch64 broad-lock-free handled-syscall return** | **`STRUCTURALLY_COMPLETE`** · `LIVE_EVIDENCE_PENDING_AND_CONDITIONAL_PRODUCTION_ENABLEMENT` | `split_finalize_handled_syscall` | `stage199d_split_return_without_broad_lock` | — |
| 23 | **RISC-V off-lock NR6/NR7** | **OPEN** · `CODE_THEN_ENABLEMENT_THEN_EVIDENCE` | `ipccall_direct_proof_enabled` | `stage199a2c3_matrix_guards` | — |
| 24 | SMP=2 cross-CPU request/reply preservation | COMPLETE | `drain_direct_reply_post_work` | `stage199d_smp_oracle_request_framing` | `STAGE_199_X86_DIRECT_IPC_FINAL_SEAL` → `cross_cpu_request_smp2=1` |

**In-scope tally: 19 COMPLETE, 2 STRUCTURALLY_COMPLETE (live + enablement pending), 1 PARTIAL,
1 OPEN.** Plus 1 `DEFERRED_TO_CANONICAL_199E`, excluded from the tally.

#### Coordinate 13 — the 199D terminal-arbitration safety proposition

What 199D owns here is a **safety** proposition, and it is COMPLETE: a terminal-arbitrated NR7
declines **before any mutation**, so the legacy terminal lease wins the causal reply-vs-timeout
race intact. Its evidence is therefore the causal reply-win set, each entry emitted by the
function that performs the step it attests:

| Evidence | Emitter | Required observation |
|---|---|---|
| `IPC_DIRECT_PRODUCTION_QUIESCENT` | `emit_quiescent` (`src/kernel/direct_ipc_counters.rs`) | `arbitrated=1` |
| `IPC_REPLY_WIN_RESERVE` | `reserve_reply_win_before_copy` (`src/kernel/boot/ipc_state.rs`) | count = 1 |
| `IPC_REPLY_BEATS_TIMEOUT_OK` | `commit_reply_win_after_delivery` (`src/kernel/boot/ipc_state.rs`) | count = 1 |
| `IPC_REPLY_WIN_ROLLBACK` | `rollback_reply_win` (`src/kernel/boot/ipc_state.rs`) | count = 0 |
| `IPC_REPLY_TIMEOUT_DEFERRED` | `drain_reply_timeout_post_work` (`src/runtime.rs`) | count = 0 |

Read together: the reply reserved the terminal once, beat the timeout once, never rolled back,
and the timeout never deferred a claim — while the direct path recorded the arbitrated decline.
That is the causal chain. `IPC_DIRECT_TRANSFER_CAP` is **not** part of it and must never be
assigned here again; `the_terminal_arbitration_coordinate_carries_the_causal_evidence_set` pins
both the required set and that exclusion.

#### Coordinate 14 — deferred, not blocking

Porting the terminal lease **into** the direct transaction, so the direct path can arbitrate
rather than decline, is a canonical **199E** deliverable. 199D's contract is the decline, and the
decline is proven at coordinate 13. Coordinate 14 is recorded so the deferral is visible and
typed — **not** so it can be counted. It is `DEFERRED_TO_CANONICAL_199E`, excluded from the tally
and from `CANONICAL_199D_CLOSABLE`: it can neither close 199D nor block it.

#### Verdict

```
CANONICAL_199D_CLOSABLE=no
```

Three in-scope blockers remain. None is a defect in the landed x86_64 production path.

| Order | Blocker | Kind | Why |
|---|---|---|---|
| 1 | AArch64 off-lock NR6/NR7 + authoritative dispatch (#21) | `LIVE_EVIDENCE_PENDING_AND_CONDITIONAL_PRODUCTION_ENABLEMENT` | Landed and exercised hosted (`stage199d_aarch64_offlock_dispatch`, 17 tests). Needs the sequence below; **not** flipped in this audit-only increment. |
| 2 | AArch64 broad-lock-free handled-syscall return (#22) | `LIVE_EVIDENCE_PENDING_AND_CONDITIONAL_PRODUCTION_ENABLEMENT` | Same sequence, behind #1 — the return path is only observable live once a direct AArch64 transaction runs. |
| 3 | ServerDies live — AArch64 and RISC-V (#16, 1 of 3 earned) | `LIVE_EVIDENCE_PENDING` | x86_64 is sealed (`f5669cb5`). The AArch64 cell falls out of the AArch64 sequence; the RISC-V cell is the **last** link of the separate RISC-V chain. |
| 4 | RISC-V off-lock NR6/NR7 (#23) | `CODE_THEN_ENABLEMENT_THEN_EVIDENCE` | The code does not exist — proof-gated only, with no post-lock dispatch. Four independent links, below; **link 1 (target-spec) is closed, links 2–4 are not**, so the coordinate stays OPEN. |

**AArch64 required sequence.** AArch64 is not "just a missing emulator": live evidence
additionally requires enabling the production predicate, and that enablement is only justified
once the proof/oracle run has passed.

1. proof/oracle QEMU run under `qemu-system-aarch64`, AArch64 production predicate still **OFF**;
2. enable the AArch64 production predicate;
3. on **one exact commit**: normal feature-off boot + direct oracle + ServerDies + timeout
   regressions.

**RISC-V dependency chain — four independent links, not one missing emulator.** Naming RISC-V
alongside AArch64 as a single environment gap was the taxonomy error this repair corrects.

1. **kernel target-spec / toolchain repair** — link 1 is **CLOSED**; see §6.1.16.
2. **RISC-V off-lock NR6/NR7 code** — proof-gated only today, with no post-lock dispatch.
3. **RISC-V production enablement.**
4. **live RISC-V NR6/NR7 and ServerDies evidence.**

Only link 4 is an evidence gap; links 1–3 are code, and each strictly precedes the next.
Coordinate 23 remains **OPEN** on links 2–4.

### 6.1.16 RISC-V dependency-chain link 1 — CLOSED (target-spec only)

A target-spec-only repair. **No runtime semantics, production predicate, ABI, ISA, memory layout
or linker semantics changed**, and no QEMU seal is required or claimed. Coordinate 23 stays OPEN,
the closure tally is unchanged, and the live-evidence ledger is unchanged at 39 / 7 / 46.

**The failure, reproduced at the clean parent `2db42681`:**

```
error: failed to parse target machine config to target machine:
       could not create LLVM TargetMachine for triple: riscv64gc-unknown-none-elf
```

**The rejected field is `llvm-target`.** `riscv64gc` is a **Rust target-name** component, not an
LLVM architecture — LLVM has no `riscv64gc` arch, so `Triple` parsed it as unknown and
`createTargetMachine` failed before any codegen. The filename and Rust target name never had to
equal the LLVM triple, and in this tree they never did elsewhere: every other spec already names
a real LLVM arch (`x86_64-unknown-none`, `aarch64-unknown-none`), and the sibling
`riscv64-yarm-user-none.json` has always declared `riscv64-unknown-none-elf` and built fine.

The accepted triple was derived from the installed toolchain, not substituted blindly. The
toolchain's own built-in `riscv64gc-unknown-none-elf` **Rust target** declares
`llvm-target: "riscv64"` — proof that the `gc` belongs to the Rust name and the ISA belongs in
`features`. Probing rustc 1.99.0-nightly / LLVM 22.1.8 directly:

| Candidate triple | Result |
|---|---|
| `riscv64gc-unknown-none-elf` | **REJECTED** — `could not create LLVM TargetMachine` |
| `riscv64-unknown-none-elf` | accepted |
| `riscv64` | accepted |
| `riscv64-unknown-elf` | accepted |

**The repair is one token** — `riscv64gc-unknown-none-elf` → `riscv64-unknown-none-elf`, matching
the sibling user target. Every other field is byte-identical: `arch` riscv64, features
`+m,+a,+f,+d,+c`, `llvm-abiname` lp64d, little endian, 64-bit pointers, static relocation, medium
code model, max atomic width 64, panic abort, and the same `-Ttargets/riscv64-yarm-none.ld`.

**Proofs.**

1. **rustc creates the target and prints its cfg** — the command that previously failed now
   emits a full cfg set.
2. **`target_arch="riscv64"`; `target_feature` covers `m`, `a`, `f`, `d`, `c`** (plus LLVM 22's
   implied decompositions `zaamo`, `zalrsc`, `zca`, `zicsr`). The ISA/ABI cfg is **identical** to
   the already-working RISC-V user target, so the expansion is the toolchain's, not the repair's.
3. **Freestanding kernel builds feature-off** against the custom spec.
4. **Kernel builds with `riscv-ipccall-direct-oracle`** and with `riscv-shared-region-direct-oracle`.
5. **RISC-V user target still builds.**
6. **Control-plane and fs server packages build** for the RISC-V user target
   (`yarm-control-plane-servers`, `yarm-fs-servers`); `init_server`, `process_manager` and
   `vfs_server` all link as `EXEC`, machine RISC-V, flags `0x5` — the same ABI as the kernel.
7. **The linked kernel ELF is static and correct**: `EXEC` (not `DYN`), entry `0x80200000` =
   `_start` = `__kernel_start` from the linker script, two `PT_LOAD` segments R‑E and RW,
   `.text.boot` first, **no `INTERP`, no dynamic section, 0 undefined symbols, 0 relocations**.
8. **ELF flags `0x5` = `EF_RISCV_RVC | EF_RISCV_FLOAT_ABI_DOUBLE`** — RV64GC + lp64d, intact.
9. **x86_64 and AArch64 freestanding kernel and user checks are unchanged** (all pass).

**Differential against the existing build path.** `scripts/build-qemu-riscv64-artifacts.sh` and
`.cargo/config.toml` drive the RISC-V kernel through the **built-in** `riscv64gc-unknown-none-elf`
Rust target with the YARM linker script applied via `rustflags` — which is why the broken custom
spec never blocked the artifact build. Linking the same binary both ways gives identical
`type=EXEC`, `machine=RISC-V`, `entry=0x80200000` and `flags=0x5, RVC, double-float ABI`, with
`PT_LOAD` addresses, permissions and alignment identical; only segment byte sizes differ slightly
(the built-in target lists `+zicsr,+zifencei` explicitly). **The layout contract is unchanged.**
Nothing in the build path was repointed — that is not a target-spec repair.

**Guards.** `stage199d_riscv_target_spec_guards` (8 tests) pins the LLVM triple, the ISA feature
set and the ABI as three **independent** propositions, so the triple can never be "fixed" by
weakening what it used to imply. The triple's architecture component must be exactly `riscv64`;
`riscv64gc` must never reappear in any RISC-V spec; each of `+m`, `+a`, `+f`, `+d`, `+c` is
asserted individually **by name**; `lp64d` is pinned; the kernel and user specs must agree on ISA
and ABI; and the preserved machine-shape fields plus the `ENTRY(_start)` /
`KERNEL_LOAD_BASE = 0x80200000` contract are pinned. Each guard is mutation-tested: restoring the
`riscv64gc` triple, dropping `+c`, and switching `lp64d` to `lp64` each fail by name.

---


---
### 6.1.17 RISC-V dependency-chain link 2 — AUDIT ONLY: `RISCV_199D_READINESS=case_c`

An audit. **No runtime code, production predicate or target spec changed.** One question:

> Can an eligible RISC-V NR6/NR7 transaction complete end-to-end without entering or re-entering
> the broad `KernelState` lock?

**No.** A return-path code blocker remains even at SMP=1.

```
RISCV_199D_READINESS=case_c
```

Coordinate 23 stays **OPEN**. It is *not* reclassified to
`STRUCTURALLY_COMPLETE / CONDITIONAL_PRODUCTION_ENABLEMENT_AND_LIVE_EVIDENCE`, because that
classification would assert the mechanism is already structurally production-ready, and it is not.

#### The traced path

| Stage | Finding |
|---|---|
| ecall ABI import (`a7`, `a0..a5`) | **Complete.** `yarm_riscv64_trap_bridge` sets `a7` → `syscall_num` and `a0..a5` → `arg(0..5)`; all 31 GPRs are mirrored into the portable frame. `TRAPFRAME_ARG_REGS = 6`. |
| Pre-global-lock decoding and admission | **Present but proof-gated.** `handle_riscv_trap_entry_shared` Phase 1 admits NR6/NR7 — but on `ipccall_direct_proof_enabled()`, **not** the canonical `ipccall_direct_admission_enabled()`. **Blocker 1.** |
| Request/reply eligibility, pre-mutation declines | **Inherited unchanged.** `direct_eligibility` / `direct_disposition` contain **zero** `target_arch` references. A decline mutates nothing and falls to legacy. |
| Acknowledgement claim and direct transaction | **Inherited unchanged and broad-lock-free.** The whole of `ipccall_direct_txn.rs` takes no broad lock; its only two `target_arch` references are the x86 wake sends. |
| Payload/meta projection, transfer-cap sentinel | **Available.** `SYSCALL_ARG_TRANSFER_CAP = TRAPFRAME_ARG_REGS - 1` = `a5` on RISC-V, which the import provides; `SYSCALL_NO_TRANSFER_CAP` is the shared sentinel. |
| Record / reverse-link lifecycle, census | **Inherited unchanged**, architecture-neutral. |
| Caller/server enqueue target | **Authoritative.** `sr_enqueue_committed_receiver_split` returns the CPU it committed to; the drains compare it to `current_cpu_split_read()`. |
| Local vs remote wake authority | **Local: correct on RISC-V.** No IPI for a local target, on any architecture. **Remote: absent.** Both wake sends are `#[cfg(target_arch = "x86_64")]`, and RISC-V exposes no IPI seam — the SBI surface carries HSM (hart start/status) and no IPI extension. **Blocker 3**, latent only because RISC-V is BSP-only. |
| Result-lane encoding | **Parity.** Same-task return writes `a0=ret0`, `a1=ret1`, `a2=ret2`, `a3=error` — the YARM ABI, matching what `apply_direct_disposition` produces. |
| `sepc` advancement | **Exactly once.** One site: `let advance = if scause == EXC_USER_ECALL { 4 } else { 0 };`, pre-applied at import; `handle_trap_entry` deliberately adds no second `+4`. |
| `sstatus`, SATP/ASID, GPRs, `tp`/TLS | **Preserved** — but the SATP activation is one of the broad-lock acquisitions below. `tp` (x4) is mirrored back from the saved frame; the write-back skip list is ABI lanes only and never contains `tp`. |
| Trap return to the issuing task | **Correct**, and the issuer *is* still `current` (see below) — but the resume identity is re-derived through the broad lock. |
| Broad-lock fallback / post-work reacquisition | **Three unconditional acquisitions bracket every trap.** **Blocker 2 — decisive.** |

#### The decisive finding

The RISC-V trap **wrapper** is clean: `handle_riscv_trap_entry_shared` Phase 1 returns
`ReturnToCurrent` *before* the broad-lock phase, without ever setting
`GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE`, so no drain is owed and nothing is left true across the
`sret`. The blocker is in the **bridge that wraps it**. `yarm_riscv64_trap_bridge` calls:

* `let entering_tid = shared.current_tid_authoritative(cpu)` — **before** the split dispatcher;
* `.current_tid_authoritative(cpu)` for `resume_tid` — **after** the handler returns;
* `.with_cpu(cpu, |k| k.task_asid(resume_tid))` — the SATP asid lookup, also after.

`current_tid_authoritative` is `self.with_cpu(cpu, |kernel| kernel.current_tid())` — a broad-lock
acquisition. All three are outside the wrapper, so the Phase-1 early return does not avoid any of
them: **a handled RISC-V NR6/NR7 enters the broad lock three times**, even though the transaction
it performs is entirely broad-lock-free. This is the RISC-V analogue of AArch64 readiness blocker
(ii) — same class of defect, different site.

#### What is NOT a blocker: post-lock dispatch

Neither NR6 nor NR7 clears `current`, so **no post-lock authoritative dispatch is owed on
RISC-V.** Waking a task is not switching to it, and the audit checked this rather than inferring
it:

* **NR6 is request-send-only** — "success returns now (the caller blocks via a later recv)". It
  copies off-lock, claims the ack, runs the transaction, encodes the success lanes and returns to
  its own caller.
* **NR7's replier stays `current`** — it "delivers the reply and wakes the caller"; the replier
  itself returns `Ok`.
* Neither split handler contains a `set_current` or a `dispatch_next_task`.
* The `current`-clear that genuinely owes a post-lock dispatch is in
  `block_current_on_receive_with_deadline` — the **recv** path, a different syscall — and its
  publication is `#[cfg(target_arch = "aarch64")]`, so RISC-V publishes nothing.
* `direct_dispatch::try_publish` refuses `DirectDispatchClass::IpcReply` unconditionally, so the
  reply direction cannot acquire a debt even by mistake.

Consequently `offlock_authoritative_dispatch_enabled()` resolving to `false` on RISC-V is **not**
a blocker for NR6/NR7.

#### Blocker map

| # | Blocker | Severity | Site |
|---|---|---|---|
| 1 | NR6/NR7 admission asks `ipccall_direct_proof_enabled()`, not the canonical `ipccall_direct_admission_enabled()` — enabling the production default alone is a **silent no-op** | silent no-op | `src/arch/riscv64/trap.rs` |
| 2 | The trap bridge brackets every trap with three unconditional `with_cpu` acquisitions — entering identity, resume identity, SATP asid — so a **handled** direct transaction still enters the broad lock three times | **decisive** | `src/arch/riscv64/boot.rs` |
| 3 | No RISC-V cross-hart wake authority: both post-enqueue reschedule sends are x86_64-cfg-gated, and the SBI surface has no IPI extension | latent at current topology (BSP-only) | `src/kernel/ipccall_direct_txn.rs`, `src/arch/riscv64/sbi.rs` |

Blocker 2 is why this is **case C** rather than case B: the SMP=1 path is not complete, so the
question fails before remote wake is even reached.

#### Smallest next code increment

**Make the RISC-V trap bridge's identity and SATP lookups broad-lock-free** — blocker 2, alone.
The replacement seams already exist and are architecture-neutral, so this is a call-site swap,
not a new mechanism and not a RISC-V semantic copy:

* `shared.current_tid_authoritative(cpu)` → `shared.current_tid_split_read(cpu)` (rank-1
  scheduler seam; `current_tid_split_read_matches_with_cpu_current_tid_entering_snapshot` and its
  exiting-snapshot twin already prove the equivalence);
* `with_cpu(cpu, |k| k.task_asid(resume_tid))` → `task_asid_for_tid_split_read(resume_tid)`
  (rank-2 task seam, already used by the NR6/NR7 split handlers themselves).

Blocker 1 is a one-line predicate swap, but it must **not** land first or alone: admitting NR6/NR7
while the bridge still brackets the trap would produce a path that claims off-lock NR6/NR7 while
entering the broad lock three times per syscall — false evidence. Blocker 3 is only reachable once
RISC-V boots more than one hart, and is not on the path to a first SMP=1 cell.

The audit is executable: `stage199d_riscv_production_readiness_audit` (18 tests) pins the exact
admission predicate, the absence of any broad lock inside the transaction, the presence of the
three bridge acquisitions, the ABI import and return-lane parity, `sepc` advancing exactly once,
`tp` preservation, local enqueue authority, the absent remote wake, the inherited transfer-cap and
terminal-arbitration declines, that neither NR6 nor NR7 clears `current`, and the case-C verdict
computed from the blocker map.

---

### 6.1.18 RISC-V readiness blocker 2 — CLOSED (narrow trap-bridge snapshots)

§6.1.17 answered *"can an eligible RISC-V NR6/NR7 transaction complete end-to-end without
entering or re-entering the broad `KernelState` lock?"* with **no**, and named the decisive
reason: `yarm_riscv64_trap_bridge` bracketed **every** trap — including a handled Phase-1 direct
transaction — with broad-lock acquisitions. **That blocker 2 is **CLOSED**.**

Scope: a call-site swap to seams that already existed. No new mechanism, no RISC-V semantic copy,
and no change to NR6/NR7 admission, production predicates, cross-hart wake, scheduler policy,
AArch64, ServerDies or Stage 199E. No QEMU seal.

| Site | Was | Now |
|---|---|---|
| entering identity | `current_tid_authoritative(cpu)` | `current_tid_split_read(cpu)` |
| typed-idle invariant read | `current_tid_authoritative(cpu)` | `current_tid_split_read(cpu)` |
| resume identity | `current_tid_authoritative(cpu)` | `current_tid_split_read(cpu)` |
| SATP asid | `with_cpu(\|k\| k.task_asid(resume_tid))` | `task_asid_for_tid_split_read(resume_tid)` |

§6.1.17 counted three sites; the bridge in fact held a **fourth** — the typed-idle invariant read
in the `EnterKernelIdle` arm. It is converted too, because the structural guard is *no broad-lock
acquisition anywhere in the bridge*, and leaving one behind would have left the guard unenforceable.

#### Why the equivalence holds here, given `current_tid_split_read` is marked TRAP_FORBIDDEN

`current_tid_split_read` carries an explicit warning — *"stale at the pre-global-lock x86_64 trap
seam (Stage 29A proof: returned tid 0 instead of running requester)"* — and the Stage 4T+6R revert
records an x86_64 service-chain stall when the entering/exiting snapshots were converted. That
warning was taken seriously rather than waved past.

`KernelState::current_tid()` is `current_tid_on(self.current_cpu())`, and `with_cpu(cpu, ..)`
calls `set_current_cpu(cpu)` **first**. So the broad-lock read resolves to `current_tid_on(cpu)`
— exactly what the narrow seam reads. The two can differ in only two ways:

1. **Serialization** against a concurrent broad-lock holder. RISC-V is BSP-only
   (`online_cpus == 1`), so there is no second hart to hold it.
2. **The `set_current_cpu` side effect**, which a pure read does not perform. This is the more
   important one, and is the likelier mechanism behind the x86_64 revert: converting the read
   also removed a binding. On this bridge it is vacuous — the bridge always passes
   `BOOTSTRAP_CPU_ID` (`= 0`), and `validate_online_cpu` refuses every other CPU while one hart
   is online, so `current_cpu` is invariant and the rebind can only ever be a no-op.

`riscv_current_cpu_binding_is_invariant` pins premise 2 — including that binding `CpuId(1)` is
refused while one CPU is online — so if RISC-V ever boots a second hart, the test fails rather
than the bridge silently regressing. The TRAP_FORBIDDEN annotation on the seam remains correct
**for x86_64**, where neither premise holds.

#### The fail-closed SATP translation

This is the one place where a naive swap would have been a real defect.
`with_cpu(|k| k.task_asid(tid))` returns `Option<Asid>`, and `None` means *leave the installed
SATP alone*. `task_asid_for_tid_split_read` returns a raw `u64` and reports **both** "no such TID"
and "TID has no address space" as `0`. Passing that through unchanged would have made a stale or
missing resume identity install address space **0** — activating some other task's page table
instead of declining, and `cr3_for_asid` would have materialised a root for it.

The bridge therefore translates explicitly:

```rust
let resume_asid = match shared.task_asid_for_tid_split_read(resume_tid) {
    0 => None,
    raw => Some(crate::kernel::vm::Asid(raw as u16)),
};
```

`0` is unambiguous as an absence because the ASID allocator never hands out `Asid(0)`
(`kernel::vm`: *"ASID 0 must never be allocated"*). The activation and `sfence.vma` ordering below
it is untouched, and absence still skips activation entirely — the existing fatal/idle contract.

#### Preserved semantics

Both snapshots are taken at the **same program boundaries** as before — entering before the
wrapper call, resume after it and before the register write-back — and SATP is still selected from
the **exact** resume TID. The `unwrap_or(entering_tid)` fallback is unchanged, so a missing current
still resolves to the entering task and never names some other one. Every trap outcome is
unaffected: same-task return, split NR6/NR7 success and handled error, ordinary broad-lock syscall
fallback, queue-advancing switch to an incoming task, `ExitCurrentTask` with replacement,
intentional no-current idle, and stale/missing resume identity.

#### Effect on the audited question

A handled Phase-1 direct transaction now returns to userspace **without any broad-lock
acquisition at all**: the wrapper's handled path already returned before the broad-lock phase, and
the bridge that wraps it is now clean on both sides.
`a_handled_direct_transaction_never_touches_the_broad_lock` proves the whole handled trap is
broad-lock-free, before and after the wrapper call.

**Coordinate 23 remains OPEN, and `RISCV_199D_READINESS=case_c` still stands.** Closing the
return-path blocker does not make RISC-V production-ready:

* **Blocker 1 — still open.** NR6/NR7 admission still asks `ipccall_direct_proof_enabled()`
  rather than the canonical `ipccall_direct_admission_enabled()`, so with the proof gate off an
  SMP=1 production boot does not run NR6/NR7 off-lock **at all**. That is a code blocker
  reachable at SMP=1, which is why the case-C verdict is unchanged.
* **Blocker 3 — still open.** No cross-hart wake authority: both post-enqueue sends remain
  x86_64-cfg-gated and the SBI surface still exposes no IPI extension.

`stage199d_riscv_narrow_trap_snapshots` (16 tests) proves the narrow snapshots match the old
authoritative results for the same-current, switched-current, replacement and no-current cases,
proves the fail-closed asid translation against the broad-lock read it replaces, and pins the
structural guards: no broad-lock acquisition inside the bridge, none before or after a handled
direct transaction, blocker 1 still proof-gated, blocker 3 still open, production predicates
unchanged, and coordinate 23 still OPEN.

---

### 6.1.19 RISC-V readiness blocker 1 — CLOSED; readiness recomputed to `case_b`

The RISC-V Phase-1 whitelist asked `ipccall_direct_proof_enabled()` **directly**. That made the
RISC-V production predicate un-flippable in practice: with the proof gate off, `nr` never reached
`try_split_dispatch_into_frame`, so enabling production would have been a **silent no-op**. It now
asks the canonical `ipccall_direct_admission_enabled()` — blocker 1 is **CLOSED**.

Neither predicate's implementation changed, and the RISC-V production default was **not** enabled.

#### Why this is behaviour-preserving today

```rust
pub fn ipccall_direct_admission_enabled() -> bool {
    ipccall_direct_production_enabled() || ipccall_direct_proof_enabled()
}
pub const fn ipccall_direct_production_enabled() -> bool { cfg!(target_arch = "x86_64") }
```

On RISC-V the first disjunct is a **compile-time false**, so canonical admission reduces to
`false || proof` — *exactly* the predicate the site used to ask. Concretely:

* **RISC-V production predicate remains false** — `cfg!(target_arch = "x86_64")` is unchanged and
  no default moved.
* **Proof selector OFF** — admission is closed, NR6/NR7 are not split-eligible, and they fall
  through unchanged to the broad-lock handler. A normal boot stays byte-identical.
* **Proof selector ON** — admission is open for exactly the same population as before; the
  disjunction adds no term on RISC-V.
* **No ordinary feature-off production traffic is newly admitted** — the whitelist still admits
  exactly `DebugLog`, `FutexWake` and the gated direct-IPC term, and nothing else.
* **x86_64 and AArch64 are unchanged** — the portable trap entry already asked the canonical
  helper and was not touched.

#### No direct proof-predicate dependency remains

All three admission questions now flow through the one canonical helper, so a future production
flip cannot silently no-op:

| Question | Where | Status |
|---|---|---|
| NR6/NR7 ABI import | `yarm_riscv64_trap_bridge` | **unconditional** — `a7`→nr and `a0..a5`→args are imported for every ecall, so there was never a proof dependency here |
| whitelist admission | `handle_riscv_trap_entry_shared` | **canonical** (repaired here) |
| direct-handler reachability | `syscall_split::try_split_dispatch_into_frame` | **canonical** (already) |

No `ipccall_direct_proof_enabled()` **call** survives anywhere in `src/arch/` — only a comment in
the repaired site explaining what it used to ask.

#### Recomputed readiness

```
RISCV_199D_READINESS=case_b
```

Blockers 1 and 2 are closed, so the **SMP=1 / local path is structurally complete**: admission is
canonical, the contract stack is inherited broad-lock-free, the trap bridge is broad-lock-free on
both sides of the wrapper call, the ABI import and return lanes have parity, `sepc` advances
exactly once, `tp` is preserved, and local enqueue authority is architecture-neutral.

What remains is **blocker 3 — no cross-hart wake authority**. Both post-enqueue reschedule sends
are `#[cfg(target_arch = "x86_64")]` and the SBI surface exposes HSM but no IPI extension. That is
precisely case B: *local delivery complete, remote enqueue lacks an authoritative wake mechanism.*
The §6.1.17 case-C finding and its blocker map are preserved — an audit does not un-find what it
found — with the recomputed verdict recorded beside it.

**Coordinate 23 remains OPEN.** Structural completeness of the SMP=1 path is not production
readiness: the remote-wake requirement is unresolved, the RISC-V production predicate is still
false, and no live evidence has been earned.

#### QEMU evidence — TAKEN (revalidation; adds no live cell)

`qemu-system-riscv64` was **installed for this purpose** (Ubuntu 24.04, `apt-get install
qemu-system-misc`, giving QEMU 8.2.2) and both runs were executed from a clean `c9840e0f` tree
after a fresh `scripts/build-qemu-riscv64-artifacts.sh`. `qemu-system-aarch64` was deliberately
**not** installed — no AArch64 run was in scope.

**Run 2 — `scripts/qemu-ipccall-reply-direct-riscv64-smoke.sh`: PASS.**

```
STAGE_199_IPCCALL_REPLY_DIRECT_LIVE_SEAL arch=riscv64 classes=2 live_cells=2
  duplicate_replies=0 duplicate_wakes=0 result=ok
```

| Required observation | Live evidence |
|---|---|
| proof-gated NR6 request delivery | `IPCCALL_DIRECT_REQUEST_OK arch=riscv64 source_copy_offlock=1 reply_cap=1 server_wakes=1` |
| proof-gated NR7 reply delivery | `IPCREPLY_DIRECT_OK arch=riscv64 source_copy_offlock=1 caller_wakes=1 one_shot=1` |
| request userspace validation | `IPCCALL_DIRECT_ORACLE_SERVER_RECV opcode=1543 opcode_ok=1 data_ok=1 plen=8 reply_cap_ok=1` |
| reply userspace validation | `IPCCALL_DIRECT_ORACLE_CLIENT_REPLY_RECV plen=8 reply_ok=1` |
| deliberate duplicate NR7 rejected | `IPCCALL_DIRECT_ORACLE_SERVER_DUP dup_rejected=1 err=Err(WrongObject)`, and `IPC_DIRECT_ACK_FUSES … dir=nr7 duplicate=1` — the one deliberate duplicate, refused |
| return-lane parity | `IPCCALL_DIRECT_ORACLE_CLIENT_CALL_RET2 ret2=18446744073709551615 expected=18446744073709551615 ret2_ok=1 result=ok` — `ret2` is `SYSCALL_NO_TRANSFER_CAP`, the same success lane the shared encoder writes |
| zero broad-lock NR6/NR7 entries | `GLOBAL_LOCK_RETIRE_CLASS_DONE arch=riscv64 class=IpcCallDirectRequest result=ok` and `class=IpcReplyDirect result=ok`; **no** `UNEXPECTED_INLOCK_DISPATCH`, `YARM_SPLIT_DISPATCH_FALLBACK` or `UNLOCK_GRADUATED_FALLBACK` anywhere in the log |
| no fault / stale / duplicate wake / overwrite fuse / fatal | `RISCV_EARLY_TRAP`, `PAGE_FAULT_UNHANDLED`, `PANIC`, `FATAL`, `RISCV_TRAP_HANDLE_FAILED`, `USER_FAULT` all **0**; settled fuses `capacity_refused=0 overwrite_fuse=0 stale=0 foreign=0 dup_release=0 crossed=0 not_committed=0` both directions |
| lease balance | nr6 and nr7 both `reserve=1 commit=1 consume=1 release=0 cancel=0 live=0`, i.e. `reserve == consume + release + cancel + live`, at `capacity=256` — the structural bound, not the retired magic 8 |
| round-trip summary | `RISCV_IPCCALL_DIRECT_ROUNDTRIP_DONE request_ok=1 reply_ok=1 duplicate_reply=rejected server_wakes=1 caller_wakes=1 client_continuations=1 server_continuations=1 result=ok` |

**Selector OFF / ON.** The script builds a **feature-off** kernel and fails if the binary contains
any direct-class literal — it is marker-clean, so feature-off NR6/NR7 stay on the legacy path.
With the selector on, the admitted population is the same single oracle round trip as before
`c9840e0f`: exactly one NR6 and one NR7 reservation (`reserve=1` each), which is what the
canonical-admission swap predicted, since on RISC-V admission reduces to the proof gate.

**Two observations are attested indirectly, and are recorded as such.** No marker in this oracle
prints `sepc` on the return path or the `tp` value, so *"sepc advanced exactly once"* and
*"tp/TLS preserved"* are not directly asserted by a log line. What the run does show is
`client_continuations=1 server_continuations=1` with both userspace sides completing their
round trip and no fault: a missing advance would loop on the `ecall`, and a double advance would
fault into the next instruction — neither happened. SATP/ASID preservation is likewise attested
by both tasks continuing to execute in their own mappings (`USER_MAP_PA_CHECK asid=1 …`) with
zero page-fault markers, rather than by a dedicated SATP marker.

#### Run 1 — feature-off RISC-V core boot: the stale harness blocker, now CLOSED

The first attempt failed on a **harness** pattern, not a kernel boundary:

```
[fail] rejected pattern present: \bcapacity\b
[fail] qemu-riscv64-core-smoke: 1 check(s) failed (qemu_status=124)
```

The boot itself was healthy — `YARM_BOOT_OK`, service chain up, ending in the script's own
expected `RISCV_KERNEL_IDLE_WAITING_FOR_IO reason=no_runnable_task all_services_blocked` →
`RISCV_TRAP_HALTED reason=kernel_idle_awaiting_io`, with **no other rejected pattern present**.

**Why the pattern existed.** `\bcapacity\b` was added at **Stage 181 (`2a30515d`)**, in the
resource-exhaustion cluster beside `Vm\(Full\)` and `\boom\b`. At that time no kernel marker
printed the word benignly, so a bare word match was a sound proxy for "an allocator or table ran
out". Stage 199D (`fcfc55e3`) replaced the magic ack-store capacity of 8 with a structural bound
and added the independent waiter census; those reporters print `capacity=256`, `ack_capacity=256`
and `capacity_refused=0` on **every** healthy boot, so the proxy stopped discriminating.

**The repair narrows, it does not delete.** Every current log form carrying the word was
enumerated and split:

| Benign — must be ACCEPTED | Genuine exhaustion — must still be REJECTED |
|---|---|
| `IPC_DIRECT_ACK_COUNTERS … capacity=256` | `IPC_DIRECT_ACK_FUSES … capacity_refused=N` (N > 0) |
| `IPC_DIRECT_PRODUCTION_ACK_QUIESCENT … capacity=256` | `SHARED_REGION_CANCEL_FUSE_SET reason=capacity_exhausted` |
| `IPC_DIRECT_WAITER_CENSUS … ack_capacity=256 eligible=0 live_leases=0` | `IPC_SERVER_REPLY_LINK_REGISTER_FAIL … reason=capacity result=rolled_back` |
| `IPC_DIRECT_ACK_FUSES … capacity_refused=0` | `FORK_COW_FAIL reason=cow_capacity` |
| `EXIT_TASK_PREFLIGHT_OK … deferred_capacity=ok result=ok` | `D6_KERNEL_SWITCH_STACK_MAP_ACTIVE_FAILED … reason=page_table_capacity` / `reason=user_vm_capacity` |
| | `EXIT_TASK_SYSCALL_DECLINED … reason=deferred_capacity result=would_block` |
| | `IPC_RECV_REPLY_CAP_MATERIALIZE_FAIL … cnode_capacity=` |

The single generic pattern is replaced by that exact set. Two boundary subtleties are deliberate:
`reason=capacity\b` does **not** match `reason=capacity_exhausted` (`_` is a word character, so
there is no boundary), and `reason=deferred_capacity` does **not** match the benign
`deferred_capacity=ok`. `CapabilityFull` and `TaskTableFull` — which surface only as the Debug
field `kernel_error={:?}` of `FORK_COW_FAIL` and never contained the substring "capacity", so the
retired word match never covered them — are named explicitly, so the narrowing leaves no `*Full`
error unchecked beside the already-rejected `Vm\(Full\)`.

`tests/riscv_core_smoke_capacity_rejection.rs` (11 tests) is **behavioural, not textual**: it
parses `REJECT_PATTERNS` out of the script and evaluates fixture lines with `rg` exactly as the
script does (`rg -n "$pat"`, regex). It proves `capacity=256`, `ack_capacity=256`,
`capacity_refused=0` and `deferred_capacity=ok` are accepted; that `capacity_refused=1` and every
multi-digit refusal, plus all seven named exhaustion forms, are rejected; that `Vm(Full)` is
rejected **by the `Vm\(Full\)` pattern specifically** rather than incidentally; that OOM, PANIC,
FATAL, ASSERT, page-fault, early-trap, in-lock-dispatch and publish-race rejections remain active;
and that no bare-word capacity rejection survives.

#### Both RISC-V SMP=1 smokes now PASS

From one clean repair tree, after a fresh `scripts/build-qemu-riscv64-artifacts.sh`:

**Feature-off core smoke — PASS.** `[ok] qemu-riscv64-core-smoke passed (smp=1, qemu_status=124)`,
exit 0. `YARM_BOOT_OK present_cpus=1 online_cpus=1`; service chain up; terminal
`RISCV_KERNEL_IDLE_WAITING_FOR_IO reason=no_runnable_task all_services_blocked`. Every genuine
exhaustion predicate reads 0, as do `Vm\(Full\)`, `\boom\b`, `RISCV_EARLY_TRAP`,
`PAGE_FAULT_UNHANDLED`, `RISCV_TRAP_UNHANDLED`, `UNEXPECTED_INLOCK_DISPATCH`,
`UNLOCK_GRADUATED_FALLBACK`, `D2_PUBLISH_RACE_UNWIND` and `duplicate_wake`. **The direct-oracle
markers are absent**, as a feature-off run requires: `IPCCALL_DIRECT_REQUEST_OK`,
`IPCREPLY_DIRECT_OK`, `IPCCALL_DIRECT_ORACLE*`, the live seal and both direct retirement classes
all count 0.

**Proof-gated direct smoke — PASS, unchanged.**
`STAGE_199_IPCCALL_REPLY_DIRECT_LIVE_SEAL arch=riscv64 classes=2 live_cells=2
duplicate_replies=0 duplicate_wakes=0 result=ok`, with request and reply userspace validation,
the deliberate duplicate NR7 refused (`dup_rejected=1 err=Err(WrongObject)`), both direct classes
retired off-lock and zero in-lock dispatch or fallback markers, and settled fuses clean in both
directions apart from the one deliberate `duplicate=1`.

**No live cell is added.** This revalidates historical proof-gated evidence after the trap-bridge
and admission changes; the ledger stays **39 production / 7 non-production / 46 total**,
`RISCV_199D_READINESS` remains **`case_b`**, and coordinate 23 remains **OPEN** solely on the
cross-hart wake requirement, production enablement, and production live evidence. The stale
harness blocker is **closed**; it was never a kernel defect and closing it earns no cell.

`stage199d_riscv_canonical_admission` (11 tests) pins: NR6 and NR7 both use canonical admission;
no `ipccall_direct_proof_enabled()` call remains in the RISC-V ingress; all three admission
questions flow through the canonical helper; production-disabled admission equals the proof gate;
feature-off stays marker-clean; the bridge stays broad-lock-free before and after a handled
Phase-1 transaction (blocker 2 still closed); blocker 3 explicitly open; and no production
predicate changed.

---

### 6.1.20 RISC-V readiness blocker 3 — remote-wake audit: `RISCV_REMOTE_WAKE=D_REMOTE_ENQUEUE_UNREACHABLE_UNDER_CURRENT_TOPOLOGY`

Audit only. No runtime code, production predicate or scheduler policy changed; nothing was
implemented, no hart was given user work, no affinity was re-homed. Ledger stays **39 / 7 / 46**
and coordinate 23 remains **OPEN**.

```
RISCV_REMOTE_WAKE=D_REMOTE_ENQUEUE_UNREACHABLE_UNDER_CURRENT_TOPOLOGY
```

**The chain does not fail at the transport. It fails at its first link.** A remote enqueue cannot
even be *requested* on RISC-V, so every downstream question is moot until the topology changes.

#### The intended chain, traced

| # | Link | Seam | Status |
|---|---|---|---|
| 1 | a RISC-V task can be pinned to a non-boot CPU | `set_task_home_cpu` (RISC-V scope) | **absent** |
| 2 | the scheduler brings CPU 1 online | `RISCV_SCHEDULER_SMP_ONLINE` | **absent** |
| 3 | committed wake target compared to the enqueueing CPU | `if success.wake_target_cpu != enqueueing_cpu` | present |
| 4 | a RISC-V arch wake seam | `riscv64::smp::send_reschedule_ipi_to` | **absent** |
| 5 | an SBI IPI transport | `SBI_EXT_IPI` | **absent** |
| 6 | `sie.SSIE` enabled | `SSIE` | **absent** |
| 7 | hart 1's `stvec` is the YARM trap vector | `RISCV_SECONDARY_TRAP_VECTOR_INSTALLED` | **absent** |
| 8 | cause 1 has a decoder arm | `IRQ_SUPERVISOR_SOFT` | **absent** |
| 9 | cross-CPU work consumer on a trap path | `kernel.process_cross_cpu_work_for_cpu(cpu)` | present |
| 10 | hart 1 can perform saved user dispatch | `RISCV_SECONDARY_USER_DISPATCH` | **absent** |

Only links 3 and 9 exist. `stage199d_riscv_remote_wake_readiness` (13 tests) computes the
classification from the earliest missing link, and each `present` flag is checked against the
tree under an **architecture-scoped** probe — scoping matters, because
`set_task_home_cpu(.., CpuId(1))` does exist in the tree, for x86_64, and an unscoped search
would have reported the RISC-V pin as present when no RISC-V path has one.

#### Answers

**1. Does YARM start hart 1 through SBI HSM under `-smp 2`? YES.** Live:

```
YARM_RISCV64_SMP_HART_START hart=1 ret=0 ack=1 state=parked_not_online entry=0x80200062 …
RISCV_SECONDARY_HART_PARK hart=1
YARM_RISCV64_SMP_SECONDARY_PARKED
RISCV_SECONDARY_HARTS_PARKED count=1
```

**2. Online, or parked? Parked.** The marker says so itself, and the scheduler confirms it:

```
RISCV_DTB_CPU_SCAN_DONE bitmap=0x3 count=2
RISCV_HART_TOPOLOGY present_cpus=2 present_bitmap=0x3 boot_hart=0
RISCV_SCHEDULER_BSP_ONLY online_cpus=1 reason=riscv_smp_scheduler_not_enabled
YARM_BOOT_OK present_cpus=2 present_bitmap=0x3 online_cpus=1
```

Hart 1 is **present** and **started**, but **not a scheduler-online CPU**. Differential against
`-smp 1`, same tree: `present_cpus=1 present_bitmap=0x1 online_cpus=1`, `count=0` secondaries.

**3. What hart 1 owns.** From `yarm_riscv64_secondary_entry`: a private stack (`ld sp, 8(a1)`
from the handoff); an `stvec` pointing at a **local `wfi` park label**, *not*
`yarm_riscv64_trap_entry`; `sstatus.SIE` explicitly **cleared** (`csrc sstatus, t1`, t1 = 2); and
**no `sscratch`, no `satp`, no per-CPU binding**. `yarm_riscv64_secondary_boot` writes its ack,
emits the park marker and spins in `wfi` forever, touching no scheduler or kernel state.

**4. Supervisor software interrupts enabled? NO, on either hart.** The only `sie` bit the tree
ever sets is `STIE` (bit 5, timer), in `timer.rs`. `SSIE` (bit 1) is not named anywhere.
`sstatus.SIE` is set on the boot hart after the trap vector is installed, and explicitly cleared
on hart 1.

**5. Can an SBI IPI reach hart 1 and enter a real YARM handler? Firmware yes; YARM no.** The
QEMU-virt firmware advertises the transport — `OpenSBI v1.3`, `Platform HART Count : 2`,
`Platform IPI Device : aclint-mswi` — so `sbi_send_ipi` would raise a supervisor software
interrupt on hart 1. YARM never calls it: the SBI surface carries HSM and TIME only. And even if
one arrived, hart 1 has `sstatus.SIE` clear and an `stvec` that parks, so it could not be taken.

**6. Could the handler acknowledge and consume cross-CPU work? The consumer exists; the handler
does not.** `process_cross_cpu_work_for_cpu(cpu)` is wired into
`handle_trap_entry_with_fault_bookkeeping_mode`, so the **boot hart** consumes cross-CPU work on
every trap. `decode_trap_context` recognises only causes 5 (timer) and 9 (external); a supervisor
software interrupt is cause **1**, which falls to `TrapEvent::Unknown`. There is no pending-bit
acknowledgement path because there is no arm to acknowledge from.

**7. Can hart 1 perform saved user dispatch? No — park-only.** Its terminal state is a `wfi` loop
inside `yarm_riscv64_secondary_boot`; there is no AP dispatcher and no user-return path.

**8. Can a real NR6/NR7 commit a waiter to CPU 1? No — every task is effectively `home_cpu=0`.**
`sr_enqueue_committed_receiver_split` uses `affinity.unwrap_or(sched.current_cpu)`; the affinity
is `ReceiverCommit::Committed(tcb.cpu_affinity)`; `cpu_affinity` is set only by
`set_task_home_cpu`, whose sole `CpuId(1)` caller is `build_ap_workload`, reached only from
`arch/x86_64/smp.rs`. No RISC-V path calls it. So the committed `wake_target_cpu` always equals
the enqueueing CPU, and the remote branch is dead code — which is additionally
`#[cfg(target_arch = "x86_64")]` on both directions.

**9. Unreachable, or reachable-but-unwakeable? UNREACHABLE.** Two independent reasons, either
sufficient: no RISC-V task is ever pinned to CPU 1, and CPU 1 is not scheduler-online
(`riscv_smp_scheduler_not_enabled`), so an enqueue could not target it even if one were
requested.

**10. Minimum needed: (d) a larger RISC-V SMP foundation.** Computed, not asserted: links
1, 2, 6, 7, 8 and 10 are missing *in addition to* the transport links 4 and 5, so (a)
transport-only, (b) transport + trap consumer and (c) transport + AP dispatch are all
insufficient.

#### The SBI IPI calling convention, derived and pinned

Not assumed — derived from the SBI specification's ASCII extension encoding and cross-checked
against the firmware this environment actually runs.

| Item | Value |
|---|---|
| Extension ID (EID) | `0x735049` — ASCII `'s'<<16 \| 'P'<<8 \| 'I'` = `"sPI"` |
| Function ID (FID) | `0` — `sbi_send_ipi` |
| `a0` | `hart_mask` — a bit vector of harts, **relative to `hart_mask_base`** |
| `a1` | `hart_mask_base` — the hart id that bit 0 of `hart_mask` refers to |
| all-harts form | `hart_mask_base == usize::MAX` (all ones) ⇒ ignore the mask, send to every hart |
| success | `sbiret.error == SBI_SUCCESS (0)` |
| invalid hart | `SBI_ERR_INVALID_PARAM (-3)` — a hart id in the mask is invalid or not startable |
| target = current hart | permitted; the sender receives its own software interrupt |
| target offline/absent | reported through `sbi_hart_get_status`/`SBI_ERR_INVALID_PARAM`, never silently dropped |
| effect | raises the **supervisor software interrupt** (`sip.SSIP`) on each target; the handler must clear `sip.SSIP` to acknowledge |

To wake hart 1 specifically from hart 0: `hart_mask = 1 << (1 - hart_mask_base)` with
`hart_mask_base = 1`, i.e. `a0 = 0b1, a1 = 1`. The existing `sbi.rs` already has the ecall
wrapper and `SbiError::from_error_code`, so the transport is a small addition — it is simply not
the binding constraint.

**This convention is recorded, not implemented.** An empirical `probe_extension(0x735049)` is
deferred to the implementation increment, where it is a hard-stop precondition.

#### Smallest next code increment

**Bring CPU 1 online in the RISC-V scheduler and give hart 1 a real trap vector — nothing else.**
That is link 2 plus link 7, the two that make every other link testable. Concretely: replace the
secondary park with an entry that installs `yarm_riscv64_trap_entry` in `stvec`, establishes
`sscratch` and the per-CPU binding, activates the kernel address space, and registers the hart as
scheduler-online — then stops, still with `sstatus.SIE` clear and no user work.

Hard-stop conditions for that increment:

1. **`probe_extension(0x735049)` must return non-zero** on the target firmware before any IPI code
   is written. If it does not, stop and report — the transport assumption is wrong.
2. **`-smp 1` must remain byte-identical.** If the single-hart boot changes at all, stop.
3. **Hart 1 must not run user code.** No dispatch, no user return, no affinity change — if the
   increment needs any of those to show progress, it is too large.
4. **`online_cpus` must go 1 → 2 with the service chain still healthy.** If bringing CPU 1 online
   destabilises the boot hart's chain, revert and stop.
5. **No production predicate flip, no ServerDies, no AArch64, no Stage 199E.**
6. If enabling `sie.SSIE` on either hart produces an unhandled cause-1 trap before the decoder arm
   exists, that ordering is wrong — install the decoder arm first.

Only after CPU 1 is genuinely online does the question "was that enqueue remote?" become
answerable on RISC-V, and only then is the IPI transport worth writing.

---

### 6.1.21 RISC-V blocker 3, link 7 — TRAP-READY PARKED secondary hart

Recorded outcome: **link 7 and its hart-local prerequisites are structurally closed.** Hart 1 now owns a valid
kernel execution/trap context and parks with every interrupt admission disabled. **Link 2 remains
absent** — CPU 1 is *not* scheduler-online, runs no scheduler work and no userspace — so
`RISCV_REMOTE_WAKE` remains **D**, `RISCV_199D_READINESS` remains **`case_b`**, coordinate 23
remains **OPEN**, and the ledger stays **39 / 7 / 46**.

#### Pre-analysis: where the trap bridge assumed BSP-only

Exactly one code site assumed it — `yarm_riscv64_trap_bridge` opened with

```rust
let cpu = CpuId(crate::arch::platform_constants::BOOTSTRAP_CPU_ID);
```

and the accepted **§6.1.18 (`d82ef8de`) narrow-snapshot proof rested on that premise**: it argued
the dropped `set_current_cpu` rebind was vacuous *because* `current_cpu` was invariant on a
BSP-only architecture. Requirement 5 forces the bridge to derive the real CpuId, so that premise
can no longer hold by construction.

**It is replaced, not deleted.** The per-hart argument: `with_cpu(cpu, ..)` calls
`set_current_cpu(cpu)` and then reads `current_tid_on(current_cpu)`, so both reads resolve to
`current_tid_on(cpu)` **for whatever `cpu` the bridge derives** — the equivalence never depended
on *which* CPU it was. What the old premise additionally bought is now bought by the fact that
only the boot hart can reach the bridge at all: the secondaries park with every interrupt
admission disabled and never enter userspace, so no second hart generates a trap.
`the_bridge_derives_a_per_hart_cpu_identity` replaces
`riscv_current_cpu_binding_is_invariant` and pins both halves.

#### What was implemented

| # | Requirement | Implementation |
|---|---|---|
| 1 | authoritative logical CpuId, validated, fail-closed | `claim_logical_cpu_id_for_hart` refuses out-of-range (`MAX_CPUS`, handoff table, 64-bit claim word), the boot hart's own id, and duplicates — via an atomic `compare_exchange`. A rejected mapping leaves `cpu_id = usize::MAX` and the secondary parks **without** installing a vector (`RISCV_SECONDARY_TRAP_READY_DECLINED`). |
| 2 | kernel address space + `sfence.vma`, no `Asid(0)` | The handoff carries the **boot hart's live `satp`**, read from its CSR at prepare time. The secondary writes it, executes `sfence.vma x0, x0`, then reads the CSR back. **No ASID is allocated**, so `Asid(0)` cannot be materialised — the guard forbids `ensure_asid_root`, `activate_asid`, `cr3_for_asid` and `Asid(0)` in that function. |
| 3 | hart-local CPU binding before any shared code | `RISCV64_SECONDARY_CPU_IDS` / `RISCV64_SECONDARY_TRAP_STACK_TOPS` are published by the boot hart *before* the start request; the secondary's first marker is the binding. |
| 4 | `sscratch` per the existing ABI | The trap vector's first act is `csrrw sp, sscratch, sp`, so `sscratch` is this hart's **private** trap-stack top from a dedicated `RISCV64_SECONDARY_TRAP_STACKS` array — not the boot hart's `RISCV_TRAP_STACK` (which it would corrupt) and not its own execution stack (which a trap would clobber). **No second frame convention.** |
| 5 | the bridge derives the trapping CpuId | `riscv_logical_cpu_for_trap_frame(frame_ptr)`. Every trap frame is allocated on the trapping hart's own trap stack, so matching the frame against the published regions names the hart authoritatively — no new CSR, no per-CPU register convention, no lock. A frame outside every secondary region is the boot hart's, reproducing the previous constant exactly. |
| 6 | real vector installed LAST | `stvec ← yarm_riscv64_trap_vector` only after identity, SATP, fence and `sscratch`; pinned by `the_real_vector_is_installed_last_of_the_state_steps`. |
| 7 | all interrupt admission disabled | `csrw sie, zero` (SSIE, STIE and SEIE all off) and `csrci sstatus, 2`, both read back. |
| 8 | dedicated trap-ready park | Terminal `wfi` loop; the guards forbid any scheduler, dispatch, enqueue, cross-CPU, timer, `sret` or `enter_user` token in the function's code lines. |

**Markers report read-back CSR values, never intended values** — each `csrw` is followed by a
`csrr` into the binding the marker prints, and `sfence.vma` sits between the SATP write and its
read-back. `the_csr_markers_report_read_back_values` pins that ordering.

#### A defect found and fixed by the live run

The first `-smp 2` run passed the smoke but produced **garbled console output**: the secondary
wrote its `ack` *before* emitting its markers, so the boot hart resumed from
`wait_for_secondary_ack` mid-sequence and interleaved its own lines with hart 1's, corrupting
both. The acknowledgement now lands **after** the trap-ready sequence, so `ack=1` attests
"trap-ready and parked" rather than merely "reached Rust", and the two harts are never on the
shared SBI console at once. The bounded poll budget was raised to cover the six console lines.

#### Live evidence

**`-smp 1` — unchanged.** `[ok] qemu-riscv64-core-smoke passed (smp=1)`,
`YARM_BOOT_OK present_cpus=1 present_bitmap=0x1 online_cpus=1`, and **all six secondary markers
count 0**. The proof-gated direct-IPC smoke still passes:
`STAGE_199_IPCCALL_REPLY_DIRECT_LIVE_SEAL arch=riscv64 classes=2 live_cells=2 result=ok`.

**`-smp 2` — `[ok] qemu-riscv64-core-smoke passed (smp=2)`.** Each marker exactly once, in causal
order:

```
RISCV_SECONDARY_CPU_ID_BOUND hart=1 cpu=1 trap_stack_top=0x815fd7c0
RISCV_SECONDARY_KERNEL_SATP_ACTIVE hart=1 cpu=1 satp=0x0 sfence=1
RISCV_SECONDARY_SSCRATCH_READY hart=1 cpu=1 sscratch=0x815fd7c0
RISCV_SECONDARY_TRAP_VECTOR_INSTALLED hart=1 cpu=1 stvec=0x8023a290 expected=0x8023a290
RISCV_SECONDARY_INTERRUPTS_DISABLED hart=1 cpu=1 sie=0x0 sstatus_sie=0 ssie=0 stie=0 seie=0
RISCV_SECONDARY_TRAP_READY_PARKED hart=1 cpu=1 online=0 user=0 scheduler=0
YARM_RISCV64_SMP_HART_START hart=1 ret=0 ack=1 state=parked_not_online …
```

* **`present_cpus=2`** — `RISCV_HART_TOPOLOGY present_cpus=2 present_bitmap=0x3 boot_hart=0`.
* **HSM start + ack exactly once** — `ret=0 ack=1`, one `RISCV_SECONDARY_HART_PARK`.
* **Hart 1 owns `CpuId(1)`, not `CpuId(0)`** — every marker reads `cpu=1`.
* **`stvec` is the real YARM trap entry** — read back equals `expected`.
* **`sscratch` is valid for the existing frame ABI** — it equals the `trap_stack_top` reported by
  the binding marker, a private per-hart trap stack.
* **SIE, SSIE, STIE (and SEIE) all disabled** — read back as `0`.
* **`online_cpus` remains exactly 1** — `RISCV_SCHEDULER_BSP_ONLY online_cpus=1
  reason=riscv_smp_scheduler_not_enabled`, `YARM_BOOT_OK … online_cpus=1`.
* **No user task, dispatch or timer on hart 1**, and no `RISCV_SECONDARY_TRAP_READY_DECLINED` or
  `RISCV_SECONDARY_CPU_ID_REJECTED`.
* **Boot hart healthy** — service chain up, terminal `RISCV_KERNEL_IDLE_WAITING_FOR_IO`.
* **No unexpected trap, fault, reset or duplicate startup** — `RISCV_EARLY_TRAP`,
  `RISCV_TRAP_UNHANDLED`, `PAGE_FAULT_UNHANDLED`, `RISCV_TRAP_HANDLE_FAILED`, `PANIC` all 0.

**On `satp=0x0`.** That is the boot hart's *live* value at secondary-start time: RISC-V YARM
executes the kernel in **bare mode** and switches `satp` only to enter a user address space. The
correctness property being established is that hart 1's translation state is **identical to the
boot hart's**, captured rather than invented; the marker reports the read-back, so it claims
exactly that and no more.

#### No hard-stop triggered

Interrupts were not enabled to make the vector safe (the park emits no faulting instruction);
neither the kernel `satp` nor the CpuId required a new global-lock dependency (a CSR read and a
lock-free atomic claim); `online_cpus` stayed 1; hart 1 executed no userspace or scheduler work;
`-smp 1` is unchanged; and no trap reached the new vector.

**Chain status after this increment: links 3, 7 and 9 present; 1, 2, 4, 5, 6, 8, 10 absent.**
The next increment remains link 2 — bring CPU 1 online — under the hard-stops already recorded in
§6.1.20, including that `probe_extension(0x735049)` must succeed before any IPI transport work.

---

### 6.1.22 RISC-V remote-wake chain link 2 — CPU 1 scheduler-online, WAKE-ONLY

Logical CPU 1 is registered scheduler-online in the explicitly **non-dispatchable** wake-only
state. **The verdict does not move**: link 1 is still absent, so `RISCV_REMOTE_WAKE` remains
**D**, `RISCV_199D_READINESS` remains **`case_b`**, coordinate 23 remains **OPEN**, and the ledger
stays **39 / 7 / 46**. No production flip, no new live cell.

#### Pre-audit: the tree already represents the required state — no hard-stop

The hard-stop asked whether `present=1`, `online=1`, `wake_only=1` plus no dispatch, no placement,
no timer, no queue consumption and no interrupt admission can be represented simultaneously. They
can, through the **generic** mechanism x86_64 (Stage 183.5) and AArch64 (Stage 195D) already use:

* `mark_cpu_wake_only(cpu, bool)` → `Scheduler::set_cpu_wake_only`, with `wake_only_bitmap()`.
* **`least_loaded_online_cpu` skips wake-only CPUs outright** — *"wake-only CPUs are online but
  accept no task placement (no dispatcher runs on them yet) — never balance onto them."* This is
  the decisive fact: onlining does **not** make CPU 1 eligible for ordinary placement.
* `dispatching = online_cpu_bitmap() & !wake_only_bitmap()` keeps user dispatch BSP-only.
* `install_ap_idle_current(cpu)` installs the scheduler-owned idle current (tid 0), the existing
  convention for an online-but-non-dispatching CPU.

No RISC-V-private scheduler and no second online bitmap were created.

#### Ordering, as required

1. HSM start succeeds (§6.1.21). 2. The secondary publishes CpuId, SATP read-back, private
`sscratch`, real `stvec` and the interrupts-disabled proof. 3. It publishes
`RISCV_SECONDARY_TRAP_READY_PARKED` and **only then** sets `ack`. 4. The boot hart consumes the
ack and records the CpuId in `RISCV64_TRAP_READY_ACKED` — so the bit means *link 7 completed on
that hart*. 5. On the boot hart, with a live `&mut KernelState`,
`riscv_bring_trap_ready_secondaries_online_wake_only` marks **wake-only first** (no window in
which the CPU is online and placement-eligible), then brings it up, then installs the idle
current. 6. `RISCV_SCHEDULER_SMP_ONLINE` is published **only after** the scheduler state reads
back `present=1 online=1 wake_only=1`; a mismatch rolls the wake-only mark back and reports
`reason=readback_mismatch` instead.

The secondary itself never calls the scheduler — it stays in its link-7 `wfi` park with
`SIE`/`SSIE`/`STIE`/`SEIE` all off.

#### A real defect the live run exposed

The first `-smp 2` run after registration failed with `online_cpus=1` and **`hart=0 cpu=0`** in
the secondary's own markers. The cause: **OpenSBI chooses the boot hart nondeterministically** —
that run entered on hart 1 — while the trap bridge always names the boot hart
`CpuId(BOOTSTRAP_CPU_ID)`. Link 7's mapping assumed `hart_id == logical CpuId`, so secondary
hart 0 claimed logical CpuId 0: **the boot hart's own id**. The duplicate check could not catch
it because nothing had claimed bit 0 — the claim word was initialised to `0` even though its own
doc comment said bit 0 was pre-claimed.

Fixed: logical id 0 is genuinely pre-claimed for the boot hart, and each secondary is allocated
the **lowest free id ≥ 1** in hart-id order. Verified across three consecutive `-smp 2` runs that
booted on hart 0 *and* hart 1 — `cpu=1` and `online_cpus=2` in every one. This was a latent link-7
defect that only a second hart could expose.

A second, smaller defect: the new smoke assertions used `rg -n` on a log containing control
bytes, so ripgrep printed *"binary file matches"* instead of line numbers and `set -u` tripped on
the extracted value. The line-number and count lookups now use `-a`, and the ordering comparison
validates both operands are numeric before arithmetic.

#### The smoke gate had to change, and how

`scripts/qemu-riscv64-core-smoke.sh` hard-required `online_cpus=1` at **any** `-smp`, which would
have failed this increment by construction. It now expects `online_cpus == present_cpus` and adds
per-CPU assertions that make onlining safe to accept: every secondary marker exactly
`N-1` times, the full non-dispatch tuple, the trap-ready-before-registration ordering, and — at
`-smp 1` — that **no** secondary marker appears at all. The pre-bootstrap
`RISCV_SCHEDULER_BSP_ONLY` breadcrumb is unchanged and still required; it records the state at DTB
scan time, which `RISCV_SCHEDULER_SMP_ONLINE` supersedes later in the boot.

#### Live evidence

**`-smp 2` — `[ok] qemu-riscv64-core-smoke passed (smp=2)`.**

```
RISCV_SECONDARY_CPU_ID_BOUND hart=1 cpu=1 trap_stack_top=0x815fd7c0
RISCV_SECONDARY_KERNEL_SATP_ACTIVE hart=1 cpu=1 satp=0x0 sfence=1
RISCV_SECONDARY_SSCRATCH_READY hart=1 cpu=1 sscratch=0x815fd7c0
RISCV_SECONDARY_TRAP_VECTOR_INSTALLED hart=1 cpu=1 stvec=0x8023a380 expected=0x8023a380
RISCV_SECONDARY_INTERRUPTS_DISABLED hart=1 cpu=1 sie=0x0 sstatus_sie=0 ssie=0 stie=0 seie=0
RISCV_SECONDARY_TRAP_READY_PARKED hart=1 cpu=1 online=0 user=0 scheduler=0
RISCV_SCHEDULER_SMP_ONLINE cpu=1 present=1 online=1 wake_only=1 dispatchable=0 user_dispatch=0 timer=0 queue=0 irq=0
YARM_BOOT_OK present_cpus=2 present_bitmap=0x3 online_cpus=2
```

`present_cpus=2`, `online_cpus=2`, CPU 1 explicitly `wake_only=1`; all six link-7 markers exactly
once and in causal order with `stvec`, `sscratch`, SATP and interrupt read-backs **unchanged**
from §6.1.21. **Zero CPU-1 activity**: no `SCHED_DISPATCH`, `RISCV_STARTUP_ARGS`, dequeue, timer
tick or task switch names `cpu=1` — hart 1's *only* log lines are its trap-ready sequence and its
HSM start. Services and the boot-hart idle chain are healthy; `RISCV_EARLY_TRAP`,
`RISCV_TRAP_UNHANDLED`, `PAGE_FAULT_UNHANDLED`, `RISCV_TRAP_HANDLE_FAILED`, `PANIC`,
`RISCV_SCHEDULER_SMP_ONLINE_FAIL`, `RISCV_SECONDARY_TRAP_READY_DECLINED` and
`RISCV_SECONDARY_CPU_ID_REJECTED` are all 0.

**`-smp 1` — unchanged.** `[ok] … passed (smp=1)`,
`YARM_BOOT_OK present_cpus=1 present_bitmap=0x1 online_cpus=1`, **zero** secondary and link-2
markers, and the proof-gated direct-IPC seal is still green:
`STAGE_199_IPCCALL_REPLY_DIRECT_LIVE_SEAL arch=riscv64 classes=2 live_cells=2 result=ok`.

#### Chain status

Links **2, 3, 7 and 9 present**; **1, 4, 5, 6, 8 and 10 absent**. The earliest missing link is now
**1** — no RISC-V task is pinned to a non-boot CPU, so the committed `wake_target_cpu` is still
always the enqueueing CPU and a remote enqueue still cannot be *named*, let alone signalled. That
is why the verdict stays **D** even with CPU 1 online. `probe_extension(0x735049)` was **not**
probed and remains a hard-stop for the later transport increment.

---

### 6.1.23 RISC-V chain link 1 — HARD-STOP on retirement; link 1 remains ABSENT

Pre-audit only. **No code was written**, no oracle was added and no chain entry moved. Links
2, 3, 7 and 9 remain present; **1, 4, 5, 6, 8 and 10 remain absent**; `RISCV_REMOTE_WAKE` recomputes
unchanged to **D**; `RISCV_199D_READINESS` remains **`case_b`**; coordinate 23 remains **OPEN**;
the ledger remains **39 / 7 / 46**. `probe_extension(0x735049)` remains uncalled.

The hard-stop asked whether a disposable proof task can satisfy five conditions.
**Conditions 1–4 are satisfiable. Condition 5 is not.**

| # | Condition | Verdict |
|---|---|---|
| 1 | created and run initially on CPU 0 | **YES** — the existing RISC-V direct-IPC oracle already spawns a disposable child server that runs on the boot hart. |
| 2 | parked in the exact NR6/NR7 waiter state | **YES** — that server blocks in recv-v2 and publishes its blocked-server acknowledgement; the green `STAGE_199_IPCCALL_REPLY_DIRECT_LIVE_SEAL` is the standing proof. |
| 3 | `home_cpu = CpuId(1)` via generic `set_task_home_cpu` | **YES** — the seam is architecture-neutral and already carries `CpuId(1)` on x86_64. |
| 4 | remotely enqueued to CPU 1 without executing there | ~~**YES**~~ → **CORRECTED TO NO by §6.1.25.** `enqueue_on_with_priority` does **not** accept CPU 1 merely because it is online: it *denies* placement on a wake-only CPU. `sr_enqueue_committed_receiver_split` discards that `Err` and returns the requested CPU anyway, which is what made this row read YES. |
| 5 | **safely removed/retired afterwards, without modifying production scheduler or lifecycle semantics** | **NO** |

#### Why condition 5 fails

Once the transaction commits the enqueue, the task sits in **CPU 1's runqueue**, and CPU 1 never
dispatches. Removing it requires a seam that does not exist:

* `RingQueue::remove_tid` is **private to `scheduler.rs`** and is reachable from exactly one
  caller — `on_preempt_prefer`, which also *dispatches*. Verified exhaustively: no reference to
  `remove_tid` exists anywhere outside `src/kernel/scheduler.rs`.
* There is **no `Scheduler`-level** "remove this TID from that CPU's runqueue" method.
* There is **no `KernelState`-level** equivalent — `task_present_in_any_runqueue` and
  `task_present_anywhere` are read-only, and `peek_next_runnable_on` is a non-mutating peek.

That leaves two routes, and both are excluded by the condition itself:

1. **Dispatch it on CPU 1** (`dispatch_next_on(CpuId(1))`, then `block_current_on`). This
   *schedules* the task onto CPU 1 — it becomes CPU 1's `current` — which is exactly the
   "no ordinary runnable-task placement / `target_executed=0`" property the proof is supposed to
   establish, and it destroys the wake-only idle-current invariant: `install_ap_idle_current`
   refuses to restore the tid-0 placeholder while a current task is present
   (`SchedulerError::AlreadyQueued`). It also leaves a window in which the task is `Runnable` but
   in no queue — a lost task.
2. **Add a generic `Scheduler` removal seam.** That is new **production scheduler surface**, which
   condition 5 explicitly forbids.

Fabricating a proof that skipped retirement — observing the commit and leaving the task parked on
CPU 1's runqueue forever — would violate the required post-cleanup evidence ("CPU 1 runqueue
contains no leaked oracle task") and is not done here.

#### The exact contract that must be split first

Link 1 needs a **generic, arch-neutral, non-dispatching runqueue withdrawal**:

```
Scheduler::withdraw_queued_tid_on(cpu: CpuId, tid: ThreadId) -> bool
```

removing `tid` from `cpu`'s priority queues **without** touching `current`, **without**
dispatching, and **without** altering the task's TCB status — with a `KernelState` wrapper beside
`task_present_in_any_runqueue`. `RingQueue::remove_tid` already implements the queue mechanics and
already compacts correctly (`ring_queue_remove_tid_compacts_correctly`); what is missing is the
non-dispatching path to it. That seam is production scheduler surface and belongs in its own
increment, with its own guards proving it never dispatches and never changes task status.

Only once withdrawal exists can a disposable task be enqueued to CPU 1 and then retired, which is
what makes the `RISCV_REMOTE_ENQUEUE_COMMITTED … target_executed=0 result=ok` observation safe to
take.

#### Scope note

Both directions were considered. Splitting into **link 1A (NR6)** and **link 1B (NR7)** does not
help: retirement blocks both identically, since both end in a committed enqueue on CPU 1. No
partial proof is claimed.

---

### 6.1.24 The generic non-dispatching runqueue-withdrawal foundation

This closes the **contract split** §6.1.23 named as the prerequisite for link 1. It is **not**
link 1. Nothing is wired: the seam has no caller outside the scheduler, its `KernelState` wrapper
and tests, and that is machine-checked by a source-tree walk
(`the_seam_is_wired_into_no_oracle_or_production_path`).

**Verdicts unchanged.** Links 2, 3, 7, 9 present; **1, 4, 5, 6, 8, 10 absent**;
`RISCV_REMOTE_WAKE` = **D**; `RISCV_199D_READINESS` = **`case_b`**; coordinate 23 **OPEN**;
ledger **39 production / 7 non-production / 46 total**; **no live cell, no QEMU seal**. The
production predicate is still `cfg!(target_arch = "x86_64")`.

#### The seam

```
PriorityScheduler::withdraw_queued_tid(tid)          -> WithdrawOutcome   (pub(crate))
SmpScheduler::withdraw_queued_tid_on(cpu, tid)       -> WithdrawOutcome   (pub(crate))
KernelState::withdraw_queued_tid_on(cpu, tid: u64)   -> WithdrawOutcome   (pub(crate))
```

§6.1.23 sketched the return type as `bool`. **`bool` is genuinely ambiguous here**, so the
smallest typed outcome is used instead: a bare `false` would conflate *not queued*, *is the
CPU's current task*, *appears more than once* and *that CPU is not online* — four different
facts with four different correct responses. Hence:

| Outcome | Meaning |
|---|---|
| `Removed` | Exactly one queued incarnation was removed. |
| `NotQueued` | The TID holds no queued slot on that CPU. Nothing mutated. |
| `RefusedCurrent` | The TID is that CPU's `current` — including the scheduler-owned tid-0 idle placeholder. Refused **before** any mutation. |
| `RefusedDuplicate` | More than one queued slot. **Fail closed**: no queue modified. |
| `InvalidCpu` | CPU id out of range or not online. |

#### Required semantics and where each is proved

| # | Requirement | Proof |
|---|---|---|
| 1 | Exact CPU confinement | `withdraw_leaves_a_tid_queued_on_another_cpu_untouched`; only `schedulers[idx]` is reachable from the seam. |
| 2 | Non-dispatching | Behavioural: `withdraw_changes_no_topology_or_current_state`. Structural: `the_seam_contains_no_dispatch_or_context_switch_token` bans `dispatch_next`, `on_preempt_prefer`, `block_current`, `set_current`, `install_ap_idle_current`, `yield_current`, `switch_to`, `context_switch`, `preempt_reenqueue`, `self.current =`, `enqueue`. |
| 3 | Current-task protection, before mutation | `withdraw_refuses_the_current_task_without_mutation`; the `current` check is the first statement. |
| 4 | Idle-task protection | `withdraw_preserves_the_scheduler_owned_idle_current` — tid 0 is `current` on a wake-only CPU, so (3) already covers it. |
| 5 | Exact-one rule | `withdraw_fails_closed_on_a_duplicate_occurrence_with_zero_mutation`. `count_tid` scans **all three** priority queues first; mutation happens only at a total of exactly 1. |
| 6 | Wrong-CPU behaviour | Same test as (1) — CPU 0's withdrawal reports `NotQueued` and CPU 1's queue is intact. |
| 7 | No policy changes | `withdraw_changes_no_topology_or_current_state` compares online/present/wake-only bitmaps and both current slots; `the_seam_changes_no_policy_state` bans the topology, affinity, priority, timeslice, balancing and timer tokens. |
| 8 | No task-state mutation | `the_wrapper_leaves_the_task_status_byte_for_byte_unchanged` images the TCB `status` field's raw bytes before and after, for `Runnable`, `Blocked(Poll)`, `Blocked(Join)` and `Exited`; `the_seam_contains_no_task_state_mutation_token` proves the seam never names a TCB at all. |
| 9 | Queue integrity | `withdraw_handles_head_middle_and_tail_positions` (FIFO order of survivors) and `withdraw_compacts_a_wrapped_ring_queue` (head 56, len 10 over a 64-slot ring, removal at the last physical slot with successors past the wrap). |

#### No duplicated queue algorithm

Removal delegates to the existing `RingQueue::remove_tid` compaction, and the exact-one count
reuses the ring's own `Self::index` mapping (`the_seam_reuses_the_existing_compaction_mechanism`,
which also proves `count_tid` is a pure scan). The **one** thing withdrawal must do that
`remove_tid` alone does not is update the membership mirror: `remove_tid`'s only pre-existing
caller, `on_preempt_prefer`, moves the task queue → `current`, so it stays present in that
scheduler and membership must *not* change. Withdrawal removes it from the scheduler entirely, so
membership must be cleared — otherwise a later `enqueue_with_priority` would refuse the TID as
already queued. `withdraw_removes_from_each_priority_queue` re-enqueues after every withdrawal
precisely to pin that.

#### Architecture neutrality and visibility

`the_seam_has_no_architecture_specific_reference` bans `riscv`, `aarch64`, `x86`, `target_arch`,
`hart`, `sbi`, `ipi`, `satp` and `BOOTSTRAP_CPU_ID`. `the_seam_is_not_a_public_api` pins every
level — and `WithdrawOutcome` itself — at `pub(crate)`.

Each forbidden-token guard was mutation-tested: injecting `dispatch_next_on` into the scheduler
seam fails (2); injecting `set_task_status_for_test` into the `KernelState` wrapper fails (8)
*and* fails the byte-for-byte status proof. Both guards also assert their own extraction is
non-degenerate, so a broken slice cannot make them pass vacuously.

#### What is still missing for link 1

Withdrawal alone. The remaining link-1 work — spawning a disposable proof task, parking it in the
NR6/NR7 waiter state, assigning `home_cpu = CpuId(1)`, observing
`RISCV_REMOTE_ENQUEUE_COMMITTED … target_executed=0`, then retiring it through this seam — is a
separate increment and is **not** claimed here.

> **Superseded by §6.1.25.** "Withdrawal alone" was wrong. The attempt found a second, earlier
> blocker: the remote CPU cannot hold the task at all while it is wake-only.

---

### 6.1.25 RISC-V chain link 1 (NR6) — HARD-STOP on wake-only placement; link 1 remains ABSENT

The requested proof was that a genuine NR6 transaction commits its wake target to CPU 1 while
CPU 1 is simultaneously **online**, **wake-only**, and holding the server **queued exactly once**.
**Those three facts are mutually exclusive**, and the contradiction is in the production scheduler
contract, not in the oracle, the affinity seam or the withdrawal foundation.

**Verdicts unchanged.** Link 1 **ABSENT**; links 2, 3, 7, 9 present; 1, 4, 5, 6, 8, 10 absent;
`RISCV_REMOTE_WAKE` recomputes mechanically to **D**; `RISCV_199D_READINESS` remains **`case_b`**;
coordinate 23 remains **OPEN**; ledger remains **39 / 7 / 46**; **no new canonical live cell**.
**NR7 remote reachability is NOT live-proved** — it was out of scope for this increment and the
NR6 blocker applies to it identically, since both end in the same rank-1 placement.

#### What was built, run, and then reverted

The full choreography was implemented and booted, because a hard-stop asserted from source alone
would not have been trustworthy: steps 1–5 and 7–9 all *work*, and only a live run distinguishes
"the target was committed" from "the target was requested". The mechanism — a recv-entry pin
through the generic `set_task_home_cpu`, and a post-lock observe → withdraw → rehome → requeue
bounce on the direct-request drain, both default-off and oracle-gated — was **reverted in full**
once it produced its evidence. Nothing of it survives in the tree; a source-tree walk
(`no_remote_enqueue_proof_mechanism_landed`) proves it.

#### The live evidence

QEMU-virt/OpenSBI, `-smp 2`, `yarm.riscv_ipccall_direct_oracle=1`, one clean boot:

```text
RISCV_SECONDARY_TRAP_READY_PARKED hart=1 cpu=1 online=0 user=0 scheduler=0
RISCV_SCHEDULER_SMP_ONLINE cpu=1 present=1 online=1 wake_only=1 dispatchable=0 …
YARM_BOOT_OK present_cpus=2 present_bitmap=0x3 online_cpus=2
RISCV_REMOTE_ENQUEUE_SERVER_PINNED direction=nr6 tid=10008 endpoint=6 home_cpu=1 \
    target_online=1 target_wake_only=1 result=ok
SCHED_ENQUEUE_DENIED_WAKE_ONLY cpu=1 tid=10008 reason=no_ap_dispatcher_yet     ← the blocker
IPCCALL_DIRECT_REQUEST_OK arch=riscv64 source_copy_offlock=1 reply_cap=1 server_wakes=1
RISCV_REMOTE_ENQUEUE_COMMITTED direction=nr6 enqueueing_cpu=0 wake_target_cpu=1 \
    target_online=1 target_wake_only=1 target_executed=0 queued_exactly_once=0 result=fail
RISCV_REMOTE_ENQUEUE_WITHDRAWN direction=nr6 cpu=1 outcome=NotQueued target_executed=0 result=fail
RISCV_REMOTE_ENQUEUE_REHOMED direction=nr6 from_cpu=1 to_cpu=0 queued_cpu1=0 queued_cpu0=1 result=ok
USER_LOG tid=1 msg=RISCV_IPCCALL_DIRECT_ROUNDTRIP_DONE request_ok=1 reply_ok=1 … result=ok
```

Read it precisely. The pin landed. The transaction ran off-lock and completed. `wake_target_cpu`
came back as **1**. The round trip finished and the ack/waiter censuses stayed clean. But the
server was **never queued on CPU 1** — `queued_exactly_once=0`, and the withdrawal that followed
found `NotQueued`. Requirement 6 is unsatisfiable.

#### The first missing causal boundary

`SmpScheduler::enqueue_on_with_priority` refuses placement on a wake-only CPU:

```rust
// Stage 183.5: a wake-only online CPU runs no dispatcher — a task placed on
// its queue would strand forever. Deny placement explicitly (nothing pins
// work to APs today; 183.6 lifts this per CPU when the AP dispatcher lands).
if self.wake_only & (1u64 << idx) != 0 {
    crate::yarm_log!("SCHED_ENQUEUE_DENIED_WAKE_ONLY cpu={} tid={} reason=no_ap_dispatcher_yet", …);
    return Err(SchedulerError::CpuOffline);
}
```

Wake-only *means* "explicit placement is denied" — it is the property that made §6.1.22's
onlining safe in the first place. So "CPU 1 is wake-only" and "the server is queued on CPU 1" are
the same question asked twice with opposite answers. `queued_on_the_target_and_target_wake_only_are_mutually_exclusive`
computes the truth table over the real scheduler: `(wake_only=false → queued)`,
`(wake_only=true → not queued)`. There is no satisfying row.

Lifting the denial is **Stage 183.6** work with a named owner (the AP dispatcher), and it is
production scheduler policy — which the increment's own hard-stop list forbids modifying. So does
clearing `wake_only` around the enqueue, which mutates the exact topology state §6.1.24
requirement 7 pins as immutable. There is no third route.

#### A second finding: the committed wake target is not a committed placement

`sr_enqueue_committed_receiver_split` documents its return value as *"the CPU the receiver was
**actually enqueued on**"*, read "out of the same rank-1 acquisition, so the two cannot disagree".
They can:

```rust
let cpu = affinity.unwrap_or(sched.current_cpu);
let _ = sm.enqueue_on_with_priority(cpu, ThreadId(tid), priority);   // Err discarded
cpu                                                                  // returned regardless
```

`the_committed_wake_target_can_report_a_placement_that_never_happened` drives the real seam against
an online wake-only CPU and shows it returning `CpuId(1)` for a task that ends up in **no**
runqueue. This is what made §6.1.23 score condition 4 as **YES** — that scoring is hereby
**corrected to NO**: `enqueue_on_with_priority` does *not* accept CPU 1 merely because it is
online.

The defect is **latent, not live, on x86_64**: the SMP oracle brings its AP up dispatching, so
`wake_only` is clear there and the denial never fires. It is not repaired in this increment —
it is production behaviour on the one architecture where NR6 is the default, and repairing it
(returning the achieved placement, or failing the transaction when the wake cannot be placed) is
its own increment with its own live seal.

> **Repaired in §6.1.26.** Both halves: the seam now reports what it actually did, and a refused
> placement rolls the publication back instead of leaving a Runnable-but-unqueued receiver.



#### The exact contract that must be split first

Before link 1 can close, one of these must land — each is a production scheduler change, not a
proof:

1. **Split `wake_only`** into "excluded from balanced placement / dispatch" and "may receive an
   explicit remote enqueue", so a CPU can hold a queued task it will not run. This is the minimal
   split, and it is exactly what a remote-enqueue proof needs: a task parked on a queue that is
   never drained, then withdrawn.
2. **Or land the AP dispatcher** (Stage 183.6) so CPU 1 is a genuine dispatching CPU — which
   requires links 4, 5, 6, 8 and 10 as well, i.e. the whole remote-wake chain.

Route 1 is far smaller and is the recommended next increment. It must also carry the
`sr_enqueue_committed_receiver_split` repair, or the proof would still be reading a requested
target rather than an achieved one.

#### Scope note

The withdrawal foundation from §6.1.24 is **not** implicated: it worked exactly as specified —
`outcome=NotQueued` on a TID that was genuinely not queued is its correct, fail-closed answer.
It remains unwired (`the_withdrawal_foundation_remains_unwired`).


### 6.1.26 The false-success enqueue contract — repaired

§6.1.25's second finding, closed. This changes no production predicate, no wake-only semantics
and no RISC-V status; it repairs the seam that reports a wake and the two transactions that
consume it.

**RISC-V status is untouched and recomputes unchanged:** link 1 **ABSENT**; links 2, 3, 7, 9
present; 4, 5, 6, 8, 10 absent; `RISCV_REMOTE_WAKE` = **D**; `RISCV_199D_READINESS` = **`case_b`**;
coordinate 23 **OPEN**; ledger **39 / 7 / 46**; **no new live cell**. The §6.1.24 withdrawal
foundation remains unwired.

#### The contract

```rust
let cpu = requested_target;
let _ = enqueue_on_with_priority(cpu, tid, priority);   // Err discarded
cpu                                                     // reported regardless
```

became

```rust
match sm.enqueue_on_with_priority(cpu, ThreadId(tid), priority) {
    Ok(())     => ReceiverEnqueue::Enqueued { cpu },
    Err(error) => ReceiverEnqueue::Rejected { cpu, error },
}
```

> **Partly unsound — corrected in §6.1.27.** `Rejected`'s documented meaning ("the receiver is in
> no run queue") is false for `AlreadyQueued`, which reports *pre-existing* membership.

`ReceiverEnqueue` adds no parallel taxonomy. The five distinctions the direct paths must tell
apart are exactly `SchedulerError`'s — `InvalidCpu`, `CpuOffline`, `WakeOnly`, `QueueFull`,
`AlreadyQueued` — so the outcome carries `SchedulerError` verbatim and adds only the one thing it
cannot say: *which CPU* a success landed on. `WakeOnly` is new: it used to be folded into
`CpuOffline`, and "the target is down" versus "the target is up but refuses work" are materially
different answers for a wake.

**The load-bearing rule is structural.** `enqueued_cpu()` is the only accessor, it returns `None`
for every rejection, and both transactions bind through an `Enqueued` let-else — so a
`wake_target_cpu` cannot be written down unless `enqueue_on_with_priority` returned `Ok` under the
authoritative rank-1 acquisition. No success object exists on failure, and the drain's IPI and
retirement marker both sit inside `if let Ok(success)`, so a refused enqueue signals nothing.

#### Route B: complete rollback, not a bare `Err`

At the enqueue, NR6 has already reserved and published a reply record, minted and bound a
server-local reply cap, copied payload and metadata into the server's buffers, claimed the
endpoint waiter, transitioned the server `Runnable` and registered the reverse link. Returning
`Err` there would leave the server **Runnable but in no run queue** — unschedulable forever, and
invisible to the reply timeout, which only fires for a *blocked* task. That is what
`ca55400b`'s live `-smp 2` run actually produced.

Preflight admission (route A) was rejected: `QueueFull` is genuinely racy against other CPUs'
enqueues, so a preflight could not stay authoritative, and the real enqueue result must remain the
authority. So the publication is undone, in exact reverse order:

| # | published | undone by |
|---|---|---|
| 11b | reverse link registered | `unregister_server_reply_link_split` (idempotent) |
| 11 | record `Reserved → Available` | `cancel_direct_reply_record_split` |
| 6/7 | provisional server-local reply cap | `sr_revoke_split` |
| 10 | server `Blocked → Runnable` | `sr_uncommit_blocked_receiver_split` *(new)* |
| 9 | endpoint waiter claimed | `sr_restore_endpoint_waiter_split` |

Order matters twice. Record and cap go first, so no reply authority is reachable at any instant.
And the receiver must return to `Blocked` **before** its waiter is restored, because
`sr_restore_endpoint_waiter_split` prevalidates exactly that. The wait reason is captured from the
TCB immediately before the commit clears it. The blocked-return registers the commit zeroed are
deliberately not restored — they are the recv syscall's return lanes, meaningless while blocked,
and rewritten by whatever eventually completes the task. `blocked_recv_state` is untouched: it was
consumed into the snapshot at ack production, *before* the transaction began, so it is already
`None` in the state being restored to.

The payload and metadata already copied into the receiver's buffers cannot be un-copied. They are
never *observed*: the receiver returns to `Blocked` with no reply cap and no record, so it never
returns from `recv` on the strength of them, and the next delivery overwrites the same buffers.

**NR6 is retryable.** The ack lease is restored when the exact server is blocked again, so a later
drain can deliver. The end-to-end test proves this the strongest available way: after the
rollback it removes the refusal and re-runs the *same* transaction, which succeeds and yields a
real `wake_target_cpu`. That can only happen if the record slot, reverse link, CNode slot and
waiter were all genuinely returned.

**NR7 is terminal.** Its enqueue sits after `consume_reply_record_split` — the one-shot barrier.
Un-consuming would re-arm a second reply, so the record stays `Consumed`, which is the same
terminal the `CallerGone` arm uses (`discard_reply_record_split` *is* the consume). What is undone
is the caller's own state: back to `Blocked` with its waiter reinstalled, so the existing reply
timeout can still complete it. The lease is discarded, never restored.

> **WRONG — corrected in §6.1.27.** There is no "existing reply timeout" for this population.
> `classify_direct_reply_eligibility` declines `terminal_arbitrated` replies *before any
> mutation*, and `terminal_arbitrated` means exactly "a reply timeout is armed for this record
> incarnation" — so every direct-eligible reply is **untimed**. Leaving the caller `Blocked` with
> the record spent stranded it with **no terminal owner at all**.

The NR6 reverse-link-registration failure arm left the identical state and had the identical gap;
it now runs the same shared rollback rather than a second, weaker one.

#### Disposition

Both `EnqueueRejected` variants classify as `Failed(SyscallError::Internal)`, not
fallback-eligible. This file's standing rule is that anything past the copy line never falls
through, and both sit after step (8)/(4). The kernel could not *place* the wake — an internal
condition, not a userspace error.

#### Evidence

Twelve focused tests drive the **real** seam for each distinction — successful local, successful
remote to a dispatching CPU, online wake-only, offline, out-of-range, full queue, already queued —
plus the two end-to-end transaction rollbacks through the existing NR6/NR7 blocked fixtures, and
the structural proof that no rejection can yield a wake target by any route.

The regression test reproduces `ca55400b` exactly: target online **and** wake-only, enqueue
refused, task in no runqueue, and the seam must not answer `Enqueued { cpu: CpuId(1) }`.

Three guards that pinned the old contract were **updated, not deleted** —
`a_stale_home_cpu_fails_closed` had literally asserted `assert_eq!(target, bogus)` alongside "and
nothing is queued there", which is the defect written down as if it were correct.


### 6.1.27 The two unsound enqueue-rejection contracts — repaired

§6.1.26 fixed the false-success contract but shipped two unsound rejection contracts. Both are
closed here. No production predicate, no `wake_only` policy, no affinity, no SBI/IPI, no AArch64,
no 199E, and **no RISC-V status change**: link 1 **ABSENT**; links 2, 3, 7, 9 present; 4, 5, 6, 8,
10 absent; `RISCV_REMOTE_WAKE` = **D**; `RISCV_199D_READINESS` = **`case_b`**; coordinate 23
**OPEN**; ledger **39 / 7 / 46**; **no new live cell**.

#### Finding A — `AlreadyQueued` is not "nothing is queued"

`ReceiverEnqueue::Rejected` documented itself as "the receiver is in **no** run queue". That holds
for `InvalidCpu`, `CpuOffline`, `WakeOnly` and `QueueFull` — all fail before touching a queue. It
is **false** for `AlreadyQueued`, which reports *pre-existing membership*. Worse:
`PriorityScheduler::contains_tid` reads the membership mirror, which tracks the queues **plus the
dispatched `current` task** — so `AlreadyQueued` can mean *the receiver is executing right now*.
Running the ordinary `Runnable → Blocked` + waiter-restore rollback on that produces a `Blocked`
task that is still queued or current, and claims a restoration that did not happen.

Three changes:

1. **The reason survives.** `EnqueueRejected(SchedulerError)` on both directions, instead of one
   information-free variant. `Rejected` now states only that *this* enqueue did not commit.
2. **Reconciliation shares the detecting acquisition.** On `AlreadyQueued` the seam calls
   `withdraw_queued_tid_on` **inside the same `with_scheduler_split_mut` closure**, on the same
   `sm` binding. The seam contains exactly one acquisition — a second one *would be* the
   unlock/relock window — and never reconciles through `self`, which would re-acquire.
   `Rejected` carries the resulting `WithdrawOutcome`.
3. **Only `Removed` may roll back.** `receiver_is_unplaced()` is true for the four
   never-touched-a-queue reasons, and for `AlreadyQueued` only when the reconciliation removed
   exactly one queued entry — which by construction was *not* `current`. `RefusedCurrent`,
   `RefusedDuplicate`, `NotQueued` and `InvalidCpu` fail closed through
   `EnqueueRejectedUnreconciled(WithdrawOutcome)`: the externally visible authority is reclaimed,
   and **no claim is made that the receiver was restored**.

> **Placed too late — corrected in §6.1.28.** The preflight sat *after* the record reservation,
> the reply-cap mint and the user copy, so it could not support retry or restoration.

**On the hard-stop.** "Could an `AlreadyQueued` task already have executed or observed the
publication?" Rather than argue the window, it is **closed**: both transactions now run a
pre-commit membership preflight (NR6 step 9c, NR7 step 5c). A receiver that is still `Blocked`
with its endpoint waiter exclusively claimed cannot legitimately hold scheduler membership and
nothing can wake it while both are true — so the preflight does not race — and it declines
**before the first irreversible mutation**. After that, a post-publication `AlreadyQueued` can only
come from an invariant violation, and it fails closed rather than pretending. The `RefusedCurrent`
branch is precisely the "may already have observed the publication" case, and it never claims
restoration.

#### Finding B — a direct-eligible NR7 has no timeout owner

§6.1.26 justified leaving the caller `Blocked` with the record `Consumed` by saying the "existing
reply timeout" would complete it. That is false for exactly the population that reaches the direct
path: `classify_direct_reply_eligibility` declines `terminal_arbitrated` replies **before any
mutation**, and `terminal_arbitrated` *means* a reply timeout is armed for that record incarnation.
The whole direct-eligible reply population is therefore **untimed**. The claim is deleted.

**Route A** is implemented: the exact one-shot authority is restored so the same reply retries.
`restore_consumed_reply_record_split` moves the record `Consumed → Available` only when

* the slot is at the **exact generation** the transaction owned (a recycled slot can never be
  re-armed), and
* it is still bound to the **exact replier identity** `{tid, asid}` that consumed it, and
* it is in `Consumed` — not `Cancelled`, not already `Available`, so the restore cannot stack.

`consume_reply_record_split` closes the reverse link on the same edge, so the restore
re-registers it: the replier owes the reply again, exactly as before. The acknowledgement lease is
restored, not discarded.

> **Not atomic — corrected in §6.1.28.** The record was published `Available` *before* the link
> registration was attempted, so a refused registration left `Available`-without-link. Re-arming is admissible **only** when `receiver_is_unplaced()` holds —
otherwise the caller may already have observed the delivery, and arming a second reply against it
would be unsound.

#### What is proved after every recoverable rejection

Driven end-to-end through the existing NR6/NR7 blocked fixtures, for **each** of the four
reachable `SchedulerError` reasons:

* the task is `Blocked` on the **exact original recv cap** (compared as
  `WaitReason::EndpointReceive(cap)`, not merely "some Blocked");
* its exact waiter is restored **once**;
* it is neither queued nor current on any CPU;
* no success object, retirement marker or IPI exists — structurally, since both bind through an
  `Enqueued` let-else and the drain's IPI and marker sit inside `if let Ok(success)`;
* no reply cap, record or reverse-link leak — for NR6 proved by re-running the *same* transaction
  to success; for NR7 by the restored authority retrying, succeeding **exactly once**, and a
  duplicate remaining rejected, with `live_server_reply_link_count_split_read() == 1`.

#### Live verification of the layout change

Embedding the table changes production `IpcSubsystem` layout, so a live run is required rather
than optional. `scripts/qemu-x86_64-core-smoke.sh` (`-smp 1`) boots clean on the new layout: all
six service entries present exactly once, every `UNLOCK_GRADUATED_*` marker present, and the
WA1-GATE seals re-emitted unchanged from the live kernel —

```text
IPC_DIRECT_PRODUCTION_DISABLED_SEAL production_enabled=0 ordinary_nr6_direct=0 \
    ordinary_nr7_direct=0 proof_nr6_available=1 proof_nr7_available=1 result=ok
IPC_DIRECT_PRODUCTION_QUIESCENT_SEAL_SKIPPED reason=production_default_disabled census_ok=1 result=ok
```

The x86_64 and RISC-V direct **proof** builds still compile unchanged; neither gains a cell. A
hosted test additionally asserts a freshly booted kernel holds **zero** ownership claims, which is
what helper-only means at runtime.

#### Mutation results

Three mutations, each caught:

| Mutation | Caught by |
|---|---|
| reconciliation replaced by a constant `Removed` (i.e. no atomic withdrawal) | `the_membership_reconciliation_shares_the_detecting_acquisition`, `already_queued_distinguishes_exactly_one_current_and_duplicate`, `no_blocked_task_ever_holds_queued_or_current_membership` |
| replier identity guard dropped from the authority restore | `the_reply_authority_restore_is_identity_and_generation_exact` |
| reverse-link re-registration dropped | initially **structural only** — a behavioural link-count assertion was added, and it now fails too |


### 6.1.28 Three rejection-safety defects — repaired

Review of §6.1.27 found three. All closed here. No production predicate, no `wake_only` policy,
no affinity, no SBI/IPI, no AArch64, no ServerDies, no 199E, no RISC-V link-1 work.
**RISC-V status recomputes unchanged**: link 1 **ABSENT**; links 2, 3, 7, 9 present; 4, 5, 6, 8,
10 absent; `RISCV_REMOTE_WAKE` = **D**; `RISCV_199D_READINESS` = **`case_b`**; coordinate 23
**OPEN**; ledger **39 / 7 / 46**; **no new live cell**.

#### 1. Membership detection moved before user-visible mutation

§6.1.27's preflight was placed too late. Traced exactly:

| direction | first userspace-observable mutation | old preflight | new |
|---|---|---|---|
| NR6 | (5) record reserved → visible to another transaction; (6)/(7) provisional cap in the **server's own cnode**; (8) copy into the server's user memory | (9c), after all three | **(4a)**, before all three |
| NR7 | (3) record `Available → Reserved`; (4) copy into the caller's user memory | (5c), after both | **(2b)**, before both |

A receiver reported `RefusedCurrent` may already be executing and may already have read those
bytes, so a check after them cannot support retry or authority restoration. The new check runs
before any user copy, any provisional capability in the receiver's cnode, any record state
exposed to another transaction, any waiter claim and any TCB mutation.

A legitimate `Blocked` receiver holding a committed waiter cannot acquire scheduler membership —
nothing can wake it while both are true — so an early positive result is an **invariant
violation**, not a retryable `WouldBlock`. It performs no mutation, **discards** the claimed
acknowledgement rather than re-arming it, returns the typed
`ReceiverMembershipViolation`, and claims nothing about the receiver being restored or unplaced.

The post-copy defence remains for genuine invariant violations, classified by the
same-acquisition `WithdrawOutcome`: `Removed` → rollback may proceed; `RefusedCurrent` →
publication may have been observed, never restore; `RefusedDuplicate` / `NotQueued` /
`InvalidCpu` → fail closed. And **no post-copy membership detection returns retryable
authority**: NR6 settles the lease only when `reconciled.is_none()`, NR7 restores the authority
only when `reconciled.is_none()` — so even a cleanly-`Removed` collision found after the copy is
terminal.

`direct_server_exact_still_blocked` and `direct_caller_exact_still_blocked` now also require the
absence of scheduler membership: `Blocked` plus an intact waiter is **not** sufficient when the
task is queued or current.

#### 2. The NR7 authority restore is all-or-nothing

The old version published `Consumed → Available` and only afterwards attempted registration,
which permits `Available`-without-link — an invokable reply with no teardown-visible link.

Replaced by one composed transaction. The task lock (rank 2) is taken first and held across the
whole operation; the ipc lock (rank 3) is nested inside it — ascending rank order, so the
discipline is preserved. Under that single hold: **(1)** locate the exact replier incarnation and
validate its link slot free (or already exactly ours) and its status live, **writing nothing**;
**(2)** at rank 3 validate the record (exact index, exact generation, `Consumed`, bound to this
exact replier) and flip it; **(3)** install the link through the shared
`install_server_reply_link` decision, which also stamps the leak accounting.

Step 1 makes step 3 unreachable, but the revert is implemented anyway and **exercised**: a
`#[cfg(test)]`-only fault hook forces the install to fail after the flip, and
`a_forced_link_install_failure_leaves_the_record_consumed` proves the record is reverted. Only
two outcomes are observable: **A** record `Available` and the exact link installed, or **B**
record `Consumed` and no newly installed link.

Five failure cases each proved to leave outcome B, with the live link count unchanged and no
second reply invokable: occupied link slot, changed replier incarnation, recycled generation,
already-`Available` record, `Cancelled` record.

#### 3. The hidden shared-region side effect is gone

§6.1.27 put the `AlreadyQueued` reconciliation in the seam that the shared-region finalizer also
calls, so that caller silently withdrew a pre-existing scheduler entry it had no rollback for.

**Option A** was taken — a separate seam, not a flag:

* `sr_enqueue_committed_receiver_split` — generic, **never** reconciles (`reconciled: None`);
* `sr_enqueue_committed_receiver_reconciled_split` — direct-IPC only, used by exactly the two
  transactions.

The shared-region finalizer cannot select the reconciling one because it calls the other
function; a guard pins that it names neither `reconciled_split` nor `withdraw_queued_tid`, and
that the reconciling seam has exactly two call sites.

Its rejection contract is repaired too: it no longer returns `Some(true)` after a refusal. On the
four never-touched-a-queue reasons it restores its own receiver (`Runnable → Blocked` on the
exact recv cap, waiter reinstalled) and reports `None`. On an unreconciled `AlreadyQueued` it
fails closed with **zero mutation** — it does not force `Blocked` on top of live membership and
does not touch the queue entry. Behavioural tests cover `WakeOnly`, `QueueFull`, and
`AlreadyQueued` as exactly-once, current and duplicate, each proving the pre-existing membership
is untouched.


### 6.1.29 Waiter-ownership exclusivity — HARD-STOP; Part B delivered

Three items were requested. **Part B is delivered.** **Parts A and C are hard-stopped** on the
same finding, and the audit that establishes it is executable and pinned
(`stage199d_waiter_ownership_exclusivity_audit`).

**`WAITER_OWNERSHIP_EXCLUSIVE=no`.**

Status unchanged: link 1 **ABSENT**; links 2, 3, 7, 9 present; 4, 5, 6, 8, 10 absent;
`RISCV_REMOTE_WAKE` = **D**; `RISCV_199D_READINESS` = **`case_b`**; coordinate 23 **OPEN**;
ledger **39 / 7 / 46**; **no new live cell**.

> **Read with §6.1.30.** This section was written while the x86 direct production default was
> still ON. Stage 199D-WA1-GATE subsequently disabled it — the **production default is OFF** on
> every architecture, admission and acknowledgement publication both require an explicit
> proof/oracle selector, and `AlreadyQueued` + `Removed` is terminal on every freestanding
> runtime build. Where the two sections touch the same contract, **§6.1.30 is operative**; the
> statements below have been corrected in place rather than left to be overridden silently.

#### A/C — the hard-stop

Both parts rest on the same premise: that claiming the exact generation-bearing endpoint waiter
*before* any user-visible mutation gives the transaction **exclusive** ownership of the receiver,
so nothing else can wake it between the claim and the commit. The instruction was explicit —
audit every non-endpoint wake owner, and hard-stop if any can legitimately wake this exact
receiver without invalidating or arbitrating against the owned claim.

Six owners were enumerated and classified from named source seams:

| Owner | Reaches the receiver via | Arbitrates against an owned claim? |
|---|---|---|
| endpoint send delivering to a blocked receiver | `take_endpoint_waiter` | **yes** — through the waiter table |
| the direct NR6/NR7 transactions | `sr_claim_endpoint_waiter_split` | **yes** — same table, one winner |
| server-death completion | `try_claim_peer_death_terminal` → revalidate caller + endpoint generation + blocked-recv generation → waiter claim | **yes** — terminal *and* waiter |
| reply-receive timeout (token-bearing) | `reply_terminal_ownership` | **yes** — terminal *and* waiter |
| **ordinary IPC timeout scan** | `tcb.ipc_timeout_deadline`, by TCB scan | **NO** |
| **notification signal wake** | `notification_waiters[i]`, by TID | **NO** |

The last two break exclusivity, and they break it *mechanically*, not incidentally:

* **The ordinary timeout scan wakes before it invalidates.** Phase 1 (rank 2) walks every TCB and
  sets `Runnable` for any `Blocked(WaitReason::EndpointReceive(_))` whose deadline expired. Phase 2
  (rank 3) clears the waiter slots **afterwards**. Because the wake strictly precedes the waiter
  invalidation, an owned claim cannot make it lose — there is no point at which the scan consults
  the waiter table before waking. Pinned by
  `the_timeout_scan_wakes_before_it_clears_the_waiter`.
* **The notification signal wake never consults the endpoint waiter at all.** It takes a TID out
  of `notification_waiters`, and its only guard is `matches!(tcb.status, Blocked(_))` — true for
  our receiver right up to commit. `endpoint_waiters` is never read. Pinned by
  `the_notification_wake_never_consults_the_endpoint_waiter`. The code's own comment documents
  that a snapshotted notification waiter can race a task woken by another route, so the staleness
  this depends on is an acknowledged, handled condition — not a hypothetical.

Ordinary recv/send deadlines are armed **independently** of `reply_timeout_token`, so
`terminal_arbitrated` — which gates direct NR7 eligibility — does not exclude them. The ordinary
timeout race with a direct publication is therefore **production-reachable**, not merely a
mechanism-level concern. The seven-site assignment evidence is in §6.1.30.

So the reorder is **not performed**. NR6 keeps its single claim at step (9) and NR7 at step (5);
there is still exactly one claim per transaction (pinned). The pre-mutation membership checks from
§6.1.28 remain — they are TOCTOU preflights, correctly described as such, and they are strictly
better than nothing while being no substitute for ownership.

**Part C is hard-stopped for the same reason.** The shared-region publication lease must claim the
waiter before the first receiver-visible cap mint, mapping or metadata copy, and then rely on that
claim to guarantee the receiver cannot run while resources are being published. That guarantee is
exactly what this audit refutes. Building the lease anyway would encode the same false exclusivity
into a third subsystem, and the instruction's own escape applies: *"Hard-stop and document the
missing terminal owner if the current case cannot be represented without revoking resources
underneath a running receiver."* The missing terminal owner is a **waiter-claim-aware wake
arbitration** that the ordinary timeout scan and the notification wake both honour.

**The contract that must be split first.** Either (1) reorder the ordinary timeout scan to
invalidate the waiter *before* waking, and give the notification wake a waiter-claim check, so
both lose to an owned claim; or (2) introduce a per-task wake-arbitration token that every wake
owner must claim, of which the endpoint waiter becomes one holder. Route 1 is smaller and is the
recommended next increment; both are production wake-path changes with their own live seals, and
neither belongs in a rejection-safety repair.

#### B — the rollback algebra for same-acquisition `Removed` (hosted-test evidence only)

§6.1.28's rule that every `reconciled.is_some()` is terminal was over-broad **as an algebraic
statement**, and §6.1.29 corrected the algebra. What `WithdrawOutcome::Removed` actually proves is
narrower than that correction originally claimed:

> `Removed` proves that **exactly one queued entry was withdrawn under the detecting scheduler
> acquisition**, and that the task was **not `current` at that acquisition**. It does **not**
> prove the task never ran, and it does not prove the task never observed an earlier publication.

Those two are different facts, and only the first is established. While waiter ownership is
non-exclusive another wake owner could have dispatched the task and re-queued it, leaving exactly
one entry and a non-`current` task at the moment we look.

**So the split is:**

| build | `AlreadyQueued` + `Removed` |
|---|---|
| hosted `#[cfg(test)]` | recoverable — the rollback algebra is exercised end-to-end |
| **every freestanding runtime build**, proof/oracle kernels included | **terminal**: restores neither the acknowledgement nor NR7 reply authority, and never returns the retryable variant |

`ReceiverEnqueue::rejection_is_runtime_recoverable()` is the predicate that enforces it
(§6.1.30 §3). The four reasons that provably fail **before touching a runqueue** —
`InvalidCpu`, `CpuOffline`, `WakeOnly`, `QueueFull` — keep their existing policy in every build.
Terminal in every build: `RefusedCurrent`, `RefusedDuplicate`, `NotQueued`, and an `InvalidCpu`
withdrawal outcome.

##### Hosted test-only evidence

Everything below is `#[cfg(test)]` evidence about the **rollback algebra**, not a description of
production behaviour. Because §6.1.28's pre-mutation check makes an ordinary post-copy
`AlreadyQueued` unreachable, the path is driven by a `#[cfg(test)]`-only hook that injects
membership immediately before the final enqueue (`1` = one queued entry → `Removed`;
`2` = dispatched → `RefusedCurrent`). Through the real transactions:

* **NR6 (hosted test only)** — `EnqueueRejected(AlreadyQueued)`, server `Blocked` on the exact
  original recv cap, exact waiter restored once, zero membership, lease restored, and the same
  transaction retried successfully.
* **NR7 (hosted test only)** — `EnqueueRejected(AlreadyQueued)`, exact caller `Blocked` on the
  exact recv cap, exact waiter restored once, record `Available` **at the same generation**,
  exact replier reverse link present (live link count 1), acknowledgement restored, the same
  reply retried successfully, and a duplicate still rejected.
* **NR6 `RefusedCurrent`** — `EnqueueRejectedUnreconciled(RefusedCurrent)`, lease discarded, no
  restoration claim. This one holds in **every** build.

On a freestanding runtime build neither retry above occurs: the rejection is terminal before it
can restore anything. §6.1.30 §3 is the operative contract; this section does not override it.

The accepted §6.1.28 composed record + reverse-link restore is preserved unchanged.


### 6.1.30 WA1-GATE — the x86 direct production default is OFF

Predicate, rejection policy, evidence and ledger only. No timeout processing, notification wake,
waiter representation, `WaitReason`, endpoint-waiter claiming, shared-region publication, RISC-V
link 1, SBI/IPI, affinity, `wake_only`, ServerDies or 199E work.

#### 1. The reachability record, corrected

§6.1.29 argued the ordinary timeout mechanism was unreachable for the direct-eligible population.
**That is retracted.** The argument rested on a grep for `ipc_timeout_deadline = Some(...)`, which
found exactly one site — the reply-receive timeout, which also arms `reply_timeout_token` under an
open terminal cell. A complete audit of `ipc_timeout_deadline\s*=` finds **seven** assignments,
of which three arm an ordinary deadline by assigning a variable:

| site | wait reason set alongside | reply token? |
|---|---|---|
| `ipc_state.rs:2011` `= Some(deadline_tick)` | reply-receive (terminal-arbitrated) | **yes** |
| `ipc_state.rs:3110` `= deadline` | `Blocked(EndpointReceive(plan.recv_cap))` in `recv_block_phase_b_task` | no |
| `ipc_state.rs:3412` `= deadline` | `Blocked(EndpointSend(send_cap))` | no |
| `ipc_state.rs:6441` `= deadline` | `Blocked(EndpointReceive(recv_cap))`, queued-recv block | no |
| `322`, `2968`, `3588` | clears (`= None`) | — |

**Corrected result.** Ordinary recv/send deadlines exist **independently** of
`reply_timeout_token`, so `terminal_arbitrated` — which gates direct NR7 eligibility — does not
exclude them. `process_ipc_timeout_deadlines` can therefore genuinely race an endpoint-blocked
receiver **during** a direct NR6/NR7 publication: it wakes at task rank before invalidating the
waiter at ipc rank, so it never consults the claim. `WAITER_OWNERSHIP_EXCLUSIVE` remains **no**,
and this is a **reachable production safety issue**, not a mechanism-level concern.

`every_ipc_timeout_deadline_assignment_site_is_enumerated` pins the complete list, so the
`= Some` error cannot recur.

#### 2. The gate

`ipccall_direct_production_enabled()` returns `false` on **every** architecture. Its body is
exactly `false` — no `target_arch`, no `cfg!`, no `||`, no atomic load — so no architecture branch
or boot selector can silently restore it. `ipccall_direct_admission_enabled()` is unchanged in
form (`production || proof`); only its production term went false, which makes admission exactly
the proof gate. Every explicit selector survives verbatim: `ipccall_direct_proof_enabled`, the
three per-arch oracle selectors and the umbrella `ipccall_direct_oracle_enabled`.

Driven behaviourally: with the proof selector clear, admission **and** publication are both
false — ordinary NR6/NR7 reach neither the direct transaction nor the blocked-waiter
acknowledgement, and fall back to the accepted legacy path. With it set, both are true.

#### 3. `AlreadyQueued + Removed` fails closed

`Removed` proves only that one queued entry was removed under the detecting scheduler
acquisition. It does **not** prove the receiver never ran, nor that it never observed an earlier
publication, while waiter ownership is non-exclusive. `rejection_is_runtime_recoverable()`
therefore makes it recoverable only under `cfg!(test)`; every freestanding runtime build —
including the explicit proof/oracle kernels — takes the terminal path, restoring no
acknowledgement, no NR7 reply authority and never returning the retryable variant. The four
reasons that provably fail before touching a runqueue keep their existing policy unchanged.

#### 4. Evidence, not re-labelled

The `0b5ec254` seal remains valid **historical** evidence that the x86 production default once
ran NR6 and NR7 off-lock. It is not evidence about the current configuration, and it is **not**
re-emitted with changed semantics. A distinct current-state seal is added beside it, computed
from the same authoritative per-direction counters (`REQUEST.completed` / `REPLY.completed`) —
never inferred from absent user logs:

```text
IPC_DIRECT_PRODUCTION_DISABLED_SEAL production_enabled=0 ordinary_nr6_direct=0 \
    ordinary_nr7_direct=0 proof_nr6_available=1 proof_nr7_available=1 result=ok
```

#### 5. Ledger: 39 / 7 / 46

Exactly one cell moves. The ledger records the x86 NR6+NR7 production-default increment as **one
combined cell**, not two, so current production goes 40 → **39** and non-production/mechanism
evidence goes 6 → **7**. The historical total is **46**, unchanged — nothing is retracted and **no
new live cell is earned**.

The seventh non-production cell is described accurately: originally earned **under the x86_64
production default** at `0b5ec254`, retained as historical mechanism/production evidence, and no
longer a claim about the current production predicate. It was **not** originally earned under a
proof knob.

#### Status

* ordinary timeout reachability correction — **accepted**
* `WAITER_OWNERSHIP_EXCLUSIVE` — **no**
* waiter-claim-aware timeout arbitration — **not implemented**
* generation-bearing notification arbitration — **not implemented**
* x86 direct production default — **OFF**
* explicit proof/oracle direct mechanism — **preserved**
* current ledger — **39 / 7 / 46**
* canonical 199D — **OPEN**
* RISC-V links/status — unchanged (link 1 ABSENT; 2, 3, 7, 9 present; 4, 5, 6, 8, 10 absent;
  `RISCV_REMOTE_WAKE=D`; `RISCV_199D_READINESS=case_b`; coordinate 23 OPEN)
* **no new live cell**

---

### 6.1.31 WA2A — the generation-bearing waiter-ownership primitive (helper-only)

> **Read with §6.1.32.** WA2A-R1 replaced this section's primitive (associative 64-slot table,
> transparent token, wrapping generation, `src/kernel/waiter_ownership.rs`) with an
> endpoint-indexed one owned by `IpcSubsystem`, and narrowed its census claim. Where the two
> sections touch the same contract, **§6.1.32 is operative**; §6.1.31 records what WA2A shipped
> and why it was insufficient.

An ownership-model increment only. No production call site, no transaction reorder, no timeout,
notification, shared-region, RISC-V, SBI/IPI, affinity, `wake_only`, ServerDies or 199E change,
and no change to the production predicate. §6.1.30 remains operative on every contract it states.

#### 1. The waiter-primitive callsite census

> **Relabelled at WA2A-R1.** This table is a **waiter-primitive callsite census**, not an
> exhaustive wake-owner census. Its collection method — grepping the four waiter primitives —
> can only find paths that touch a waiter, so by construction it cannot see a path that wakes a
> blocked receiver without touching one. §6.1.32 runs the independent census that can, finds
> seven more owners. §6.1.32 D set `WAITER_OWNER_CENSUS_COMPLETE` to `no`; §6.1.34 resolves the
> remaining twelve rows and raises it to **yes**.

Every production path that calls one of the four waiter primitives (`set_endpoint_waiter`,
`take_endpoint_waiter`, `clear_endpoint_waiter_if_identity`,
`clear_endpoint_waiters_for_identity`), mapped to its enclosing function:

| # | function | lock domains | waiter identity | endpoint generation | consults waiter? |
|---|---|---|---|---|---|
| 1 | `publish_recv_waiter_live` | ipc(3) | exact `{tid,asid}` | yes (slot) | installs |
| 2 | `try_publish_recv_waiter_audit_only` | ipc(3) | exact | yes | installs |
| 3 | `sr_claim_endpoint_waiter_split` | ipc(3) | exact | **yes, compared** | claims exactly once |
| 4 | `sr_restore_endpoint_waiter_split` | task(2) probe → ipc(3) | exact | yes | restores exact identity |
| 5 | `ipc_try_send_to_plain_receiver_endpoint_only` | ipc(3) | exact | yes | clears iff identity matches |
| 6 | `ipc_try_send_sync_endpoint_only` | ipc(3) | exact | yes | clears iff identity matches |
| 7 | `ipc_clear_plain_receiver_waiter_only` | ipc(3) | exact | yes | clears iff identity matches |
| 8 | `wake_waiter_for_endpoint` | ipc(3) → task(2) → sched(1) | index only | slot | takes unconditionally |
| 9 | `ipc_reply` | broad | index only | slot | takes unconditionally |
| 10 | `ctx_finalize_and_wake` (shared region) | ipc(3) | index only | slot | takes unconditionally |
| 11 | `clear_ipc_waiters_for_tid` (teardown) | ipc(3) | identity sweep | — | clears by identity |
| 12 | **`process_ipc_timeout_deadlines`** | task(2) **then** ipc(3) | identity sweep | — | **NO — wakes at task rank first** |
| 13 | **notification signal wake** | ipc(3) → task(2) → sched(1) | TID only | — | **NO — different table** |
| 14 | server-death completion | terminal → ipc(3) → task(2) | exact + blocked-recv gen | yes | claims terminal *and* waiter |
| 15 | reply-receive timeout | terminal → ipc(3) → task(2) | exact + blocked-recv gen | yes | claims terminal *and* waiter |

Rows **12** and **13** are the two exclusivity breaks named in §6.1.29/§6.1.30 — the same two
that keep `WAITER_OWNERSHIP_EXCLUSIVE=no`. Rows **8**, **9** and **10** take by index with no
identity compare, so they cannot distinguish a replacement waiter from the one they meant. The
census lives in `doc/` and in the module doc of `src/kernel/boot/waiter_ownership.rs`;
`the_already_found_facts_stay_explicit` pins that every row, both breaks and the three
index-only removers survive the relabelling.

#### 2. The primitive

`WaiterOwnershipTable` is a bounded (`WAITER_OWNERSHIP_SLOTS = 64`), allocation-free typed state
machine. Its key is exact in **four** dimensions where the waiter table is exact in two:

* endpoint index **and** endpoint generation,
* waiter `tid` **and** `asid` (via the existing `ReceiverWaiterIdentity`),
* the task's blocked-receive generation.

A recycled endpoint slot, a reused numeric TID under a new ASID, and a task that unblocked and
reblocked are therefore three distinct keys, and none can inherit another's claim. State is
`Available → Claimed{owner, claim_generation} → Consumed | Cancelled`, never a bool; owners are
the typed `DirectRequest | DirectReply | OrdinaryTimeout | LegacyDelivery | Notification |
Teardown`. **Naming an owner does not wire that path.**

`claim()` returns an owned, `Copy` `WaiterClaimToken`. Restoration (`consume`, `restore`,
`cancel`) validates the full key **and** the owner **and** the `claim_generation`, so a token
retained across a claim/cancel/re-claim cycle is rejected — including when the *same* owner
re-claims, which is the case a naive owner-only check would miss.

#### 3. Lock discipline

The module acquires **nothing**. It is a pure state machine over owned data borrowed as
`&mut WaiterOwnershipTable`, so the caller supplies the rank-3 ipc guard and the primitive
structurally cannot nest task(2) or scheduler(1) beneath it. The token is `Copy` and outlives the
guard. `the_primitive_touches_no_other_subsystem_and_takes_no_lock` pins that the source contains
no lock acquisition and no cross-subsystem import.

#### 4. Mutation results

Five mutations were applied to the real primitive and the hosted suite re-run:

| # | mutation | caught? |
|---|---|---|
| M1 | drop the owner comparison in `validate` | yes — 3 tests fail |
| M2 | drop the `claim_generation` comparison | **initially NO** |
| M3 | drop endpoint generation from the key | yes |
| M4 | drop ASID from the key | yes |
| M5 | drop the blocked-wait generation from the key | yes |

M2 survived because the existing stale-token test re-claimed under a *different* owner, so the
owner check masked the missing generation check. That is a real gap in the test, not in the
primitive: `a_stale_token_is_rejected_even_when_the_same_owner_reclaims` was added, and M2 now
fails. The gap is recorded rather than quietly repaired.

#### 5. Why no QEMU run

The precondition is mechanical, not asserted. Three files change: `src/kernel/mod.rs` (one module
declaration), `src/kernel/boot/tests.rs` (`#[cfg(test)]`-gated in its entirety) and the new
`src/kernel/waiter_ownership.rs`. Building the freestanding `x86_64-yarm-none` kernel at the base
commit and at this commit and diffing the defined-symbol sets of `libyarm.rlib` gives:

```text
symbols removed: 0
symbols changed: 0
symbols added:   2   (kernel::waiter_ownership::WaiterOwnershipTable::{new, Default::default})
```

Both added symbols are constructors with **zero callers anywhere in the tree**. No existing
symbol was added, removed or renamed, so no production runtime path changed and there is nothing
new for a live run to observe. `the_primitive_has_no_production_caller` walks the source tree and
enforces the zero-caller property going forward, with a non-vacuity assertion so it cannot pass
by finding nothing.

#### Status

* `WAITER_OWNERSHIP_EXCLUSIVE` — **no** (unchanged; rows 12 and 13 still break it)
* waiter-claim-aware timeout arbitration — **not implemented**
* generation-bearing notification arbitration — **not implemented**
* NR6/NR7 waiter claim position — **unchanged** (single late claim, step 9 / step 5)
* x86 direct production default — **OFF** on every architecture
* current ledger — **39 / 7 / 46** (no cell moves)
* canonical 199D — **OPEN**
* RISC-V links/status — unchanged (link 1 ABSENT; 2, 3, 7, 9 present; 4, 5, 6, 8, 10 absent;
  `RISCV_REMOTE_WAKE=D`; `RISCV_199D_READINESS=case_b`; coordinate 23 OPEN)
* **no new live cell**

---

### 6.1.32 WA2A-R1 — repairing the ownership foundation

> **Read with §6.1.33.** WA2A-R2 replaced this section's slot model — `claim` could install a key
> over a *terminal* one, which accepted a delayed request for an older incarnation — with an
> explicitly armed current incarnation, made the view truthful, and corrected the encapsulation
> claim in §B below. Where the two sections touch the same contract, **§6.1.33 is operative**.

Four repairs to §6.1.31, and nothing else: no NR6/NR7, timeout, notification, legacy-delivery,
shared-region or teardown wiring; neither late waiter claim moved; no production predicate,
scheduler policy, RISC-V link, ServerDies path, canonical 199E path or live-cell change. §6.1.30
remains operative on every contract it states.

#### A. The capacity leak, and the structural bound that replaces it

The WA2A table was **associative**: 64 slots, matched on the full incarnation key, retaining
`Available`, `Consumed` and `Cancelled` keys for incarnations that were long finished. Because a
key carries the blocked-wait generation, 64 sequential *completed* waits — one task, one endpoint,
blocking and unblocking 64 times — exhausted it with **zero live claims**. That is a leak in
lifetime, not in size, and no amount of extra slots fixes it.

The table is now **endpoint-indexed**: exactly one slot per endpoint index, with

```rust
pub(crate) const WAITER_OWNERSHIP_SLOTS: usize = ENDPOINT_WAITER_SLOTS;
const _: () = assert!(WAITER_OWNERSHIP_SLOTS == ENDPOINT_WAITER_SLOTS);
```

— derived, never a second numeric literal, and pinned at compile time exactly as
`DIRECT_ACK_STORE_CAPACITY` is. The structural argument is the same one
`crate::kernel::direct_ack_store` makes: at most one endpoint receive-waiter can exist per
endpoint index, so at most one ownership claim can either. A finished incarnation now occupies its
slot only until the **next** incarnation of that endpoint index claims it.

Per-index semantics, all typed and fail-closed:

| state | claim with the exact key | claim with a different incarnation | settle with an exact live token |
|---|---|---|---|
| `Vacant` | claim | claim | `NoSuchClaim` |
| `Claimed` | `AlreadyClaimed{by}` | `AlreadyClaimed{by}` | consume / restore / cancel |
| `Consumed{key}` | `Consumed` | **replace** with a claim | `NotClaimed{Consumed}` |
| `Cancelled{key}` | `Cancelled` | **replace** with a claim | `NotClaimed{Cancelled}` |

An index `>= ENDPOINT_WAITER_SLOTS` is a typed `EndpointIndexOutOfRange`, never a wrap or a panic.
A **live** claim is never evicted — not by a foreign incarnation, not by a later one, not by the
same owner. `restore` returns the slot to `Vacant` rather than to a key-bearing state, so nothing
is held against a future incarnation. A token for an evicted old key gets `NoSuchClaim` and
mutates nothing. `CapacityExhausted` is gone: an endpoint-indexed table has no such mode, and
`the_capacity_is_derived_from_the_endpoint_bound_not_duplicated` pins that it cannot return.

Nine hosted tests cover the required cases, including three that run **10 001** cycles each —
claim/restore, claim/consume/new-incarnation and claim/cancel/new-incarnation — over changing wait
generations without exhausting anything, and one that holds `ENDPOINT_WAITER_SLOTS` simultaneous
live claims.

#### B. Rank-3 ownership by construction

The table is no longer a free-standing type a caller could hold. It is a **private field of
`IpcSubsystem`** (`waiter_ownership_stores = 1`), indexed identically to `endpoint_waiters`, so
reaching it at all requires the ipc rank-3 guard the caller already holds. The module lives under
`src/kernel/boot/`, and every raw method on the table is **module-private**, so the typed
`IpcSubsystem::waiter_ownership_*` methods are the only way to *operate* it.

The field's visibility is `pub(in crate::kernel::boot)`, which is the tightest Rust can express
here: `pub(in …)` requires an *ancestor* module of the declaration, and `boot` is the nearest
ancestor shared by `defs` (which declares the field) and the ownership module (which must reach
it).

> **Corrected at WA2A-R2.** This section originally called a boot-domain `&mut
> WaiterOwnershipTable` **inert**. That was an overclaim: a boot sibling cannot call a method on
> the table, but it could still replace the whole thing by assignment, `mem::replace`/`swap` or a
> raw pointer write. The accurate description is **rank-3 co-location plus source-guarded
> encapsulation, not complete type-system-enforced inertness** — see §6.1.33 D.

Embedding the field changed `IpcSubsystem`'s layout, so the single initializer in
`bootstrap_state.rs` was updated explicitly with `WaiterOwnershipTable::vacant()` — a `const fn`,
matching `TerminalCell::vacant()` / `DeadlineTokenCell::vacant()`. No `unsafe`, no zero
initialization. It is the **only** construction site in the tree.

#### C. An opaque, generation-safe token

`WaiterClaimToken`'s fields are private to the module, so no struct literal anywhere else in the
crate can forge one — only a successful claim mints a token, at exactly one site. The read-only
accessors are the three facts a future owner genuinely needs (`owner()`, `endpoint_index()`,
`waiter()`); the claim generation is **not** among them, is not carried by the public
`WaiterOwnershipView`, and is not reported by `SettleError::StaleClaimGeneration`. Test
introspection and the two forgery helpers are `#[cfg(test)]`.

`wrapping_add` is gone. Generations start at 1 and advance by `checked_add(1)`; at saturation the
claim fails with a typed `ClaimGenerationExhausted`, leaving the slot **and** the counter
untouched, so no ancient token can ever be made valid again and neither zero nor a wrapped value
is issuable. The last usable generation is still handed out before the table closes.

#### D. The census: an independent pass, and an honest narrowing

The §6.1.31 table is relabelled a **waiter-primitive callsite census**. Its collection method
grepped the four waiter primitives, so by construction it could only find paths that touch a
waiter — and the interesting owners are exactly the ones that do not.

The independent pass starts from task status instead. In this kernel `status` is a plain field of
`ThreadControlBlock`, and a mechanical sweep of production source establishes that **no other
writer exists**: no `&mut …status` alias, no whole-TCB overwrite, no `mem::replace`/`swap`, and no
production path that removes a TCB from the array (the single write into it,
`task_policy_state.rs`, targets a slot already proven `is_none()`). So the set of paths that can
move a task out of `Blocked` is exactly the set of status-assignment sites. There are **37**,
across eight files, and `every_production_task_status_assignment_site_is_enumerated` pins the
per-file counts so a new one cannot appear unclassified.

Classified:

| verdict | n | sites |
|---|---|---|
| **CAN act on `Blocked(EndpointReceive)`** | 12 | below |
| **CANNOT**, with an exact source guard | 4 | `futex_wake_inner` and its split twin (`!= Blocked(Futex(addr))` → `continue`); `wake_joiners_for` (`!= Blocked(Join(t))` → `continue`); AP registration (`task_status(tid).is_some()` → `continue`, so the TCB is brand new) |
| transitions *into* `Blocked` | 7 | the three endpoint-block sites, two futex blocks, the send block, the join block |
| fresh-TCB constructor | 1 | `ThreadControlBlock::new` |
| non-production | 1 | `set_task_status_for_test` (`cfg(any(test, hosted-dev))`) |
| **UNPROVEN** | 12 | spawn/dispatch/yield/fault sites that assign a status with **no guard on the previous one** |

The twelve CAN owners, with what they actually check:

| owner | guard | identity | consults waiter? |
|---|---|---|---|
| blocked-recv completion (`ipc_state.rs:414`) | terminal claim upstream | exact + blocked-recv gen | yes |
| **`wake_tid_to_runnable`** | `Blocked(_)` | **TID only** | **no** |
| `process_ipc_timeout_deadlines` | `Blocked(EndpointReceive/Send)` | identity sweep, **after** the wake | **no** |
| notification signal wake | `Blocked(_)` | **TID only** | **no — different table** |
| **`wake_destroyed_notification_waiter`** | `Blocked(_)` | **TID only** | **no** |
| **`sr_wake_receiver_split`** | `Blocked(_)` | **TID only** | **no** |
| `sr_commit_blocked_receiver_split` | `Blocked(EndpointReceive(_))` | exact | yes, compared |
| **`apply_cross_cpu_wake_task`** | `Blocked(_)` | **TID only** | **no** |
| **`exit_task`** | **none** | TID only | no |
| **restart resume** (`restart_state.rs:311`) | **none** | TID only | no |
| **`mark_task_dead`** | **none** | TID only | no |
| **`reap_faulted_task_noalloc_cleanup`** | **none** | TID only | no |

Seven of these (bold) are **new** — the callsite census could not see them, because none of them
touches a waiter primitive. `wake_tid_to_runnable` is the sharpest: it is a *generic* wake helper
that accepts any `Blocked(_)` task by numeric TID, and it is what `wake_waiter_for_endpoint` and
the split wake plan both call. `the_newly_found_unguarded_wake_owners_are_recorded` pins that each
one really is guarded only on `Blocked(_)` (or not at all) and consults no endpoint waiter.

> **Superseded by §6.1.34.** All twelve unproven rows below are now resolved — nine to CAN, three
> to CANNOT — and the verdict is `WAITER_OWNER_CENSUS_COMPLETE=yes`. The reasoning recorded here
> is why they were open, not the current state.

**`WAITER_OWNER_CENSUS_COMPLETE` was `no` at WA2A-R1.** The candidate *enumeration* is mechanical
and pinned. The per-site *negative* was not: the twelve UNPROVEN sites assign a status with no guard on the
previous one, so "it cannot act on `Blocked(EndpointReceive)`" rests on a dynamic invariant — the
task was just registered, or is `current`, or was just dequeued — that the source alone does not
establish. Rather than retain an unsupported exhaustive claim, the completeness flag is set to
`no`. Proving those twelve negatives was prerequisite work for any increment that wants to claim
`WAITER_OWNERSHIP_EXCLUSIVE=yes`; §6.1.34 does that proof.

The three facts already established stay explicit and are separately pinned: the ordinary timeout
is non-exclusive; the notification wake uses a separate waiter table; and
`wake_waiter_for_endpoint`, `ipc_reply` and the shared-region finalization remove by index with no
exact identity comparison.

#### Mutation results

Ten mutations were applied to the repaired primitive and the hosted suite re-run. All ten fail:

| # | mutation | caught by |
|---|---|---|
| M1 | drop the owner comparison in `validate` | 1 test |
| M2 | drop the claim-generation comparison | 1 test |
| M3 | drop the key comparison in `validate` | 1 test |
| M4 | key equality ignores `endpoint_generation` | 2 tests |
| M5 | key equality ignores the waiter ASID | 2 tests |
| M6 | key equality ignores the blocked-wait generation | 4 tests |
| M7 | `wrapping_add` instead of `checked_add` | 2 tests |
| M8 | evict a live claim for a new incarnation | 1 test |
| M9 | `restore` retains the old key instead of `Vacant` | 5 tests |
| M10 | out-of-range endpoint index wraps instead of failing closed | 1 test |

M2 is the one that survived in WA2A (the stale-token test re-claimed under a different owner, so
the owner check masked it); the test added there still catches it here. M9 is the WA2A leak
itself, now caught rather than shipped.

#### Status

* `WAITER_OWNERSHIP_EXCLUSIVE` — **no** (the break is now known to be *wider*, not narrower)
* `WAITER_OWNER_CENSUS_COMPLETE` — **no** (12 unproven negatives)
* waiter-claim-aware timeout arbitration — **not implemented**
* generation-bearing notification arbitration — **not implemented**
* NR6/NR7 waiter claim position — **unchanged** (single late claim)
* x86 direct production default — **OFF** on every architecture
* current ledger — **39 / 7 / 46** (no cell moves)
* canonical 199D — **OPEN**
* RISC-V links/status — unchanged (link 1 ABSENT; 2, 3, 7, 9 present; 4, 5, 6, 8, 10 absent;
  `RISCV_REMOTE_WAKE=D`; `RISCV_199D_READINESS=case_b`; coordinate 23 OPEN)
* **no new live cell**

---

### 6.1.33 WA2A-R2 — an authoritative current incarnation

Lifecycle, view and encapsulation-claim repairs only. Still **zero production callers**: no NR6,
NR7, timeout, notification, legacy-delivery, teardown, shared-region or RISC-V wiring; neither
late direct waiter claim moved; no predicate, scheduler policy, seal, ledger or live-cell change.

#### The defect

WA2A-R1's `claim` accepted any key that differed from a terminal one, on the reasoning that "a
different incarnation is taking over". A key states *which* incarnation, never *when*, so that
reasoning cannot distinguish a newer incarnation from an older delayed one:

```text
claim A → consume A        (slot = Consumed{A})
claim B → consume B        (slot = Consumed{B};  B replaced A because B ≠ A)
delayed claim A            → Ok(token, claim_generation = 3)     ← WRONG
```

Reproduced against `e3e5de91` before the repair: the delayed claim for the already-consumed older
incarnation A returned a fresh token. The primitive rejected stale **tokens** and accepted stale
**claim requests** — which is the more dangerous half, because a token is only as good as the
claim that minted it.

#### A. An explicitly armed current incarnation

`claim` no longer installs a key. The only way a key enters a slot is `arm_current`, which the
eventual authoritative waiter-publication path will call under the same ipc rank-3 acquisition
that installs the receive-waiter:

```text
  Vacant ──arm_current(k)──► Available{k} ──claim(k,owner)──► Claimed{k,owner,gen}
    ▲                            ▲                              │
    │                            └──────── restore(token) ──────┤
    │                                                           │
    └── retire_current(k) ◄── Consumed{k} / Cancelled{k} ◄── consume/cancel(token)
```

| operation | accepted from | refused, with no mutation |
|---|---|---|
| `arm_current(k)` | `Vacant` only | any occupied slot → `SlotOccupied{holding}`; out-of-range |
| `claim(k, owner)` | `Available` **holding exactly `k`** | `Vacant` → `NoCurrentWaiter`; a different incarnation → `NoSuchCurrentIncarnation{holding}`; same key already claimed → `AlreadyClaimed{by}`; terminal → `Consumed`/`Cancelled`; out-of-range; generation exhausted |
| `restore(token)` | `Claimed` | → `Available{k}`, **not** `Vacant` |
| `consume`/`cancel(token)` | `Claimed` | → `Consumed{k}` / `Cancelled{k}` |
| `retire_current(k)` | `Available`/`Consumed`/`Cancelled` **holding exactly `k`** | a live claim → `LiveClaim{by}`; a stale key → `NoSuchCurrentIncarnation{holding}`; `Vacant` → `NotArmed`; out-of-range |

`restore` returns to `Available`, not `Vacant`, because the waiter is still published — unarming
there would let an older delayed request arm the slot, reintroducing the same defect through the
rollback path.

**Bounded and leak-free, with an explicit obligation.** No historical-key store returns: a slot
names exactly one incarnation, and `retire_current` returns it to `Vacant`. What buys that is a
duty the wiring increment inherits and which this section states rather than hides — **a terminal
slot blocks its endpoint index until it is retired.** That is fail-closed (a stale incarnation can
never be claimed) but it is a liveness obligation: whatever clears the authoritative receive-waiter
must also retire the slot, under the same rank-3 acquisition. Three 10 001-cycle tests
(arm/claim/consume/retire, /cancel/, /restore/) assert `occupied_count() == 0` after every cycle.

#### B. Stale incarnations cannot move the slot backward

| test | proves |
|---|---|
| `a_delayed_claim_for_a_consumed_and_retired_incarnation_is_refused` | the exact reported sequence; **fails against `e3e5de91`** |
| `a_delayed_claim_for_a_cancelled_and_retired_incarnation_is_refused` | same via the cancel path |
| `an_older_incarnation_is_refused_in_every_identity_dimension` | old endpoint generation, old ASID, old wait generation |
| `a_stale_arm_or_retire_can_neither_erase_nor_replace_the_current_incarnation` | a late arm and a late retire both bounce off `Available`, `Claimed` and terminal slots |
| `a_live_claim_can_be_neither_armed_over_nor_retired_nor_evicted` | including `retire_current` of the *exact* live key → `LiveClaim{by}` |
| `restore_returns_the_exact_incarnation_to_available_not_to_vacant` | and an older request still cannot take it over afterwards |
| `a_stale_token_is_rejected_even_when_the_same_owner_reclaims` | the claim-generation check, unmasked by owner or key |
| three 10 001-cycle tests | no leak across complete lifecycles |
| `every_endpoint_slot_can_be_armed_and_claimed_simultaneously` | `ENDPOINT_WAITER_SLOTS` live at once, then fully unwound |

#### C. A truthful, fail-closed view

WA2A-R1's view reported `Vacant` for an out-of-range index **and** for a key different from the
one occupying the slot. Both are claims that `claim(key)` could succeed, and both were false. The
view now distinguishes:

| view | means | claim-eligible? |
|---|---|---|
| `EndpointIndexOutOfRange` | no such slot | no |
| `Vacant` | nothing armed | no — `NoCurrentWaiter` until armed |
| `Available` | armed for **this** incarnation, unclaimed | **yes — eligible** |
| `Claimed { owner }` | this incarnation is owned | no |
| `Consumed` / `Cancelled` | this incarnation is terminal | no |
| `ForeignIncarnation { holding }` | a **different** incarnation holds the slot | no |

> **Corrected at WA2B-CENSUS.** This section originally headed the third column "would `claim`
> succeed?" and answered `Available` → **yes**. That is not literally true: with the generation
> counter saturated, an `Available` slot rejects with `ClaimGenerationExhausted`. The exact
> contract is that `Available` means armed-and-unclaimed and is the **only slot state
> structurally eligible** for a claim — not that a claim from it succeeds. §6.1.34 A carries the
> repair.

No variant carries `claim_generation` or any counter state, and
`the_view_carries_no_claim_generation_anywhere` shows the view is identical across two different
live generations.

#### D. The structural claim, corrected (route 2)

§6.1.32 B said a boot-domain `&mut WaiterOwnershipTable` was **inert**. It is not. A boot sibling
cannot call a single method on the table — all of them are module-private — but it could still
replace the whole table by assignment, `mem::replace`/`swap` or a raw pointer write, because the
field and `vacant()` are visible within `crate::kernel::boot`.

Route 1 (making replacement unnameable) requires either moving the ownership module under `defs`
and threading a ~25-field `IpcSubsystem` constructor through the single initializer, or hiding the
field behind a lazily-initialized `Option` that is itself still assignable. Both are broad
construction churn for a property the guards already give, so **route 2 is taken and stated
plainly**: what the primitive has is **rank-3 co-location plus source-guarded encapsulation, not
complete type-system-enforced inertness.**

`no_boot_sibling_can_replace_or_borrow_the_ownership_table` walks the whole production tree and
rejects, outside the ownership module: `.waiter_ownership =`, `waiter_ownership = `,
`mem::replace(&mut`, `mem::swap(&mut`, `ptr::write(&mut`, `&mut ipc.waiter_ownership`,
`&ipc.waiter_ownership`, `&mut self.waiter_ownership` and `&self.waiter_ownership`. It carries a
non-vacuity assertion on the file count, a positive control that exactly two files outside the
module name the field at all (the declaration and the single initializer), and an assertion that
neither the module doc nor this audit re-asserts the retracted "inert" wording.

#### Mutation results

All sixteen fail — the ten from §6.1.32 re-run against the new implementation, plus six for the
lifecycle:

| # | mutation | caught by |
|---|---|---|
| M1 | drop the owner comparison in `validate` | 2 tests |
| M2 | drop the claim-generation comparison | 1 test |
| M3 | drop the key comparison in `validate` | 1 test |
| M4 | key equality ignores `endpoint_generation` | 6 tests |
| M5 | key equality ignores the waiter ASID | 5 tests |
| M6 | key equality ignores the blocked-wait generation | 8 tests |
| M7 | `wrapping_add` instead of `checked_add` | 2 tests |
| M8 | evict a live claim | 4 tests |
| M9 | `restore` makes the incarnation terminal | 6 tests |
| M10 | out-of-range endpoint index wraps | 1 test |
| **M11** | **`claim` replaces a different terminal key** (the R2 defect) | 2 tests |
| **M12** | **`claim` from `Vacant` without `arm_current`** | 3 tests |
| **M13** | **`restore` to `Vacant` instead of `Available`** | 9 tests |
| **M14** | **retire a live claim** | 2 tests |
| **M15** | **report a foreign live incarnation as `Vacant`** | 3 tests |
| **M16** | **stale retire clears a newer incarnation** | 2 tests |

#### Live verification

The slot representation grew a variant, so the layout check was re-run:
`scripts/qemu-x86_64-core-smoke.sh` (`-smp 1`) boots clean, six service entries exactly once, all
`UNLOCK_GRADUATED_*` markers present, and the WA1-GATE seals re-emitted unchanged
(`IPC_DIRECT_PRODUCTION_DISABLED_SEAL production_enabled=0 … result=ok`). A hosted test asserts a
freshly booted kernel has `occupied_count() == 0` — helper-only, at runtime.

#### Status

* helper-only, **zero production callers**
* `WAITER_OWNERSHIP_EXCLUSIVE` — **no**
* `WAITER_OWNER_CENSUS_COMPLETE` — **no**
* x86 direct production default — **OFF** on every architecture
* NR6/NR7 waiter claim position — **unchanged** (single late claim)
* canonical 199D — **OPEN**
* current ledger — **39 / 7 / 46** (no cell moves)
* RISC-V links/status — unchanged (link 1 ABSENT; 2, 3, 7, 9 present; 4, 5, 6, 8, 10 absent;
  `RISCV_REMOTE_WAKE=D`; `RISCV_199D_READINESS=case_b`; coordinate 23 OPEN)
* **no new live cell**

---

### 6.1.34 WA2B-CENSUS — the wake-owner census, resolved

Census resolution and one contract repair. Still **zero production callers**: waiter ownership is
wired into nothing, neither late direct waiter claim moved, and no NR6/NR7, timeout, notification,
legacy-delivery, teardown, shared-region, scheduler-policy, predicate, seal, RISC-V, ServerDies or
199E change.

#### A. The view contract, corrected

§6.1.33 C headed its third column "would `claim` succeed?" and answered `Available` → **yes**.
That is not literally true:

```text
slot = Available { key };  next_claim_generation = u64::MAX
claim(key, owner) → Err(ClaimGenerationExhausted)
```

The implementation is unchanged and correct — the exhaustion path is exactly the fail-closed
behaviour §6.1.32 C asked for. Only the *contract* was overstated. It now reads:

* `Available` means the exact incarnation is **armed and unclaimed**;
* it is the **only** slot state structurally eligible for a claim;
* a claim from it may still fail closed with `ClaimGenerationExhausted`.

`the_view_agrees_with_what_claim_would_do` is replaced by
`available_is_the_only_claim_eligible_view_but_is_not_a_promise_of_success`, which proves all
three parts: every non-`Available` view necessarily rejects; an ordinary `Available` view admits;
and an **exhausted** `Available` view stays `Available`, rejects with `ClaimGenerationExhausted`
and mutates nothing. The view still exposes neither `claim_generation` nor any counter state — it
reports the same `Available` before and after saturation, so exhaustion is not predictable from
it.

#### B. The twelve unproven rows, resolved

Each row was resolved against the standard §6.1.32 D set: a site is **CANNOT** only when a
source-enforced precondition or a behavioural test proves it cannot receive an endpoint-blocked
task. "The task should be current", "it was probably just dequeued" and "spawn normally uses a
fresh TCB" are not proofs, and none of them was accepted.

| # | site | resolution | why |
|---|---|---|---|
| 1 | `build_ap_workload` (AP client spawn) | **CANNOT** | the write is lexically inside `if request_client && self.task_status(client_tid).is_none()`, and `client_tid` is bound locally (`base_tid + 1000`) |
| 2 | `spawn_user_task_from_image` | **CAN** | `spec.tid` is caller-supplied at **24** call sites, and `register_task_with_class` is *idempotent* (`if task_status(tid).is_some() { return Ok(()) }`) — an existing, possibly endpoint-blocked TID passes straight through to the `Runnable` write |
| 3 | `dispatch_next_task` → `Running` | **CAN** | `scheduler.rs` contains **zero** occurrences of `TaskStatus`: the run queue carries bare TIDs with no status precondition, and there is no status read between the dequeue and the write |
| 4 | `yield_current` → outgoing `Runnable` | **CAN** | selects `current_tid()` with no status read; a task that has just executed `recv_block_phase_b_task` is `Blocked(EndpointReceive)` **and** still `current` |
| 5 | `yield_current` → incoming `Running` | **CAN** | as row 3 |
| 6 | `yield_current_to` → outgoing `Runnable` | **CAN** | as row 4 |
| 7 | `yield_current_to` → incoming `Running` | **CAN** | as row 3, via `on_preempt_prefer_on` |
| 8 | `spawn_user_thread` | **CANNOT** | `let tid = self.allocate_thread_id()?` — the signature takes only `parent_tid`, so no caller can supply the TID written |
| 9 | `fork_complete_post_clone` | **CANNOT** | `let child_tid = match self.allocate_thread_id()` — same closure |
| 10 | `d6_genuine_mark_running_via_task_seam` | **CAN** | as row 3; `incoming` comes from the same dequeue |
| 11 | `direct_dispatch_rollback_split` | **CAN** | as row 3; undoes a dispatch with no status read |
| 12 | `fault_current_task_with_fault` | **CAN** | `let faulted_tid = self.block_current_cpu()` — selects by `current`, never by status |

Rows 8 and 9 rest on `allocate_thread_id`, which returns a candidate only where
`self.task_status(candidate).is_none()` and otherwise fails closed with `TaskTableFull`. That is a
source-enforced precondition, and `every_cannot_row_pins_its_guard_and_its_caller_closure` pins
both halves of it.

#### C. Call-graph closure

Every **CANNOT** — the four from §6.1.32 plus the three above — is closed *locally*, so no caller
can bypass it. Two shapes:

| shape | sites | closure |
|---|---|---|
| value-level filter over every TCB | `futex_wake_inner`, `futex_wake_split_mut`, `wake_joiners_for` | the loop visits all TCBs and `continue`s unless the exact `WaitReason` matches; no parameter selects a victim |
| locally bound TID behind a fail-closed check | `build_ap_workload` ×2, `spawn_user_thread`, `fork_complete_post_clone` | the TID is derived inside the function (`base_tid + i`, `base_tid + 1000`, `allocate_thread_id()`), never taken as the parameter that is written |

There is no third shape and no "helper trusted by its callers": had one existed, the site would
have been **CAN**.

Four guards fail on drift: a new or removed status assignment
(`every_status_writer_is_classified_by_its_enclosing_function`, which compares a *mechanical*
extraction of `(file, enclosing fn, count)` against the classified table); a disappeared
precondition or closure fact (`every_cannot_row_pins_its_guard_and_its_caller_closure`); an
unclassified writer (same extraction — an unlisted function makes the sets differ); and a
**newly appearing** guard that would invalidate a CAN row
(`the_can_verdicts_rest_on_absent_guards_that_must_stay_absent` pins that `scheduler.rs` still
knows nothing about `TaskStatus`, that dispatch still reads no status, and that registration is
still idempotent).

#### D. The verdict, computed

Counts derived from the classification table, not written down:

| class | sites (WA2B) | sites (after WA3A) |
|---|---|---|
| CAN | 21 | **13** |
| CANNOT | 7 | **15** |
| INTO_BLOCKED | 7 | 7 |
| FRESH_CONSTRUCTOR | 1 | 1 |
| NON_PRODUCTION | 1 | 1 |
| UNPROVEN | 0 | 0 |

Both columns total **37**, the mechanically enumerated set. §6.1.35 (WA3A) is what moved the nine
Group-3 sites, by enforcing their preconditions in production.

**`WAITER_OWNER_CENSUS_COMPLETE=yes`.** Zero rows remain dependent on an unstated runtime
invariant. No runtime code was changed to reach this: nine of the twelve resolved *against* the
comfortable answer, which is why the CAN set grew from 12 to 21.

**`WAITER_OWNERSHIP_EXCLUSIVE=no` is unchanged.** Completing the census says who the owners are;
it does not make them arbitrate. Not one of the 21 routes through the primitive yet.

#### E. Owner / origin matrix

**Design only — nothing here is implemented in this increment.**

##### E.0 Writer sites and logical origins are two different layers

WA2B-CENSUS's first matrix conflated them: a "Group 1 — eight status-writer sites" heading over a
table of ten *path* rows mixing writers, helpers and callers, with `ipc_reply` listed as though it
were a direct caller of `wake_tid_to_runnable` when the real chain runs through
`apply_scheduler_wake_plan`. The two layers are now separate, and the caller sets are pinned by
`the_helper_writer_direct_caller_sets_are_pinned`.

**Layer 1 — the 21 CAN status-assignment sites** (this is the census; the counts are the
classification table's):

| file | function | sites |
|---|---|---|
| `ipc_state.rs` | `rt_commit_receiver_runnable`, `wake_tid_to_runnable`, `process_ipc_timeout_deadlines`, `signal_notification`, `wake_destroyed_notification_waiter` | 5 |
| `runtime.rs` | `sr_commit_blocked_receiver_split`, `sr_wake_receiver_split`, `d6_genuine_mark_running_via_task_seam`, `direct_dispatch_rollback_split` | 4 |
| `restart_state.rs` | `exit_task`, `restart_task`, `mark_task_dead`, `reap_faulted_task_noalloc_cleanup` | 4 |
| `exec_state.rs` | `spawn_user_task_from_image`, `dispatch_next_task`, `yield_current` ×2, `yield_current_to` ×2 | 6 |
| `scheduler_state.rs` | `apply_cross_cpu_wake_task` | 1 |
| `fault_state.rs` | `fault_current_task_with_fault` | 1 |

**Layer 2 — logical origins**, i.e. every direct production caller of each helper writer:

| helper writer | direct production callers | count |
|---|---|---|
| `wake_tid_to_runnable` | `recv_block_unwind_race`, `wake_waiter_for_endpoint`, `apply_scheduler_wake_plan` | 3 |
| `apply_scheduler_wake_plan` | `apply_split_receiver_wake_plan`, `apply_split_sender_wake_plan`, `ipc_recv_with_optional_deadline` ×2, `ipc_reply` ×2, `ipc_send_with_optional_deadline`, `try_ipc_recv`, `execute_dispatch_post_work`, `execute_blocked_waiter_reply_cap_delivery`, `execute_blocked_waiter_ordinary_cap_delivery` | 11 |
| `apply_split_receiver_wake_plan` | `ctx_finalize_and_wake` ×2, `emit_fault_report_for_fault`, `handle_ipc_send`, `handle_ipc_call` | 5 |
| `wake_waiter_for_endpoint` | `send_message_to_endpoint_and_wake`, `ipc_send_with_optional_deadline` ×2 | 3 |
| `apply_cross_cpu_wake_task` | `apply_cross_cpu_work` (the `WorkItem::WakeTask` drain) | 1 |
| `rt_commit_receiver_runnable` | `complete_reply_timeout_over`, `complete_server_death_over` | 2 |

So `ipc_reply` is an origin **two hops** from the writer (`ipc_reply` →
`apply_scheduler_wake_plan` → `wake_tid_to_runnable`), and `ctx_finalize_and_wake` is **three**
(→ `apply_split_receiver_wake_plan` → `apply_scheduler_wake_plan` → `wake_tid_to_runnable`).

##### E.1 `rt_commit_receiver_runnable` — two terminal origins, not one

The writer has exactly two callers and they carry **different** terminal claimants:

| caller | result written | terminal cell already won as |
|---|---|---|
| `complete_reply_timeout_over` | `SyscallError::TimedOut` | `TerminalClaimant::Timeout` |
| `complete_server_death_over` | `SyscallError::ServerDied` | `TerminalClaimant::PeerDeath` |

Classifying the common writer as `OrdinaryTimeout` would map `ServerDied` onto a timeout, which is
wrong: they are distinct one-shot claimants of the *same* `TerminalCell`, and the peer-death path
additionally emits `IPC_SERVER_DEATH_TERMINAL_CLAIM terminal=PeerDeath`.

Both callers reach the writer having **already won** the reply-terminal cell. The design question
is therefore not "which owner claims the waiter" but **how an already-won terminal claimant is
translated into waiter ownership** — the terminal cell and the waiter slot are two authorities and
one must derive from the other.

> **Prerequisite for the wiring increment.** `WaiterOwner` today has `OrdinaryTimeout` and
> `Teardown` and no way to say "reply-terminal, claimant = Timeout | PeerDeath". Either it gains a
> `ReplyTerminal { claimant: TerminalClaimant }` variant, or the wiring proves that holding the
> terminal cell *is* waiter ownership for this population and the primitive is not consulted at
> all. **The enum is not changed in this documentation-only increment.**

##### E.2 Notification is not an endpoint owner

`signal_notification` and `wake_destroyed_notification_waiter` take a **bare TID** out of
`notification_waiters`, guard only on `matches!(tcb.status, TaskStatus::Blocked(_))`, and never
read `endpoint_waiters`. If that TID has since re-blocked on an endpoint receive, the wake lands on
a *valid, unrelated* endpoint wait.

The earlier matrix said these should claim the endpoint slot and settle it with `cancel`. **That
is wrong and is retracted.** Cancelling would make a stale notification destroy a live endpoint
wait — converting a lost notification into a lost IPC reply, which is strictly worse. A
notification that finds an endpoint-blocked task has nothing to deliver *to that wait*; it must
leave it alone.

Both paths therefore move **out** of the "should claim endpoint ownership" set and into the
production-enforced proven-negative / refusal set. The required future repair:

1. **generation-bearing notification waiter identity** — `notification_waiters` stores
   `Option<ThreadId>`; it must store the exact `{tid, asid}` plus a wait generation, exactly as
   `endpoint_waiters` stores `ReceiverWaiterIdentity`;
2. **a notification-specific blocked reason** (or an equivalent exact wait token), so
   "is this task still waiting on *this* notification" is answerable without guessing from
   `Blocked(_)`;
3. **mismatch or stale record → clear or ignore the notification waiter**, never wake;
4. **never consume, cancel or retire an unrelated endpoint waiter** — the endpoint slot is not
   this owner's to settle.

> Whether `WaiterOwner::Notification` survives is **open**. Once notification waits carry exact
> identity, a notification can no longer collide with an endpoint wait, and the variant may become
> obsolete. It is **kept** for now: removing it before the exact-identity design exists would
> lose the record that this collision was found. Decide at the wiring increment, not here.

##### E.3 `wake_tid_to_runnable` — three origins, three different policies

One `LegacyDelivery` policy cannot cover all three.

**(1) `recv_block_unwind_race` — D2 receive-publication rollback.** The waiter publication
*failed*; this undoes the task block. It is not endpoint delivery, and there is no delivery to
own. Requirement: exact task incarnation + blocked generation, and a **proof that no ownership
slot is armed** for that endpoint incarnation (if one were armed, the publication succeeded and
this is not the rollback path). Settle: nothing to settle — the correct action is
`retire_current` of a slot that was never armed, i.e. a no-op that must *assert* rather than
silently succeed.

**(2) `wake_waiter_for_endpoint` — genuine endpoint delivery.** A valid ownership claimant:
`LegacyDelivery`, exact `WaiterKey`, token required, settle with `consume`. Note it takes the
waiter **by index with no identity compare** today (census rows 8–10), so the key must be
reconstructed from the slot before the claim, not after.

**(3) `apply_scheduler_wake_plan` — a generic `SchedulerWakePlan::Wake(tid)`.** The plan carries
`ThreadId` and nothing else, and its 11 origins span at least five distinct causes:

| origin | cause |
|---|---|
| `apply_split_receiver_wake_plan` (5 origins beneath it) | endpoint delivery / shared region / fault report |
| `apply_split_sender_wake_plan` | blocked **sender** wake |
| `ipc_reply` ×2 | reply delivery |
| `ipc_send_with_optional_deadline` | send-to-blocked-receiver |
| `try_ipc_recv`, `ipc_recv_with_optional_deadline` ×2 | receive-side sender wake |
| `execute_dispatch_post_work`, `execute_blocked_waiter_{reply,ordinary}_cap_delivery` | post-lock capability delivery |

**A bare `Wake(tid)` is insufficient.** `SchedulerWakePlan` must either carry a typed cause plus
the exact identity the cause implies (an endpoint `WaiterKey` for delivery, a `{tid, asid}` +
blocked generation for a sender wake, nothing for a rollback), or `apply_scheduler_wake_plan` must
**refuse in production** to wake a task whose status is `Blocked(EndpointReceive)` without a token.
Refusal is the smaller change and is fail-closed; the typed cause is the better long-term shape.

##### E.4 Cross-CPU wake items must be typed

`WorkItem::WakeTask { tid }` is a bare TID. Only an **endpoint-delivery-origin** remote wake may
carry an endpoint waiter ownership token; every other origin must carry its own identity and must
not be able to settle an endpoint slot. Required future forms (or an equivalent typed payload):

| form | payload it must carry | may carry an endpoint token? |
|---|---|---|
| endpoint delivery | `WaiterKey` + the claim token minted by the origin CPU | **yes** |
| futex / other scheduler wake | `{tid, asid}` + the futex wait address or wait generation | no |
| notification | `{tid, asid}` + notification index + notification wait generation | no |
| rollback / cancellation | `{tid, asid}` + blocked generation + the transaction it rolls back | no |
| lifecycle wake (spawn, restart, resume) | `{tid, asid}` + the lifecycle epoch | no |

Every form must carry enough to **reject stale TID reuse** — at minimum `{tid, asid}`, since a
numeric TID alone cannot distinguish a replacement task. It is explicitly **not** the design that every generic `WakeTask` carries an endpoint token.

> There is currently **no production producer** of `WorkItem::WakeTask`: the only occurrence in
> production source is the drain arm in `apply_cross_cpu_work`, and
> `the_helper_writer_direct_caller_sets_are_pinned` asserts that. So this is prerequisite design
> for the first producer, not remediation of an existing one — and the guard fails the moment a
> producer appears.

##### E.5 Group 3 preconditions must be production-enforced, not `debug_assert`

The earlier matrix suggested `debug_assert`. That is insufficient: `debug_assert` is compiled out
of release kernels, so the proof would not exist where it matters. Every future CANNOT proof must
be **enforced in production and fail closed** — a typed refusal, an error return, or an explicit
fatal, chosen per site.

| site | exact expected transition | fail-closed action on violation |
|---|---|---|
| `dispatch_next_task`, `yield_current` (incoming), `yield_current_to` (incoming), `d6_genuine_mark_running_via_task_seam` | `Runnable → Running` **only** | refuse the dispatch and re-enqueue; a dequeued task in any other state is a scheduler-invariant break |
| `yield_current` (outgoing), `yield_current_to` (outgoing) | `Running → Runnable` **only** | refuse the yield; leave `current` untouched |
| `direct_dispatch_rollback_split` | the exact transaction predecessor: `Running → Runnable`, and only for the `incoming` this transaction dispatched | refuse the rollback and take the existing explicit fatal path |
| `fault_current_task_with_fault` | the target is the **current, running** task on this CPU | refuse to fault a task that is not current-and-running |
| `spawn_user_task_from_image` | the destination TID is **absent** (no TCB) or freshly registered by this call | refuse with an error; never write into an existing TCB |

**`spawn_user_task_from_image` is the sharpest of these.** Two things that look sufficient are not:

* **`register_task_with_class` idempotence is not a precondition.** It *returns `Ok(())`* for an
  existing TID, so it silently licenses the write instead of refusing it. The check must be at the
  spawn site and must fail, not succeed.
* **Checking "not `Blocked(EndpointReceive)`" is not enough either.** The write does not merely
  wake a blocked receiver — it overwrites the entry point, stack, ASID and register context of
  **whatever task already holds that TID**. A `Runnable` or `Running` victim is corrupted just as
  thoroughly, it simply is not an ownership problem. The precondition is *absence*, not
  *not-endpoint-blocked*.

Enforcing all five would move these nine sites CAN → CANNOT and shrink the eventual arbitration
set from 21 to 12. That is a separate increment; recording the exact transitions here is the
point of the matrix.

##### E.6 The resulting owner sets

| set | sites | intended `WaiterOwner` | disposition |
|---|---|---|---|
| endpoint delivery, direct IPC | `sr_commit_blocked_receiver_split` | `DirectRequest` (NR6) / `DirectReply` (NR7) | claim → consume on delivery, restore on rollback |
| endpoint delivery, legacy + shared region | `wake_tid_to_runnable` (via `wake_waiter_for_endpoint`), `sr_wake_receiver_split`, `apply_cross_cpu_wake_task` (endpoint-origin form only) | `LegacyDelivery` | claim → consume; key rebuilt from the slot **before** the claim, since these take by index today |
| ordinary deadline scan | `process_ipc_timeout_deadlines` | `OrdinaryTimeout` | claim → consume on fire, restore on loss; requires the ipc-first reorder |
| already-terminal, translation undecided | `rt_commit_receiver_runnable` (Timeout **and** PeerDeath) | **none yet** — see E.1; not `OrdinaryTimeout` | E.1 prerequisite |
| teardown | `exit_task`, `restart_task`, `mark_task_dead`, `reap_faulted_task_noalloc_cleanup` | `Teardown` | claim → cancel; retire paired with `clear_ipc_waiters_for_tid` |
| rollback, not delivery | `wake_tid_to_runnable` (via `recv_block_unwind_race`), `direct_dispatch_rollback_split` | **none** | assert no slot armed; never claim |
| **must refuse, never claim** | `signal_notification`, `wake_destroyed_notification_waiter` | `Notification` — **retracted**, see E.2 | exact notification identity, then refuse |
| **production-enforced precondition, not a claim** | the nine group-3 scheduler/lifecycle sites | **none** | E.5 transitions |

#### Status

* helper-only, **zero production callers**
* `WAITER_OWNER_CENSUS_COMPLETE` — **yes** (0 UNPROVEN of 37)
* `WAITER_OWNERSHIP_EXCLUSIVE` — **no** (21 CAN paths, none arbitrating)
* x86 direct production default — **OFF** on every architecture
* NR6/NR7 waiter claim position — **unchanged** (single late claim)
* canonical 199D — **OPEN**
* current ledger — **39 / 7 / 46** (no cell moves)
* RISC-V links/status — unchanged (link 1 ABSENT; 2, 3, 7, 9 present; 4, 5, 6, 8, 10 absent;
  `RISCV_REMOTE_WAKE=D`; `RISCV_199D_READINESS=case_b`; coordinate 23 OPEN)
* **no new live cell**

---

### 6.1.35 WA3A — production-enforced scheduler/lifecycle transition barriers

The first WA3 increment that changes **production executable code**. Scope is exactly the nine
Group-3 CAN sites from §6.1.34 E.5. Waiter ownership is still wired into nothing, NR6/NR7 claim
position is unchanged, and no predicate, timeout, notification, teardown, RISC-V link, seal or
ledger policy moved.

#### A. The barrier

`src/kernel/task_transition.rs` is a new production module: a typed, **release-build**,
fail-closed transition primitive. `debug_assert` was explicitly rejected — it compiles out of
release kernels, which is exactly where the proof has to hold.

| transition | from → to | used by |
|---|---|---|
| `DispatchIncoming` | `Runnable → Running` | `dispatch_next_task`, both yields' incoming, the D6 seam |
| `ContinueCurrent` | `Running → Running` | the queue-neutral continuation (see below) |
| `PreemptOutgoing` | `Running → Runnable` | both yields' outgoing |
| `PreemptOutgoingIdle` | `Runnable → Runnable`, **`IDLE_TID` only** | idle, see D |
| `RollbackDispatchedIncoming` | `Running → Runnable` | `direct_dispatch_rollback_split` |
| `FaultRunningCurrent` | `Running → Faulted` | `fault_current_task_with_fault` |

There is no "set status" escape hatch. Refusals are typed (`TaskMissing`,
`IncarnationMismatch{observed}`, `WrongStatus{observed}`, `NotIdleTask`) and **write no field of
the TCB**, so a refusal is observationally identical to never having called. Where the caller
knows which incarnation it means it passes `expect_asid`, and a recycled numeric TID under a
different address space is refused before the status is even read.

`the_barrier_is_the_only_writer_for_the_group3_cohort` pins that the module contains exactly one
`tcb.status =` write, that it writes only `transition.resulting()`, that no `debug_assert!` or
`cfg(debug…)` gates it, and that none of the eight barriered functions writes `tcb.status`
directly any more.

**One honest widening.** `PriorityScheduler::dispatch_next` returns the existing `current`
*without* dequeuing whenever one is set and is not idle, so a dispatch site legitimately sees
either `Runnable → Running` (a real dequeue) or `Running → Running` (a continuation), and cannot
tell them apart from outside. Both are accepted; every other status — `Blocked(_)` above all, and
also `Faulted`, `Exited`, `Dead` — is refused. `ContinueCurrent` is a separate typed variant
precisely so the idempotent case cannot launder a non-`Running` task into `Running`.

#### B. No partial scheduler/TCB commit

Each path is resolved by one of the three permitted shapes, never by "a status check after an
irreversible scheduler mutation":

| path | shape | detail |
|---|---|---|
| `yield_current` / `yield_current_to` **outgoing** | **(1) validate before either mutation** | the outgoing transition is the first mutation in the function; a refusal returns before `on_preempt_current_cpu`, so `current` is untouched and nothing is enqueued |
| `fault_current_task_with_fault` | **(1) validate before either mutation** | the `Running`-and-current precondition is evaluated at rank 2 *before* `block_current_cpu` (rank 1), which is not undone below. A second check confirms the scheduler blocked out exactly the validated victim |
| `dispatch_next_task`, both yields' **incoming** | **(3) exact rollback** | the rank-1 dequeue already ran, so a refusal calls `preempt_reenqueue_current_cpu` — the existing inverse of `dispatch_next_on` — and only when a real dequeue happened (`outgoing != incoming`) |
| `d6_genuine_mark_running_via_task_seam` | **(3) exact rollback** | the seam now takes `cpu`, returns `bool`, and performs `preempt_reenqueue_only_on` itself on refusal, so the invariant is restored regardless of caller |
| `direct_dispatch_rollback_split` | **(2) typed transaction** | the task half must succeed as `RollbackDispatchedIncoming` before the scheduler half runs; if it is refused, the scheduler half is **skipped** — re-enqueuing a task this transaction does not own could displace a live `current` |
| `spawn_user_task_from_image` | **(1) validate before either mutation** | see C |

On refusal, in every case: no task is lost from a run queue (the dequeue is undone, or never
happened); no blocked task becomes current; no second task becomes current on a CPU; outgoing and
incoming statuses are unchanged; and CPU affinity / current ownership are coherent.
`a_blocked_receiver_cannot_be_dispatched_running` and
`the_d6_seam_refuses_a_blocked_incoming_and_rolls_the_dequeue_back` assert the run-queue length
and the `current` slot explicitly.

**No new scheduler primitive was needed** — `preempt_reenqueue_only_on` / `preempt_reenqueue_
current_cpu` already existed as the exact inverse of the dequeue — and **no new broad-lock
acquisition and no task(2) → scheduler(1) inversion** was introduced: every barrier call runs
inside a rank-2 acquisition the site already held, and every rollback runs in a separate rank-1
acquisition.

All eleven `d6_genuine_mark_running_via_task_seam` call sites (x86_64, AArch64 and RISC-V trap
drains) were updated to consume the `bool` and skip the resume on refusal.

#### C. Spawn must be fresh — **HARD-STOP**

The absence gate was implemented, and then **reverted**, because it is not satisfiable by the
current boot sequence. `src/arch/x86_64/boot.rs:633-638` (and its AArch64/RISC-V twins) calls

```rust
kernel.register_task_with_class(RING3_SUPERVISOR_TID, TaskClass::SystemServer)?;
kernel.register_task_with_class(RING3_PM_SERVER_TID,   TaskClass::SystemServer)?;
kernel.register_task_with_class(RING3_INIT_SERVER_TID, TaskClass::SystemServer)?;
```

**before** the corresponding `spawn_user_task_from_image` calls, so the boot capability grants
have a cnode and a kernel stack to target. With the gate in place, an ordinary `-smp 1` boot
refuses its own supervisor:

```text
SPAWN_REFUSED_TID_PRESENT tid=2 reason=destination_not_absent
failed to bootstrap first user task: TaskTableFull
```

That is live evidence from `scripts/qemu-x86_64-core-smoke.sh`, not a hosted inference.

> **Method note.** The first core-smoke run of this increment reported PASS against a **stale
> prebuilt** `build-x86_64/kernel_boot.elf` — the smoke script boots artifacts and does not
> rebuild. Rebuilding via `scripts/build-qemu-x86_64-artifacts.sh` and re-running is what exposed
> the refusal. Every live result below is from a genuinely rebuilt kernel.

**No weaker predicate was substituted.** "Not `Blocked(EndpointReceive)`" would still permit
overwriting a live `Runnable` or `Running` task's register context, entry point, stack, ASID,
scheduler membership, startup capabilities and process ownership — the precondition is *absence*
or nothing. Making it absence requires restructuring the register → grant → spawn sequence across
three architectures, which is outside this increment's stated scope and is not live-verifiable
for AArch64 here (no `qemu-system-aarch64`).

`spawn_user_task_from_image` therefore stays **CAN**, and the finding is pinned rather than
narrated:

* `spawn_still_overwrites_a_present_tid_pending_the_boot_sequence_repair` asserts the current
  behaviour — a present, endpoint-blocked TID is accepted and overwritten — so a future fix is
  detected rather than assumed;
* `the_barrier_is_the_only_writer_for_the_group3_cohort` pins that the gate is genuinely reverted
  (not silently retained), that no weaker predicate replaced it, and that x86 boot still
  registers before spawning — so the hard-stop is **falsifiable**: change the boot sequence and
  the guard fails, and the site can be re-derived.

The remaining eight Group-3 sites are enforced as described in A and B. **CAN 21 → 13**, not 12.

#### D. Idle and special paths

The hosted suite found a real invariant break while this was being built: **the idle task
(`TID 0`) is `current` while `Runnable`.** The rank-1 scheduler makes it current without any
mark-running step, so preempting it out is `Runnable → Runnable`, not `Running → Runnable`.

Rather than weaken `PreemptOutgoing`, that case gets its **own** transition,
`PreemptOutgoingIdle`, which the primitive refuses for any TID other than `IDLE_TID` with a typed
`NotIdleTask`. `the_idle_branch_does_not_admit_an_ordinary_runnable_current_task` drives an
ordinary task into the same `current`-and-`Runnable` state through a raw rank-1 preempt and
asserts the yield is still refused. `IDLE_TID` is documented as the same TID
`PriorityScheduler::dispatch_next` special-cases.

#### E. The recomputed census

Eight sites no longer write `tcb.status` at all; the ninth (spawn) keeps its unguarded write, per
the C hard-stop. The census is recomputed, not edited: it covers **29 remaining raw writes + 8
barriered sites = 37**, and `every_status_writer_is_classified_by_its_enclosing_function` derives
that sum from the fingerprint table and `WA3A_BARRIER_SITES` together.

| class | sites |
|---|---|
| CAN | 13 |
| CANNOT | 15 |
| INTO_BLOCKED | 7 |
| FRESH_CONSTRUCTOR | 1 |
| NON_PRODUCTION | 1 |
| UNPROVEN | 0 |

`WAITER_OWNER_CENSUS_COMPLETE=yes` and `WAITER_OWNERSHIP_EXCLUSIVE=no` both stand. The remaining
**13** CAN paths are the eight endpoint-delivery owners, four teardown paths of §6.1.34 E.6, and
`spawn_user_task_from_image` pending the C repair. The rest of Group 3 is out of the arbitration
set.

#### F. Mutation results

Nine mutations survive the C hard-stop (M4, the spawn gate, went with it). Each removes one
production check, and all nine fail a **named** behavioural test:

| # | mutation | caught by |
|---|---|---|
| M1 | barrier drops the from-status check | 10 tests |
| M2 | barrier drops the incarnation check | `a_stale_incarnation_cannot_move_a_replacement_task` |
| M3 | idle branch admits an ordinary task | `the_idle_branch_does_not_admit_an_ordinary_runnable_current_task` |
| M5 | dispatch barrier → unconditional write | `a_blocked_receiver_cannot_be_dispatched_running` |
| M6 | yield-outgoing barrier → unconditional write | `a_blocked_receiver_cannot_be_yielded_to_runnable` |
| M7 | fault pre-validation removed | `the_fault_path_refuses_a_non_running_victim` |
| M8 | D6 seam drops the exact scheduler rollback | `the_d6_seam_refuses_a_blocked_incoming_and_rolls_the_dequeue_back` |
| M9 | rollback barrier → unconditional write | `a_stale_rollback_plan_cannot_alter_a_replacement_incarnation` |
| M10 | dispatch refusal skips the scheduler rollback | `a_blocked_receiver_cannot_be_dispatched_running` |

#### Status

* production executable code — **changed** (this is not a documentation increment)
* eight of nine Group-3 sites — **production-refused**; `spawn_user_task_from_image`
  **hard-stopped** (C), pinned, and left CAN
* CAN — **21 → 13**
* new broad-lock acquisitions — **none**; no task(2) → scheduler(1) inversion
* helper waiter ownership — still **zero production callers**
* `WAITER_OWNER_CENSUS_COMPLETE` — **yes**; `WAITER_OWNERSHIP_EXCLUSIVE` — **no**
* x86 direct production default — **OFF** on every architecture
* NR6/NR7 waiter claim position — **unchanged**
* canonical 199D — **OPEN**; ledger — **39 / 7 / 46**; RISC-V — unchanged
* **no new live cell**

---

### 6.1.36 WA3A-R2-SEAL — the final combined production repair for WA3A

WA3A-R1 typed the dispatch provenance and the rollback identity, and in doing so introduced a
torn state of its own. This increment repairs that and five related gaps. It changes production
executable code on all three architectures. Waiter ownership is still wired into nothing, the
spawn hard-stop of §6.1.35 C is untouched, and no predicate, timeout, notification, teardown,
seal, ledger entry or canonical stage status moved.

#### A. Identity availability is part of the same rank-2 decision

**The defect this repairs, in full.** In WA3A-R1 the mark seam did this:

1. `ContinuedCurrent { tid: T }` for a non-idle `Running` current — a legal `Running → Running`
   transition, which **succeeded**;
2. `DispatchMarkToken::new` then refused, because a user task with no ASID has no exact
   incarnation to name;
3. the common failure branch performed `RollbackDispatchedIncoming`, moving `T` back
   `Running → Runnable`;
4. `undo_dispatch_selection(ContinuedCurrent)` correctly mutated **no** scheduler state — a
   continuation removed no runqueue entry, so there is nothing to undo.

The result was `current = T` while `status(T) = Runnable`: the CPU believes `T` is running, the
task table says it is merely runnable, and nothing in the system can tell which is right. The
"rollback" was the corruption.

The repair is structural rather than a reordered branch: identity resolution moved **inside the
same rank-2 acquisition** as the transition, and **strictly before** it
(`MarkedIncarnation::resolve`, then `apply_dispatch_transition`). A non-idle task with no ASID
now refuses with the TCB untouched, so `undo_dispatch_selection` has only the scheduler step to
undo — and for a continuation that step is, correctly, nothing.

| selection | refusal outcome | task status | `current` | run queue |
|---|---|---|---|---|
| `ContinuedCurrent { T }` | `RefusedNoSchedulerChange` | unchanged (`Running`) | unchanged (`T`) | unchanged |
| `Dequeued { T }` | `RefusedRolledBack` | unchanged (`Runnable`) | cleared | `T` restored **once** |
| `Idle` | `Idle` | — | — | — |

Pinned by `an_asid_less_continued_current_is_refused_with_zero_mutation` and
`an_asid_less_dequeued_incoming_is_refused_and_restored_exactly_once`.

#### B. Typed provenance through the in-lock Group-3 cohort

WA3A-R1 typed the five **off-lock** seams and left the three **in-lock** ones reconstructing
`let dequeued = outgoing_tid != Some(tid);`. That reconstruction is not merely inelegant — it is
wrong, and the counterexample is ordinary:

> A lone task yields. `on_preempt` re-enqueues it and then genuinely dequeues it again. The
> outgoing task and the incoming task are the same task, so every reconstruction says "not
> dequeued" — and a refusal would then skip the re-enqueue and **lose the only runnable task**.

`KernelState` gained the provenance-preserving forms `local_dispatch_step_split_selection`,
`on_preempt_current_cpu_selection` and `on_preempt_prefer_current_cpu_selection` (the legacy
`Option<u64>` functions are now thin wrappers, so there is still exactly one queue-manipulating
body each). All three in-lock Group-3 sites now commit through **one** shared barrier,
`commit_dispatch_selection_in_lock`, and undo through `undo_dispatch_selection_in_lock`.

That also removed a second, quieter reconstruction: the old sites tried `DispatchIncoming` and
fell back to `ContinueCurrent`, which *infers* the transition from which one happens to succeed —
and would launder a double-queued `Running` task through the dequeue path. The transition is now
chosen by provenance alone.

**One honest consequence.** Making the transition exact exposed that the idle/bootstrap task's
status is not governed by the ordinary dispatch contract at all: boot leaves `TID 0` `Running`
and re-dequeues it, while a later queue-neutral step finds it `Runnable`. Rather than weaken
`DispatchIncoming` or `ContinueCurrent`, each got an idle-only twin —
`RedispatchIdleAlreadyRunning` (`Running → Running`) and `ContinueCurrentIdle`
(`Runnable → Runnable`) — refused for every TID but `IDLE_TID`, joining `PreemptOutgoingIdle`
from §6.1.35 D. `apply_dispatch_transition` carries the fallback in one place, so the in-lock and
off-lock commits cannot drift on which statuses idle may hold.

`no_group3_caller_reconstructs_a_dequeued_bool` pins that no production Group-3 caller contains
`let dequeued =`, `outgoing_tid != Some(tid)` or `incoming.is_some()`. It caught two survivals in
this increment's own work, and both were repaired rather than exempted:

* the `scheduler_context_switches` counter compared outgoing to incoming. That is a genuinely
  different question — "did the resumed task change?", not "was the queue advanced?" — so it
  moved into the named `note_context_switch_if_task_changed` helper and is pinned to exactly
  that one site, where the comment says why it is not provenance;
* the `result=` field of `D6_GENUINE_MUT_DISPATCH_STEP_SPLIT` and both
  `D2_*_GENUINE_DISPATCH_STEP_SPLIT` markers was formatted from `Option::is_some`. The emitted
  text is unchanged (the smoke gates pin `result=switch`), but the field is now read off the
  typed `DispatchSelection`, so there is no second source of truth about what the dispatch did.

#### C. Only dequeue authority may undo a dequeue

`direct_dispatch_rollback_split` used to accept any `DispatchMarkToken`. A `ContinuedCurrent`
mark removed no runqueue entry, so using it to "undo a dequeue" would enqueue the **current**
task — which for a `Blocked(EndpointReceive)` current is exactly the unarbitrated wake Stage 199D
exists to prevent.

It now takes a `DequeuedDispatchMarkToken`, a sealed newtype whose only constructor is
`DispatchMarkToken::into_dequeued_authority` — `Some` only when the provenance is a genuine
dequeue **of that very TID**. Presenting a continuation is not a refused call at the mutation
site; it is **unrepresentable**.
`a_continued_current_mark_is_not_dequeue_rollback_authority` mints a real successful
`ContinuedCurrent` mark, shows the narrowing yields `None`, and asserts status, `current`,
register context and queue count are byte-for-byte unchanged — plus that the narrowing is the
only constructor.

#### D. The token's CPU is scheduler-authenticated

The off-lock seams mutated `sched.current_cpu` while the token recorded the trap `cpu` the caller
supplied. Nothing checked they agreed, so a caller could stamp an unverified CPU into rollback
authority — and a rollback would then re-enqueue on, and clear `current` of, the wrong core.

The seams now authenticate the requested CPU against the authoritative dispatch CPU **before any
mutation** and return a `CpuDispatch`, which binds the selection to the CPU that produced it. On
mismatch nothing is dequeued and a `RefusedCpuMismatch` is returned (`DISPATCH_STEP_REFUSED_CPU_MISMATCH`).
`d6_genuine_mark_running_via_task_seam` consequently takes **no `cpu` argument at all**: there is
nothing left for a caller to get wrong. `a_mismatched_cpu_refuses_before_any_mutation_and_mints_no_token`
proves zero mutation and no token; `the_mark_seam_cannot_be_handed_an_unverified_cpu` pins that
the guard precedes the dequeue in all five seams.

#### E. `RefusedTorn` is unignorable

`DispatchMarkOutcome::may_resume()` collapsed `RefusedRolledBack`, `RefusedNoSchedulerChange` and
`RefusedTorn` into one `false`. It is **removed**. Every one of the eleven production consumers
on x86_64, AArch64 and RISC-V now matches all five outcomes explicitly, each with its own
evidence marker, and `RefusedTorn` routes to `dispatch_torn_fatal(cpu, tid, site) -> !`.

A torn dispatch means the rank-1 scheduler and the rank-2 task table disagree about who is
running. Resuming a frame, running ordinary fallback dispatch, entering WFI/HLT as though
`current` were clear, or returning to userspace would each run an arbitrary frame under an
arbitrary address space, so the fatal is the only correct disposition.

The pure mapping is `DispatchMarkOutcome::disposition()`, which is total and **injective on the
refusals** — `ResumeIncoming`, `SettleIdle`, `DeclineDequeueUndone`, `DeclineSchedulerUntouched`,
`Fatal` — so there is no value two different refusals share.
`every_mark_outcome_has_its_own_disposition` checks the mapping and the injectivity;
`every_architecture_caller_matches_all_five_outcomes_and_torn_is_fatal` counts the consumers per
arch file and asserts each of the five arms, and `dispatch_torn_fatal`, appears exactly once per
consumer.

#### F. The post-mark resume uses the token's exact identity

After `Marked(token)` the AArch64 direct-dispatch resume re-resolved the task by numeric TID for
ASID activation, saved-context/TLS restore and the pending-completion take. A replacement
incarnation that reused the TID would therefore have **its** address space activated and **its**
context copied into the outgoing frame.

`direct_dispatch_resume_incoming` now takes the `DispatchMarkToken`, and each of
`direct_dispatch_activate_asid_split`, `direct_dispatch_restore_context_split` and
`direct_dispatch_take_completion_split` resolves on the exact `{tid, asid}` pair the mark
recorded. A mismatch refuses (`AARCH64_DIRECT_DISPATCH_IDENTITY_REFUSED`) and the caller takes its
rollback/fatal path. `the_post_mark_resume_refuses_a_replacement_incarnation` runs the required
counterexample: mark A `{tid = T, asid = A}`, replace the TCB with B `{tid = T, asid = B}`,
then show nothing is activated, B's context is never handed back, and B is byte-for-byte
unchanged.

#### Census

Unchanged in every class. Three rows moved file, because the in-lock dispatch mark is now one
shared commit rather than three copies: `exec_state.rs` drops from 10 to 7 sites and
`scheduler_state.rs` rises from 1 to 4, with `commit_dispatch_selection_in_lock` carrying 3
`Cannot` sites. **29 remaining raw writes + 8 barriered sites = 37**, and
**CAN 13 / CANNOT 15 / INTO_BLOCKED 7 / FRESH_CONSTRUCTOR 1 / NON_PRODUCTION 1 / UNPROVEN 0**.

#### Status

* production executable code — **changed**, on all three architectures
* the WA3A-R1 torn `current = T` / `status(T) = Runnable` state — **eliminated**, mutation-free
* provenance reconstruction in the Group-3 cohort — **none remaining**
* dequeue rollback authority — **sealed**; a continuation cannot express it
* token CPU — **scheduler-authenticated**; the mark seam takes no `cpu`
* `RefusedTorn` — **fatal at every one of the eleven callers**
* post-mark resume — **exact-incarnation** on every step
* census — **13 / 15 / 7 / 1 / 1 / 0**, unchanged
* new broad-lock acquisitions — **none**; no task(2) → scheduler(1) inversion
* helper waiter ownership — still **zero production callers**
* `WAITER_OWNER_CENSUS_COMPLETE` — **yes**; `WAITER_OWNERSHIP_EXCLUSIVE` — **no**
* direct production default — **OFF**; NR6/NR7 claim position — **unchanged**
* canonical 199D — **OPEN**; ledger — **39 / 7 / 46**; **no new live cell**

WA3A is sealed. The next increment is the one-shot `ReservedUnstarted → LiveSpawned` TCB
protocol, which closes the final Group-3 CAN site and moves CAN 13 → 12.

---

### 6.1.37 WA3B — the one-shot spawn reservation/consumption protocol

Closes the last Group-3 `CAN` site, `spawn_user_task_from_image`, and with it the §6.1.35 C
hard-stop. Production executable code changes on all three architectures. Waiter ownership is
still wired into nothing.

#### A. What was actually wrong

Spawn's only authorization was that `register_task_with_class` is **idempotent**: handed a TID
that already had a live TCB it returned `Ok(())`, and the spawn then overwrote that task's
register context, entry point, stack, ASID, thread group and scheduler membership. A
`Blocked(EndpointReceive)` receiver was as overwritable as an idle one.

WA3A's obvious repair — "the destination TID must be absent" — was implemented and reverted,
because it is genuinely incompatible with bootstrap: all three architectures register the
supervisor / PM / init TIDs *before* spawning onto them, so the boot capability grants have a
destination CNode. The gate refused the kernel's own supervisor on an ordinary boot
(`SPAWN_REFUSED_TID_PRESENT tid=2`).

The resolution is not a better predicate. It is that "provisioned" and "live" were the same
thing, and they should not be.

#### B. The reservation lifecycle

```text
  ReservedUnstarted --claim--> Spawning --commit--> LiveSpawned (Runnable)
                         ^                 |
                         +-----restore-----+   (spawn failed; same incarnation)
                         |
                    cancel (setup failed before spawn) --> gone
```

`TaskStatus::Reserved` is a genuinely non-live status, and choosing a **status variant** rather
than a side field is what makes the invariants hold rather than be asserted: every existing site
that allow-lists statuses now refuses reservations automatically. `wake_tid_to_runnable` already
gated on `Blocked|Runnable|Running`, so ordinary wake refuses a reservation with no edit at all;
the cross-CPU wake path gained an explicit `SkippedReserved`; the reap path refuses it as
non-terminal. Only two exhaustive `TaskStatus` matches existed in the kernel, which is why the
blast radius was small enough for this to be the right representation.

| invariant | how it is enforced |
|---|---|
| not `Runnable`/`Running`/`Blocked` | a distinct status variant |
| cannot be enqueued | `refuse_enqueue_of_spawn_reservation` on both enqueue seams — the run queue carries bare TIDs, so the enqueue seam is the only possible choke point |
| cannot be dispatched | it can never be enqueued, and `DispatchIncoming` requires `Runnable` |
| cannot be woken | `Reserved` is outside every wake path's allow-list |
| cannot block / publish a waiter | blocking is reachable only for the *current* task, and it can never become current |
| owns pre-spawn resources | the reservation reuses the SAME capacity / CNode-slot / class / kernel-context provisioning as ordinary registration |

**Process identity.** `task_cnode` resolves a task's CNode through `thread_group_id`, so
`ThreadControlBlock::reserved` sets it to the OWNING process from the moment the TCB exists.
Storing `process_pid` in the reservation record while leaving `thread_group_id` defaulted to the
TID would have left the pre-spawn grants resolving to the wrong CNode —
`a_reservation_resolves_its_process_cnode_before_spawn` pins both halves.

#### C. Exact identity

`SpawnReservationToken` has private fields and no public constructor; the only way to hold one is
to have created the reservation. It carries `{tid, generation, class, process_pid}`, and the
generation comes from a monotonic kernel counter that is **never derived from the TID** — so a
token minted for an earlier occupant of numeric TID `T` cannot authorize a later occupant.
`claim_for_spawn` validates all four plus the phase atomically under one rank-2 acquisition,
before any spawn-specific mutation.

#### D. Failed spawn restores the exact baseline

"All fallible work completes before any TCB mutation" is **not** achievable here, and the audit
should say why rather than assert the convenient version: spawn binds the incoming ASID before
the fallible stack allocation and user-memory copy, because the x86_64 kernel switch-frame retry
and the stack allocator both need the target address space bound.

So WA3B takes the other permitted proof. `claim_for_spawn` captures a `SpawnBaseline` — status,
thread group, ASID, user entry, user stack top, register context, TLS pointer, CPU affinity — and
a failed spawn replays every one of them. After an ordinary returned error the reservation is
observationally identical to the pre-claim reservation: no stale ASID, no partial live identity,
nothing enqueued, and the caller's token is exactly as valid as it was.
`the_baseline_replay_restores_every_field_the_spawn_body_may_write` dirties each field and
asserts the whole TCB comes back byte-for-byte.

VM and capability cleanup on a failed spawn is **unchanged** and remains a pre-existing gap; this
stage does not make it worse and deliberately does not open it.

#### E. Cancellation

`cancel_spawn_reservation(token)` covers setup failing *before* spawn is invoked. It validates
read-only first, so a stale token, a token naming a replacement occupant, a `Spawning`
reservation and a live task are all refused with zero mutation. On success it releases the kernel
context and removes the TCB through the EXISTING cleanup primitives (`release_kernel_context`,
then the no-alloc process-CNode reap, which itself only proceeds when no other thread owns the
process). After cancellation the token names nothing.

#### F. The converted caller closure

Every production caller reserves, or consumes a reservation made earlier:

| file | callers | shape |
|---|---|---|
| `src/arch/x86_64/boot.rs` | 3 | reserve → grant → consume |
| `src/arch/aarch64/boot.rs` | 3 | reserve → grant → consume |
| `src/arch/riscv64/boot.rs` | 3 | reserve → grant → consume |
| `src/kernel/syscall/process.rs` | 4 | reserve immediately before spawn |

**13 production callers, not the 18 previously counted** — the earlier figure included six
`#[cfg(test)]` call sites. `the_production_spawn_caller_closure_is_pinned` recomputes the set
mechanically and fails if it changes; test callers go through an explicitly test-only
`reserve_and_spawn_user_task_from_image_for_test` convenience, which still goes through the real
reservation path.

#### G. Census — 38, not 37

The two movements this stage predicted hold exactly:

| class | before | after |
|---|---|---|
| CAN | 13 | **12** |
| CANNOT | 15 | **16** |
| INTO_BLOCKED | 7 | 7 |
| FRESH_CONSTRUCTOR | 1 | **2** |
| NON_PRODUCTION | 1 | 1 |
| UNPROVEN | 0 | 0 |
| **total** | 37 | **38** |

The total moved because `ThreadControlBlock::reserved` is a genuinely new status writer that did
not exist before. It is classified `FRESH_CONSTRUCTOR` alongside `new`: it fills a slot that was
`None`, so it can wake nothing — and unlike `new` it starts *not* runnable. Forcing the total
back to 37 would have meant hiding either it or spawn's departure from the raw-write set, which
is exactly what the census exists to prevent. `spawn_user_task_from_image` now writes no status
at all; it is tracked as a reservation-barriered site, the same way WA3A's eight are tracked as
transition-barriered.

Composition: **29 raw writes + 8 transition-barriered + 1 reservation-barriered = 38.**

`WAITER_OWNER_CENSUS_COMPLETE=yes` and `WAITER_OWNERSHIP_EXCLUSIVE=no` both stand.

#### Status

* production executable code — **changed**, on all three architectures
* bootstrap — **reserve → grant → consume** on x86_64, AArch64 and RISC-V
* spawn overwriting an arbitrary existing TCB — **impossible**; registration idempotence is no
  longer authorization anywhere
* stale tokens — cannot consume, cancel, or affect a reused TID
* reserved tasks — non-live, non-enqueueable, non-dispatchable, non-wakeable
* CAN **13 → 12**; CANNOT **15 → 16**; total **37 → 38** (disclosed above)
* new broad-lock acquisitions — **none**; no task(2) → scheduler(1) inversion
* helper waiter ownership — still **zero production callers**
* direct production default — **OFF**; canonical 199D — **OPEN**; ledger — **39 / 7 / 46**
* **no new live cell**

Spawn work stops here. The next stage is production endpoint-waiter ARM/RETIRE wiring, which
must create the first real production callers of the WA2A ownership primitive.

---

### 6.1.38 WA3C1 — generation-bearing waiter record, central removal, and the `destroy_endpoint` repair

WA3C set out to wire the WA2A ownership primitive into the live waiter lifecycle. It was **split**,
because the strict single-waiter publication that exact ownership requires turned out to conflict
with a semantic the kernel actively depends on. WA3C1 is the half that is independently correct
and independently useful; WA3C2 owns the semantic question.

#### A. Why the split — the proven blocker

Exact ownership needs strict publication: `arm_current` refuses an occupied ownership slot, so a
silent "last receiver wins" replacement would strand the previous blocked receiver with neither a
waiter nor an owner. Making publication strict produced a **reproducible hang** in
`vfs_file_grant_ro_relay_preserves_transferred_cap`. The same test passes in ~0.03 s at the
accepted base and ~0.08 s under WA3C1, so the hang is attributable to strict publication and
nothing else.

That is not an isolated test. Waiter replacement is *deliberately exercised* by a cohort:

| test | what it pins |
|---|---|
| `replacement_publishes_and_is_not_removable_by_stale_identity` | a replacement survives a stale identity clear |
| `waiter_replaced_by_other_tid_rejects_claim` | a claim against a replaced waiter is refused |
| `replacement_waiter_rolls_back_zero_wake` | replacement rollback wakes nobody |
| `replaced_restore_never_strands_never_clobbers` | restore neither strands nor clobbers |
| `d_reply_vs_endpoint_replacement_stale_claim_rejected` | reply-vs-endpoint replacement ordering |
| `c15_waiter_replacement_before_fire` | replacement before a timeout fires |
| `stage199d_delivery_projection_differential` (35 tests) | the delivery projection differential |

Replacement is therefore a **contract**, not an accident, and the relay, direct-request,
direct-reply and reply-timeout paths each build rollback behaviour on it. Deciding whether YARM
should keep it is a design question that must be answered from those contracts and from the relay
hang — not settled as a side effect of wiring ownership. WA3C1 changes none of it.

#### B. What WA3C1 actually changes

**The authoritative waiter record is generation-bearing.** `endpoint_waiters` stores
`EndpointWaiterRecord { receiver, wait_generation }` instead of a bare `ReceiverWaiterIdentity`.
The generation lives *in* the published record rather than in a parallel array, so the waiter and
its incarnation have one lifetime — there is no second field to update a moment later, or forget.
Narrow projections (`endpoint_waiter_identity`, `tid()`, `asid()`) keep the many `{tid, asid}`
callers off the storage layout.

**A fresh blocked-receive generation per real cycle.** `recv_block_phase_b_task` mints it with
`checked_add(1)` under task rank 2, in the same acquisition that marks the task `Blocked`, and it
is threaded through `RecvBlockPhasePlan` into Phase C — never re-read later by bare TID. Wrapping
would let a stale record compare equal to a newer one, which is the exact confusion the generation
exists to prevent, so exhaustion fails closed and the scheduler/task block is unwound coherently.
This is the only new publication failure WA3C1 introduces.

**One central removal body.** `remove_endpoint_waiter_at` owns the three things that must happen
exactly once per departing waiter — slot clear, direct-ack lease release, census unlink — and
`take_endpoint_waiter`, `clear_endpoint_waiter_if_identity` and
`clear_endpoint_waiters_for_identity` all delegate to it. `remove_endpoint_waiter_exact` is added
for callers that already hold the record, so the generation is not discarded where it is known.
The direct-ack lease key is deliberately **unchanged** — WA3C1 changes the waiter record, not the
lease contract.

#### C. Two real production defects fixed

1. **Displacement leaked a direct-ack lease.** `set_endpoint_waiter` replaced the record without
   releasing the displaced waiter's lease. A replaced waiter is a departing waiter; its lease now
   retires with it.
2. **`destroy_endpoint` bypassed the waiter lifecycle entirely.** It did
   `ipc.endpoint_waiters[endpoint_idx] = None;` — a raw slot write — and then advanced
   `endpoint_generations`. So it never released the lease and never unlinked the census, and the
   leak was *permanent*, because the lease is keyed on the generation the next line advanced past.
   It now removes through the central lifecycle while the OLD generation is still authoritative,
   and only then advances.

#### D. The stranded receiver

The same function also discarded a parked receiver without waking it, leaving it
`Blocked(EndpointReceive)` on an endpoint that no longer existed, with nothing that could ever
wake it. WA3C1 snapshots the complete record under IPC rank 3, releases the lock, and wakes the
receiver **only** if `{tid, asid, wait_generation, Blocked(EndpointReceive)}` all still match.

This makes `destroy_endpoint` a **new production caller of `wake_tid_to_runnable`** — recorded in
the pinned caller set rather than suppressed. It is safe because it is exact in every dimension: a
replacement task that reused the TID under a different ASID is refused by the ASID compare; a
receiver that already completed is refused by the status compare; and a receiver that has since
re-blocked under a newer generation is refused by the generation compare. No numeric-TID-only
resurrection is possible.

#### E. Ownership remains dormant

Production callers of all six ownership operations — `arm_current`, `claim`, `consume`, `cancel`,
`restore`, `retire_current` — remain **zero**, pinned by guard. The record WA3C1 introduces is the
representation a future `WaiterKey` will be derived from, but nothing derives one yet.

#### Census

Recomputed mechanically and **unchanged**: **CAN 12 / CANNOT 16 / INTO_BLOCKED 7 /
FRESH_CONSTRUCTOR 2 / NON_PRODUCTION 1 / UNPROVEN 0, total 38.** The generation write is
`tcb.blocked_recv_generation`, not a `TaskStatus` write, so it introduces no new status authority.
The one census-relevant movement is the new `wake_tid_to_runnable` caller above.

#### Status

* production executable code — **changed**
* waiter representation — **generation-bearing**
* last-receiver-wins — **preserved**; the replacement cohort passes unchanged
* waiter removal — **centralized**; lease + census settled exactly once
* displacement lease leak — **fixed**
* `destroy_endpoint` raw clear / lease leak / census drift / stranded receiver — **fixed**
* ownership ARM / RETIRE / CLAIM / settle production callers — **0**
* `WAITER_OWNER_CENSUS_COMPLETE` — **yes**; `WAITER_OWNERSHIP_EXCLUSIVE` — **no**
* direct production default — **OFF**; canonical 199D — **OPEN**; ledger — **39 / 7 / 46**
* **no new live cell**

WA3C2 begins with the semantic investigation: keep waiter replacement, or move to strict
single-waiter publication — decided from the relay hang and the DirectRequest / DirectReply /
reply-timeout replacement contracts, before ownership ARM/RETIRE is made live.

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
