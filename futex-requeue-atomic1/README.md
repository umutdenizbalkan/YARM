# FUTEX-REQUEUE-ATOMIC1 evidence

Raw logs behind the FUTEX-REQUEUE-ATOMIC1 stage record (`doc/KERNEL_UNLOCKING.md`). Frozen candidate
F = `16c31398` (the full matrix ran at `5939c705`; `16c31398` changes only race-witness ring-3 code,
and its appended rerun shows every non-witness artifact byte-identical); baseline B = `279eb4f5`.
The record commit `c2b9893e` adds only the record.

| directory | contents |
|---|---|
| `baseline_repro_279eb4f5.log` | x00–x05 run against the baseline: the model passes, all five races are NOT LINEARIZABLE there |
| `nc/` | the 13 hosted negative controls at F (filter `futex`, 195 tests): patch, log, `summary.txt` |
| `nc-run1-69dad264/`, `nc-run2-140b60c7/` | the earlier control runs, kept: NC2 undetected before x17; NC11's first form a non-defect; NC5's stale anchor and NC8's split-only patch, and their appended reruns |
| `live/` | at F: the race witness on x86_64, AArch64, RISC-V (SMP 2), and the live negative control NC1 on x86_64 (fails) |
| `live-140b60c7/` | the same at `140b60c7` |
| `qual/` | the full predeclared matrix at `5939c705` against B (`scripts/fra_schedule.md`, run by `scripts/fra_matrix.sh`); its one witness-caused failure is FX8 RISC-V (`core_smoke=rejected`, signallers never parked) |
| `qual-16c31398/` | the appended rerun at `16c31398`: P0, H1–H6, FX7/FX8 ×3, Z, and `E_equiv.log` (default and strict artifacts of all three ports byte-identical to the `5939c705` bindings) |
| `qual-aborted-140b60c7/` | the superseded run at `140b60c7`, stopped after H4 failed; not evidence for F |
| `hosted/` | the full hosted runs before freezing, the SCHED-LOCK1 x86_64 check of the witness-only line, the hosted lib / integration / doc-fragmentation guard on the record commit, and `check-contract-doc-enforcement.sh` failing identically on F and B (it still expects ABI v10; v11 came with FUTEX-REQUEUE1) |
| `scripts/` | runners, the control patcher, the schedule |

Build outputs are excluded; files over 1 MB are gzipped. `MANIFEST.sha256` covers every other file.
