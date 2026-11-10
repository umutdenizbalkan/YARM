# IRQ-AUTH1 evidence

Evidence-only branch: no source code. It belongs to `wip/irq-auth1`.

| Item | Commit |
|---|---|
| candidate qualified | `578daf4` (code `82995cf`, regressions `61164db`, `578daf4`) |
| doc commit, delivered tip | `c703226` |
| baseline | `41403e1` |

Contents:

* `ia1_schedule.md`, `ia1_matrix.sh`, `ia1_nc_run.sh`, `predeclared.sha` — the schedule, the
  runners and their hashes, taken before the qualifying run. `ia1_matrix_resume.sh` (its hash
  appended to `predeclared.sha`) ran M2–Z once each after a session worker restart killed the
  runner during M2; the frozen commands are unchanged and M2's partial log is kept.
* `ia1_baseline_tree.py`, `ia1_baseline_tests.rs` — the baseline reproducers B1–B10 against the
  old raw-line API, with only a test hook added to `41403e1`.
* `ia1_nc.py` — the twelve negative-control mutations.
* `ia1_lockattr.sh` — the per-test lock-order attribution appended to H6.
* `ia1_docs.py`, `ia1_record.md` — the doc commit's generator and the stage record.
* `raw/ia1-evidence-logs.tar.xz` — every raw log, rc, summary, artifact binding, NC patch, the
  baseline reproduction, the H6 attribution and the c09 variance runs, the doc-commit reruns and
  the pre-commit development runs. Listed in `filelist.txt`, hashed in `raw-logs.sha256`.
  Kernel binaries are excluded.
