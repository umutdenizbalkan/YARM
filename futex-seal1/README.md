# FUTEX-SEAL1 evidence

The integrated futex seal witness: kernel futexes (ABI v13), the runtime's timed mutex and
condition variable, and RT budgets together on x86_64, AArch64 and RISC-V SMP2. It also includes
a paired measurement of compare-requeue against wake-all in a condition-variable broadcast. The
production behaviour is unchanged; every addition is default-off witness code.

- **Baseline B:** `c4cac895211a52bb69914ebe13c5689fcef4812b` (the delivered FUTEX-SYNC-TIME1 head).
- **Tested code commit C:** `9d5282c4d8e132f834a5be21ee14e083d10878ff`, tree
  `2d1dc946a5b623c154cf67ed297748c3ada53d9f`, on `wip/futex-seal1`.
- **Record-only commit D:** `1c2b8dfc4ed439d32a9cc59d0bed34c0e9196b0a`, the stage record in
  `doc/KERNEL_UNLOCKING.md` (*FUTEX-SEAL1*). It touches only that file.
- **Verdict: QUALIFIED** on C (qualification run 3). The claim-by-claim verdicts, the per-port
  counts and the limits are in the stage record.

## Layout

| path | what |
|---|---|
| `scripts/fs1_qual.sh` | the run-3 qualification runner, steps in order (`fs1_qual.run1.sh`, `fs1_qual.run2.sh`: runs 1 and 2) |
| `scripts/nc.py` | the negative-control mutations (`ncA_wakeall_in_rq`, `ncB_condvar_no_reacquire`, `ncC_duplicate_settlement`) |
| `scripts/mktables.py` | builds the record's paired-broadcast and budget tables from the six seal `summary.json` files |
| `scripts/p7_build.sh` | the rebuild of 253ca580 for the FUTEX-SYNC-TIME1 erratum |
| `scripts/dev/` | development drivers (not graded) |
| `evidence/qual/predeclared.sha` | written before run 3: hashes of the runner, `nc.py`, the seal, sync and time graders, the three witness smoke scripts and the AArch64 completion checker, plus C's commit and tree |
| `evidence/qual/summary.txt` | every step: command, worktree head and porcelain, start, rc, duration |
| `evidence/qual/*.log[.gz]` | one log per step |
| `evidence/qual/W_seal_{gp,rtb}_<port>/` | the seal boots: build logs, artifact binding, raw boot log, core smoke, `grade.txt`, `summary.json` (every paired-broadcast round, raw) |
| `evidence/qual/W_time_<port>/`, `W_sync_<port>/` | the FUTEX-ABS-BITSET1 time cell and the FUTEX-SYNC-TIME1 sync cell, rerun |
| `evidence/qual/NCLA/`, `NCLB/` | the live controls (RISC-V, freshly built): wake-all in the arm labelled `rq` (GP); condvar timeout without reacquiring (RT + budgets) |
| `evidence/qual/kernels/SHA256SUMS`, `initramfs/SHA256SUMS` | C and B default kernels and core initramfs images (hashes only) |
| `evidence/qual/H5_*`, `S_profile_markers.txt` | lock-order pair types (B side reused from FUTEX-SYNC-TIME1 run 3, `f88db55a`), profile and budget markers |
| `evidence/qual-1-FAILED/` | run 1 on `bc5194a5`: FAILED; the verdict and causes are at the end of its `summary.txt` |
| `evidence/qual-2-FAILED/` | run 2 on `acbf79c1`: FAILED at `W_time_riscv64`; `DISPOSITION.md` |
| `evidence/dev/`, `evidence/dev3/` | development boots, with READMEs or summary notes for the invalid, inconclusive, aborted and failed ones (`dev3/README.md`) |
| `evidence/addA/` | post-run analyses: `kernel_sections.txt` (C vs B kernels), `p7_initramfs_sections_recheck.txt` (the FUTEX-SYNC-TIME1 erratum); `README.md`. The record's tables: `claimtable.md`, `tables_run3.md` |
| `MANIFEST.sha256` | every file in this directory except itself |
| `OMITTED.sha256` | files kept out of the pack, with their hashes: kernel ELFs and initramfs images (rebuildable from the commits), every `boot.norm.log` (derived: `tr '\r' '\n' < boot.log`), and the x86 core smoke's shell trace `smoke.log` for runs 1 and 2 and the dev boots (run 3 keeps them) |

## Run 3 (expected results)

- **Steps.** All `rc=0` except the four controls: `NC_hosted_ncC_duplicate_settlement` and
  `NC_hosted_ncB_condvar_no_reacquire` (rc=101), and `NC_live_ncA_wakeall_in_rq_riscv64` and
  `NC_live_ncB_condvar_no_reacquire_riscv64` (rc=1).
- **Live boots.** The six seal boots report `FUTEX_SEAL_WITNESS_SEAL … result=ok`. W_time and
  W_sync report `result=ok` on every port.
- **Controls.** ncA fails PB only (`rq arm made no NR 17 call`). ncB fails RB (`own=0`).

## Reproduction

1. **Worktrees.** Check out C and B in two worktrees (the runner expects B at `c4cac895` in a
   clean worktree).
2. **Paths.** Edit the paths at the top of `scripts/fs1_qual.sh` (`S`, `W`, `BW`, the `nc.py`
   location and the reused lock-order pair file). Then run `C=<sha of C> bash
   scripts/fs1_qual.sh`. It takes about three and a half hours on four host CPUs. The RISC-V
   boots run to their wall timeout, under TCG.
3. **A single seal boot** from C's worktree:
   `SEAL_CELLS="os nr17 lh pb rb" SEAL_PROFILE=rtb LOGDIR=/tmp/seal scripts/qemu-futex-seal-witness-smoke.sh riscv64 2`.
   Use `SEAL_PROFILE=gp` for the GP profile. The grader can also be run on its own:
   `python3 -I scripts/grade-futex-seal-witness.py --arch riscv64 --profile rtb --cells os,nr17,lh,pb,rb <boot.norm.log>`
   (`--self-test` checks its rules).
4. **The record's tables:** `python3 -I scripts/mktables.py evidence/qual`.
5. **Verify this pack:** `sha256sum -c MANIFEST.sha256` from this directory.

QEMU timings are observations, not bounds. Boots under load can differ in timing-dependent
counts (lateness, re-parks, LH classes) but must pass the same rules.
