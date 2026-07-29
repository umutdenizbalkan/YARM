<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Umut Deniz Balkan -->

# YARM Kernel-Unlock Audit

**Evidence-based audit of the broad `SpinLock<KernelState>` ("global lock") and the
canonical roadmap to full kernel unlocking.**

This document is generated from the source tree, not from prior stage reports. Where a
prior report and the source disagree, the source wins and the disagreement is recorded.
Historical stage numbering (`199A2D2C2B3`, `200D-2B1D5B`, …) is **not** used as a
progress measure; it is retained only as commit-evidence. The canonical stage ladder is
§4 (`199C` … `205D`).

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
| `cargo test --lib -- --test-threads=1` | **ok — 3725 passed, 0 failed, 2 ignored** |
| `cargo test --tests -- --test-threads=1` | **ok — 3725 lib + 139 integration = 3864 passed, 0 failed** |
| `cargo test --lib` (default parallel harness) | **ABORTS — `double free or corruption`, SIGABRT, 3 of 3 attempts** |

The parallel abort is reproducible and is recorded as blocker **B9** (§5). It means the
CI step `cargo test -q` in `.github/workflows/compat-gates.yml` is not currently a
reliable gate. Every "hosted evidence" claim in this document is a **single-threaded**
claim.

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

Stage identities below are **canonical**, defined by this audit against the source at
`757993b`. The "historical" column records which legacy stage labels contributed, purely
as commit-evidence.

Status values: **COMPLETE** = production path landed and evidenced; **PARTIAL** =
mechanism landed but an architecture, a gate, or a live cell is missing; **OPEN** = not
implemented.

