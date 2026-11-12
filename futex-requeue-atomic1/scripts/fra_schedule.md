# FUTEX-REQUEUE-ATOMIC1 — predeclared qualification schedule

* **Candidate F** = the frozen head of `wip/futex-requeue-atomic1`, worktree `fra-wt`, clean:
  `5939c705` (repair + tests `69dad264`, x17 `c14cce00`, live race witness `f41d68f3`, docs
  `140b60c7`, SCHED-LOCK1 claim-site staging `5939c705`), or a later commit named in the run's P0
  line.
* **Superseded run.** A first run at `140b60c7` (this schedule, then naming `140b60c7`) stopped at
  D3 after H4 failed — the SCHED-LOCK1 grader's claim-site census (7 found, 6 expected), which the
  integration binaries check and the pre-freeze hosted runs had not included. Its logs are kept
  (`qual-aborted-140b60c7`); nothing of it is used as evidence for F.
* **Baseline B** = `279eb4f5`, the canonical dev head (FUTEX-REQUEUE1), worktree `frq-wt`, clean.
* **Runner:** `fra_matrix.sh`, sequential; every step runs whatever earlier steps returned; no
  retries in place (a rerun is appended, never substituted). Logs, rcs, durations, HEAD,
  porcelain and every `ARTIFACT_BINDING` line kept under `fra-evidence/qual`; build outputs are
  not committed.
* **Before the matrix, not part of it:** the baseline reproducer run (x01–x05 against
  `279eb4f5`), the hosted negative controls (`fra_nc.py`, `fra_nc_run.sh`) at F, and the live runs
  (`fra_live.sh`: the race witness ×3 and the live negative control NC1 on x86_64), all kept
  under `fra-evidence`.

| id | step | expected |
|---|---|---|
| P0 | provenance | HEAD=F, porcelain 0 |
| P1 | source: diff stat against B; no scheduler acquisition inside `compare_and_requeue_locked` | captured |
| H1 | `cargo fmt --all --check` | rc 0 |
| H2 / H2b | clippy `--lib --features hosted-dev`; class and count comparison against B | identical (425) |
| H3 | hosted lib, serial | 0 failed |
| H4 | integration binaries | 0 failed |
| H5 | subsets: futex_requeue1::atomic1, futex_requeue1, futex_wake_cap1, futex_found1, futex, robust, scheduler, sched_, enqueue, ap_sched1, stage199d, stage191b, stage191d, stage200d0a, stage29, syscall, u9qa, kthread, exit, reap, spawn, thread, fork, ipc_displace1, sender_wake | 0 failed each |
| H6 | lock order per (test, pair), F vs B, common test set (`futex_requeue1::atomic1` skipped) | every change in a predeclared class (below); nothing nests |
| D1–D3, O1–O3 | default builds ×3; IPC recv-v2 oracles ×3 | rc 0 |
| S1–S6 | CI-exact strict builds + strict core smokes ×3 | rc 0 |
| FX1 | first-cohort futex seal on the CI-exact artifacts | 12/12 live cells (idle-oracle variance tracked separately) |
| S7 | CI strict timer grep | inherited debt #6 |
| W | IPC-DISPLACE1 SMP1 ×3 | `witness=ok core_smoke=ok` |
| FX2–FX6 | overtaken ×2; AP-scheduling, KTHREAD1, SCHED-LOCK1 SMP2 ×3 | pass |
| **FX7** | **FUTEX-REQUEUE1 live witness SMP2 ×3** (unchanged grading: census 10, woken=1 requeued=9, all ten released, park CPUs) | `result=ok`, `cross_cpu=yes` |
| **FX8** | **race witness SMP2 ×3**: every broadcast window holds a FutexWake from the other CPU that ran wholly inside it, and every such wake took nobody; accounting woken + requeued + signaller-woken = 10 | `result=ok`, `covered=3` |
| FB0–FB3 | B: CI-exact strict builds, then the seal and the overtaken witnesses | as on F |
| U1–U3, M1–M4 | IRQ witnesses ×3; reply-direct matrix; server-dies ×3 | rc 0 |
| C1–C5 | CI profiles, reply-timeout matrix, SMP-request script, on F and B | identical; inherited #1–#5 and d2-send |
| Z | every QEMU log of F for the marker list | reported per file; any hit explained |

**H6 predeclared classes:** (a) a test that issues a compare-and-requeue (its preflight alternates
task and one CPU's scheduler lock, its placement claims and fills: the same pairs FutexWake takes,
counted again); (b) the run-to-run variant `c09_reply_vs_timeout_terminal_cas_race`; (c) a
FUTEX-REQUEUE1 test whose body this stage re-derived (r15, r16, r17, r21, r22, r24); (d) nothing
else.

**Z marker list:** FUTEX-REQUEUE1's list, plus an in-window wake that took a waiter
(`FUTEX_REQUEUE_RACE_IN_WINDOW_WAKE ... woke=[1-9]`) and any grader `race:` failure line.
`FUTEX_REQUEUE_PREFLIGHT_STALE` and `FUTEX_REQUEUE_PLACEMENT_REFUSED` are legitimate and are
reported, not failures.

**Appended rerun (declared after the 5939c705 run, before any of its results were used).** FX8
riscv64 at `5939c705` returned `witness=ok core_smoke=rejected` (QEMU timeout before the riscv64
PLIC and idle markers): the race-mode signallers yield-spun forever after the last round. Fixed in
`16c31398` (signallers park once the witness is done; race-witness builds only). The 5939c705 run
is kept in full under `qual`; the rerun goes to `qual-16c31398` and covers P0, the hosted group
(H1–H6), the requeue group (FX7 and FX8 ×3), and **E** — rebuilding D1–D3 and S1/S3/S5 at
`16c31398` and comparing kernel and initramfs sha256 with the 5939c705 bindings: identical
artifacts carry every other 5939c705 cell over unchanged. A difference re-opens those cells.
