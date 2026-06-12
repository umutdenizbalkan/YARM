// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

# YARM Kernel Decomposition Scaffold Status

**Written at:** Stage 101 — Kernel unlocking restart.
**Owner:** kernel decomposition / unlocking workstream.

This document tracks the plan / scaffold types introduced during the
kernel-unlocking decomposition so that stale or dead scaffold can be
identified before it drifts into the codebase.

Each entry is one of:

- **live** — the type is on a live-wired code path that runs in production.
- **helper-only** — the type is exported but only consumed by tests or by a
  single private helper (no live trap/syscall site).
- **fallback-only** — the type is constructed only on the fallback (global
  lock) side and is part of the contract between the split path and the
  fallback path.
- **deferred** — the type exists for a future stage; it is not consumed at
  all today.
- **obsolete** — the type can be removed in the next maintenance stage; kept
  here to flag the removal candidate.

---

## 1. recv_core plan types

| Type | File | Status (Stage 101) | Notes |
|------|------|--------------------|-------|
| `RecvPlan` | `kernel/recv_core.rs` | **live** | Returned by `plan_recv_core`; consumed by `try_split_recv_queued_plain_with_snapshot_locked`. Branches: `KernelPlainEligible` / `UserPlainEligible` / `UserPlainV2Eligible` / `FallbackRequired`. |
| `RecvWritebackPlan` | `kernel/recv_core.rs` | **live** | Variants `KernelRegister`, `UserMemory`, `UserMemoryV2` all live. |
| `RecvSchedulerWakePlan` | `kernel/recv_core.rs` | **live** | `WakeSender` applied after `ipc_state_lock` released. |
| `RecvCapTransferPlan` | `kernel/recv_core.rs` | **helper-only** (Stage 101) | Populated by `extract_cap_transfer_plan` and read by the syscall-side materialize call. Stage 100 still materializes under the global lock; D1 (Stage 104+) will live-wire the rank-4 split. |
| `CapTransferRecvClass` | `kernel/cap_transfer_split.rs` | **helper-only** (Stage 103, `D1_HELPER_ONLY` / `D1_DEFAULT_OFF`) | Pure flag classification of a delivered `Message`. |
| `CapTransferRecvSnapshot` | `kernel/cap_transfer_split.rs` | **helper-only** (Stage 103) | Phase A output: `Copy` snapshot for Phase B. |
| `CapTransferMaterializeOutcome` | `kernel/cap_transfer_split.rs` | **helper-only** (Stage 103) | Phase B output: receiver-local `CapId`. |
| `CapTransferSplitResult` | `kernel/cap_transfer_split.rs` | **helper-only** (Stage 103) | Combined A → B entry-point outcome: `None` / `Materialized` / `FallbackRequired` / `Failed`. |
| `FallbackReason` | `kernel/recv_core.rs` | **live** | Used by every `try_recv_core_*` adapter. Variant `CapTransfer` is documented as no longer produced; kept for the sender-waiter-with-cap-transfer fallback case. **Deferred (variant)** — `CapTransfer` is the only deferred discriminant. |
| `RecvOutcome` | `kernel/recv_core.rs` | **live** | `Delivered` / `WouldBlock` / `TimedOut` / `FallbackRequired` / `Error`. `TimedOut` is **deferred** (no live producer yet). |

### 1.1 recv_shared_v3 (NR 30) types

| Type | File | Status (Stage 101) | Notes |
|------|------|--------------------|-------|
| `RecvV3MappingPlan` | `kernel/recv_core.rs::recv_shared_v3` | **live** | Returned by the mapping-plan helper; consumed by `handle_recv_shared_v3`. |
| `RecvV3CleanupToken` | `kernel/recv_core.rs::recv_shared_v3` | **live** | Encoded into `RecvSharedV3Output.cleanup_token`. |
| `RecvV3CleanupIdentity` | `kernel/recv_core.rs::recv_shared_v3` | **live** | Stored in the cleanup-token table. |
| `RecvV3CleanupReleaseResult` | `kernel/recv_core.rs::recv_shared_v3` | **live** | Returned by `release()`. |
| `RecvSharedV3Request` (ABI) | `kernel/recv_core.rs::recv_shared_v3` | **live** | Frozen ABI. |
| `RecvSharedV3Output` (ABI) | `kernel/recv_core.rs::recv_shared_v3` | **live** | Frozen ABI offsets. |

---

## 2. VM / TLB plan types

