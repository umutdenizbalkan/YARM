# IPC-SENDER-ID1 evidence

Evidence-only branch: no source code. It belongs to `wip/ipc-sender-id1`.

| Item | Commit |
|---|---|
| candidate qualified | `811b4867` (code + re-derived guards `9347cb2d`, regressions `811b4867`) |
| doc commit, delivered tip | `414afb86` |
| baseline | `16e5c04` |

Contents:

* `isi1_schedule.md`, `isi1_matrix.sh`, `isi1_lockattr.sh`, `isi1_clipcmp.sh`, `predeclared.sha` —
  the schedule, the runners and their hashes, taken before the qualifying run. The first launch
  did not start (the runner lacked its execute bit; no step ran): `raw/…/launch1_failed.txt`.
* `isi1_patch.py`, `isi1_testfix.py`, `isi1_add_tests.py`, `isi1_tests.rs`, `isi1_tests_sr.rs` —
  the production patch, the re-derivation of the existing guards, and the regressions, as
  applied to `16e5c04`.
* `isi1_baseline_tree.py` — the baseline-reproducer tree (`16e5c04` plus only the regressions
  that use production functions).
* `isi1_nc.py`, `isi1_nc_run.sh` — the twelve frozen negative controls. `isi1_nc_r.py` — NC1r,
  added after the frozen run because NC1 also disarmed the deadline; disclosed in the record
  (`nc.sha` holds all hashes).
* `isi1_docs.py`, `isi1_record.md` — the doc commit's generator and the stage record.
* `raw/isi1-evidence-logs.tar.xz` — every raw log, rc, summary, artifact binding and NC patch.
  Files are listed in `filelist.txt` and hashed in `raw-logs.sha256`, both relative to the
  archive root. The contents:
  * `qual/`: the qualifying run of `811b4867`. It includes `H6_attribution/` (per-test lock
    order, F/B, and the refused-publication list) and the clippy class comparison.
  * `qual-D/`: fmt, hosted and integration on `414afb86`.
  * `nc/`: the baseline reproducers, NC1–NC12, NC1r and the unmodified-candidate control.
  * `baseline_reproducers.log`: the first baseline run, before S3a was strengthened to check the
    completion before the wake.
  * `explore/`: the exploratory interleavings that found the defect.
  * `precommit/`: the development runs.

  Kernel binaries are excluded.
