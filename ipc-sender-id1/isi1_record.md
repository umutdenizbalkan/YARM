#### IPC-SENDER-ID1 (from delivered `16e5c04`)

**Audit.** Every writer, remover and consumer of `endpoint_sender_waiters` was traced:

* the broad and split block (insert);
* the broad and split receive take, with refill and direct delivery (consume);
* the three receiver-side wake seams;
* the off-lock send-timeout drain;
* the broad exit and death sweep and the reap/exit claim body;
* the retired broad timeout scan;
* `destroy_endpoint`;
* the split duplicate-waiter refusal;
* the endpoint-removal census check.

**Numeric-TID comparisons.** There are four. None is a defect:

* *Exit/death sweep and reap/exit claim body.* Safe, by proof: a TID names one TCB for the life
  of the system.
  * Registration of an existing TID is a no-op.
  * The dynamic allocator never returns a TID a TCB carries.
  * Only `spawn_reservation::clear_reservation_slot` removes a TCB, never one that ran.
  * So S1 and S6 (TID recycling) are not reachable.
* *Broad timeout scan.* Unreachable by construction.
* *Split duplicate-waiter refusal.* It can only refuse a block.

The `{tid, send_generation}` timeout removal was safe too: the claimed sender is neither queued
nor current until the drain's own enqueue.

**Findings.** The defect was in the receiver-side wake, not the sweeps. All three seams published
the cycle's completion by exact identity, then woke the bare TID unconditionally, and the
publication left the sender `Blocked`. With a send timeout settled on another CPU inside a
receiver's take→wake window:

| Reproducer | Result on `16e5c04` | Defect |
|---|---|---|
| S2 (×3 seams), S6 | fail | the stale wake woke the sender's NEXT blocking send: no completion, deadline disarmed, its waiter stranded |
| S3 timeout after the receiver's publication | fails | the timeout claimed the same cycle and overwrote `OK` with `TimedOut` |
| S3 timeout before the wake (×3 seams), S4 dead sender, S5 shared-region | fail | the broad receive returned `WouldBlock` after taking the message, dropping it and its envelope |
| duplicate claim | fails | a second receiver publication succeeded |
| ASID-less cycle | fails | an ASID-bearing target woke an ASID-less sender |
| arrival order | fails | a timeout or exit removal left a hole, and the next sender overtook older waiters |
| S1, S5 exit-after-take, S5 queued settle | pass | invariants that already held |

The races need two CPUs in kernel at once. Blocking send with a timeout is user ABI (the
`IpcSend` timeout argument), and every CPU's timer scan settles any task's send deadline. The
arrival-order defect is single-CPU.

**Repair.** The contract is in `doc/IPC.md` §8.12; the owner is `sender_waiter.rs`.

* *One rank-2 claim* (`claim_blocked_send_cycle_locked`): the receiver-side publication (broad
  and split) and the send-timeout drain both make it. It parks the completion, makes the sender
  `Runnable` and disarms the deadline in one acquisition.
* *The three seams* wake only a claimed cycle. A refused claim is not an error.
* *Removals:* `remove_sender_waiter_for_cycle_locked` (timeout, the whole triple) and
  `remove_sender_waiters_for_task_locked` (exit, death and reap, by TID, by proof). Both compact.
* *Not added:* no syscall, ABI change, lock, capacity or receiver-arbitration change.

Commits:

* code `9347cb2d`, which also re-derives three guards:
  * the status-writer census moves the timeout's row to the claim (same class, totals
    unchanged);
  * the U3 sender-wake fixture now uses a sender actually blocked in a send cycle (the old one
    was receive-blocked), plus a not-in-cycle case;
  * the U7 ordering test reads the claim;
* regressions `811b4867`.

**Resource accounting.** Each is checked independently, not inferred from return codes:

* one completion and one wake per cycle (S3, the duplicate-claim test, run-queue counts on both
  CPUs);
* no completion written for a newer cycle (S2, S6);
* envelope and pin owned by whoever removed the waiter, settled exactly once (S5 ×3; U6's timeout,
  exit and second-settle tests);
* no stranded sender after a legitimate ending (S2's new cycle intact; S3/S4 woken exactly once
  or not at all);
* no unrelated removal (the owner tests).

**Qualification of `811b4867`.** This followed a predeclared schedule. The steps ran sequentially
and none was retried. Logs, rcs, artifact bindings and hashes are kept.

A first launch did not start: the runner lacked its execute bit, so no step ran. It is recorded,
and the same frozen file was relaunched. P0's `HEAD_IS_F` echo compared a seven-character short
hash with eight; the full HEAD it logs is `811b4867393b`.