| Type | File | Status (Stage 101) | Notes |
|------|------|--------------------|-------|
| `VmAnonMapPlan` | `kernel/boot/mod.rs` | **live** | Used by `handle_vm_anon_map`. |
| `VmAnonMapProgressPlan` | `kernel/boot/mod.rs` | **live** | Captures successful page-mapping range for rollback. |
| `VmAnonMapRollbackTlbPlan` | `kernel/boot/mod.rs` | **live** | Captures rollback range for TLB shootdown on failure. |
| `VmBrkPlan` | `kernel/boot/mod.rs` | **live** | Used by `handle_vm_brk`. |
| `VmBrkShrinkTlbPlan` | `kernel/boot/mod.rs` | **live** | Aggregates per-page TLB-shootdown bitmaps for brk shrink. |
| `TlbShootdownRequestPlan` | `kernel/boot/mod.rs` | **live** | Computed under VM lock; consumed by the IPI emit. |
| `TlbShootdownWaitPlan` | `kernel/boot/mod.rs` | **live** | Returned by `unmap_page_phase1`; consumed by `execute_tlb_shootdown_wait_plan`. |

D3 (`VmAnonMap` two-phase live) is **deferred**; the plan types exist but are
consumed inside the still-global-locked `handle_vm_anon_map`. No live wiring
to the split path yet.

---

## 3. Scheduler / IPC plan types

| Type | File | Status (Stage 101) | Notes |
|------|------|--------------------|-------|
| `SchedulerWakePlan` | `kernel/boot/mod.rs` | **live** | Used by destroyed-notification wake path. |
| `SchedulerHandoffPlan` | `kernel/boot/mod.rs` | **live** | Used by `apply_scheduler_handoff_plan`. |
| `IpcSchedulerPlan` | `kernel/boot/mod.rs` | **live** | Carries deferred wake from split-recv / split-send to the post-lock wake site. |

D6 (per-CPU scheduler locking) is **deferred** until IPC split work is
stable.

---

## 4. Capability / control-plane plan types

| Type | File | Status (Stage 101) | Notes |
|------|------|--------------------|-------|
| `ControlPlaneCnodePlan` | `kernel/boot/mod.rs` | **live** | Consumed by `control_plane_set_process_cnode_slots_planned` and by the Stage 29 live split-dispatch path. |
| `DriverBundlePlan` | `kernel/boot/types.rs` | **live** | Used by `delegate_driver_bundle`. |

---

## 5. Syscall-split scaffold

| Type | File | Status (Stage 101) | Notes |
|------|------|--------------------|-------|
| `SplitEligibleSyscall` | `kernel/syscall_split.rs` | **live (whitelist-only)** | Variants: `ControlPlaneCnodeSlots` (live), `IpcRecvKernelTask` (live via frame-level seam). |
| `EndpointRecvCapSnapshot` | `runtime.rs` | **live** | Consumed by `try_split_recv_queued_plain_with_snapshot_locked`. |
| `FatalTrapReadSnapshot` | `runtime.rs` | **live** | Consumed by the x86_64 fatal-trap log path. |

---

## 6. Deferred / removal candidates

| Type / variant | Reason | Suggested stage |
|----------------|--------|-----------------|
| `FallbackReason::CapTransfer` | No longer produced by Stage 42+43 split adapters; reserved for the sender-waiter-with-cap-transfer fallback. | Re-evaluate at Stage 102 (D1) — keep if D1 still needs to fall back for sparse sender-waiter queues; remove if D1 absorbs it. |
| `RecvOutcome::TimedOut` | Documented as "reserved for future timed-recv integration". | Re-evaluate when timed-recv is split-wired. |

No types are flagged as **obsolete** at Stage 101.

---

## 6.3 Stage 106 / Pass 3 — D2 IPC recv blocking split (LIVE)

`src/kernel/recv_waiter_split.rs` + `KernelState::publish_recv_waiter_live`:

| Type / fn | Status (Stage 106) | Notes |
|-----------|--------------------|-------|
| `PublishWaiterPlan` | **live-adjacent** | Phase 1 plan type (used by the helper API). |
| `PublishWaiterOutcome` | **live** | Consumed by the live call site in `block_current_on_receive_with_deadline`. |
| `KernelState::publish_recv_waiter_live` | **LIVE** (`D2_LIVE_SPLIT`) | Atomic queue-recheck + waiter publish, one rank-3 critical section; overwrite semantics preserved; `QueueNonEmpty` steers the no-lost-wakeup unwind. Telemetry: `d2_recv_waiter_publishes`, `d2_publish_race_unwinds`. |
| `try_publish_recv_waiter` / `try_publish_recv_waiter_audit_only` | **helper-only** | Stage 105 strict-single-waiter variant (refuses on existing waiter); retained for the future design study; no live caller (test-enforced). |

**NOT SMOKE-ACCEPTED** until the Milestone 1 checklist runs
(`KERNEL_UNLOCKING_MILESTONE_1.md`). The notification-recv blocking path and
sender-side blocking remain canonical (FALLBACK_GLOBAL_LOCK).