| Canonical | Scope | Status | Source / test evidence | Missing production work | Missing live cells | Depends on |
|-----------|-------|--------|------------------------|-------------------------|--------------------|------------|
| **199C** | Off-lock direct `IpcCall`/`IpcReply` transaction (x86_64), incarnation-safe reply records, reserve→commit→cancel | **PARTIAL** | `syscall_split.rs:295/307`; `ipccall_direct_txn.rs`; hosted suite; `STAGE_199_X86_DIRECT_IPC_FINAL_SEAL … result=ok` (`STAGE_199A2D3`) | gate `ipccall_direct_proof_enabled()` is **default-OFF** — NR6/NR7 never take the off-lock path in a normal boot | 0 (all NR6/NR7 cells are proof-gated) | — |
| **199D** | Cross-arch NR6/NR7 admission (AArch64 `199A2C1`, RISC-V `199A2C2`) | **PARTIAL** | `trap_entry.rs:1384`; `riscv64/trap.rs:442`; `qemu-ipccall-reply-direct-matrix-seal.sh` | same default-OFF gate | proof-gated only | 199C |
| **200A** | Reply / timeout / peer-death terminal ownership model | **COMPLETE** | `doc/STAGE_200A…` §10 hosted seal `result=ok`; `terminal_ownership.rs` | — | n/a (hosted-only stage) | — |
| **200B** | Generation-bearing deadline token store | **COMPLETE** | `deadline_token.rs`; hosted seal `result=ok` | — | n/a | 200A |
| **200C** | Reply-receive deadline completion transaction + three-arch live matrix | **COMPLETE** | `STAGE_200_IPC_REPLY_TIMEOUT_MATRIX_SEAL`, commit `72a4ebf`; `scripts/qemu-ipc-reply-timeout-matrix-smoke.sh` | — | **6/6 earned** (timeout-wins + reply-wins × x86_64/AArch64/RISC-V) | 200B |
| **200D** | Off-lock reply-timeout retirement (scan off the broad lock) | **PARTIAL** | `IPC_REPLY_TIMEOUT_LOCK_STATUS arch=x86_64 scan_broad_lock=0 … result=ok`; `OffLockReplyTimeout` (`runtime.rs:247`) | AArch64 + RISC-V scans still `scan_broad_lock=1`; `run_reply_timeout_completion_locked` (`runtime.rs:3725`) not deleted | x86_64 only | 200C |
| **201A** | `ExitCurrentTask` NR 16 ABI + non-returning disposition | **COMPLETE** | `SYSCALL_EXIT_CURRENT_TASK_NR = 16` (`syscall.rs:37`); hosted suite | — | n/a | — |
| **201B** | `ExitCurrentTask` live cells — x86_64, AArch64 | **COMPLETE** | sealed at `0b5e98f`; `EXIT_TASK_BROAD_LOCK_RELEASED arch=aarch64 … result=ok` | — | **2/3 earned** | 201A |
| **201C** | `ExitCurrentTask` live cell — RISC-V | **OPEN** | runner bound corrected at `5488d8e` (4096→64 yields) | none — kernel chain proven correct at `fb5f040` | **0/1** — re-run never executed | 201B |
| **201D** | Server-death terminal mechanism + deferred post-lock completion | **COMPLETE** | `2bba07f` (hosted mechanism), `a1eb8cb` (deferred completion); `drain_server_death_post_work` | — | n/a | 200A |
| **202A** | ServerDies liveness foundation — 9 transition classes, 15 hard-fail literals, 24 races, 9 wiring guards | **COMPLETE** | `mod.rs:3985`–`4152`; `35a98f2`, `dc5f5c5`; foundation seal `result=ok` | — | 0 by design (`live_cells=0` is part of the seal) | 201D |
| **202B** | Three-architecture return contract + ServerDies live readiness | **COMPLETE** | `7e811cc`; readiness seal `qemu_boots=0 live_cells=0 result=ok` | — | 0 by design | 202A |
| **202C** | x86 post-drain restore-owner revalidation + typed restore-failure contract | **COMPLETE (hosted)** | `OwnerRevalidation`/`OwnerCommit` (`runtime.rs:272`–`322`); `revalidate_idle_owner_after_drains` (`runtime.rs:665`, wired `x86_64/descriptor_tables.rs:1324`); 9 + 11 hosted cases, 7 + 10 mutation guards killed | AArch64/RISC-V equivalents not needed (they consume post-drain — A4) | **0** — never exercised in QEMU | 202B |
| **202D** | ServerDies link accounting repair — `LINK_LEAK created=54 detached=1 result=fail` | **OPEN** | defect proven live (Stage 200D-2B1D4); counters at `mod.rs:4116`; creation `ipc_state.rs:1250`, detach `ipc_state.rs:1383` | scope the counters to the audited death, or count `unregister_server_reply_link` too and drop the `== 1` expectation | **blocks all of 203** | 202C |
| **203A** | x86_64 ServerDies live cell | **OPEN** | 4 attempts, none sealed (`200D-2B1D`, `D2`, `D4`, `D5`) | none known beyond 202D | 0/1 | 202D |
| **203B** | AArch64 ServerDies live cell | **OPEN** | runner `scripts/qemu-aarch64-server-dies-smoke.sh` exists | — | 0/1 | 203A |
| **203C** | RISC-V ServerDies live cell | **OPEN** | runner `scripts/qemu-riscv64-server-dies-smoke.sh` exists | — | 0/1 | 203A |
| **203D** | ServerDies three-architecture matrix seal | **OPEN** | — | combined exact-commit runner | 0/3 | 203A–C |
| **204A** | Retire blocking `IpcRecv` (NR 2) off the broad lock | **OPEN** | today only the kernel-task queued-plain case is split | off-lock block-publish + queue-advancing dispatch for the general receiver | 0/3 | 203D |
| **204B** | Retire `IpcSend` (NR 1) off the broad lock | **OPEN** | plain / ordinary-cap / shared-region all enter broad | off-lock send transaction | 0/3 | 204A |
| **204C** | Retire `FutexWait` (NR 9) block+dispatch off the broad lock | **OPEN** | seams landed **helper-only** at Stage 191D — `futex_wait_would_block_split_read`, `futex_wait_publish_block_split_mut` (`syscall_split.rs:786`) | wire the seams live; needs the multi-stage dispatch rewrite the comment disclaims | 0/3 | 204A |
| **204D** | Retire the non-syscall trap path (timer / external IRQ / page fault) | **OPEN** | whole handler is inside `with_cpu` | per-domain IRQ + fault handling | 0/3 | 204A–C |
| **205A** | Remove the AArch64 split return-path broad re-acquisition (A1) | **OPEN** | `trap_entry.rs:1432` | off-lock arch return export | 0/1 | 204D |
| **205B** | Collapse `trap_entry.rs` / `riscv64/trap.rs` to a single bounded broad seam | **OPEN** | 12 + 8 callsites today | drain re-acquisitions replaced by split restores | 0/3 | 205A |
| **205C** | Retire the residual non-trap `with_cpu` sites (`smp.rs` ×4, `descriptor_tables.rs` ×2, `riscv64/boot.rs`, `thread_state.rs`) and the 10 broad `with` sites | **OPEN** | §1.3, §1.4 | split seams for AP bring-up, identity snapshot, resume, thread creation | 0/3 | 205B |
| **205D** | Delete `SharedKernel::with` / `with_cpu` and the `SpinLock<KernelState>` field — **full unlock** | **OPEN** | `runtime.rs:232` | the field itself | 0/3 | 205C |

