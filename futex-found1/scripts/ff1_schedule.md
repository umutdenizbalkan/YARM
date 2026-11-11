# FUTEX-FOUND1 — predeclared qualification schedule

* **Candidate F** = the frozen head of `wip/futex-found1`, worktree `ff1-wt`, clean: the keyed
  futex foundation and its re-derived tests (`c3ba41b1`), the reproducers F1–F15 (`a2502d0f`),
  and the strengthened F8 (`5db787b8`, or a later commit named in the run's P0 line).
* **Baseline B** = `a7a27fb4`, the canonical dev head (ENDPOINT-ROLLBACK1), worktree `er1-wt`,
  clean.
* **Runner:** `ff1_matrix.sh`. Steps run sequentially, and every step runs whatever earlier steps
  returned.
* **No retries in place:** a rerun is appended, never substituted.
* **Kept** under `ff1-evidence/qual`: raw logs, rcs, durations, HEAD, porcelain and every
  `ARTIFACT_BINDING` line. Build outputs (ELF/BIN/images) are not committed to the evidence branch.

| id | step | expected |
|---|---|---|
| P0 | provenance | HEAD=F, porcelain 0 |
| P1 | source evidence: the diff stat against B; no address-only futex predicate and no `observed` in the futex owners; every user-side futex call carries a scope | captured |
| H1 | `cargo fmt --check` | rc 0 |
| H2 | clippy `--lib --features hosted-dev`, then class comparison against B | identical class set (counts may only fall) |
| H3 | hosted lib, serial, `CARGO_INCREMENTAL=0` | 0 failed |
| H4 | integration (`--test '*' --no-fail-fast`): includes the broad-lock census (field authority) and the doc-fragmentation guard | 0 failed |
| H5 | subsets (list below) | 0 failed each |
| H6 | lock-order warnings per (test, pair), F vs B, on the common test set (`futex_found1` skipped) | every changed (test, pair) falls in a predeclared class (below); nothing nests |
| D1–D3 | default freestanding builds, three ISAs | rc 0 |
| O1–O3 | IPC recv-v2 oracle, three ISAs | rc 0 |
| S1–S6 | CI-exact strict build + `QEMU_SMOKE_STRICT=1` core smoke, three ISAs | rc 0 |
| S7 | CI strict x86 timer grep | inherited debt #6 |
| W1–W3 | IPC-DISPLACE1 live witness SMP1, three ISAs (sender-waiter) | `witness=ok core_smoke=ok` |
| FX1 | first-cohort retirement seal: FutexWake, FutexWait switch and FutexWait idle oracles on all three ISAs, plus core/SMP/crash-restart | `FIRST_COHORT_CROSS_ARCH_SEAL ... result=ok` |
| FX2–FX3 | overtaken witness, x86_64 and AArch64: W pinned to CPU 0 waits, K pinned to CPU 1 wakes, two processes on one MemoryObject — the live, default-off CROSS-CPU, CROSS-PROCESS (shared key) futex test, CPU placement graded by `grade-overtaken-witness.py` | `result=ok` |
| FX4 | AP-scheduling witness SMP=2, three ISAs (AP FutexWait/FutexWake) | rc 0 |
| FX5 | KTHREAD1 witness SMP=2, three ISAs | rc 0 |
| FB1–FB3 | FX1–FX3 on B, for comparison | as on F |
| U1–U3 | UART (x86_64), PL011 (AArch64), UART (RISC-V) IRQ witnesses | rc 0 |
| M1 | reply-direct matrix seal (direct IPC) | rc 0 |
| M2–M4 | server-dies on x86_64, AArch64, RISC-V | rc 0 |
| C1 / C3 | default x86 rebuild, on F / on B | rc 0 |
| C2 / C4 | CI profiles, on F / on B (profiles below) | see below |
| C5 | reply-timeout matrix and SMP-request script, on F and on B | inherited #1 and #5 on both |
| Z | every QEMU log of F grepped for the marker list below | reported per file; any hit explained |

**H5 subsets:** futex_found1, futex, robust, stage191b, stage191d, u9qa, stage199d, overtaken,
apsched, kthread, fork, vm_, scheduler, sched_, exit, reap, spawn, thread, task_lifecycle,
generation, shared_region, ipc_displace1, sender_wake.

**H6 predeclared classes:**

* (a) a test that issues a futex wait, wake or robust exit: its futex key and word are now
  resolved under vm (rank 5) then memory (rank 6) and its registration and claim run under the
  task lock (rank 2), so its lock sequence legitimately changes;
* (b) the run-to-run variant `c09_reply_vs_timeout_terminal_cas_race`;
* (c) nothing else. No futex owner holds two locks out of rank order: the vm→memory resolution is
  ascending and is released before any task-lock acquisition.

**C2 / C4 profiles:** sender-wake, d2-send, smp2-core, smp4-core, smp2-sender-wake,
smp4-sender-wake, sched-timeout. Expected:

* sender-wake and smp2/smp4-core pass;
* smp2/smp4-sender-wake and sched-timeout fail as inherited debts #2, #3 and #4;
* d2-send shows the inherited observation, identically on F and B.

**Z marker list:**

* `SCHED_WAKE_FAIL`, `SCHED_TIMEOUT_STRANDED_WAITER`, `IPC_RECV_BLOCKED_COMPLETE_FAILED`;
* the ports' fatal channels;
* the send-arbitration refusals and `endpoint=Published` / `outcome=Published`,
  `SPAWN_LEDGER_OVERFLOW`;
* the new futex refusals: `FUTEX_WAKE_ENQUEUE_DEFERRED`, `FUTEX_WAIT_PARK_REFUSED`,
  `reason=capacity_exhausted`, and any overtaken-witness `result=fail`.

`FUTEX_WAIT_PARK_REFUSED reason=value_changed` and `FUTEX_WAKE_ENQUEUE_DEFERRED` are legitimate
production interleavings (a changed word; a wake landing in the publication window), so a hit is
explained, not a failure, unless it accompanies a stranded or lost task.
