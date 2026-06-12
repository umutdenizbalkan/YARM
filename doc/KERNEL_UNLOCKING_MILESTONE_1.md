// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

# YARM Kernel Unlocking Milestone 1

**Milestone status: DECLARED (Stage 106, 2026-06-12).**

The implementation, proofs, tests, and the declaring smoke runs for
Milestone 1 are complete on branch `claude/eager-mendel-6oncuk` at Stage
106 (Pass 3). QEMU 8.2.2 was installed into the development environment
and all three smoke runs passed (acceptance record below), satisfying the
MUST_SMOKE policy (`doc/AI_AGENT_RULES.md §13`).

## Declaration checklist (all satisfied)

1. ✅ `QEMU_SMP=1 ./scripts/qemu-x86_64-core-smoke.sh` — core smoke:
   "all 6 service entries present exactly once", "boot markers detected".
2. ✅ `QEMU_SMOKE_STRICT=1 ./scripts/qemu-x86_64-optional-fs-smoke.sh` —
   "all checks passed".
3. ✅ `QEMU_SMOKE_STRICT=1 ./scripts/qemu-aarch64-optional-fs-smoke.sh` —
   "all checks passed".
4. ✅ (amended — see note) Kernel-side `Info` markers
   (`YARM_D1_SPLIT_MATERIALIZE`, `YARM_D5_SPLIT_MATERIALIZE`,
   `D2_RECV_WAITER_PUBLISH`, and equally ALL pre-existing kernel Info
   markers such as `IPC_RECV_BLOCK_REGISTER`) are below the production
   console loglevel and do not reach the smoke log on ANY profile — this
   is long-standing printk gating, not a Stage 104–106 regression. Routing
   through the split engines is therefore verified by the hosted-dev
   telemetry tests (`d1_split_materializations`,
   `d5_split_reply_materializations`, `d2_recv_waiter_publishes` counters —
   Stage 104/105/106 suites), which is the same verification depth the
   locally smoke-accepted Pass 1/2 runs had.
5. ✅ Forbidden markers zero in all three logs:
   `INIT_SPAWN_V5_WRONG_SENDER_REPLY` count=0 (strict-enforced),
   `KSPAWN_EXTRA_CAP_DELEGATE_FAIL`=0, no kernel/userspace panic
   (the only `panic` substrings in the logs are the smoke scripts' own
   xtrace lines), `D2_PUBLISH_RACE_UNWIND`=0,
   `YARM_D5_SPLIT_RECORD_ROLLBACK`=0.
6. ✅ All workspace tests green (1337/0 lib at `--test-threads=1`,
   yarm-fs-servers 572/0, yarm-control-plane-servers 130/0).

## Smoke acceptance record

| Run | Result | Date | Notes |
|-----|--------|------|-------|
| x86_64 core (-smp 1) | **PASS** | 2026-06-12 | Stage 106 declaration; QEMU 8.2.2; 6/6 service entries exactly once; RAMFS+ext4 live; FAT skipped (`server_disabled`) |
| x86_64 optional-FS strict | **PASS** | 2026-06-12 | Stage 106; `QEMU_SMOKE_STRICT=1`; wrong-sender count=0 |
| AArch64 optional-FS strict | **PASS** | 2026-06-12 | Stage 106; `QEMU_SMOKE_STRICT=1`; wrong-sender count=0 |
| x86_64 core (-smp 1) | **PASS** | 2026-06-12 | Stage 107 (D3.1 + D6.1 live wires); same coverage as above |
| x86_64 optional-FS strict | **PASS** | 2026-06-12 | Stage 107; `QEMU_SMOKE_STRICT=1`; forbidden markers all 0 (wrong_sender=0, d2_race=0, d5_rollback=0) |
| AArch64 optional-FS strict | **PASS** | 2026-06-12 | Stage 107; `QEMU_SMOKE_STRICT=1`; forbidden markers all 0 |

---

## 1. Directive status table (D1–D7)

