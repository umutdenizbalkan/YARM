# Second-Cohort Plain IpcSend Cross-Architecture Parity Seal (Stage 198A)

This document is the authoritative record of the **two plain-payload `IpcSend`
success classes** brought to live parity across all three architectures. It is
built from the current source (not copied from prior stage reports).

> **Scope.** Exactly two classes go live on all three arches:
> `IpcSendPlain` (plain send to an already `recv-v2`-blocked receiver) and
> `IpcSendPlainEnqueue` (plain no-waiter enqueue + later `recv-v2` dequeue).
> **No capability transfer** is exercised or provisioned in this stage — plain
> inline payload only, no ordinary cap, no reply cap, no shared region, no cap
> materialization, no MemoryObject mapping. The retirement *mechanism* already
> existed and was x86-live; Stage 198A (a) **arch-tags** the two retirement
> markers, (b) **replicates the x86-only oracle slot provisioning** into AArch64
> and RISC-V `boot.rs`, and (c) makes the arch-neutral oracle workloads emit
> **canonical per-arch attestations**. Zero ABI change (`SYSCALL_COUNT = 32`,
> `Syscall::VARIANT_COUNT = 23`), zero new kernel lock, NR 27 stays absent.

> **Preserves the first cohort.** `FIRST_COHORT_LIVE_MATRIX arches=3 classes=4
> live_cells=12 result=ok` and `FIRST_COHORT_CROSS_ARCH_SEAL arches=3 classes=4
> result=ok` are untouched (see `doc/FIRST_COHORT_RETIREMENT_SEAL.md`).

## 1. Canonical cohort identity

```
SecondCohortPlain = { IpcSendPlain, IpcSendPlainEnqueue }
```

Canonical class-name vocabulary (exact spelling/case, no drift, reuse of the
existing production names — normalized once, not duplicated):
`IpcSendPlain`, `IpcSendPlainEnqueue`.

**Explicitly excluded** (NOT part of this stage; remain x86-only live targets or
unretired):
`IpcSendOrdinaryCap` (193C), `IpcSendReplyCap` (193D),
`IpcSendOrdinaryCapEnqueue` (193F), reply-cap enqueue, shared-region transfer,
`D2` recv/send drains, VM mapping, any AP user dispatch, and any further syscall
retirement class.

## 2. Retirement model (unchanged mechanism, now arch-tagged + cross-arch live)

Both classes use the established **canonical in-lock publication + arch-neutral
post-lock boundary drain** model — NOT a pre-lock split.