### 3.1 Documentation defects found during the stage mapping

* **Stage 200C2C is entirely undocumented.** The three-architecture reply-timeout matrix —
  arguably the most significant landed result in the audited range, 6 live cells — exists
  only in commit messages (`1ef1dcc`…`72a4ebf`). No `doc/` file mentions it and
  `KERNEL_UNLOCKING.md` contains **zero** occurrences of any stage identifier at or above
  199C. Migrated into `doc/KERNEL_UNLOCKING.md` and `doc/IPC.md` by this pass.
* **`doc/SYSCALL_ABI.md` was stale**: it declared "Public syscall count: 16 (`0..=15`)"
  while NR 16 `ExitCurrentTask` had landed. Corrected by this pass.
* **`doc/STATUS.md` was ~70 stages stale**, describing Stage 129–132 as the frontier.
* Three doc references were already broken before this pass:
  `doc/ABI_CONTRACT_FREEZE.md` (**actively grepped** by
  `scripts/check-contract-doc-enforcement.sh` — that gate cannot pass),
  `doc/IPC_RECV_V2_ORACLE.md` (comment in `scripts/qemu-ipc-recv-v2-oracle-smoke.sh`), and
  `doc/ROADMAP.md` (referenced by `DOCUMENTATION_MAP.md` and `STATUS.md`).

---

## 4. Dependency-ordered roadmap to full unlock

Each stage below is scoped to **one production path**, small enough for a single
implementation turn. "Broad-lock reduction" is the expected change to the §1.2 count of
**51**.