| Step | Result |
|---|---|
| fmt; clippy `--features hosted-dev` | clean; 427 warnings, identical classes to `16e5c04` |
| hosted lib (serial); integration (`--no-fail-fast`, broad-lock census and doc-fragmentation guards) | 6391 passed, 0 failed, 3 ignored; 41 binaries, 396 passed |
| subsets: ipc_sender_id1, u3_ordinary_cap_sender_wake, u7_production_timeout_promotion, u6_offlock_blocking_send_commit, u6_frame_exact_envelope_preservation, stage199d_wa2, ipc_displace1, sender_wake, send, timeout, exit, reap, transfer, shared_region, u9f_split, notif_waiter1, irq_delivery1 | 15, 16, 22, 49, 16, 29, 19, 91, 368, 164, 318, 47, 203, 176, 33, 19, 15 passed |
| lock order, common test set | 7 changed (test, pair) entries, all attributed (below) |
| default builds ×3; recv-v2 oracles ×3; CI-exact strict builds + `QEMU_SMOKE_STRICT=1` core smokes ×3 | pass |
| IPC-DISPLACE1 live witness SMP1 ×3 | `witness=ok core_smoke=ok` |
| reply-direct matrix seal; server-dies ×3 | pass |
| CI profiles on clean default bindings, F and B: sender-wake, d2-send, smp2-core, smp4-core, smp2/smp4-sender-wake, sched-timeout | sender-wake and smp2/smp4-core pass on both. sender-wake ×2 and sched-timeout fail with the inherited signatures. d2-send fails identically on F and B (see below) |
| reply-timeout matrix; SMP-request script; CI strict timer grep | inherited #1, #5, #6, identical on F and B |
| every QEMU log of F (70 files): `SCHED_WAKE_FAIL`, `SCHED_TIMEOUT_STRANDED_WAITER`, `U6_SEND_COMPLETION_REFUSED_STALE`, `IPC_RECV_BLOCKED_COMPLETE_FAILED`, the ports' fatal channels | none. Live `U6_SEND_COMPLETION_PUBLISHED` in d2-send and sched-timeout shows the claim path ran |

*`d2-send`* had not run in an earlier stage's matrix. It fails on `16e5c04` and on the candidate
with the same eight missing markers: the in-lock `D2_SEND_GENUINE_*` phases and
`IPC_RECV_PROOF_SENDER_WAKE_BLOCKED_OK`. The proof's sender completes on both
(`SENDER_DONE`, `SEQUENCE_DONE`), so the in-lock markers it expects belong to a route the
production blocking send no longer takes. This is recorded as an inherited profile failure, not
investigated here and not counted among the six debts.

*Lock-order attribution.* Every change is in one of the predeclared classes:

* *(a) re-derived tests:* three `u3_ordinary_cap_sender_wake` tests (`scheduler←task` −6, −1,
  +2).
* *(b) the run-to-run variant:* `c09_reply_vs_timeout_terminal_cas_race`.
* *(c) a refused claim no longer followed by a wake:* two `u9_recv_queue1_parity` tests
  (`scheduler←task` −1 each).
  * They inject an ASID-less waiter for a task that is not blocked. On B the bare wake still
    enqueued it; on F it is not woken.
  * The predeclared mechanical list for (c) keyed on `U6_SEND_COMPLETION_REFUSED_STALE`, which B
    does not log for an ASID-less target. These two were therefore attributed by reading the
    tests, which is disclosed.

Nothing is nested.

**Hosted negative controls.** Each removes one protection, in a disposable worktree, and all 13
were detected. The unmodified candidate passes all 99 tests in the subset. On the baseline tree
(`16e5c04` plus only the reproducers that use production functions), 9 of 12 fail.

| Control | Protection removed | Detected by |
|---|---|---|
| NC1 | the receiver claim leaves the sender `Blocked` (but still disarms the deadline) | the duplicate-claim test |
| NC1r | the baseline shape exactly: the receiver claim parks the completion and writes nothing else | the duplicate-claim test, S3 (timeout after the claim) |
| NC2 | the broad seam wakes whatever the claim said | S2, S3, S4, S6, the ASID-less test, S5 |
| NC3 | the in-lock split seam does | S2, S3, S4, S6 and 3 more |
| NC4 | the off-lock seam does | S2, S3, S4, S6, U3 not-in-cycle and 3 more |
| NC5 | the claim ignores the generation | S2, S6, the claim owner test, U3 not-in-cycle and 1 more |
| NC6 | the claim matches the TID alone | the ASID-less test, the claim owner test |
| NC7 | the timeout claim ignores the armed deadline | the claim owner test |
| NC8 | the cycle removal matches the TID alone | the cycle-removal owner test |
| NC9 | the cycle removal leaves a hole | arrival order, the cycle-removal owner test |
| NC10 | the task removal leaves holes | the task-removal owner test |
| NC11 | the task removal drops cap-bearing waiters unreported | the task-removal owner test, U6 sender-exit settle |
| NC12 | the timeout claims but never removes the waiter | the queued-envelope settle test, U6 timeout settle |

NC1r was added after the frozen controls ran, because NC1 also disarmed the deadline. It is kept
alongside NC1, in its own script with a recorded hash.

**Limits.**

* The races are proven at the rank-2/rank-3 linearization points with hosted, serial two-CPU
  schedules of the production phases. No live witness drives a timeout inside another CPU's
  receive window.
* Still open (`doc/KERNEL_UNLOCK_DEFERRED_DEFECTS.md`):
  * a timeout that claims after a receiver took the waiter still reports `TimedOut` for a
    delivered message (proposed SEND-TIMEOUT-ARB1; it no longer wakes, publishes or settles
    twice);
  * `destroy_endpoint` does not wake parked senders (oracle rollback only).
* The six baseline debts and the IRQ-AUTH1 endpoint-removal observation remain as recorded. The
  NOTIF-WAITER1 sender-waiter observation is closed as verified.

Verdict: **IPC_SENDER_ID1_ACCEPTED_WITH_BASELINE_DEBT**. Refs: `claude/yarm-kernel-unlock-u0-f2zh8p`
and `wip/ipc-sender-id1` at the commit that adds this record; `main` `b81e0317` and every other ref
unchanged.
