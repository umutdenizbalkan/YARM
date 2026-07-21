<!-- SPDX-License-Identifier: Apache-2.0 -->
# Stage 199A2D1 — Direct-IPC Race Model, Memory-Ordering Audit, and x86_64 SMP=2 Cross-CPU Status

This stage models the concurrency of the off-lock NR6 (`IpcCall`) / NR7 (`IpcReply`)
direct transactions with **deterministic, forced-interleaving** proofs, audits the
memory ordering of the single-slot acknowledgement and reply-record lifecycle, bounds the
single-slot acknowledgement to one outstanding oracle pair with a fail-closed fuse, and
honestly assesses the x86_64 SMP=2 cross-CPU seal.

No production off-lock code path was changed. The functional 3-architecture SMP=1 matrix
(6/6 live cells) is untouched.

## Part 1 — Deterministic forced-interleaving race model

Hosted module `stage199a2d1_races` (`src/kernel/boot/tests.rs`). Every case forces the
dangerous interleaving with an EXPLICIT mechanism — never by merely removing
`--test-threads=1`. Symmetric contended-primitive races use two real threads aligned by a
`std::sync::Barrier`; asymmetric lifecycle races use bounded step control that interposes
an exit / endpoint replacement between the claim and the commit. Each case asserts EXACT
counts.

