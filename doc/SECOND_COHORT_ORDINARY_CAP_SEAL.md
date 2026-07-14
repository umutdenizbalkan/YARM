# Second-Cohort Ordinary-Cap IpcSend Cross-Architecture Parity Seal (Stage 198B)

This document is the authoritative record of the **two ordinary-capability
`IpcSend` success classes** brought to live parity across all three
architectures. It is built from the current source, not copied from prior stage
reports.

> **Scope.** Exactly two classes go live on all three arches:
> `IpcSendOrdinaryCap` (an **ordinary** endpoint cap transferred to an already
> `recv-v2`-blocked receiver) and `IpcSendOrdinaryCapEnqueue` (an ordinary-cap
> no-waiter enqueue + later `recv-v2` dequeue delivery). **Ordinary caps only** —
> a `Reply` cap, a shared-region transfer, a `D2` recv/send drain, a new syscall
> class, AP user dispatch, and VM mapping through IPC are all **out of scope** and
> stay unretired / x86-shaped. The retirement *mechanism* already existed and was
> x86-live; Stage 198B (a) **arch-tags** the two ordinary-cap retirement markers,
> (b) **replicates the x86-only oracle slot provisioning** into AArch64 and
> RISC-V `boot.rs`, and (c) makes the arch-neutral oracle workloads emit
> **canonical per-arch attestations** that carry an **authoritative object-identity
> proof**. Zero ABI change (`SYSCALL_COUNT = 32`, `Syscall::VARIANT_COUNT = 22`),
> zero new kernel lock, no CNode capacity increase, NR 27 stays absent.

> **Preserves the earlier cohorts.** `FIRST_COHORT_LIVE_MATRIX arches=3
> classes=4 live_cells=12 result=ok` and `SECOND_COHORT_PLAIN_SEAL arches=3
> classes=2 live_cells=6 result=ok` are untouched (see
> `doc/FIRST_COHORT_RETIREMENT_SEAL.md`, `doc/SECOND_COHORT_PLAIN_SEAL.md`).

## 1. Canonical cohort identity

```
SecondCohortOrdinaryCap = { IpcSendOrdinaryCap, IpcSendOrdinaryCapEnqueue }
```

Canonical class-name vocabulary (exact spelling/case, reuse of the existing
production names — normalized once, not duplicated): `IpcSendOrdinaryCap`,
`IpcSendOrdinaryCapEnqueue`.

**Explicitly excluded** (NOT part of this stage; remain x86-only live targets or
unretired): `IpcSendReplyCap` (193D) and reply-cap enqueue (Stage 193G exclusion
preserved), shared-region transfer, `D2` recv/send drains, VM mapping through
IPC, any AP user dispatch, and any further syscall retirement class.

### Reply-cap vs ordinary-cap classification (authoritative, not flag-derived)

A transfer is an **ordinary-cap** path iff the transfer envelope's
`source_object` is **not** a `Reply` object. The split reply arm
(`materialize_split_reply_cap_equivalent` / `phase_a_take_reply_envelope`)
classifies by `envelope.source_object == CapObject::Reply { .. }` — the
kernel-authoritative object kind — **never** solely from the user-controlled
`FLAG_REPLY_CAP` message flag. Ordinary-cap delivery therefore runs the
`grant_task_to_task_with_rights` derive path; reply caps run the separate
direct-mint one-shot seam and are untouched by this stage.

## 2. Retirement model (unchanged mechanism, now arch-tagged + cross-arch live)

Both classes use the established **canonical in-lock publication + arch-neutral
post-lock boundary drain** model — NOT a pre-lock split.

* **`IpcSendOrdinaryCap` (blocked receiver).** The in-lock producer
  (`syscall.rs::produce_blocked_waiter_ordinary_cap_delivery`) snapshots the
  payload + the transfer envelope by value and publishes a
  `DispatchPostWork::BlockedWaiterOrdinaryCapDelivery` (setting the per-CPU
  `IPC_SEND_BOUNDARY_ORIGIN` flag) — **no cap materialization, no user copy, no
  wake under the broad borrow**. The arch-neutral drain
  (`runtime.rs::execute_dispatch_post_work`, run from every arch's trap-entry
  `drain_dispatch_post_work`) materializes the receiver-local cap on the **live D1
  split path** (`cap_transfer_split::materialize_split_transfer_cap_equivalent` →
  `phase_a_take_transfer_envelope` + `phase_b_materialize_transfer_cap`), copies
  the payload to the waiter ASID via `copy_to_user_split` (**no locks**), wakes the
  receiver, and — gated on `ipc_send_boundary_origin_take(cpu)` — emits the
  arch-tagged retirement.
