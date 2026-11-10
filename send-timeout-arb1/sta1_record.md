#### SEND-TIMEOUT-ARB1 (from delivered `414afb86`)

**Defects.** IPC-SENDER-ID1 left one ordering between a blocking send's delivery and its timeout:

* the timeout claimed the cycle at rank 2 and only then removed its waiter at rank 3;
* a receiver does the opposite: it takes the waiter (and the message) at rank 3, then claims at
  rank 2.

The audit of every remover of a sender waiter found two more owners of a parked message that
could lose it. The four defects:

* *A1, take→claim.* A timeout on another CPU between a receiver's take and its claim ended the
  cycle `TimedOut`, although the receiver delivered the message. A sender that retries on
  `TimedOut` sends it twice. This holds on every receiver seam and on a buffered refill.
* *A2, Phase B→C.* A timeout between the broad block's `Blocked` write and its waiter's
  publication ended the cycle, then the waiter was published anyway. That left a deliverable
  orphan of a `Runnable` sender.
* *A3a, in-lock envelope.* The in-lock blocking route returned `Err(WouldBlock)` after parking.
  The `IpcSend` syscall's error path then released the transfer envelope the parked waiter still
  named. On SMP this is the production route, because the off-lock publication is refused when
  more than one CPU dispatches.
* *A3b, a refused Phase C.* A full waiter queue returned with the sender `Blocked(EndpointSend)`
  and no waiter.

**Audit of the remaining paths** (A3). Every one was traced; none needed a change beyond the four
above:

* *Refill and direct delivery.* These are receiver takes, covered by the contract (R4).
* *Broad and split receive.* They take at rank 3 like the in-lock take (R1 ×3 seams).
* *Capability-bearing messages and pins.* The remover settles them once (R5, R5b, R9).
* *Endpoint destruction.* It removes waiters, so under the contract it owns their cycles. This
  is why the IPC-SENDER-ID1 observation that it does not wake senders is inseparable, and it is
  resolved here.
* *Exit.* Death ends a cycle without a claim (R8, R9).
* *Simultaneous receivers.* Only one take finds the waiter, because the take is at rank 3.
* *Multiple collectors.* Duplicate timeout work for one cycle finds one waiter, at most once (R7,
  `the_cycle_removal_matches_the_whole_triple`).

**Contract** (`doc/IPC.md` §8.13). The waiter is the arbitration token. Its exact rank-3 removal
is the cycle's irrevocable point, and the remover owns the ending and the message's obligations.
The timeout drain:

1. checks the deadline is due;
2. prechecks read-only at rank 2;
3. withdraws the exact waiter at rank 3;
4. claims `TimedOut` at rank 2, uncontested;
5. settles the envelope;
6. wakes, only if it claimed.

Finding no waiter, the drain changes nothing and leaves the deadline armed.

* *Q1, a failed claim after the withdrawal.* This is possible only if the sender left the cycle
  between steps 3 and 4. Only a death does that, and a death ends the cycle without a claim. The
  drain still settles the envelope and wakes nobody (R9).
* *Q2, the pre-publication window.* No actor can end a cycle that has no waiter, so a timeout in
  Phase B→C refuses. The waiter is published for a live cycle, and a later timer pass ends it
  (R3).

No sender-transaction redesign was needed. The repair reorders one drain and makes the other
three removers end what they remove. It adds no lock, syscall, capability type, table or
capacity, and no IPC serialization.

**Linearization.** Each row is a two-CPU interleaving run as the production phases.

| Interleaving | Linearization point | Outcome | Test |
|---|---|---|---|
| take, timeout, claim (×3 seams) | the receiver's rank-3 take | `OK`; the timeout refuses `NO_WAITER`; one wake | R1 |
| refill, timeout, claim | the refill's rank-3 take | `OK`; the message is in the buffer | R4 |
| drain withdraws, receiver tries, drain claims | the drain's rank-3 withdrawal | `TimedOut`; the receiver gets nothing | R2 |
| block (B), timeout, publish (C) | none in the window (no waiter) | the timeout refuses; the waiter is published; a later timeout ends the cycle once | R3 |
| `now = deadline − 1`; `= deadline`, receiver first; `= deadline`, timeout first | the first rank-3 removal | refuse; `OK`; `TimedOut` | R6 |
| stale work of cycle 1 after cycle 2 blocks | the precheck (generation) | refused with no mutation; cycle 2 intact | R7 |
| take, death, timeout and claim | the take; death ends the cycle | no publication, no wake, settled once | R8 |
| withdraw, death, claim | the withdrawal | the claim is refused; envelope cancelled once; no wake; no pin left | R9 |
| block, queue full | the refused publication | the blocker ends its own cycle with the refusal's error | refused-publication test |
| park, `destroy_endpoint` | destruction's rank-3 removal | settled, then the cycle ends with `WrongObject`; one wake | destruction test |
| in-lock capability park | the waiter's publication | the envelope stays live while parked | R5 |

