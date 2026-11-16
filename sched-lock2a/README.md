# SCHED-LOCK2A evidence

The first SCHED-LOCK2 pass: home-CPU placement made structural (SC-CPU-2), and the x86_64 SMP
blocked-server oracle's fact collection gated on its own predicate.
- Baseline: `a1b06b2c`.
- Candidate: `189a890d`. `cabe3ccc` adds only a repair of one witness script's bash syntax error.
- Record: `c9fbc7ef` on `wip/sched-lock2a`.
- Verdict: **QUALIFIED**. See `doc/KERNEL_UNLOCKING.md` "SCHED-LOCK2A" and
  `doc/SCHEDULING_CONTRACT.md` §20.

| path | contents |
|---|---|
| `evidence/qual/summary.txt` | every step: rc, seconds, the checkout's commit and porcelain count |
| `evidence/qual/predeclared.sha` | the runner and both addenda, hashed before each ran |
| `evidence/qual/H*`, `G*`, `B*` | fmt, hosted lib and integration, the 18 focused subsets, contract gates, builds |
| `evidence/qual/L1..L5` | live cells (run directories and step logs); `L5.boot.log` is the ordinary x86_64 SMP 2 boot with the oracle off |
| `evidence/qual/A1_L2*`, `A2_L1_base*`, `A3*`, `A4*` | addendum A: the oracle-armed cell on `cabe3ccc`, L1 on the baseline, the log-only home-refusal scan, lock-order pair types for C and B |
| `evidence/qual/B1*` | addendum B: the oracle-armed cell on the baseline with only the script repair applied |
| `evidence/qual/D1_*` | the old-path demonstration: the two baseline-API regressions run on `a1b06b2c`, both failing |
| `evidence/qual/R_*` | contract gates on the record tree |
| `evidence/dev/` | development hosted runs (`lib1`…`lib4`, reruns) with the failures they found, fixed in the candidate |
| `scripts/` | the runner, both addenda, and the remote-refs snapshot taken at the start |

Files over 1 MB are gzipped. Build outputs and kernel images are excluded; their hashes are in the
logs' `ARTIFACT_BINDING` lines. `MANIFEST.sha256` covers every file. Logs are kept exactly as
recorded.