* **`IpcSendOrdinaryCapEnqueue` (no waiter).** The send enqueues the ordinary-cap
  message (envelope retained) in-lock via the endpoint-only Stage 4E seam; the
  cap is materialized at the **later** `recv-v2` dequeue, again through the D1
  split path. The retirement fires from the arch-neutral in-lock enqueue seam.

The arch string on both retirement markers is selected by `cfg(target_arch)` in
`maybe_log_ipc_send_ordinary_cap_retired` /
`maybe_log_ipc_send_ordinary_cap_enqueue_retired` (`src/kernel/boot/mod.rs`) so
every arch emits the canonical
`GLOBAL_LOCK_RETIRE_CLASS_{BEGIN,DONE} arch=<arch> class=<class> result=ok`.

### Cap materialization preserves object identity (SAME object, fresh CapId)

The live ordinary-cap blocked-waiter delivery and the enqueue's later `recv-v2`
dequeue **both** materialize the receiver-local cap through the
`materialize_received_cap_snapshot_with_delegation_split`
(`src/kernel/boot/cap_transfer_delegation_split.rs`) seam — an atomic mint of
`Capability::new(snapshot.object, snapshot.rights)` into the receiver's cspace
(`+` a sender→receiver delegation link when `source_tid != dest_tid`). The mint
produces a **fresh receiver-local `CapId`** that references the **same underlying
`CapObject::Endpoint { index, generation }`** the sender transferred — it does
**not** mint a new object. Stage 198B adds a kernel-side **authoritative
object-identity comparison** at exactly this chokepoint: after the mint (and
delegation-link) commit, it re-resolves the minted cap back **out of the
receiver's cspace** (a real cnode lookup) and compares its endpoint index to the
source object's, emitting

```
IPC_ORDINARY_CAP_OBJECT_IDENTITY receiver_tid=<tid> src_endpoint=Some(i) dst_endpoint=Some(i) match=1
```

A mismatch (a real bug) would emit `match=0` and fail the seal. This is a
**meaningful authoritative object comparison** (a cnode resolve of the installed
cap), not a `CapId != 0` check. Because it sits on the single seam both cells take
on every arch, the proof fires for the actual oracle deliveries on x86_64,
AArch64, and RISC-V alike (not from an incidental unrelated cap grant).

### RISC-V return-outcome audit (required before implementation)

The ordinary-cap `IpcSend` **sender** on RISC-V stays on the canonical handler
and returns `RiscvTrapEntryOutcome::ReturnToCurrent`. The in-lock handler
publishes the delivery and returns normally; the post-lock
`drain_dispatch_post_work(cpu)` materializes + wakes the receiver but **does not
set `switched`**, so the tail return selects `ReturnToCurrent` — the sender stays
current and `sret`s back to itself. It is **never** `EnterKernelIdle` (no idle)
and **never** `ReturnToIncoming` (no switch). Ordinary-cap `IpcSend` is therefore
**not** added to the RISC-V selective pre-lock gate.

### RISC-V blocking-syscall terminal idle provenance (Stage 198A1 → renamed 198B)

The RISC-V typed blocking-idle provenance (a syscall that blocks the last
runnable task yields a typed `EnterKernelIdle`, never state-only inference)
carries the **blocking class** authoritatively. The reason variant is
`RiscvIdleReason::BlockedIpcNoRunnable` (renamed from the too-narrow
`BlockedRecvNoRunnable`, since IpcCall and IpcSend also reach it), and the
canonical seam records the concrete `BlockingSyscallClass { IpcRecv, IpcCall,
IpcSend }` alongside the tid token (`BLOCKED_SYSCALL_IDLE_PROVENANCE` +
`BLOCKED_SYSCALL_IDLE_CLASS`). `FutexWait` keeps its own `FutexWaitNoIncoming`
typed idle; `Yield` never idles; the former state-inferred `ExistingTerminalIdle`
reclassification stays **removed** from both the `Ok` and `Err` paths.

## 3. Authoritative 3×2 implementation matrix