| Group | Race | Mechanism | Invariant proven |
|-------|------|-----------|------------------|
| (a) | Ack claim race — 2 threads claim the SAME published ack | Barrier + real threads (BOTH `BlockedServerAck` and `BlockedCallerAck`) | claim successes=1, failures=1, work owners=1, publications=1; loser mutates nothing (repeated 200×) |
| (b) | Reply-record alias race — 2 aliases to the same `(index, generation)` | Barrier + two concurrent full `ipc_reply_direct_txn` with distinct payloads | reservation successes=1, failures=1, payload copies=1 (caller memory holds exactly the winner's bytes), caller wakes=1, final state Consumed |
| (c) | Reply vs caller exit | Bounded step: reserve → caller `mark_task_dead` → commit | commit=GoneDead; stale authority restored=0, caller wakes=0, record left Reserved=0, reply alias usable=0, ack restored=0 |
| (d) | Reply vs endpoint replacement | Bounded step: claim ack → bump endpoint generation + install replacement waiter → stale-gen waiter claim | replacement waiter mutated=0, old waiter restored=0, stale ack restored=0, wake=0 |
| (e) | Request vs server exit | Bounded step: claim server waiter → server `mark_task_dead` → commit | commit=GoneDead; cap minted into replacement CNode=0, reply record leak=0, server wake=0, stale ack restore=0 |
| (f) | Wake/enqueue race — 2 completion attempts for one blocked receiver | Barrier + real threads racing `sr_claim_endpoint_waiter_split` | waiter claims=1, Runnable transitions=1, scheduler enqueues=1, continuations=1, duplicate wakes=0 |

The winner in each contended-primitive race is chosen by a single atomic CAS
(`CLAIMED`) or a single `SpinLock`-guarded FSM transition (`Available → Reserved`,
`take_endpoint_waiter`). The OUTCOME (the counts) is therefore invariant across every real
scheduling — the assertions hold deterministically on every run, not merely usually.

## Part 2 — Memory-ordering audit

Hosted module `stage199a2d1_memory_ordering`. Source guards pin the exact strong orderings
(and fail if any is weakened); functional tests prove the observable contracts.

### Chain 1 — single-slot acknowledgement (cross-CPU publish → claim)
`ipccall_direct_ack` / `ipcreply_direct_ack` write all fields `Relaxed` FIRST, then
`VALID.store(true, Release)` LAST. Readers take `VALID.load(Acquire)` FIRST. Ownership
transfers via `CLAIMED.compare_exchange(false, true, AcqRel, Acquire)`. `restore(seq)`
re-arms (`CLAIMED.store(false, Release)`) only when `SEQ == seq` for the SAME still-`VALID`
publication, and `SEQ` advances monotonically on every fresh `publish`.

```
ack fields init → VALID Release → VALID/CLAIMED Acquire/AcqRel claim → claimant observes the complete ack
```

Proven: no claim observes a partially-initialized ack (all-or-nothing publication); an
older sequence cannot `restore` over a newer publication; only the successful claim owner
proceeds.

### Chain 2 — reply delivery (transaction writes → Consumed → enqueue → resume)
Under the internal `SpinLock<KernelState>`, a delivery copies the reply bytes, THEN
transitions the record `Reserved → Consumed`, THEN — strictly LAST and non-fallibly —
enqueues the caller. Only the reservation OWNER may consume/release; an alias fails closed.

```
reply bytes copied → record Consumed → scheduler enqueue → resumed caller observes reply bytes + a Consumed (non-invokable) record
```

Proven: the caller cannot dispatch before BOTH the reply bytes and the `Consumed` barrier
are visible; the enqueue publishes the completed wake state to whichever CPU dispatches.

## Part 3 — Single-slot acknowledgement boundary

The two ack modules are classified **ORACLE-ONLY / SINGLE-OUTSTANDING-PAIR** proof
infrastructure (module doc in `src/kernel/boot/mod.rs`). One outstanding request+reply pair
is sufficient. On a REAL build each `publish` carries a fail-closed **overwrite fuse**: it
REFUSES to overwrite an active (`VALID && !CLAIMED`) acknowledgement — a
second-simultaneous-pair condition — preserving the active ack and marking the fuse
(`IPCCALL_DIRECT_ACK_OVERWRITE_FUSE` / `IPCREPLY_DIRECT_ACK_OVERWRITE_FUSE`). Endpoint
confinement plus the single provisioning slot already keep the system to one pair; the fuse
is defence-in-depth and never trips in the sealed one-pair flow. Hosted builds keep
last-writer-wins so the wiring-test fixtures (which share the process-global statics) are
unaffected. Guards: `stage199a2d1_single_slot_boundary`.

**HARD-STOP evaluation:** the memory-ordering audit (Part 2) confirms one outstanding pair
is handed across CPUs correctly (Release→Acquire; monotone `SEQ` defeats stale restore), so
no single pair can lose an ack under valid cross-CPU sequencing. The HARD-STOP that would
require replacing the slot with an endpoint-indexed, generation-bearing bounded store
BEFORE QEMU is therefore **not triggered**. Such a store is required only to support
genuine MULTI-pair production concurrency (out of scope here).

## Part 4 — x86_64 SMP=2 cross-CPU seal — HONEST BLOCKER

**A genuine cross-CPU NR6/NR7 round trip cannot be produced with the current
infrastructure, so no SMP seal is emitted.**

The x86 AP user-dispatch scaffold (Stage 189C6 / 190B) — `live_ap_user_dispatch` and
`ap_sched_next_or_idle` in `src/arch/x86_64/smp.rs` — runs only an ISOLATED, hardcoded
per-AP probe workload (a `Yield` + magic-park stub built by `build_ap_workload`) and then
idles. It does NOT host a userspace IPC server BLOCKED on an endpoint SHARED with a BSP
client, and there is no cross-CPU delivery / remote-wake path that resumes a blocked IPC
receiver on a remote AP. Presenting same-CPU execution as a cross-CPU proof is a HARD-STOP.

### What the SMP=2 boot DOES prove (honest data point)
`scripts/qemu-ipccall-reply-direct-x86_64-smp-smoke.sh` performs a fresh feature build and
boots ONE `-smp 2` QEMU. Observed, clean, in one boot:

* both CPUs online (`X86_AP_ONLINE cpu=1`);
* the oracle provisions (`IPCCALL_DIRECT_ORACLE_PROVISION_OK`);
* the off-lock DIRECT round trip completes exactly once on the BSP under SMP=2 —
  `IPCCALL_DIRECT_REQUEST_OK arch=x86_64 …`, `IPCREPLY_DIRECT_OK arch=x86_64 …`,
  `X86_IPCCALL_DIRECT_ROUNDTRIP_DONE … result=ok` — i.e. **SMP=2 introduces no regression**
  in the off-lock NR6/NR7 machinery; and
* the AP runs its isolated probe workload and seals (`X86_AP_USER_DISPATCH_SEAL_DONE`),
  confirming it carries no shared-endpoint IPC.

Because the request/reply run on ONE CPU (the BSP), the strictly-cross-CPU markers
`IPCCALL_DIRECT_SMP_REQUEST_OK` / `IPCREPLY_DIRECT_SMP_REPLY_OK` (with `sender_cpu !=
receiver_cpu`, `replier_cpu != caller_cpu`) are absent. The smoke encodes that exact
acceptance contract and emits:

```
STAGE_199_IPCCALL_REPLY_DIRECT_SMP_SEAL arch=x86_64 smp=2 pairs=1 cross_cpu_request=0 cross_cpu_reply=0 result=blocked reason=ap_cross_cpu_ipc_oracle_not_wired
```

It NEVER emits `result=ok` unless both cross-CPU markers appear with DISTINCT CPU IDs. When
the cross-CPU oracle is wired in a later stage, the same script enforces the full SMP count
contract (transactions/claims/reservations/consumptions/copies/wakes = 1; duplicate
replies / duplicate wakes / wrong-waiter mutations / stale-authority restores / overwrite
fuse = 0) before sealing.

### What a future cross-CPU seal requires
1. A cross-CPU IPC oracle workload: a shared request+reply endpoint provisioned across a
   BSP-hosted client and an AP-hosted server (distinct ASIDs).
2. AP scheduler support for a userspace IPC server that BLOCKS on `recv` and is later
   RESUMED by cross-CPU work (the `submit_cross_cpu_work` / `WakeTask` plumbing exists but
   is not integrated with the AP user-dispatch loop nor with the off-lock NR6/NR7 enqueue).
3. The off-lock NR6/NR7 transactions firing from BOTH CPUs' trap paths, with the
   scheduler enqueue honouring remote-CPU affinity (`sr_enqueue_committed_receiver_split`
   already carries affinity — the missing piece is the AP dispatch of that woken task).

### AArch64 / RISC-V SMP readiness
Blocked earlier in the chain than x86_64. Both keep BSP-only userspace in production and
have no LIVE AP user-dispatch scaffold at all (x86_64's isolated-probe AP path is the only
one). A cross-CPU direct-IPC seal on those architectures additionally requires an AP
ring3-entry/user-dispatch path before any of items 1–3 above. Not attempted this stage; no
AArch64/RISC-V QEMU was run.

## Preserved invariants
SYSCALL_COUNT=32, VARIANT_COUNT=22, NR27 absent, DebugLog cap=192,
REPLY_CAP_QUEUEING_SUPPORTED=false, Stage 198F classes=10 / cells=30, Stage 199 functional
cells=6. Queued `IpcCall` unsupported; timeouts, notifications, and server-death-caller-wake
remain unretired.