| # | Stage | One production path | Exit criteria | Hosted tests | Live cells | Broad-lock callsite Δ |
|---|-------|---------------------|---------------|--------------|------------|----------------------|
| 1 | **202D** | `server_dies_counters` accounting (`mod.rs:4116`, `ipc_state.rs:1250/1383`) | `IPC_SERVER_DEATH_LINK_LEAK` cannot fire on a boot with ≥1 ordinary `IpcCall`; `audit_success_path` scopes to the audited death; the nine-counter vector's meaning is restated in `doc/IPC.md` | ≥8 cases: multi-call boot, reply-closed link, exit-closed link, both, counter scoping, reset semantics; ≥4 mutation guards | **0** (explicitly none) | **0** |
| 2 | **203A** | x86_64 ServerDies runner (`scripts/qemu-x86_64-server-dies-smoke.sh`) | one clean boot: caller enqueued → dispatched → resumed with `code=10`; zero hard-fail literals; `202C` revalidation observed live | runner scope guards | **1** (x86_64) | 0 |
| 3 | **201C** | RISC-V `ExitCurrentTask` runner re-run | `EXIT_TASK_SURVIVOR_PROGRESS_OK` + `EXIT_TASK_SYSTEM_HEALTH_OK` within the boot timeout | bound guard already landed | **1** (RISC-V) | 0 |
| 4 | **203B/203C** | AArch64 + RISC-V ServerDies runners | per-arch clean boot, same literals | runner scope guards | **2** | 0 |
| 5 | **203D** | Combined exact-commit ServerDies matrix runner | one commit, three arches, `live_cells=3 result=ok` | matrix parser guards | **3 (seal)** | 0 |
| 6 | **200D′** | AArch64 + RISC-V reply-timeout scan → off-lock | `IPC_REPLY_TIMEOUT_LOCK_STATUS scan_broad_lock=0` on all three arches; delete `run_reply_timeout_completion_locked` | ≥10 cases | **2** | **−1** (`runtime.rs:3725`) |
| 7 | **199C′** | Flip `ipccall_direct_proof_enabled()` from proof-gate to production default on x86_64 | normal boot takes the off-lock NR6/NR7 path; all Stage 199 seals re-run green ungated | ≥12 cases incl. gate-removal guards | **2** | 0 |
| 8 | **199D′** | Same flip for AArch64 + RISC-V | three-arch ungated matrix seal | ≥8 cases | **4** | 0 |
| 9 | **205A** | AArch64 `finalize_split_handled_syscall` (`trap_entry.rs:1412`) | AArch64 `DebugLog`/`FutexWake` complete with **zero** broad acquisitions; export + ELR advance via a split seam | ≥8 cases + 4 mutation guards | **2** (AArch64 DebugLog, FutexWake) | **−1** |
| 10 | **204A** | Blocking `IpcRecv` (NR 2) off-lock block-publish + dispatch | general receiver blocks and resumes with no broad acquisition on the fast path | ≥15 cases | **3** | **−2** to **−4** |
| 11 | **204C** | Wire the Stage 191D `FutexWait` seams live | `FUTEX_WAIT_SPLIT_BLOCK_PUBLISH_OK` on the production path; 192A/195E drains become the only dispatch | ≥12 cases | **3** | **−2** |
| 12 | **204B** | `IpcSend` (NR 1) off-lock send transaction | plain + ordinary-cap classes complete off-lock | ≥15 cases | **6** | **−2** |
| 13 | **204D** | Non-syscall trap path (timer / IRQ / page fault) | timer tick and page fault handled without `with_cpu` | ≥12 cases | **3** | **−2** |
| 14 | **205B** | Collapse drain re-acquisitions in `trap_entry.rs` / `riscv64/trap.rs` | ≤1 broad seam per arch trap entry | ≥15 cases | **3** | **−16** |
| 15 | **205C** | Residual sites: `smp.rs`, `descriptor_tables.rs`, `riscv64/boot.rs`, `thread_state.rs`, the 10 broad `with` sites | zero production `with`/`with_cpu` outside `runtime.rs` | ≥20 cases | **3** | **−17** |
| 16 | **205D** | Delete `SharedKernel::with`, `with_cpu`, and `state: SpinLock<KernelState>` | the type does not compile with a broad lock present; a source guard asserts its absence | full suite | **3** | **−3 → 0 total** |

Cumulative: **51 → 0** production broad-lock acquisition sites.

---

## 5. Ten highest-priority blockers

