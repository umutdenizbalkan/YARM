# NOTIF-WAITER1 evidence

Evidence-only branch: no source code. It belongs to `wip/notif-waiter1`.

| Item | Commit |
|---|---|
| candidate qualified | `8c31d9e` (code `2e88ac9`, regressions `7415a43`, take error precedence `8c31d9e`) |
| doc commit, delivered tip | `41403e1` |
| baseline | `dc5c8fd` |

Contents:

* `nw1_schedule.md`, `nw1_matrix.sh`, `nw1_nc_run.sh`, `nw1_all.sh`, `predeclared.sha` — the
  schedule, the runners and their hashes, taken before the qualifying run.
* `nw1_nc.py` — the eleven negative-control mutations; `nw1_baseline_tree.py` — builds the
  baseline-reproducer tree (`dc5c8fd` + the test hook + the unused owner module + the tests).
* `nw1_docs.py`, `nw1_record.md` — the doc commit's generator and the stage record.
* `raw/nw1-evidence-logs.tar.xz` — every raw log, rc, summary, artifact binding and NC patch:
  `qual/` (the qualifying run of `8c31d9e`), `nc/` (baseline reproducers and NC1–NC11 on
  `8c31d9e`), `qual-D/` (fmt, hosted and integration on `41403e1`), and the superseded
  `qual-aborted-7415a43/` and `nc-7415a43/` (the first run, stopped by the operator after its
  hosted steps to restore the take's error precedence; kept, not substituted). Files are listed
  in `filelist.txt` and hashed in `raw-logs.sha256`, both relative to the archive root. Kernel
  binaries are excluded.
