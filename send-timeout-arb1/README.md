# SEND-TIMEOUT-ARB1 evidence

Evidence-only branch: no source code. It belongs to `wip/send-timeout-arb1`.

| Item | Commit |
|---|---|
| candidate qualified | `d3bc11a0` (code + re-derived guards `2ad2145f`, regressions `6c178c91`, R9 `d3bc11a0`) |
| doc commit, delivered tip | `3148a754` |
| baseline | `414afb86` |

Contents:

* **Schedule and runners:** `sta1_schedule.md`, `sta1_matrix.sh`, `sta1_lockattr.sh`,
  `sta1_clipcmp.sh` and `predeclared.sha`, which holds their hashes (and those of the patch and
  test sources), taken before the qualifying run.
* **The appended smp2-core rerun:** `sta1_rerun_smp2core.sh` and `rerun.sha`. C2's smp2-core run
  failed on a serial-interleaved AP bring-up marker; this script reran it three times on the same
  kernel binding. The failed run is kept.
* **Patch and tests:** `sta1_patch.py` (with `sta1_hookmod.rs`), `sta1_testfix.py`,
  `sta1_add_tests.py`, `sta1_tests.rs`, `sta1_tests_sr.rs` and `sta1_tests_r9.rs`. These are the
  production patch, the re-derivation of the existing guards, and the reproducers and
  regressions, as applied to `414afb86`.
* **Baseline tree:** `sta1_hooks_baseline.py`, which builds the baseline-reproducer tree:
  `414afb86` plus only the two test-only phase hooks and the new tests.
* **Negative controls:** `sta1_nc.py` and `sta1_nc_run.sh`, the twelve frozen controls
  (`nc.sha`).
* **R5 probe:** `sta1_explore.rs`, the exploratory probe that found the in-lock envelope release.
* **Docs:** `sta1_docs.py`, the doc commit's generator, and `sta1_record.md`, the stage record.
* **Raw logs:** `raw/sta1-evidence-logs.tar.xz` holds every raw log, rc, summary, artifact binding
  and NC patch. Files are listed in `filelist.txt` and hashed in `raw-logs.sha256`, both relative
  to the archive root. The archive contains:
  * `qual/`: the qualifying run of `d3bc11a0`, then the appended `R_*` rerun steps. It includes
    `H6_attribution/` (per-test lock order for F and B, the class lists and
    `attribution_analysis.txt`) and the clippy class comparison.
  * `qual-D/`: fmt, hosted and integration on `3148a754`.
  * `nc/`: the baseline reproducers, NC1–NC12 and the unmodified-candidate control.
  * `compare/`: per-profile error sets of F (C2) and B (C4).
  * `baseline_reproducers_first.log`: the first baseline run, before R9 was added.
  * `precommit/`: the development runs.

  Kernel binaries are excluded.
