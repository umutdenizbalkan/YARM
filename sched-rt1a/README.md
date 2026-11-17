# RT-SCHED1A evidence

Candidate `67351f6b` (`wip/rt-sched1a`), documentation `5360efdd`, baseline `bf06be4c`. Design
record: `doc/SCHEDULING_CONTRACT.md` §23; stage record: `doc/KERNEL_UNLOCKING.md`, "RT-SCHED1A".

## How it was run

* `qual/predeclared.sha` hashes the runner (`scripts/rt1a_qual.sh`, also `qual/runner.sh`), the
  witness grader and the witness runner at the candidate, each before the runner ran. Addendum B
  (`scripts/rt1a_addB.sh`) was hashed into the same file before it ran.
* `qual/summary.txt` lists every step in order, with the head, the porcelain count and the exit
  status. Every step's log is kept under its own name; nothing was rerun in place.
* Logs over 200 KiB are gzipped. A `boot.norm.log` over 1 MiB is omitted: it is exactly
  `tr '\r' '\n' < boot.log`. Built kernels and initramfs images are not included; their hashes
  are in `qual/kernels/SHA256SUMS`, each live cell's `sha256.txt` and `artifact-binding.txt`.

## Results

**Verdict: QUALIFIED.**

| step | result | where |
|---|---|---|
| hosted | fmt; serial lib 6,732 passed; `--tests` 43 binaries, 7,129 passed; `yarm-kernel` 10; grader self-test (14 mutants); 22 focused subsets, each non-zero (`task_priority` and `thirty_two_levels` run in the `yarm-kernel` crate); contract gate; doc enforcement; default and witness kernels checked on 3 targets | `qual/H*`, `G*`, `X*` |
| lock order | 30 descending-pair types on C and on `bf06be4c`, none new | `qual/H5_*` |
| sizes | `qual/Z1_*` (hosted `size_of`), `qual/Z2_sizes.txt` (`.text`/`.bss` of C and B) | |
| core smokes, SMP2, default C kernels | GP and `yarm.sched_profile=rt`, each port: all pass | `qual/S_*`, `S_profile_markers.txt` |
| RT witness, SMP2 | x86_64, AArch64, RISC-V: `RTW_WITNESS_SEAL … result=pass`, every rule | `qual/W1`, `W2`, `W3` |
| negative controls | tail requeue on preemption: hosted 3 fail, live RISC-V fails C. Equal-priority preemption: hosted 4 fail, live RISC-V fails ID/SEAL (no record: CPU 0 never idles) | `qual/NC*` |
| addendum B | the documentation commit: contract gate, doc enforcement, serial lib 6,732 passed | `qual/addB_*` |

## Development runs before the candidate (`dev/`)

All kept, failed ones included:
* `rtboot_*`: the stock image under the RT profile, SMP2, before the witness: all pass.
* `w1-*`: kernel build refused by the no-FP image check (an unreviewed `USER_PROGRAM` exclusion).
* `w2-*`: boot refused by the artifact binding (an initramfs built from an older dirty tree).
* `w3-*`: AArch64 pass; RISC-V fails A (UA's yields flat under the level-0 supervisor's
  one-tick poll) and D (a tick reached L before its start record); the x86_64 log graded pass
  while running and was stopped.
* `w4-*`: AArch64 and RISC-V pass; the x86_64 grade passed but its runner was edited while bash
  was executing it (`=RTW_DONE: command not found`).
* `w5-*`: after the guard fixes, AArch64 pass; RISC-V fails D on the grader's first-resume rule,
  corrected to "the first resume after H0 ran".
* `rt1a_dev1.log` (2 failures), `rt1a_dev2.log` and `rt1a_integ1.log` (the six guard failures
  plus the RISC-V mailbox guard), `rt1a_dev3.log` / `rt1a_integ2.log` (clean).
