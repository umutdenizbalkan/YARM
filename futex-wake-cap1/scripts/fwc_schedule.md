# FUTEX-WAKE-CAP1 — predeclared qualification schedule

* **Candidate F** = the frozen head of `wip/futex-wake-cap1`, worktree `fwc-wt`, clean: the
  reserved-placement wake (`d07f2b13`) and its reproducers (`84ed5cd4`), or a later commit named
  in the run's P0 line.
* **Baseline B** = `7522b866`, the canonical dev head (FUTEX-FOUND1), worktree `ff1-wt`, clean.
* **Runner:** `fwc_matrix.sh`, sequential; every step runs whatever earlier steps returned; no
  retries in place (a rerun is appended, never substituted). Logs, rcs, durations, HEAD,
  porcelain and every `ARTIFACT_BINDING` line kept under `fwc-evidence/qual`; build outputs are
  not committed.

| id | step | expected |
|---|---|---|
| P0 | provenance | HEAD=F, porcelain 0 |
| P1 | source: diff stat against B; no batch claim and no enqueue-deferral markers left in the wake owners | captured |
| H1 | `cargo fmt --check` | rc 0 |
| H2 / H2b | clippy `--lib --features hosted-dev`; class and count comparison against B | identical (424) |
| H3 | hosted lib, serial | 0 failed |
| H4 | integration: broad-lock census, doc-fragmentation guard, SCHED-LOCK1 grader | 0 failed |
| H5 | subsets: futex_wake_cap1, futex_found1, futex, robust, scheduler, sched_, enqueue, ap_sched1, stage199d, stage191b, stage191d, u9qa, kthread, exit, reap, spawn, thread, fork, ipc_displace1, sender_wake | 0 failed each |
| H6 | lock order per (test, pair), F vs B, common test set (`futex_wake_cap1` skipped) | every change in a predeclared class (below); nothing nests |
| D1–D3 | default builds ×3 | rc 0 |
| O1–O3 | IPC recv-v2 oracles ×3 | rc 0 |
| S1–S6 | CI-exact strict builds + strict core smokes ×3 | rc 0 |
| FX1 | first-cohort futex seal on those CI-exact artifacts (FutexWake, FutexWait switch and idle, Yield, ×3 ISAs) | 12/12 live cells |
| S7 | CI strict timer grep | inherited debt #6 |
| W1–W3 | IPC-DISPLACE1 SMP1 ×3 (sender-waiter) | `witness=ok core_smoke=ok` |
| FX2–FX3 | overtaken witness x86_64, AArch64 (cross-CPU, cross-process shared-key futex wake) | `result=ok` |
| FX4 | AP-scheduling witness SMP2 ×3 | `result=ok` |
| FX5 | KTHREAD1 witness SMP2 ×3 | `result=pass` |
| FX6 | SCHED-LOCK1 witness SMP2 ×3: no lost or duplicated wake, one placement per task, claims and notifications matched — now including the futex wake's reserved commit | pass |
| FB0–FB3 | B: CI-exact strict builds, then the seal and the overtaken witnesses | as on F |
| U1–U3 | UART / PL011 / UART IRQ witnesses | rc 0 |
| M1–M4 | reply-direct matrix seal; server-dies ×3 | rc 0 |
| C1–C5 | CI profiles, reply-timeout matrix, SMP-request script, on F and B | identical; inherited #1–#5 and d2-send |
| Z | every QEMU log of F for the marker list | reported per file; any hit explained |

**H6 predeclared classes:** (a) a test that issues a futex wake (the wake now takes the task and
scheduler locks once more per waiter, alternately and never nested); (b) the run-to-run variant
`c09_reply_vs_timeout_terminal_cas_race`; (c) nothing else.

**Z marker list:** the FF1 list, with the futex wake markers now `FUTEX_WAKE_STOPPED` (a wake
that met a full queue or an unplaceable waiter; legitimate, explained if hit) and
`FUTEX_WAKE_RESERVED_COMMIT_REFUSED` (unreachable by construction: any hit is a failure), plus
`FUTEX_WAIT_PARK_REFUSED`, `reason=capacity_exhausted`, overtaken `result=fail` and SCHED-LOCK1
`result=fail`.
