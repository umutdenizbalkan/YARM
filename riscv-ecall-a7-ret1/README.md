# RISCV-ECALL-A7-RET1 evidence

Stage: seal the RISC-V `ecall` register contract (`a3`, `a7` preserved on every resume) and the
raw-console changes. Candidate `3091f143` (frozen), delivered `83be1921` (+ witness-only W′, default
artifacts byte-identical) and the record commit `6ba03af9`, on `wip/riscv-ecall-a7-ret1`; baseline
`3ac4f578`. Verdict: **RISCV_ECALL_A7_RET1_ACCEPTED_WITH_BASELINE_DEBT** (record:
`doc/KERNEL_UNLOCKING.md`, "RISCV-ECALL-A7-RET1").

| dir | contents |
|---|---|
| `baseline/` | hosted reproduction on `3ac4f578`: the `ecall_contract` module inserted alone, 15 of 23 fail |
| `baseline-live/` | pre-schedule live reproductions on baseline + witness (try1–try4, PROVENANCE) |
| `trial/`, `uart/` | the candidate's trial witness run; the §8 raw-UART investigation and replay controls |
| `nc/` | the 11 hosted negative controls (patch, full hosted lib log each) and the unmodified control |
| `live/` | LB1/LB2, LF1–LF3 (LF1, LF2 failed: witness defects, `lost_*=0`), L-NC1/L-NC11, LW ×3 |
| `qual/` | the frozen matrix: `summary.txt` (incl. the Addendum B resume), every step log, `bindings.txt`, H6 attribution, X sections, FP footprint, AP scan |
| `addA/` | Addendum A: XW, hosted/integration/grader at F′, LF′ ×3, L-NC1′/L-NC11′, LB′ |
| `addC/` | Addendum C: XW2 symmetric artifact identity, AP2 reclassified scan, trap-bridge disassembly counts, summary |
| `record-checks/` | contract gate, integration, serial hosted lib, fmt on the record tree (one misinvoked parallel run kept, labelled) |
| `scripts/` | the predeclared schedule (with Addenda A–C) and every runner, control, scan and packaging script |

Files over 1 MB are gzipped; build outputs are excluded. `MANIFEST.sha256` covers every file
(`sha256sum -c MANIFEST.sha256`). Logs are as recorded; none was repaired.
