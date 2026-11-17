# FUTEX-ABS-BITSET1 evidence

Absolute `CLOCK_MONOTONIC` futex deadlines and selective (bitset) wake, ABI v13.

- **Baseline B:** `e1aee62d` (the delivered RT-SCHED1B head).
- **Candidate C:** `a8d7952df02adff3d113abecccc8752c92e4e467`: code, tests, witness, grader, gate, ABI docs.
- **Documentation D:** `253ca580`. It touches only `doc/SCHEDULING_CONTRACT.md` §25.5 and the
  `doc/KERNEL_UNLOCKING.md` stage record, so its tree differs from C's in those two files only.
- **Verdict: QUALIFIED** on C. The record is `SCHEDULING_CONTRACT.md` §25 and the
  FUTEX-ABS-BITSET1 stage record in `KERNEL_UNLOCKING.md`.

## Layout

| path | what |
|---|---|
| `scripts/fab1_qual.sh` | the qualification runner, steps in order |
| `scripts/nc/nc.py` | the negative-control mutations, applied to `src/kernel/boot/futex.rs` |
| `evidence/qual/predeclared.sha` | hashes of the runner, `nc.py` and C's grader, smoke script and cell, written before the run |
| `evidence/qual/summary.txt` | every step: command, start, rc, duration; worktree head and porcelain at each step |
| `evidence/qual/*.log` | one log per step |
| `evidence/qual/W1..W3/` | the live witness boots (x86_64, AArch64, RISC-V; SMP2, GP): build logs, boot logs, artifact binding, grade |
| `evidence/qual/NCL2/` | the live negative control (ignored wake mask, RISC-V, full build) |
| `evidence/qual/kernels/SHA256SUMS` | the C and B default kernels; B is byte-identical to RT-SCHED1B's qualified kernels |
| `evidence/qual/Z2_sizes.txt`, `S_profile_markers.txt`, `H5_*` | sizes, profile/budget markers, lock-order pair types (raw logs gzipped) |
| `evidence/qual-aborted-1/` | the first launch, stopped seconds in (it would have hit the 2 h tool time limit); nothing graded; see `REASON.txt` |
| `evidence/addA/G3_trace.log` | the gate control replayed under `bash -x` on an export of C: the gate stops at the runtime `FUTEX_FLAG_BITSET` check |
| `evidence/dev/` | development evidence (not graded for qualification) |

## Matrix (expected results)

All rc=0, except the controls: `G3_gate_control_flag_drift` rc=1, five `NC_hosted_*` rc=101,
`NC_live_nc2_ignored_wake_mask_riscv64` rc=1.

- **Hosted:** serial lib 6,769 passed; `--tests` 43 binaries, 7,166 passed; three crates; grader
  self-test; 23 focused subsets.
- **Lock order:** 30 descending-pair types on C and B, `diff_lines=0`.
- **Gates** and the gate control.
- **Checks:** cross-target default, witness kernel and witness init.
- **Builds and smokes:** C and B builds; GP, RT and RT+budget core smokes on C.
- **Live:** W1–W3 `FUTEX_TIME_WITNESS_SEAL … result=ok`.
- **NCs:** hosted (6 / 8 / 3 / 8 / 2 failing tests), live (M and C fail).
- **Post-NC:** porcelain 0 in both worktrees.

## Development (`evidence/dev/`)

- `lib1.log` / `tests1.log`: the first full runs. Four source guards failed for renamed owners and
  were re-derived (no rule relaxed). `lib2.log` / `tests2.log`: the final tree, green.
- `ncdry/`: dry runs of every mutation on a scratch copy, before the candidate.
- `W/`: development witness boots, kept as run. Each failure shaped the cell; no grader rule was
  relaxed, and the grader only gained the `S3` signaller name.
  - `riscv64-dev1`: G's 1 ms deadlines all passed before admission (the cell's own log line); E's
    60 s deadline passed during spawning.
  - `riscv64-dev2`: G fixed. E failed because the queue was not refilled after W parked.
  - `riscv64-dev3`: E proven, but the cell's redundant `pings > 0` check failed it. Also no idle A
    collection (boot work still running), and no G window (a monotonic deadline is collected by
    whichever CPU traps first).
  - `aarch64-dev1`: pass (the cell before the last two changes).
  - `x86_64-dev1`: stopped. Its build straddled a cell edit, so its provenance is ambiguous; not
    graded.
  - `riscv64-dev4`, `x86_64-dev2`: pass with the final cell (`cell_v4.sha`).
- `grader-at-base/`: the grader as it was at B, kept beside its rewrite.

## Reused, not re-run

The RT-SCHED1B evidence (`wip/rt-sched1b-evidence`, `356281b3`) for the RT witness phases A–F;
this pass changes no scheduler or budget owner (its core smokes and `rt1b_` / `sched_budget` /
`rt1a_` subsets were re-run here). Not run: a `--workspace` run and the historical matrix.
