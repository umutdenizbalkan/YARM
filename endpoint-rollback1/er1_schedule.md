# ENDPOINT-ROLLBACK1 — predeclared qualification schedule

* **Candidate F** = `b9a29032` on `wip/endpoint-rollback1`, worktree `er1-wt`, clean. It is
  `f540d447` (the repair plus the two re-derived tests) and then `b9a29032` (reproducers E1–E8).
* **Baseline B** = `3148a754`, the canonical dev head (SEND-TIMEOUT-ARB1), worktree `sta1-wt`,
  clean.
* **Runner:** `er1_matrix.sh`. Steps run sequentially, and every step runs whatever earlier steps
  returned.
* **No retries in place:** a rerun is appended, never substituted.
* **Kept** under `er1-evidence/qual`: raw logs, rcs, durations, HEAD, porcelain and every
  `ARTIFACT_BINDING` line.

| id | step | expected |
|---|---|---|
| P0 | provenance | HEAD=F, porcelain 0 |
| P1 | source evidence: the diff stat against B, the removal body, every `irq_routes` mention in the spawn/endpoint transaction files | captured |
| H1 | `cargo fmt --check` | rc 0 |
| H2 | clippy `--lib --features hosted-dev`, then class comparison against B | 427 warnings, identical classes |
| H3 | hosted lib, serial, `CARGO_INCREMENTAL=0` | 0 failed |
| H4 | integration (`--test '*' --no-fail-fast`): includes the broad-lock census and the doc-fragmentation guard | 0 failed |
| H5 | subsets (list below) | 0 failed each |
| H6 | lock-order warnings per (test, pair), F vs B, on the common test set (new tests skipped) | every changed (test, pair) falls in a predeclared class (below); nothing else changes; nothing is nested |
| D1–D3 | default freestanding builds, three ISAs | rc 0 |
| O1–O3 | IPC recv-v2 oracle, three ISAs | rc 0 |
| S1–S6 | CI-exact strict build + `QEMU_SMOKE_STRICT=1` core smoke, three ISAs | rc 0 |
| S7 | CI strict x86 timer grep | inherited debt #6 |
| W1–W3 | IPC-DISPLACE1 live witness SMP1, three ISAs | `witness=ok core_smoke=ok` |
| U1–U3 | UART (x86_64), PL011 (AArch64), UART (RISC-V) IRQ witnesses: live Notification-bound IRQ routes | rc 0 |
| M1 | reply-direct matrix seal | rc 0 |
| M2–M4 | server-dies on x86_64, AArch64, RISC-V | rc 0 |
| C1 / C3 | default x86 rebuild, on F / on B | rc 0 |
| C2 / C4 | CI profiles, on F / on B (profiles below) | see below |
| C5 | reply-timeout matrix and SMP-request script, on F and on B | inherited #1 and #5 on both |
| Z | every QEMU log of F grepped for the marker list below | reported per file; any hit explained |

**H5 subsets:** endpoint_rollback1, u9spawnic1_endpoint_cap_txn, u9spawn1_sp3_spawn_ledger,
u9spawntxn3_provisional_rollback, u9spawnvm2_provision_failure_injection, spawn, fork, endpoint,
generation, capability, exit, reap, send_timeout_arb1, ipc_sender_id1, ipc_displace1, sender_wake,
transfer, shared_region, notif_lifetime1, notif_waiter1, irq_generation1, irq_auth1,
irq_delivery1.

**H6 predeclared classes:**

* (a) the re-derived test `a_published_endpoint_is_refused_by_the_unpublished_removal`, which now
  blocks a real receiver;
* (b) the run-to-run variant `c09_reply_vs_timeout_terminal_cas_race`;
* (c) a test in which B logs `endpoint=Published` and F logs `endpoint=Removed`. The removal is
  rank-3 only on both sides and takes the same single acquisition, so (c) is expected to be empty.

**C2 / C4 profiles:** sender-wake, d2-send, smp2-core, smp4-core, smp2-sender-wake,
smp4-sender-wake, sched-timeout. Expected:

* sender-wake and smp2/smp4-core pass;
* smp2/smp4-sender-wake and sched-timeout fail as inherited debts #2, #3 and #4;
* d2-send shows the inherited observation, identically on F and B.

**Z marker list:**

* `SCHED_WAKE_FAIL`, `SCHED_TIMEOUT_STRANDED_WAITER`, `IPC_RECV_BLOCKED_COMPLETE_FAILED`;
* the ports' fatal channels;
* `U6_SEND_COMPLETION_REFUSED_STALE`, `U7_SEND_TIMEOUT_CLAIM_AFTER_WITHDRAW_REFUSED`,
  `SEND_ARB_PUBLICATION_REFUSED_UNWIND`;
* `endpoint=Published`, `outcome=Published` (a refused rollback), `SPAWN_LEDGER_OVERFLOW`.

**Classification.**

* *Hosted proof* (H*): the namespace separation, the preserved guards, stale-token refusal, slot
  reuse and the real spawn rollback with a commit-phase failure injected at an owner boundary. All
  of it uses real objects and real IRQ bindings.
* *Live regression evidence* (S*, W*, U*, M*, C*): no live boot path triggers a failed spawn, so
  the repaired branch itself is proven hosted only.

**After the matrix:**

* doc commit D on top of F, docs only;
* on D, rerun H1, H3 and H4;
* check that `git diff F..D` touches only `doc/*.md`.