**Resource accounting.** Each is checked directly, not inferred from return codes:

* *Results.* There is one completion per cycle and it agrees with the message: what the receiver
  got, whether a waiter is left, and whether a later take finds anything (R1–R4, R6).
* *Wakes.* Exactly one wake per ended cycle, and none for a refused claim or a dead sender. These
  are run-queue counts on both CPUs (R1, R3, R8, R9).
* *Envelopes and pins.* They are owned by the remover and settled exactly once. The receiver's
  envelope stays live after a late timeout, and a cancelled one leaves no pin (R5, R5b, R9, and
  U6's settle tests).
* *Orphans.* No waiter of a task not blocked in a send exists after any schedule (R3,
  `orphan_waiters`).
* *Deadlines.* A refused timeout leaves the deadline armed (R3, R6).

**Baseline reproduction.** The baseline tree is `414afb86` plus only the two test hooks and the
new tests; the hooks call no production change. On it, 10 of 12 fail:

* R1, R4, R6 and R5b report `TimedOut` (9) for a delivered message;
* in R2 a receiver obtains a message whose cycle ended `TimedOut`;
* R3 leaves an orphan waiter;
* in R5 the envelope is dead while its waiter is parked;
* the refused publication leaves its sender stranded `Blocked`;
* destruction leaves its sender `Blocked`;
* in R9 the baseline publishes into a dead task.

R7 and R8 pass on both; they guard invariants that already held.

**Code.** Commits:

* `2ad2145f`: the code, plus re-derivations of eight existing tests whose expectations encoded
  the old order:
  * ISI1 S2, S3b and S6, and the claim owner test;
  * U6 `a_reply_cap_message_is_never_published` (now `Ok(BlockedInLock)`);
  * U6's two endpoint-destruction tests (now a `WrongObject` completion);
  * the stage169 source-window guard, from 4600 to 7600 bytes;
  * U7 `the_settle_never_nests_a_domain_claim` (three claims).
* `6c178c91`: the reproducers and regressions.
* `d3bc11a0`: R9.

**Qualification of `d3bc11a0`.** This followed a predeclared schedule (`sta1_schedule.md`, frozen
with the runner and scripts by hash). The steps ran sequentially and nothing was substituted. Logs,
rcs, artifact bindings and hashes are kept. The baseline is `414afb86`.

| Step | Result |
|---|---|
| fmt; clippy `--features hosted-dev` | clean; 427 warnings, identical classes to `414afb86` |
| hosted lib (serial); integration (`--no-fail-fast`) | 6403 passed, 0 failed, 3 ignored; 41 binaries, 396 passed |
| subsets: send_timeout_arb1, ipc_sender_id1, u3_ordinary_cap_sender_wake, u7_production_timeout_promotion, u6_offlock_blocking_send_commit, u6_frame_exact_envelope_preservation, stage169, stage199d_wa2, ipc_displace1, sender_wake, send, timeout, exit, reap, transfer, shared_region, u9f_split, notif_waiter1, notif_lifetime1, irq_auth1, irq_delivery1 | 12, 25, 16, 22, 51, 16, 23, 29, 19, 91, 380, 176, 318, 47, 204, 178, 33, 19, 16, 15, 15 passed, 0 failed |
| lock order, common test set | 17 changed (test, pair) entries over 8 tests, all attributed (below); pair kinds identical |
| default builds ×3; recv-v2 oracles ×3; CI-exact strict builds + `QEMU_SMOKE_STRICT=1` core smokes ×3 | pass |
| IPC-DISPLACE1 live witness SMP1 ×3 | `witness=ok core_smoke=ok` |
| reply-direct matrix seal; server-dies ×3 | pass |
| CI profiles on clean default bindings, F and B: sender-wake, d2-send, smp2-core, smp4-core, smp2/smp4-sender-wake, sched-timeout | sender-wake and smp4-core pass on both. smp2-core passes on B and failed once on F (below). smp2/smp4-sender-wake and sched-timeout fail with identical error sets on F and B (inherited #2, #3, #4). d2-send fails with the same 8 errors on F and B |
| reply-timeout matrix; SMP-request script; CI strict timer grep | inherited #1 (`class=IpcReplyTimeout`), #5 (line 67), #6 (0/0/0), identical on F and B |
| every QEMU log of F (70 files, plus 12 rerun files) for the marker list below | no hits |

The marker list:

* `SCHED_WAKE_FAIL`, `SCHED_TIMEOUT_STRANDED_WAITER`, `IPC_RECV_BLOCKED_COMPLETE_FAILED`;
* `U6_SEND_COMPLETION_REFUSED_STALE`, `U7_SEND_TIMEOUT_CLAIM_AFTER_WITHDRAW_REFUSED`,
  `SEND_ARB_PUBLICATION_REFUSED_UNWIND`;
* the ports' fatal channels.

Live `U6_SEND_COMPLETION_PUBLISHED` lines show the claim path ran.

*The smp2-core retry.* F's C2 run of smp2-core failed on `X86_AP_SCHED_ADMIT_BEGIN`, a marker
check of the AP's bring-up. The marker was printed but garbled by the two CPUs writing to the
serial port at once: `X86_AP_SCHED_ADTMlIT_BEGIN`, with the BSP's characters spliced in. This
happens before the scheduler admits the AP and before any IPC, so the change cannot reach it. The
failed run is kept.

Appended after the matrix (`sta1_rerun_smp2core.sh`, hash recorded):

* the default x86_64 rebuild reproduced C2's kernel binding exactly (`592b34b0…`);
* smp2-core ran three more times on it, and all three passed.

*`d2-send`* is the inherited IPC-SENDER-ID1 observation. It shows the same eight missing in-lock
markers on both trees. It is not counted among the six debts.

*Lock-order attribution.* Every change is in one of the predeclared classes:

* *(a) re-derived tests:* ISI1 S2, S3 and S6 (`scheduler←task` +2, +2, +4; `scheduler←ipc` +2,
  +4; `task←ipc` −2, −4), and U6's two endpoint-destruction tests (`scheduler←task` +1,
  `task←ipc` +1 each: the destruction's added claim and wake). These two log no destruction
  marker, so they were attributed by name, as predeclared, not by the mechanical list.
* *(b) the run-to-run variant:* `c09_reply_vs_timeout_terminal_cas_race`.
* *(c) a send-timeout drain test:* the ISI1 envelope test `a_timeout_after_the_take_leaves_the_receivers_envelope_and_pin`
  (`scheduler←task` +1).
* *(d) a removal now ends its cycle:* `stage4r_no_orphaned_sender_waiter_when_queue_full`, through
  the refused-publication unwind's claim, wake and dispatch.

Nothing is nested.

**Hosted negative controls.** Each control reintroduces one defect in a disposable worktree, and
all 12 were detected. The unmodified candidate passes all 134 tests in the subset (`sta1_nc.py`,
hash recorded).

| Control | Defect reintroduced | Detected by |
|---|---|---|
| NC1 | the timeout claims before it withdraws | R1, R2, R3, R4, R6, R5b, R9, ISI1 S2, S3 |
| NC2 | a missing waiter is treated as timeout-eligible (the Phase B→C protection) | R1, R3, R4, R6, R5b, ISI1 S2, S3 |
| NC3 | the timeout's withdrawal leaves the waiter queued (the receiver can deliver after the timeout won) | R2, R3, R6, arrival order, cycle removal, U6 late receiver, U6 timeout settle and 3 U7 tests |
| NC4 | the receiver's take leaves the waiter queued (the timeout can win after delivery) | R1, R4, R6, R5b, ISI1 S2, S3, arrival order, two ISI1 envelope tests |
| NC5 | the newer-generation check removed from the precheck and the withdrawal | R7, cycle removal, U7 stale cycle |
| NC6 | the timeout's envelope settlement removed | the queued-envelope settle test, U6 timeout settle |
| NC7 | a duplicated receiver-side wake | 12 tests, including R1, R4, R6, R7, R5b |
| NC8 | a hole left after the timeout's removal | arrival order, cycle removal |
| NC9 | a forced claim failure after the withdrawal, on a live sender | R2, R3 |
| NC10 | a refused publication no longer ends its cycle | the refused-publication test |
| NC11 | destruction no longer ends the cycles it removes | the destruction test, two U6 destruction tests |
| NC12 | `IpcSend` releases a parked send's envelope again | R5 |

**Limits.**

* *Hosted proof only.* The arbitration is proven at its rank-3 linearization point with hosted,
  serial two-CPU schedules of the production phases. Test-only phase hooks enter the two windows
  that no production function returns across.
* *No live witness.* No live witness places a timeout inside another CPU's take→claim or Phase
  B→C window: that would need a new synchronization point in production. The live runs are
  regression evidence for the blocking-send, sender-wake, timeout, direct-IPC and teardown paths
  on three ports and under SMP.
* *Still open.* The six baseline debts, the inherited `d2-send` profile failure and the IRQ-AUTH1
  endpoint-removal index-space observation remain as recorded.
* *Closed.* The two IPC-SENDER-ID1 entries (SEND-TIMEOUT-ARB1 and `destroy_endpoint`) are now
  resolved.

Verdict: **SEND_TIMEOUT_ARB1_ACCEPTED_WITH_BASELINE_DEBT**. Refs: `claude/yarm-kernel-unlock-u0-f2zh8p` and `wip/send-timeout-arb1` at
the commit that adds this record; `main` `b81e0317` and every other ref unchanged.