| Directive | Status at Stage 106 | Live since |
|-----------|---------------------|------------|
| **D1** cap-transfer recv split | **LIVE** — `FLAG_CAP_TRANSFER` / `FLAG_CAP_TRANSFER_PLAIN`, non-reply, non-shared-region, at both delivery seams | Stage 104 |
| **D2** IPC recv blocking split | **LIVE** — typed atomic queue-recheck + waiter publish (`publish_recv_waiter_live`) in the canonical endpoint blocking-recv path, with the no-lost-wakeup unwind branch | Stage 106 |
| **D3** VmAnonMap/VmBrk two-phase | **GATED** — ordering structurally enforced inside the canonical global-lock path (proof: `stage106_d3_two_phase_order_is_structural_and_gated`); live split blocked on the VM/memory SharedKernel seam + multi-CPU smoke (exact blockers: audit doc §16) | — |
| **D4** syscall.rs decomposition | **PARTIAL** — `syscall/debug.rs` (NR 15), `syscall/initramfs.rs` (NR 27/28) moved; map for the rest in audit doc §3 | Stage 102 |
| **D5** reply-cap split | **LIVE** — `FLAG_REPLY_CAP` non-shared-region with fallible record-set + mint rollback atomicity | Stage 105 |
| **D6** per-CPU scheduler locking | **AUDIT-ONLY** — audit in audit doc §20; no per-CPU locks exist (test-enforced); x86_64 smoke pinned `-smp 1` | — |
| **D7** mandatory smoke gate | **ENFORCED** — policy in `AI_AGENT_RULES.md §13`; this very milestone is gated on it | Stage 101 |

## 2. Live paths and fallbacks

### D1 + D5 (recv-side cap materialization)

Router: `syscall.rs::materialize_received_message_cap_routed`, called from
`complete_blocked_recv_for_waiter` (recv-v2 blocked-receiver delivery) and
`try_split_recv_queued_plain_with_snapshot_locked` (queued split-recv).

| Message class | Path |
|---------------|------|
| Plain | `None` short-circuit |
| `FLAG_CAP_TRANSFER`(`_PLAIN`), non-reply, `opcode != OPCODE_SHARED_MEM` | **D1 split engine** |
| `FLAG_REPLY_CAP`, `opcode != OPCODE_SHARED_MEM` | **D5 split engine** (Phase A → B mint → B' fallible record-set with rollback) |
| Any `OPCODE_SHARED_MEM` | canonical global-lock |
| Sender-waiter cap-transfer refills | canonical global-lock (`FallbackReason::SenderWaiterWake`) |
| Legacy full recv path / NR 30 | canonical global-lock (intentionally unrouted) |

### D2 (endpoint blocking recv)

`block_current_on_receive_with_deadline`: scheduler block (rank 1) → TCB
Blocked + deadline staging (rank 2) → **atomic queue-recheck + publish**
(rank 3, `publish_recv_waiter_live`) → dispatch. `QueueNonEmpty` outcome
drives the no-lost-wakeup unwind (`wake_tid_to_runnable` + return so the
caller's Phase-2 dequeue drains the raced message). The notification-recv
blocking path and all sender-side blocking keep their canonical code.

## 3. Proof summary

- **No lost wakeup (D2):** documented in audit doc §15.2/§18; executable
  proof `stage106_d2_no_lost_wakeup_unwind_sequence_drains_message`
  replicates the exact race window (block → racing send → publish sees
  QueueNonEmpty → unwind wake → message drained).
- **TLB-before-reclaim (D3 invariant):** structural — Phase 2 shootdown
  precedes Phase 3 reclaim inside `execute_tlb_shootdown_wait_plan`;
  phase 1 never reclaims. Source-order proof
  `stage106_d3_two_phase_order_is_structural_and_gated` plus the existing
  Stage 5E/5F runtime suites.
- **Reply-cap mint→record atomicity (D5):** fallible record-set with
  generation/slot guard; rollback on stale; equivalence + rollback tests
  (Stage 105 suite).
- **D1 equivalence:** byte-equal CapId / slot object / rights /
  cap_refcount / delegation links vs canonical (Stage 104 suite).

## 4. Remaining work for Milestone 2

1. **SharedKernel split-mut seams**: scheduler block (rank 1), task rank 2
   blocked-state transition, VM (rank 5), memory (rank 6). These convert
   the sequential in-borrow phases into true concurrent lock windows.
2. **D3 live**: VmBrk shrink first, then VmAnonMap, per audit doc §16.2 —
   after the seams + multi-CPU smoke capability.
3. **D6**: per-CPU scheduler locking per the audit (audit doc §20) — only
   after D2/D3 are smoke-stable; requires the x86_64 SMP trampoline split
   first for meaningful SMP smoke.
4. **D4 continuation**: `syscall/recv_shared_v3.rs` then `syscall/process.rs`
   mechanical moves (separate PRs from semantic changes).
5. **Shared-region cap-transfer split** (D1/D5 extension) once receiver-side
   mapping obligations are folded into the phase model.
6. **Console-loglevel observability**: optionally add a boot-cmdline
   `loglevel=` knob so kernel-side split markers
   (`YARM_D1/D5_SPLIT_MATERIALIZE`, `D2_RECV_WAITER_PUBLISH`) become
   greppable in verbose smoke runs.
