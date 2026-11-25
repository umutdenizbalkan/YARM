# RISCV-STRICT1 evidence

The RISC-V core smoke grades the obligations of the image it bound. That closes the three strict
RISC-V verdicts IPC-D2-RULE1 left open. The stage record is `doc/KERNEL_UNLOCKING.md`,
*RISCV-STRICT1*. The helper is `scripts/riscv64-workload-obligations.py`.

The pass branches from `6d6ae30d`. Its tested candidate was `ccf582fe`, whose evidence is `3b209df1`
on `wip/ipc-d2-rule1-evidence`.

## Commits

| commit | tree | what | run |
|---|---|---|---|
| `f119fcf6` | `523cd74e` | profile-aware RISC-V core smoke, helper, fixtures, tests | `qual-1`: stopped 30 s in (disposition); `qual-2`: every step |
| `16644365` | | the stage record (`doc/KERNEL_UNLOCKING.md` only) | |

## Layout

| path | what |
|---|---|
| `scripts/rv1_qual.sh` | the runner (`C=`, `RUN=`, `FROM=`) |
| `scripts/extract_fixture.py` | how `scripts/fixtures/riscv-workload/*.txt` were extracted |
| `evidence/predeclared.sha` | written before each run: commit, tree and input hashes |
| `evidence/qual-N/summary.txt` | every step (command, head, porcelain, start, rc, duration, free disk), then the DISPOSITION |
| `evidence/qual-2/<step>.log[.gz]` | one log per step |
| `evidence/qual-2/archive/` | copied verbatim from `3b209df1`, with their hashes checked against its manifest (`A_hash.log`). They are the three open boots (`W_fd_strict_smp2_riscv64_{a,b}`, `W_1b_strict_smp2_riscv64`) with their ORIGINAL core rc, core-smoke log and grader verdicts; the default core boot; and the image hashes of every IPC-D2-RULE1 run 2 image |
| `evidence/qual-2/A_original_verdicts.log` | the original verdicts, as recorded |
| `evidence/qual-2/ID_*.log` | the rebuilt images against the archived hashes (24 of 24 identical) |
| `evidence/qual-2/R_*/` | each archived boot replayed through this smoke, against the bound images, via a QEMU shim: core-smoke log and rc, graders |
| `evidence/qual-2/S_core_*`, `W_fd_strict_smp2_riscv64_{a,b}/`, `W_1b_strict_smp2_riscv64/` | the fresh strict SMP2 RISC-V boots |
| `evidence/qual-2/I_ipcd1_smp2_riscv64/`, `GC_gate_controls_riscv64/` | IPC-DISPLACE1 witness and gate controls (non-strict) |
| `evidence/dev/` | a development hosted run, with its disposition |
| `refs_before.txt` | the remote refs before this pass |
| `MANIFEST.sha256`, `OMITTED.sha256` | every file kept (as stored), and those kept out, with hashes |

**Conventions:**
- Files over 1 MB are gzipped (`gzip -9n`).
- Kept out, with their hashes in `OMITTED.sha256`:
  - `boot.norm.log`, which is derived;
  - `rootfs-init.bin`;
  - the unpacked copies of archived `boot.log.gz`.
- The replay shims reference the unpacked path. To rerun one, run `gunzip -k archive/*/boot.log.gz` first.

To check this pack, run `sha256sum -c MANIFEST.sha256` from this directory.