* **`IpcSendPlain` (blocked receiver).** The in-lock producer
  (`ipc_state.rs::try_ipc_send_boundary_split_plain`) snapshots payload + metadata
  by value, publishes `DispatchPostWork::BlockedWaiterPlainDelivery`, and sets the
  per-CPU `IPC_SEND_BOUNDARY_ORIGIN` flag — **no user copy, no wake under the broad
  borrow**. The arch-neutral drain (`runtime.rs::execute_dispatch_post_work`, run
  from every arch's trap-entry `drain_dispatch_post_work`) copies payload + meta to
  the waiter ASID via `copy_to_user_split` (**no locks**), wakes the receiver, and —
  gated on `ipc_send_boundary_origin_take(cpu)` — emits the arch-tagged retirement.
* **`IpcSendPlainEnqueue` (no waiter).** Fully in-lock via the endpoint-only Stage 4E
  seam (`ipc_try_send_queued_plain_endpoint_only`, rank-4 IPC lock only): no user
  copy, no cap materialization, no wake, no sender block. The retirement fires from
  the arch-neutral in-lock enqueue seam (`ipc_state.rs`).

The arch string on both retirement markers is selected by `cfg(target_arch)` in
`maybe_log_ipc_send_plain_retired` / `maybe_log_ipc_send_plain_enqueue_retired`
(`src/kernel/boot/mod.rs`) so every arch emits the canonical
`GLOBAL_LOCK_RETIRE_CLASS_{BEGIN,DONE} arch=<arch> class=<class> result=ok`.

### RISC-V return-outcome audit (required before implementation)

Plain `IpcSend` on RISC-V stays on the canonical handler and returns
`RiscvTrapEntryOutcome::ReturnToCurrent`. The in-lock handler publishes the
delivery and returns normally; the post-lock `drain_dispatch_post_work(cpu)` wakes
the receiver but **does not set `switched`** (only the FutexWait/Yield switch
drains do), so the tail return selects `ReturnToCurrent` — the sender stays current
and `sret`s back to itself. It is **never** `EnterKernelIdle` (no idle) and **never**
`ReturnToIncoming` (no switch). Plain `IpcSend` is therefore **not** added to the
RISC-V NR15/NR10 selective pre-lock gate — the generic canonical-handler +
boundary-drain design already covers it.

### RISC-V recv-block terminal-idle fix (pre-existing gap, required for the seal)

Bringing the oracle live on RISC-V surfaced a **pre-existing** defect unrelated to
the plain-IpcSend mechanism: the `ipc_recv_proof` scaffolding the oracle rides on
had **never** booted cleanly on RISC-V. When a syscall BLOCKS the last runnable task
(a `recv-v2` with no incoming message) it succeeds (`Ok`) while clearing `current`,
leaving nothing runnable. Stage 197B wired `ExistingTerminalIdle` only on the `Err`
channel, so this `Ok`-path case fell through to `ReturnToCurrent`; the bridge then
`sret`'d a stale frame as tid 0 and **hot-spun** re-entering the blocked recv
(observed as an `IPC_RECV_ENTER tid=0` busy loop that never reached WFI idle). The
targeted fix adds a terminal-idle branch on the wrapper's `Ok` path, gated on
`!switched` AND the SAME positive scheduler-state predicate the `Err` path uses
(`current` is `None|Some(0)` AND zero runnable on this CPU) → return
`EnterKernelIdle { ExistingTerminalIdle }`. This is **audited and reported** per the
stage's RISC-V return-outcome rule: it does **not** touch the plain IpcSend sender
(which keeps `current` non-zero → `ReturnToCurrent`), and it never fires after a
Yield (Yield switches → `switched=true`, or keeps `current` non-zero) or a FutexWait
no-incoming (which returns `EnterKernelIdle` from its own explicit branch earlier).
First-cohort DebugLog/FutexWake/FutexWait/Yield retirements are preserved live.

> **Environment note.** The AArch64 core-smoke *wrapper* exits non-zero in this
> specific CI environment even at BASELINE (a normal boot with no proof and none of
> this stage's code times out at the smoke's short `TIMEOUT_SECS` and trips its
> verdict logic) — an environment/harness limitation, not a mechanism defect. The
> AArch64 boot itself is clean (reaches `SCHED_ENTER_IDLE_HLT`, all retirements fire)
> and the plain-IpcSend retirement + attestation markers are proven firing live, so
> the seal (which keys on the genuinely-firing markers from a clean boot with a
> forbidden-marker guard) seals all six cells.

## 3. Authoritative 3×2 implementation matrix

Legend: **provision** = kernel startup-slot wiring that arms the live oracle;
**producer** = in-lock publish site; **drain** = post-lock executor; **return** =
caller disposition; **attestation** = per-arch oracle proof marker.

### `IpcSendPlain` — plain send to an already `recv-v2`-blocked receiver

| | x86_64 | AArch64 | RISC-V |
|---|---|---|---|
| provision | `boot.rs`: slot 14 = `provision_init_ipc_send_plain_oracle_coord` (slot 13 empty) | same (added Stage 198A) | same (added Stage 198A) |
| producer | `try_ipc_send_boundary_split_plain` (in-lock snapshot + publish) | same | same |
| drain | `execute_dispatch_post_work` → `BlockedWaiterPlainDelivery` (arch-neutral, no locks) | same | same |
| user copy | `copy_to_user_split` out of every lock | same | same |
| wake | `apply_scheduler_wake_plan(Wake)` in the drain | same | same |
| return | caller stays current (x86 IRET same-task) | same | `RiscvTrapEntryOutcome::ReturnToCurrent` |
| retirement | `GLOBAL_LOCK_RETIRE_CLASS_DONE arch=x86_64 class=IpcSendPlain result=ok` | `arch=aarch64` | `arch=riscv64` |
| attestation | `IPCSEND_PLAIN_BLOCKED_RECEIVER_ORACLE_DONE arch=x86_64 result=ok payload_len=8 receiver_resumes=1` | `arch=aarch64` | `arch=riscv64` |
| proof | `YARM_IPC_SEND_PLAIN_ORACLE=1` fork oracle (child recv-blocks, init plain-sends) | same | same |

### `IpcSendPlainEnqueue` — plain no-waiter enqueue + later `recv-v2` dequeue

| | x86_64 | AArch64 | RISC-V |
|---|---|---|---|
| provision | `boot.rs`: slot 17 = 1 via `ipc_send_enqueue_oracle_active()` (slots 13+14 empty) | same (added Stage 198A) | same (added Stage 198A) |
| producer | endpoint-only Stage 4E seam `ipc_try_send_queued_plain_endpoint_only` (in-lock, rank-4 only) | same | same |
| drain | none (whole slice is the in-lock enqueue; no deferred work) | same | same |
| user copy | none at send (payload waits in queue; copied at the later `recv-v2`) | same | same |
| wake | none (no receiver blocked) | same | same |
| return | sender returns `Ok` and continues | same | `RiscvTrapEntryOutcome::ReturnToCurrent` |
| retirement | `GLOBAL_LOCK_RETIRE_CLASS_DONE arch=x86_64 class=IpcSendPlainEnqueue result=ok` | `arch=aarch64` | `arch=riscv64` |
| attestation | `IPCSEND_PLAIN_ENQUEUE_ORACLE_DONE arch=x86_64 result=ok payload_len=8 dequeue_count=1` | `arch=aarch64` | `arch=riscv64` |
| proof | `YARM_IPC_SEND_ENQUEUE_ORACLE=1` (no fork; init enqueues then recv-drains byte-identical) | same | same |

## 4. Error / rollback matrix

| condition | behavior |
|---|---|
| user-copy fault in the plain drain | `execute_dispatch_post_work` returns `Err(UserMemoryFault)`; on RISC-V `drain_dispatch_post_work(cpu)?` propagates → bridge maps to `RISCV_TRAP_HANDLE_FAILED reason=handle_trap_entry_err` (no idle, no half-delivered wake). The origin flag is consumed only on the success arm, so no retirement is emitted on failure. |
| no waiter signal / mismatched waiter | the oracle aborts boundedly (`IPC_SEND_PLAIN_ORACLE_NO_WAITER_SIGNAL` / `_WAITER_MISMATCH`) and emits **no** attestation — the seal fails closed. |
| a transferred cap on a plain cell | forbidden: the child asserts `transferred_cap=0`; the seal rejects `transferred_cap=1` as a parity break. |
| enqueue not delivered byte-identical | the later `recv-v2` `payload_match` gate stays false → no `IPCSEND_PLAIN_ENQUEUE_ORACLE_DONE`. |

## 5. Seal contract

Live proof only — no source-guard substitute. Each of the 6 (arch × class) cells is
sealed when **both** the arch-tagged retirement marker and the per-arch attestation
appear in a fresh QEMU boot. Driven by `scripts/qemu-second-cohort-plain-seal.sh`:

```
SECOND_COHORT_PLAIN_MATRIX arches=3 classes=2 live_cells=6 result=ok
SECOND_COHORT_PLAIN_SEAL arches=3 classes=2 live_cells=6 result=ok
```

Per-cell (both retirement + attestation required):

```
GLOBAL_LOCK_RETIRE_CLASS_DONE arch=<arch> class=IpcSendPlain result=ok
IPCSEND_PLAIN_BLOCKED_RECEIVER_ORACLE_DONE arch=<arch> result=ok payload_len=8 receiver_resumes=1
GLOBAL_LOCK_RETIRE_CLASS_DONE arch=<arch> class=IpcSendPlainEnqueue result=ok
IPCSEND_PLAIN_ENQUEUE_ORACLE_DONE arch=<arch> result=ok payload_len=8 dequeue_count=1
```

## 6. Exclusions preserved (zero live)

The capability-transfer IpcSend classes remain **x86_64-only live targets** and are
**not** provisioned on AArch64 / RISC-V (`provision_init_ipc_send_cap_oracle_coord`,
`provision_init_ipc_send_reply_cap_oracle`, `ipc_send_cap_enqueue_oracle_active` are
absent from the two non-x86 `boot.rs` files — guarded by hosted test
`no_capability_transfer_oracle_on_non_x86_arches`). Reply-cap enqueue, shared-region
transfer, `D2` drains, VM mapping, and NR 27 stay absent/unretired.

## 7. Hosted guards

`src/kernel/boot/tests.rs::stage198a_second_cohort_plain_parity` (11 source-scan
properties): arch-tagged markers on all three arches; no untagged legacy form; drain
stays arch-neutral + origin-gated; slot provisioning replicated on all three arches;
no cap-transfer oracle on non-x86; per-arch attestations emitted + gated on clean
delivery; RISC-V plain `IpcSend` returns `ReturnToCurrent` and is not pre-lock gated;
seal script requires 6 cells; oracle smoke acceptance is arch-parameterised; no new
retirement class; no new lock / no user copy under lock.
