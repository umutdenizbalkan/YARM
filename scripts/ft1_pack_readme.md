# FUTEX-TIME1 evidence

Stage: timed `FutexWait` (NR 9 + `FUTEX_FLAG_TIMED`, relative nanoseconds) with exact
timeout / wake / requeue / exit arbitration. Candidate F = `04a09b2b` (frozen for the hosted NC and
live phases), F' = `27956ed9` (+ tests t43–t45), F'' = `fb140666` (+ the futex-wait-witness marker
arm; the frozen matrix candidate), and the record commit `8062c216`, on `wip/futex-time1`; baseline
`6ba03af9`. Verdict: **FUTEX_TIME1_ACCEPTED_WITH_BASELINE_DEBT** (record: `doc/KERNEL_UNLOCKING.md`,
"FUTEX-TIME1").

| path | contents |
|---|---|
| `nc/` | the 28 hosted negative controls at F (patch, full hosted lib log each) and the unmodified control |
| `nc-Fpp/` | nc1–nc29 at F'' (patch, log, `summary.txt`, `compile_errors_audit.txt`) and the unmodified control |
| `live/` | LT-<isa>-1..3, L-nc2, L-nc28, LW/LE (build failures at F), LS ×3 at F; ID-<isa> identity, LW-<isa>-C, LE-C, LT-x86_64-4..6 at F'' (Addendum C); LS-<isa>-D (Addendum D); `summary.txt` |
| `addC/` | the last-slot starvation reproductions on `6ba03af9` and F' (scratch tests, never committed); futex-wait-witness fix checks |
| `featsweep-Fprime/`, `featsweep-B/`, `featsweep-compare.txt` | every kernel feature check-compiled per ISA at F' and at `6ba03af9` |
| `qual/` | the frozen matrix at F'' vs `6ba03af9`: `summary.txt`, every step log, `bindings.txt`, H6 attribution (`common_diff.txt`), X sections, FP footprint, AP scan |
| `addE/` | SCHED-LOCK1 RISC-V ×5 at `6ba03af9` and ×5 at F'', interleaved |
| `addF/` | SCHED-LOCK1 RISC-V with the scratch delay (×3 per tree), ×10 unmodified per tree, LS RISC-V ×3 per tree, `hostsplit_scan.txt` |
| `metrics/` | §19 lock order, work bounds, footprint |
| `record-checks/` | contract gate, integration, fmt on the record tree |
| `dev/` | development runs before the freeze (not qualification), incl. the x86_64 r1 run that found the resume-lane defect |
| `fprime_hosted_lib.log` | the full hosted lib at F' |
| `predeclared.sha` | sha256 of the schedule and every runner, line by line as each addendum was declared |
| `scripts/` | the predeclared schedule (`ft1_schedule.md`, Addenda A–F and the erratum) and every runner, control, scan, scratch patch and packaging script |

Files over 1 MB are gzipped; build outputs are excluded. `MANIFEST.sha256` covers every file
(`sha256sum -c MANIFEST.sha256`). Logs are as recorded; none was repaired.
