# RISCV-PARK-RET1 evidence

Raw logs behind the RISCV-PARK-RET1 stage record (`doc/KERNEL_UNLOCKING.md`). Frozen candidate
F = `194842bc`; baseline B = `2a91a6e8`. The record commit `3ac4f578` adds only the record and one
entry in `doc/KERNEL_UNLOCK_DEFERRED_DEFECTS.md`. Verdict: RISCV_PARK_RET1_ACCEPTED_WITH_BASELINE_DEBT.

| path | contents |
|---|---|
| `baseline/` | F's `riscv_park_ret1` suite run on B with the one bridge step stood in for by B's behaviour (`baseline_harness.patch`): t02, t05, t06, t07, t08, t09, t19, g05 fail, 16 pass |
| `baseline-live/` | `c2b9893e` plus only the userspace witness (diagnostic, never pushed): the RISC-V resume boundary values (`boundary_values_c2b9893e.txt`), 16/16 parked waits `Err` on RISC-V, 0/16 on AArch64 |
| `nc/` | the ten hosted negative controls at F, each against the full hosted lib: patch, log, `summary.txt`; and the unmodified control |
| `live/` | at F, SMP 2: the wait witness ×3 per ISA (`summary.txt`; AArch64 runs 1 and 2 failed on damaged markers, kept unrepaired) and the live NC2 on RISC-V (fails) and x86_64 (passes) |
| `qual/` | the predeclared matrix at F against B (`scripts/rpr_schedule.md`, run by `scripts/rpr_matrix.sh`) and its addenda A–C (`rpr_addA.sh`; B and C as in the schedule), `summary.txt`, `artifact_bindings.txt`, `H6_attribution/`, `XC_text_identity.log` (the valid `.text` identity), `X_text_identity.log`, `X7_text_identity.log`, `XB_text_identity.log` (kept: void or refused, see the record) |
| `footprint/` | RISC-V `.text` sizes and symbol sizes at F and B |
| `a7scan/` | disassembly of 14 RISC-V user binaries and the a7-after-ecall scan with its self-test |
| `trial-riscv64/`, `trial2/` | witness trials before freezing (not evidence for F) |
| `record-commit-checks/` | contract gate, hosted lib and integration on the record commit's tree |
| `scripts/` | runners, the control patcher, the schedule, the scanners, the packer |

Build outputs are excluded; files over 1 MB are gzipped. `MANIFEST.sha256` covers every other file.
