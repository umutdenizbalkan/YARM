# FUTEX-WAKE-CAP1 evidence

* `baseline/` — the wake-to-scheduler reproducers (`futex_wake_cap1_baseline` w1–w3) and their
  run on `7522b866`: all three fail (65 waiters strand 2; a partly filled queue strands 7; the
  split wake strands 2, while the wake reports every one as woken).
* `nc-run1-84ed5cd4/` — the first negative-control run: NC7 (a TID-only claim) was not detected.
* `nc/` — the controls rerun at `d64a2f2e`, after `c11` was added: all eight detected; the
  unmodified tree passes all 146 subset tests.
* `qual/` — the predeclared frozen qualification of `d64a2f2e` against `7522b866`
  (`scripts/fwc_schedule.md`, runner `scripts/fwc_matrix.sh`). Build outputs are excluded.
* `hosted/` — development runs before the freeze.
* `scripts/` — every script used. `MANIFEST.sha256` covers every file.
