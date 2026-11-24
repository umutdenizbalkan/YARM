# VFS-CORRECT1A evidence

Byte-correct inline reads and explicit completion handling on the resident VFS read path. A
normal userspace client (init, built with `vfs-correct-witness`) reads the initramfs fixture
through the production client wrappers, the resident VFS router and `initramfs_srv`, and PM
launches driver_manager through `VFS_OP_FILE_GRANT_RO` and NR 29, on x86_64, AArch64 and RISC-V
SMP2. Kernel ABI v13 is unchanged; the VFS reply format is a service wire-format change
(`doc/VFS.md` §3.1).

- **Baseline B:** `1c2b8dfc4ed439d32a9cc59d0bed34c0e9196b0a` (the delivered FUTEX-SEAL1 head).
- **Repair:** `55447e80443de27b87f06759b34978483edbb591` (qualification run 1: FAILED, evidence holes).
- **Tested code commit C:** `5bd969689922779ac16465b2237abfca0a8044dc`, tree
  `9dd02db2f6fcc9be33b85cf70a32b2ec27b070a5`, on `wip/vfs-correct1a`.
- **Record-only commit D:** `cfc14b33c537084e2add72df5f1743b0a50f5622`, the stage record in
  `doc/KERNEL_UNLOCKING.md` (*VFS-CORRECT1A*). It touches only that file.
- **Verdict: QUALIFIED** on C (qualification run 2).

## Layout

| path | what |
|---|---|
| `scripts/vc1a_qual.sh` | the run-2 runner, steps in order (`vc1a_qual.run1.sh`: run 1) |
| `scripts/nc.py` | the control mutations: `ncA_count_without_copy`, `ncB_receive_after_failed_send`, `ncC_uncapped_backend_read` |
| `scripts/dev/` | development drivers (`run_dev2.sh`) and the incident's re-application scripts (`recover_*.sh`) |
| `evidence/baseline/` | the defect on B: `CAPTURE.txt` (live AArch64 SMP2: `count=64`, all 64 destination bytes still sentinel), `SOURCE_CAPTURE.txt` (the source lines), the probe patch, logs |
| `evidence/qual/predeclared.sha` | written before run 2: hashes of the runner, `nc.py`, the grader, the smoke script and the fixture, plus C's commit and tree |
| `evidence/qual/summary.txt` | every step: command, worktree head and porcelain, start, rc, duration |
| `evidence/qual/*.log[.gz]` | one log per step |
| `evidence/qual/W_vfs_<port>/` | the witness boots: build log, artifact binding, raw boot log, core smoke, `grade.txt`, `sha256.txt` |
| `evidence/qual/NCLA/` | the live control (AArch64, freshly built): the right count, no copy |
| `evidence/qual/kernels/SHA256SUMS`, `initramfs/SHA256SUMS` | C and B default kernels and core initramfs images (hashes only) |
| `evidence/qual-1-FAILED/` | run 1 on `55447e80`; `DISPOSITION.md` |
| `evidence/dev/` | development boots and the incident note (`README.md`) |
| `MANIFEST.sha256` | every file in this directory except itself |
| `OMITTED.sha256` | files kept out of the pack, with their hashes: kernel ELFs and initramfs images, each boot's `rootfs-init.bin` (build products), every `boot.norm.log` (derived: `tr '\r' '\n' < boot.log`), and the x86 core smoke's shell trace `smoke.log` for run 1 and the dev boots (run 2 keeps them) |

Files over 1 MB are gzipped (`gzip -9n`).

## Run 2 (expected results)

- **Steps.** All `rc=0` except the four controls: `NC_hosted_ncA_count_without_copy`,
  `NC_hosted_ncB_receive_after_failed_send` and `NC_hosted_ncC_uncapped_backend_read` (rc=101),
  and `NC_live_ncA_count_without_copy_aarch64` (rc=1).
- **Live boots.** Each `W_vfs_<port>/grade.txt` reports
  `VFS_CORRECT_WITNESS_SEAL … result=ok bytes=300 grants=5 late_service_tid=10003 nr29_spawns=5`.
- **Live control.** `NCLA` fails BYTES ("wrong destination bytes: differ from the fixture at
  offset 0") with READS passing, plus ERRORS, INIT and CORE.

## Reproduction

1. **Worktrees.** Check out C and B in two worktrees (the runner expects B at `1c2b8dfc` in a
   clean worktree).
2. **Paths.** Edit the paths at the top of `scripts/vc1a_qual.sh` (`S`, `W`, `BW` and the `nc.py`
   location). Then run `C=<sha of C> bash scripts/vc1a_qual.sh`. It takes about 75 minutes on
   four host CPUs; the x86_64 and RISC-V boots run to their wall timeout under TCG.
3. **A single witness boot** from C's worktree:
   `LOGDIR=/tmp/vc1a scripts/qemu-vfs-correct-witness-smoke.sh riscv64 2`. The grader can be run
   on its own: `python3 -I scripts/grade-vfs-correct-witness.py --arch riscv64 --smp 2
   --core-rc 0 --fixture scripts/fixtures/vfs-fixture.bin --init-rootfs <rootfs/init> <boot.norm.log>`.