Legend: **provision** = kernel startup-slot wiring that arms the live oracle;
**producer** = in-lock publish site; **materialize** = receiver-local cap derive;
**drain** = post-lock executor; **return** = caller disposition; **attestation** =
per-arch oracle proof marker.

### `IpcSendOrdinaryCap` — ordinary cap to an already `recv-v2`-blocked receiver

| | x86_64 | AArch64 | RISC-V |
|---|---|---|---|
| provision | `boot.rs`: slot 13 = `provision_init_ipc_send_cap_oracle_coord` (E2 recv cap, slot 14 EMPTY) | same (added Stage 198B) | same (added Stage 198B) |
| producer | `produce_blocked_waiter_ordinary_cap_delivery` (in-lock snapshot + publish) | same | same |
| materialize | D1 split `phase_b_materialize_transfer_cap` (`grant_task_to_task_with_rights`, fresh CapId, SAME object) | same | same |
| drain | `execute_dispatch_post_work` → `BlockedWaiterOrdinaryCapDelivery` (arch-neutral, no locks) | same | same |
| user copy | `copy_to_user_split` out of every lock | same | same |
| wake | `apply_scheduler_wake_plan(Wake)` in the drain | same | same |
| return | caller stays current (x86 IRET same-task) | same | `RiscvTrapEntryOutcome::ReturnToCurrent` |
| identity | `IPC_ORDINARY_CAP_OBJECT_IDENTITY ... match=1` (authoritative, live split path) | same | same |
| retirement | `GLOBAL_LOCK_RETIRE_CLASS_DONE arch=x86_64 class=IpcSendOrdinaryCap result=ok` | `arch=aarch64` | `arch=riscv64` |
| attestation | `IPCSEND_ORDINARY_CAP_BLOCKED_RECEIVER_ORACLE_DONE arch=x86_64 result=ok payload_len=8 receiver_resumes=1 fresh_cap=1 object_identity_ok=1` | `arch=aarch64` | `arch=riscv64` |
| proof | `YARM_IPC_SEND_CAP_ORACLE=1` fork oracle (child recv-blocks, init sends ordinary cap; child round-trips a probe **through** C′) | same | same |

### `IpcSendOrdinaryCapEnqueue` — ordinary-cap no-waiter enqueue + later `recv-v2` dequeue

| | x86_64 | AArch64 | RISC-V |
|---|---|---|---|
| provision | `boot.rs`: slot 17 = 2 via `ipc_send_cap_enqueue_oracle_active()` (slots 13 + 14 empty) | same (added Stage 198B) | same (added Stage 198B) |
| producer | endpoint-only Stage 4E enqueue seam (in-lock; envelope retained for later dequeue) | same | same |
| materialize | at the later `recv-v2` dequeue, D1 split `phase_b_materialize_transfer_cap` | same | same |
| drain | recv-v2 boundary delivery (arch-neutral) | same | same |
| return | sender returns `Ok` and continues | same | `RiscvTrapEntryOutcome::ReturnToCurrent` |
| identity | `IPC_ORDINARY_CAP_OBJECT_IDENTITY ... match=1` at dequeue materialize | same | same |
| retirement | `GLOBAL_LOCK_RETIRE_CLASS_DONE arch=x86_64 class=IpcSendOrdinaryCapEnqueue result=ok` | `arch=aarch64` | `arch=riscv64` |
| attestation | `IPCSEND_ORDINARY_CAP_ENQUEUE_ORACLE_DONE arch=x86_64 result=ok payload_len=8 dequeue_count=1 fresh_cap=1 object_identity_ok=1` | `arch=aarch64` | `arch=riscv64` |
| proof | `YARM_IPC_SEND_CAP_ENQUEUE_ORACLE=1` (no fork; init enqueues an ordinary cap then recv-drains it, round-trips a probe through C′) | same | same |

## 4. Error / rollback matrix

