# RT-SCHED1B evidence

* **Candidate:** `d2027640` (`wip/rt-sched1b`).
* **Witness-rule correction:** `1e6cfd40`, with no kernel change.
* **Documentation:** `e1aee62d`.
* **Baseline:** `5360efdd`, the RT-SCHED1A head.
* **Records:** the design record is `doc/SCHEDULING_CONTRACT.md` §24; the stage record is
  `doc/KERNEL_UNLOCKING.md`, "RT-SCHED1B".

## How it was run

* Each runner was hashed before it ran, together with the witness grader, the witness runner and
  the negative-control patches. The hashes are in `qual/predeclared.sha`, `addB/predeclared.sha`
  and `addC/predeclared.sha`. The runners are `scripts/rt1b_qual.sh`, `rt1b_addB.sh` and
  `rt1b_addC.sh`, and the control patches `scripts/nc/forgive.py` and `runs.py`.
* `*/summary.txt` lists every step in order, with:
  * the head;
  * the porcelain count;
  * the exit status.

  Every step's log is kept under its own name; nothing was rerun in place.
* **File handling:**
  * logs over 200 KiB are gzipped;
  * a `boot.norm.log` over 1 MiB is omitted, because it is exactly `tr '\r' '\n' < boot.log`;
  * built kernels are not included. Their hashes are in `qual/kernels/SHA256SUMS`, each live
    cell's `sha256.txt` and `artifact-binding.txt`.

## Results

**Verdict: QUALIFIED** on `1e6cfd40`, the code of `d2027640` with the corrected witness rule.

| step | result | where |
|---|---|---|
| hosted | fmt; serial lib 6,750 passed; `--tests` 43 binaries, 7,147 passed; `yarm-kernel` 10; grader self-test; 25 focused subsets, each with a non-zero count; contract gate; doc enforcement; default and witness kernels checked on 3 targets | `qual/H*`, `G*`, `X*` |
| lock order | 30 descending-pair types on C and on `5360efdd`, none new | `qual/H5_*` |
| sizes | hosted `size_of`; `.text`/`.bss` of C and B | `qual/Z1_*`, `qual/Z2_sizes.txt` |
| core smokes, SMP2, default C kernels | each port: GP, RT, and RT + `yarm.sched_budget=on` all pass. RISC-V: GP + `yarm.sched_budget=on` passes with `result=refused_gp` (fail closed) | `qual/S_*`, `S_profile_markers.txt` |
| RT witness with budgets, SMP2 | x86_64 and AArch64: `RTW_WITNESS_SEAL … budget=on result=pass`, every rule. RISC-V: see addendum B | `qual/W1`, `W2` |
| RT witness without budgets | RISC-V: `result=pass` (A–D, IPI) | `qual/W4` |
| main-run failures | W3 RISC-V **failed E** on per-cycle heartbeat progress; every budget fact was correct (§24.7). NC2b was **invalid**: `SKIP_BASE=1` reused a clean-tree initramfs, so the artifact binding refused the patched tree before boot | `qual/W3*`, `qual/NC2b*` |
| negative controls | forgive debt: hosted, 2 tests fail. Exhausted runs: hosted, 7 tests fail | `qual/NC1a*`, `NC2a*` |
| addendum B (`1e6cfd40`) | grader self-test; re-grades of W1, W2, W4 and the failed W3 log, all pass; a **fresh RISC-V witness boot**, `result=pass`, same kernel `dffbd9eb…`; the live exhausted-runs control with a full base build fails E, F, LEDGER, PHASES and SEAL | `addB/` |
| addendum C (`e1aee62d`) | contract gate, doc enforcement, serial lib 6,750 passed; docs-only diff | `addC/` |

## Development runs before the candidate (`dev/`)

All are kept, failed ones included.

* `rv1*`: RISC-V. The sealed result failed only CFG, because the budget line's format was changed
  (cut at the 192-byte printk limit) while it ran. `rv1.norm.regex-adapted` is that log with the
  old line prefix rewritten, graded for the record: it passes.
* `aarch64-1*`: failed B. The CPU 1 tick reached H1 before the reschedule IPI. B now accepts
  either legal scheduling point, and rule IPI was added.
* `x86_64-1*`: pass.
* `pre/`: the pre-candidate hosted checks. `--tests` failed one source guard, re-derived in the
  candidate (`tests/kthread_plain_lock_scope`).
