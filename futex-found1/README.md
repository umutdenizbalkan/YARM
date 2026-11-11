# FUTEX-FOUND1 evidence

* `baseline/` — the baseline reproducers (`futex_found1_baseline`, written against the
  `a7a27fb4` ABI) and their run on `a7a27fb4`: F1, F3, F4, F10, F11, F14 fail; F2, F5/F6, F8, F13
  pass.
* `nc/` — the ten negative controls (`scripts/ff1_nc.py`), each in a disposable worktree at
  `5db787b8`, with its patch, its full log and `summary.txt`. `candidate_vs_nc_base.diff` is the
  only difference between that commit and the qualified candidate `e684de30`: a log-marker rename
  in the split wake's plan-refusal branch.
* `qual/` — the predeclared frozen qualification of `e684de30` against `a7a27fb4`
  (`scripts/ff1_schedule.md`, runner `scripts/ff1_matrix.sh`). Build outputs are excluded.
* `hosted/` — development runs before the freeze (full lib, integration, reproducers, clippy
  classes).
* `scripts/` — every script that produced the candidate's tests and patches, the controls and
  the matrix. `hashes.sha256` covers `scripts/` and `baseline/`.
