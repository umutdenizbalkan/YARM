# IRQ-DELIVERY1 evidence

Evidence-only branch: no source code. It belongs to `wip/irq-delivery1`.

| Item | Commit |
|---|---|
| candidate qualified | `3b7aa56` (code `1611de3`, regressions `ba0296a`, saturation witness `edf4ac9`, `60abd7f`, `3b7aa56`) |
| doc commit, delivered tip | `16e5c04` |
| baseline | `c703226` |

Contents:

* `id1_schedule.md`, `id1_matrix.sh`, `id1_lockattr.sh`, `id1_nc_run.sh`, `predeclared.sha` —
  the schedule, the runners and their hashes, taken before the qualifying run.
* `id1_tests.rs` — the baseline reproducers Q1–Q7, applied to `c703226` with only the test-only
  completion counter; `id1_tests2.rs` — the development copy of the later regressions.
* `id1_nc.py` — the ten negative-control mutations (`nc.sha` holds their hashes).
* `id1_docs.py`, `id1_record.md` — the doc commit's generator and the stage record.
* `raw/id1-evidence-logs.tar.xz` — every raw log, rc, summary, artifact binding and NC patch.
  Files are listed in `filelist.txt` and hashed in `raw-logs.sha256`, both relative to the
  archive root. The directories:
  * `qual/`: the qualifying run of `3b7aa56`. `qual/H6_attribution/` holds the per-test
    lock-order attribution and the isolated `long_run` reruns.
  * `qual/H2_classes/`: the clippy class comparison against `c703226`.
  * `qual-D/`: fmt, hosted and integration on `16e5c04`.
  * `nc/`: the baseline reproducers and NC1–NC10.
  * `baseline_Q.log`: Q1–Q7 on `c703226`.
  * `live-baseline/`: the saturation witness on `c703226` without the repair. It reached a
    fatal halt on all three ports.
  * `live-baseline-attempt1/`: the first such attempt, kept.
  * `id1-try/`: the development live runs, including the flaky RISC-V runs `riscv64-run2` and
    `run3` that led to the fixture gate fix.
  * `precommit/`: the pre-commit development runs.

  Kernel binaries are excluded.
