# NOTIF-LIFETIME1 evidence

Evidence-only branch: no source code. It belongs to `wip/notif-lifetime1`.

| Item | Commit |
|---|---|
| candidate qualified | `675a32c` |
| test-only guard | `fb27e94` |
| doc commit, delivered tip | `dc5c8fd` |
| baseline | `ff7cab55` |

Contents:

* `nl1_schedule.md`, `nl1_matrix.sh`, `predeclared.sha` — the schedule, the runner and their
  hashes, taken before the run.
* `nl1_nc.py`, `nl1_nc_run.sh` — the negative-control mutations and their runner.
* `raw/nl1-evidence-logs.tar.xz` — every raw log, rc, summary, artifact binding and NC patch.
  Files are listed in `filelist.txt` and hashed in `raw-logs.sha256`, both relative to the
  archive root. Kernel binaries are excluded.
* `raw/ig1-evidence-logs.tar.xz` — the IRQ-GENERATION1 baseline reproduction logs on
  `ff7cab55`, with their SHA256SUMS.
