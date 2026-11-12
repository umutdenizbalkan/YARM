# FUTEX-WAIT-ATOMIC1 evidence

Raw logs behind the FUTEX-WAIT-ATOMIC1 stage record (`doc/KERNEL_UNLOCKING.md`). Frozen candidate
F = `9ce9845f`; baseline B = `c2b9893e`. The record commit `2a91a6e8` adds only the record and two
entries in `doc/KERNEL_UNLOCK_DEFERRED_DEFECTS.md`.

| path | contents |
|---|---|
| `baseline_repro_c2b9893e.log`, `baseline_harness.patch` | F's `wait_atomic1` suite run against B with only the test hooks added: w01b, w04, w06, w16 fail, the other 17 pass |
| `contract_gate_F.log`, `contract_gate_controls.log` | `check-contract-doc-enforcement.sh` on F (rc 0) and its five mutation controls (each rc 1) |
| `nc/` | the 11 hosted negative controls at F (filter `futex`, 216 tests): patch, log, `summary.txt`; and the unmodified control |
| `nc-prelim/` | an earlier control run at `4583533f`, kept; not evidence for F |
| `live-trial-prefreeze/` | witness trials before freezing |
| `live/` | at F: the wait witness ×3 (RISC-V failed: witness placement) and the live NC1 on x86_64 (fails, as it must) |
| `qual/` | the full predeclared matrix at F against B (`scripts/fwa_schedule.md`, run by `scripts/fwa_matrix.sh`), `summary.txt`, `bindings.txt`, `H6_attribution/` |
| `qual-ae65c57e/`, `live-ae65c57e/` | the appended run at the witness-only candidate `ae65c57e` (declared in the schedule before any matrix result was used): hosted group, gates, wait witness ×3, live NC1, and `E_equiv.log` (artifacts NOT identical, so `ae65c57e` is not delivered) |
| `equiv-sections/` | why E differs: x86_64 default kernel, `.text` identical, one `.rodata` word differs by +24 (a panic-location line) |
| `diag-baseline-parked-return/` | B plus only the userspace witness (`patches/diag-baseline-51198a57`): RISC-V 16/16 parked waits return `Err`, AArch64 0/16 — the pre-existing RISC-V return-lane defect |
| `regrade_supplementary.txt` | every wait-witness log re-graded with damaged marker prefixes repaired (supplementary, not a gate) |
| `record-commit-checks/` | contract gate, hosted lib and integration on the record commit's tree |
| `patches/witness-fix-ae65c57e/` | the undelivered witness fix |
| `scripts/` | runners, the control patcher, the schedule, the packer |

Build outputs are excluded; files over 1 MB are gzipped. `MANIFEST.sha256` covers every other file.
