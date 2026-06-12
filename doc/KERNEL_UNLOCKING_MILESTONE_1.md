// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

# YARM Kernel Unlocking Milestone 1

**Milestone status: PREPARED — NOT DECLARED.**

The implementation, proofs, and tests for Milestone 1 are complete on branch
`claude/eager-mendel-6oncuk` at Stage 106 (Pass 3), but the declaring smoke
runs could not be executed in the development environment (QEMU
unavailable). Per the MUST_SMOKE policy (`doc/AI_AGENT_RULES.md §13`) and
the Pass 3 directive, **the milestone must not be declared without smoke**.

## Declaration checklist (run in a QEMU-capable environment)

The milestone is DECLARED when all of the following pass on this branch:

1. `QEMU_SMP=1 ./scripts/qemu-x86_64-core-smoke.sh` — core smoke.
2. `QEMU_SMOKE_STRICT=1 ./scripts/qemu-x86_64-optional-fs-smoke.sh` —
   optional-FS strict smoke (x86_64).
3. `QEMU_SMOKE_STRICT=1 ./scripts/qemu-aarch64-optional-fs-smoke.sh` —
   optional-FS strict smoke (AArch64).
4. Expected new markers observed at least once on cap-transfer / reply
   traffic: `YARM_D1_SPLIT_MATERIALIZE`, `YARM_D5_SPLIT_MATERIALIZE`,
   `D2_RECV_WAITER_PUBLISH`.
5. Forbidden markers absent / zero: `INIT_SPAWN_V5_WRONG_SENDER_REPLY`
   (count=0 strict), `KSPAWN_EXTRA_CAP_DELEGATE_FAIL`, `panic` (excluding
   `nonfatal=true`), `D2_PUBLISH_RACE_UNWIND` (must be 0 pre-seam-split),
   `YARM_D5_SPLIT_RECORD_ROLLBACK` (expected 0 on a clean run).
6. All workspace tests green (`cargo test --lib -- --test-threads=1`,
   `yarm-fs-servers`, `yarm-control-plane-servers`).

After a passing run, edit this file: change the status line to
**DECLARED** and record the smoke log digests below.

## Smoke acceptance record

| Run | Result | Date | Notes |
|-----|--------|------|-------|
| x86_64 core (-smp 1) | _pending_ | | |
| x86_64 optional-FS strict | _pending_ | | |
| AArch64 optional-FS strict | _pending_ | | |

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

1. **Smoke-accept this branch** (checklist above) and flip the status line.
2. **SharedKernel split-mut seams**: scheduler block (rank 1), task rank 2
   blocked-state transition, VM (rank 5), memory (rank 6). These convert
   the sequential in-borrow phases into true concurrent lock windows.
3. **D3 live**: VmBrk shrink first, then VmAnonMap, per audit doc §16.2 —
   after the seams + multi-CPU smoke capability.
4. **D6**: per-CPU scheduler locking per the audit (audit doc §20) — only
   after D2/D3 are smoke-stable; requires the x86_64 SMP trampoline split
   first for meaningful SMP smoke.
5. **D4 continuation**: `syscall/recv_shared_v3.rs` then `syscall/process.rs`
   mechanical moves (separate PRs from semantic changes).
6. **Shared-region cap-transfer split** (D1/D5 extension) once receiver-side
   mapping obligations are folded into the phase model.