| condition | behavior |
|---|---|
| cap materialize fault in the drain | the D1 split materialize returns `Err`; the drain propagates it (RISC-V `drain_dispatch_post_work(cpu)?` → `RISCV_TRAP_HANDLE_FAILED`). The envelope is consumed exactly as the canonical path (no half-delivered cap); the origin flag is consumed only on success, so **no** retirement is emitted on failure. |
| stale / wrong-object envelope | `phase_a_take_transfer_envelope` returns `InvalidCapability` / `WrongObject`, byte-identical to the canonical arm (`IPC_RECV_CAP_MATERIALIZE_FAILED kind=transfer`). |
| object-identity mismatch (bug) | `IPC_ORDINARY_CAP_OBJECT_IDENTITY ... match=0`; the seal `FORBIDDEN` list rejects `ORACLE_IDENTITY_FAIL` and a non-`match=1` proof, failing closed. |
| a reply cap on an ordinary cell | forbidden: `class=IpcSendReplyCap` / a `Reply` `source_object` routes to the reply arm and emits **no** ordinary-cap attestation; the seal rejects it. |
| receiver cnode full at materialize | transactional failure inside the grant; no leaked transient cap; **CNode capacity is NOT increased** to force the oracle to pass. |

## 5. Seal contract

Live proof only — no source-guard substitute. Each of the 6 (arch × class) cells
is sealed when the arch-tagged retirement marker, the per-arch attestation
(including `fresh_cap=1 object_identity_ok=1`), **and** the kernel-authoritative
`IPC_ORDINARY_CAP_OBJECT_IDENTITY ... match=1` marker all appear in a fresh QEMU
boot. Driven by `scripts/qemu-second-cohort-ordinary-cap-seal.sh`:

```
SECOND_COHORT_ORDINARY_CAP_MATRIX arches=3 classes=2 live_cells=6 result=ok
SECOND_COHORT_ORDINARY_CAP_SEAL arches=3 classes=2 live_cells=6 result=ok
```

Per-cell:

```
GLOBAL_LOCK_RETIRE_CLASS_DONE arch=<arch> class=IpcSendOrdinaryCap result=ok
IPCSEND_ORDINARY_CAP_BLOCKED_RECEIVER_ORACLE_DONE arch=<arch> result=ok payload_len=8 receiver_resumes=1 fresh_cap=1 object_identity_ok=1
GLOBAL_LOCK_RETIRE_CLASS_DONE arch=<arch> class=IpcSendOrdinaryCapEnqueue result=ok
IPCSEND_ORDINARY_CAP_ENQUEUE_ORACLE_DONE arch=<arch> result=ok payload_len=8 dequeue_count=1 fresh_cap=1 object_identity_ok=1
```

`FORBIDDEN` (any occurrence fails the seal): `ORACLE_IDENTITY_FAIL`,
`class=IpcSendReplyCap`, `class=IpcSendSharedRegion`.

## 6. Exclusions preserved (zero live)

Reply-cap transfer (`IpcSendReplyCap`, 193D), reply-cap enqueue (Stage 193G
exclusion), shared-region transfer, `D2` recv/send drains, VM mapping through
IPC, and NR 27 stay absent / unretired. The reply-cap classification is
authoritative (`source_object == Reply`), never taken from a user flag, so no
user-controlled input can smuggle a reply cap through the ordinary-cap seam.

## 7. DebugLog copy widening (attestation length)

The canonical ordinary-cap attestations are ~138 bytes with the `arch=<arch>`
tag — wider than an IPC `Message::MAX_PAYLOAD` (128). The **DebugLog copy seam
only** is widened to `DEBUG_LOG_MAX_BYTES = 192` in lockstep across the userspace
emitter (`yarm-user-rt` `MAX_LOG_LEN`), the global-lock handler
(`syscall/debug.rs`), and the split handler + its copy
(`syscall_split.rs::try_split_debug_log_into_frame`,
`runtime.rs::copy_from_user_asid_split_read`). This bounds only the DebugLog
message copy; **IPC message framing is unchanged**. `syscall::debug` is
`pub(crate)` so the split copy seam can reference the shared cap constant.

## 8. Hosted guards

`src/kernel/boot/tests.rs` (Stage 198B ordinary-cap parity): arch-tagged
ordinary-cap retirement markers on all three arches; no untagged legacy form;
drain stays arch-neutral + origin-gated; slot-13 + slot-17=2 provisioning
replicated on all three arches; per-arch attestations carry `fresh_cap=1
object_identity_ok=1`; the authoritative `IPC_ORDINARY_CAP_OBJECT_IDENTITY`
marker exists on the live D1 split path; RISC-V ordinary-cap `IpcSend` returns
`ReturnToCurrent`; reply-cap classification is object-derived not flag-derived;
seal script requires 6 cells; `SYSCALL_COUNT`/`VARIANT_COUNT` unchanged; no new
lock; no CNode capacity increase.
