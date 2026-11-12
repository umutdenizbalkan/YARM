# FUTEX-WAIT-ATOMIC1 — predeclared qualification schedule

* **Candidate F** = `9ce9845f`, the frozen head of `wip/futex-wait-atomic1` (worktree `fwa-wt`, clean):
  repair `1d15db5e`, window tests `c4b88d01`, contract gate `4583533f`, live witness `2fc24329`,
  docs `ba1e8280`, locking doc `9ce9845f`.
* **Baseline B** = `c2b9893e`, the canonical dev head (FUTEX-REQUEUE-ATOMIC1), worktree `fra-wt`,
  clean.
* **Runner:** `fwa_chain.sh` runs, in order, (1) the hosted negative controls at F
  (`fwa_nc_run.sh`, `fwa_nc.py`, filter `futex`, 11 controls and an unmodified control run),
  (2) F's `wait_atomic1` suite verbatim against B with the test-only hooks (`fwa_baseline.sh`,
  `fwa_hook.py`), (3) the live runs (`fwa_live.sh`: the wait witness ×3 at SMP 2 and the live NC1
  on x86_64), (4) the matrix (`fwa_matrix.sh`). Sequential; every step runs whatever earlier steps
  returned; no retries in place (a rerun is appended, never substituted); logs, rcs, durations,
  HEAD, porcelain and every `ARTIFACT_BINDING` kept under `fwa-evidence`. A preliminary control
  run at `4583533f` (`nc-prelim`) is kept and is not evidence for F.
* **Expected of (1):** every control applies and is detected by at least one behavioural test; the
  unmodified tree passes all. **(2):** w01b, w04, w06, w16 fail on B (counterexamples); the other
  17 pass. **(3):** `witness=ok core_smoke=ok result=ok` on all three ports; the live NC1 fails the
  grader with "registered although its word had changed".

| id | step | expected |
|---|---|---|
| P0 | provenance | HEAD=F, porcelain 0 |
| P1 | source: diff stat against B; the park body calls the compare-and-register once and holds no second read, no separate registration, no undo-unless-acted-on; the broad twin calls the same owner | captured |
| H1 | `cargo fmt --all --check` | rc 0 |
| H2 / H2b | clippy `--lib --features hosted-dev`; class and count comparison against B | identical (425) |
| H3 | hosted lib, serial | 0 failed |
| H4 | integration binaries | 0 failed |
| H5 | subsets: futex_requeue1::wait_atomic1, futex_requeue1::atomic1, futex_requeue1, futex_wake_cap1, futex_found1, futex, robust, scheduler, sched_, enqueue, ap_sched1, stage199d, stage191b, stage191d, stage200d0a, stage29, syscall, u9qa, kthread, exit, reap, spawn, thread, fork, ipc_displace1, sender_wake | 0 failed each |
| H6 | lock order per (test, pair), F vs B, common test set (`futex_requeue1::wait_atomic1` skipped) | every change in a predeclared class (below); nothing nests |
| G1 / G2 | `scripts/check-contract-doc-enforcement.sh` on F / on B | F rc 0; B rc 1 (the stale v10 marker this stage corrects) |
| G3 | marker scanner self-test (`fwa_zscan.sh --self-test`) | all cases hold |
| G4 | wait and requeue grader self-tests | ok |
| D1–D3, O1–O3 | default builds ×3; IPC recv-v2 oracles ×3 | rc 0 |
| S1–S6 | CI-exact strict builds + strict core smokes ×3 | rc 0 |
| FX1 | first-cohort futex seal on the CI-exact artifacts | 12/12 live cells (idle-oracle variance tracked separately) |
| S7 | CI strict timer grep | inherited debt #6 |
| W | IPC-DISPLACE1 SMP1 ×3 | `witness=ok core_smoke=ok` |
| FX2–FX6 | overtaken ×2; AP-scheduling, KTHREAD1, SCHED-LOCK1 SMP2 ×3 | pass |
| FX7 | FUTEX-REQUEUE1 live witness SMP2 ×3 (unchanged grading) | `result=ok`, `cross_cpu=yes` |
| FX8 | FUTEX-REQUEUE-ATOMIC1 race witness SMP2 ×3 | `result=ok`, `covered=3` |
| **FX9** | **FUTEX-WAIT-ATOMIC1 wait witness SMP2 ×3** | `witness=ok core_smoke=ok result=ok` |
| FB0–FB3 | B: CI-exact strict builds, then the seal and the overtaken witnesses | as on F |
| U1–U3, M1–M4 | IRQ witnesses ×3; reply-direct matrix; server-dies ×3 | rc 0 |
| C1–C5 | CI profiles, reply-timeout matrix, SMP-request script, on F and B | identical; inherited #1–#5 and d2-send |
| Z | every QEMU log of F through `fwa_zscan.sh` (FUTEX-REQUEUE-ATOMIC1's list, `race: ` matched as a word, plus `FUTEX_WAIT_WITNESS ... result=fail`) | reported per file; any hit explained |

**H6 predeclared classes:** (a) a futex test whose wait now takes task → VM → memory in one hold
instead of VM → memory, task, VM → memory (the same pairs in a different count; no new pair, no
nesting); (b) the run-to-run variant `c09_reply_vs_timeout_terminal_cas_race`; (c) a test this
stage re-derived (f06, the stage191d guards, prelock_split_membership); (d) nothing else.

**Z:** `FUTEX_WAIT_PARK_REFUSED` lines are reported and explained per file; in this stage the
only expected source is none (the witness waits do not refuse).

**Appended rerun (declared while the `9ce9845f` matrix was still running, before any of its
results were used).** The live runs at `9ce9845f` (step 3) returned RISC-V
`witness=fail core_smoke=rejected`: its first window held no wake, because the waiter's window ran
with interrupts masked while its one signaller was queued behind it on the same CPU. `ae65c57e`
fixes the witness only (two signallers, init blocks while it runs, a window opens only while a
sibling of the waiter is current on another CPU, unique store values; witness builds only). The
`9ce9845f` chain runs to the end and is kept in full. After it, an appended run at `ae65c57e`
goes to `qual-ae65c57e` and `live-ae65c57e`: P0, the hosted group (H1–H6), G1–G4, FX9 ×3, the live
NC1 on x86_64 (NC1's patch gains the same peer gate; its hosted effect is unchanged), and **E** —
rebuilding D1–D3 and S1/S3/S5 at `ae65c57e` and comparing kernel and initramfs sha256 with the
`9ce9845f` bindings: identical artifacts carry every other `9ce9845f` cell over unchanged; a
difference re-opens those cells. The hosted negative controls carry over likewise, because no
hosted build compiles the witness.