| # | Blocker | Where | Why it blocks | Blocks |
|---|---------|-------|---------------|--------|
| **B1** | `LINK_LEAK created=54 detached=1 result=fail` — `LinkCreated` counts every bound `IpcCall` system-wide, `LinkDetached` only the exit path; `audit_success_path` also demands every class `== 1`, and `reset_instance()` has no live caller | `mod.rs:4116`; `ipc_state.rs:1250` vs `1383` | the ServerDies audit hard-fails on **any** real boot, so no ServerDies live cell can ever be earned | 202D, 203A–D |
| **B2** | `revalidate_idle_owner_after_drains` — the fix for the Stage 200D-2B1D4 live hang — has **never run in QEMU** | defined `runtime.rs:665`, wired `x86_64/descriptor_tables.rs:1324` | the entire ServerDies live programme rests on an unexercised repair | 203A |
| **B3** | NR6/NR7 off-lock direct IPC is **default-OFF** (`ipccall_direct_proof_enabled()`) | `mod.rs:3095` | every Stage 199 seal describes a path a normal boot never takes; the off-lock IPC work delivers **zero** production benefit today | 199C′, 199D′ |
| **B4** | Off-lock authoritative dispatch is compile-time x86_64-only | `mod.rs:766` | AArch64 and RISC-V cannot retire any queue-advancing class; two thirds of the matrix is structurally frozen | 204A, 204C, 205B |
| **B5** | AArch64 split classes re-acquire the broad lock on return | `trap_entry.rs:1432` | AArch64 has **no** genuinely broad-lock-free syscall; the first-cohort seal overstates it | 205A |
| **B6** | `FutexWait` block+dispatch seams landed **helper-only** and were never wired | `syscall_split.rs:786`–`803` | the largest single blocking class stays broad-lock-only; the comment itself disclaims the required dispatch rewrite | 204C |
| **B7** | Reply-timeout scan is off-lock on x86_64 only; the broad-lock completion fallback survives | `runtime.rs:3725`; `IPC_REPLY_TIMEOUT_LOCK_STATUS scan_broad_lock=1` on AArch64/RISC-V | Stage 200D cannot close; a broad callsite cannot be deleted | 200D′ |
| **B8** | RISC-V `ExitCurrentTask` live cell never earned — kernel chain proven correct, runner bound corrected, **re-run not executed** | `5488d8e` | 201 stays at 2/3 with no technical obstacle — pure execution debt | 201C |
| **B9** | `cargo test --lib` **aborts** under the default parallel harness (`double free or corruption`, SIGABRT, 3/3) while passing 3725/3725 single-threaded | hosted suite; CI `cargo test -q` | the project's own hosted gate is unreliable; every hosted claim is implicitly single-threaded and unstated | all hosted evidence |
| **B10** | `scripts/check-contract-doc-enforcement.sh` greps `doc/ABI_CONTRACT_FREEZE.md`, which **does not exist** | script lines 14, 26, 27 | a contract gate that cannot pass; ABI freeze is unenforced | ABI contract enforcement |

---

## 6. Recommended immediate next stage

### **Stage 202D — ServerDies link accounting repair**

**One production path:** `src/kernel/boot/mod.rs::audit_success_path` and the two
`ServerReplyLink` accounting sites (`ipc_state.rs:1250` create, `ipc_state.rs:1383`
detach).

**Why this one, ahead of everything else:**

1. It is the **only** hard `result=fail` currently in the tree. Blockers B2–B8 are
   incompleteness; B1 is a live failing assertion.
2. It is the sole gate on four canonical stages (203A–203D) and, transitively, on the
   whole of 204–205.
3. It is genuinely one turn: a counter-scoping decision plus hosted tests. **No QEMU, no
   live cell, no scheduler change.**
4. It cannot regress anything — the counters are diagnostic, and Stage 200D-2B1D4 already
   proved the underlying link lifecycle is correct (`ipc_reply` does close links via
   `finalize_server_reply_link_for_record`). The bug is in the *audit*, not the mechanism.

**Recommended resolution:** scope the nine-counter vector to the death currently being
audited, rather than counting `unregister_server_reply_link` as a second close site.
Per-death scoping preserves the "every class == 1" expectation that Stage 200D-2B1B's
fifteen hard-fail literals were written against, so the foundation seal keeps its meaning;
counting both close sites would force that seal to be re-specified.

**Exit criteria:**
* `IPC_SERVER_DEATH_LINK_LEAK` cannot fire on a boot containing ≥1 unrelated bound `IpcCall`.
* `IPC_SERVER_DEATH_TRANSITION_AUDIT` reports the audited death only.
* `reset_instance()` either gains a live caller or is removed with its rationale recorded.
* The Stage 202A fifteen hard-fail literals and nine wiring guards still hold.

**Hosted tests (≥8):** multi-call boot with one death; reply-closed link; exit-closed
link; both in one boot; counter scoping across two deaths; reset semantics; the nine-class
vector shape; a negative case where a genuine leak *must* still be reported.

**Mutation guards (≥4):** counters revert to global scope; the genuine-leak detector is
weakened to always-pass; the audited-death filter is dropped; `== 1` is removed without
per-death scoping.

**Live cells:** **none.** State this explicitly in the stage report — 202D is a hosted
stage, and the first live cell belongs to 203A.

**Expected broad-lock callsite reduction: 0.** 202D unblocks the reduction; it does not
itself perform one. Claiming otherwise would be false.

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
