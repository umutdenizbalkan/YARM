# FUTEX-REQUEUE1 — predeclared qualification schedule

* **Candidate F** = the frozen head of `wip/futex-requeue1`, worktree `frq-wt`, clean: `15e02f59`
  (implementation `75416f5a`, tests `08a639ac`, live witness `a167fa5c`, docs `15e02f59`), or a
  later commit named in the run's P0 line.
* **Baseline B** = `3ff5a628`, the canonical dev head (FUTEX-WAKE-CAP1), worktree `fwc-wt`, clean.
* **Runner:** `frq_matrix.sh`, sequential; every step runs whatever earlier steps returned; no
  retries in place (a rerun is appended, never substituted). Logs, rcs, durations, HEAD,
  porcelain and every `ARTIFACT_BINDING` line kept under `frq-evidence/qual`; build outputs are
  not committed.
* **Negative controls** ran before the matrix (`frq_nc.py`, `frq_nc_run.sh`) at `08a639ac`; F
  differs from it only by the witness commit (init cell, knob, park marker, `FutexMutex::unlock`'s
  return value) and docs, none of which a control patches.

| id | step | expected |
|---|---|---|
| P0 | provenance | HEAD=F, porcelain 0 |
| P1 | source: diff stat against B; no scheduler acquisition inside `compare_and_requeue_locked` | captured |
| H1 | `cargo fmt --all --check` | rc 0 |
| H2 / H2b | clippy `--lib --features hosted-dev`; class and count comparison against B | identical (424) |
| H3 | hosted lib, serial | 0 failed |
| H4 | integration binaries (census, doc-fragmentation guard, SCHED-LOCK1 grader, artifact binding, …) | 0 failed |
| H5 | subsets: futex_requeue1, futex_wake_cap1, futex_found1, futex, robust, scheduler, sched_, enqueue, ap_sched1, stage199d, stage191b, stage191d, stage200d0a, stage29, syscall, u9qa, kthread, exit, reap, spawn, thread, fork, ipc_displace1, sender_wake | 0 failed each |
| H6 | lock order per (test, pair), F vs B, common test set (`futex_requeue1` skipped) | every change in a predeclared class (below); nothing nests |
| D1–D3 | default builds ×3 | rc 0 |
| O1–O3 | IPC recv-v2 oracles ×3 | rc 0 |
| S1–S6 | CI-exact strict builds + strict core smokes ×3 | rc 0 |
| FX1 | first-cohort futex seal on those CI-exact artifacts | 12/12 live cells (idle-oracle variance, seen on bases, tracked separately) |
| S7 | CI strict timer grep | inherited debt #6 |
| W1–W3 | IPC-DISPLACE1 SMP1 ×3 | `witness=ok core_smoke=ok` |
| FX2–FX3 | overtaken witness x86_64, AArch64 | `result=ok` |
| FX4–FX6 | AP-scheduling, KTHREAD1, SCHED-LOCK1 witnesses SMP2 ×3 | pass |
| **FX7** | **FUTEX-REQUEUE1 live witness SMP2 ×3**: ten ring-3 threads, three rounds, census 10, broadcast woken=1 requeued=9 off the broad lock, all ten released each round, park CPUs reported | `FUTEX_REQUEUE_WITNESS_SEAL ... result=ok`; `cross_cpu=yes` where the port places user threads on CPU 1 (else recorded) |
| FB0–FB3 | B: CI-exact strict builds, then the seal and the overtaken witnesses | as on F |
| U1–U3 | UART / PL011 / UART IRQ witnesses | rc 0 |
| M1–M4 | reply-direct matrix seal; server-dies ×3 | rc 0 |
| C1–C5 | CI profiles, reply-timeout matrix, SMP-request script, on F and B | identical; inherited #1–#5 and d2-send |
| Z | every QEMU log of F for the marker list | reported per file; any hit explained |

**H6 predeclared classes:** (a) a test that issues a futex wake or requeue (task → VM → memory
nested is ascending and is not reported; the wake portion alternates task and scheduler as
before); (b) the run-to-run variant `c09_reply_vs_timeout_terminal_cas_race`; (c) a test whose
syscall-count or ABI-version guard was re-derived (no lock change expected); (d) nothing else.

**Z marker list:** the FUTEX-WAKE-CAP1 list (`FUTEX_WAKE_STOPPED` legitimate and explained if hit;
`FUTEX_WAKE_RESERVED_COMMIT_REFUSED` a failure), plus `FUTEX_REQUEUE_SPLIT_REFUSED`,
`CRQ_WITNESS.*result=fail` and `FUTEX_REQUEUE_WITNESS_SEAL.*result=fail`.