## 6.4 Stage 105 / Pass 2 — D5 reply-cap split status (LIVE for non-shared-region reply arm)

The reply-cap recv materialization Phase A / B / B' / C engine lives in
`src/kernel/cap_transfer_split.rs`, live-wired via
`syscall.rs::materialize_received_message_cap_routed`. Phase B' uses the new
fallible `KernelState::try_set_reply_cap_waiter_cap` and rolls back the mint
on stale-record races. Labels: `D5_LIVE_SPLIT`, `FALLBACK_GLOBAL_LOCK`.
**NOT SMOKE-ACCEPTED** until x86_64 `-smp 1` + optional-FS strict smoke run.

| Path | D5 status (Stage 105) | Owner |
|------|------------------------|-------|
| `FLAG_REPLY_CAP` (non-shared-region) | **live** (`d5_split_reply_materializations`, `d5_split_reply_rollbacks` telemetry) | router → `materialize_split_reply_cap_equivalent` |
| Reply-cap with `OPCODE_SHARED_MEM` | **fallback to global lock** | canonical `materialize_received_message_cap` reply arm |
| Sender-waiter cap-transfer (any flag) | **fallback to global lock** | unchanged |
| Legacy full recv path / NR 30 handler | **unrouted this pass** | canonical helper |

## 6.2 Stage 104 — D1 cap-transfer recv split status (LIVE for transfer arm)

The cap-transfer materialization Phase A / B / C engine lives in
`src/kernel/cap_transfer_split.rs` (Stage 103 scaffold, **Stage 104
live-wired** via `syscall.rs::materialize_received_message_cap_routed` at the
recv-v2 blocked-receiver seam and the queued split-recv seam). Labels:
`D1_LIVE_SPLIT` + `FALLBACK_GLOBAL_LOCK`. **NOT SMOKE-ACCEPTED** until
x86_64 `-smp 1` + optional-FS strict smoke run (no QEMU in dev environment).
Equivalence with the global-lock path is proven by
`stage103_equivalence_split_matches_direct_take_plus_grant` and the
`stage104_router_*` tests (CapId, slot object, slot rights, cap_refcount,
delegation-link count, failure-error parity).

| Path | D1 status (Stage 104) | Owner |
|------|------------------------|-------|
| `FLAG_CAP_TRANSFER` (non-reply, no shared region) | **live** (`d1_split_materializations` telemetry) | router → `materialize_split_transfer_cap_equivalent` |
| `FLAG_CAP_TRANSFER_PLAIN` (non-reply) | **live** | same |
| Plain message (no flag) | **live**, returns `None` | same |
| `FLAG_REPLY_CAP` | **fallback to global lock** (D5: mint→record atomicity window; see audit §13.4) | `materialize_received_message_cap` reply arm |
| Sender-waiter with cap-transfer | **fallback to global lock** | `recv_core.rs` `FallbackReason::SenderWaiterWake` |
| `FLAG_CAP_TRANSFER` with shared region (OPCODE_SHARED_MEM) | **fallback to global lock** | router → canonical path |
| Legacy full recv path / NR 30 handler | **unrouted this pass** — canonical helper called directly | `materialize_received_message_cap` |

Pass 2 items (see audit doc §13.8): smoke-accept Pass 1; D5 reply-cap split
(bool-returning `set_reply_cap_waiter_cap` + mint rollback); D2 blocking-recv
split; optional `syscall/recv_shared_v3.rs` mechanical move.

---

## 6.1 Stage 102 — syscall module split status

The mechanical syscall decomposition (map: `KERNEL_UNLOCKING_STAGE101_AUDIT.md
§3`, progress: §11) began in Stage 102:

| Target module | Status (Stage 102) |
|---------------|--------------------|
| `syscall/debug.rs` | **landed** (NR 15) |
| `syscall/initramfs.rs` | **landed** (NR 27/28) |
| `syscall/recv_shared_v3.rs` | next split target |
| `syscall/sched.rs` | pending (trivial) |
| `syscall/process.rs` | pending (big, mechanical) |
| `syscall/dispatch.rs` | pending (after IPC group) |
| `syscall/ipc.rs` / `syscall/ipc_recv_core.rs` | **frozen until D1 lands** — D1 landing area, do not churn |
| `syscall/mm.rs` | frozen until D3 — D3 landing area |
| `syscall/cap.rs` | pending (tiny; tied to syscall_split.rs tests) |

`src/kernel/syscall.rs` remains the parent module (scripts and
`include_str!` tests reference the path).

---

## 7. Maintenance rule

Any new plan / scaffold type added during kernel-unlocking work MUST be
listed here with a status. If a type sits at **deferred** or **helper-only**
for more than two stages without a live-wire plan, the next maintenance
stage should either live-wire it or remove it. Long-lived helper-only types
become noise and obscure the audit surface.
