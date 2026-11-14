# SCHED-TIME-FOUNDATION1 evidence

Stage: A — full-queue rotation repair and IPC timeout placement conservation; B — TIMEKEEP1
(nanosecond monotonic / realtime clock, NR 18 `ClockGetTime`, NR 19 `ClockSetRealtime`); C — SLEEP1
(NR 20 `ClockNanosleep`); D — VDSO-TIME1 (proof-gated, deferred). Baseline B = `8062c216`. Frozen
candidate F = `c4c209a9` on `wip/sched-time-foundation1`; record commit `2866be83` (docs only).
Verdict: **SCHED_TIME_FOUNDATION1_ACCEPTED_VDSO_DEFERRED** (record: `doc/KERNEL_UNLOCKING.md`,
"SCHED-TIME-FOUNDATION1"; follow-ups: `doc/KERNEL_UNLOCK_DEFERRED_DEFECTS.md`).

| path | contents |
|---|---|
| `evidence/qual-nc/` | the 27 hosted negative controls at F (patch + full hosted lib log each), F_control, `summary.txt` |
| `evidence/qual-live/` | LST-<isa>-1..3, L-ncL1, L-ncL2, LT-<isa>, LW-<isa>, LE, LS-<isa> at F; `summary.txt`; `classification.txt` (every failure classified, timestamped as recorded, with errata) |
| `evidence/addA/`, `evidence/addB/` | Addendum A (futex-wait witness, RISC-V, x3 on B and x3 on F, interleaved) and Addendum B (RISC-V ecall witness, same) |
| `evidence/qual/` | the frozen matrix at F vs B: `summary.txt`, every step log, `bindings.txt`, `notes.txt` (classification of every non-zero step, E4 footprint), H2b item lists, X sections, FP footprint, AP scan |
| `evidence/record-checks/` | contract gate on the record tree |
| `evidence/predeclared.sha` | sha256 of the schedule and every runner at the freeze, then at each addendum |
| `evidence/A/`, `evidence/BC/`, `evidence/B_*`, `evidence/C_*`, `evidence/*_live*` | development before the freeze (not qualification): baseline reproducers, checkpoint controls (ncC5/ncC8 gaps found there, fixed in F), live development runs, an aborted wrong-tree run, interrupted runs (`.interrupted`) |
| `scripts/` | the predeclared schedule (`stf_schedule.md`, Addenda A and B), every runner, control, scan and packaging script, and the ls-remote snapshot at the stage start |

Files over 1 MB are gzipped; build outputs are excluded. `MANIFEST.sha256` covers every file
(`sha256sum -c MANIFEST.sha256`). Logs are as recorded; none was repaired.
