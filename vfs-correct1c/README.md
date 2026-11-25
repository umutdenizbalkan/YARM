# VFS-CORRECT1C evidence

This pack covers four fixes to the resident VFS:
- **F01:** its own client fd namespace;
- **F02:** opens and closes that recover from failure;
- **F03:** descriptors owned by processes;
- **F14:** canonical paths.

Where to read more:
- **Canonical contract:** `doc/VFS.md` §3.3.
- **Kernel ABI:** `doc/SYSCALL_ABI.md` (v14, NR 21 `IpcCallerSession`).
- **Stage record:** `doc/KERNEL_UNLOCKING.md`, *VFS-CORRECT1C*.

## Commits

| commit | tree | what | run |
|---|---|---|---|
| `fbc8ff493be9afbc0a95c635a2c2e74d26904b40` (B) | | baseline: the delivered VFS-CORRECT1B head | `baseline/` |
| `6e3b35649be443220f12cccb5f9428b9d29913f1` | `4992f2c8` | the repair, NR 21, witness, grader, smoke, tests, docs | `qual-1`: stopped at six source guards |
| `23e78dea3d818e1fc182d5ffbf38f52206b50882` | `b9325c83` | the guard findings | `qual-2`: stopped on the D2 race marker |
| `d9becda16831ea0b58d70ce924a4c28855440f67` | `5c4a84d6` | race exclusion withdrawn | `qual-3`: every step |
| `76c74efbaf7b275a86d2c6dcdddd445da894e333` | `3ac5677c` | NR 21 split test from a non-leader thread (test-only) | `qual-4`: hosted, ncK, artifact identity |
| `4b7b71e0` | | the stage record (`doc/KERNEL_UNLOCKING.md` only) | after the record: `--tests` 43 binaries, 7,188 passed |

**The tested code commit is `76c74efb`.** qual-4 rebuilt every image qual-3 booted, on each port:
- the default kernel and core initramfs;
- the `wide` fd-witness kernel, initramfs and init.

All 15 are byte-identical (`qual-4/summary.txt`, `IDENTITY` lines), so qual-3's live results
stand for it.

## Layout

| path | what |
|---|---|
| `scripts/vc1c_qual.sh` | the runner for qual-1 to qual-3. Steps run in order; `FROM=` resumes at a step |
| `scripts/vc1c_qual4.sh` | the qual-4 runner |
| `scripts/nc.py` | the control mutations: `ncH_handle_as_fd`, `ncP_original_spelling`, `ncR_no_rollback_close`, `ncS_thread_session`, `ncK_split_names_tid` |
| `scripts/feature_sweep.sh` | a `cargo check` of every kernel and server feature |
| `scripts/dev*.sh` | the development boot scripts |
| `evidence/predeclared.sha` | written before each run: the hashes of its runner, `nc.py`, sweep, graders, smoke scripts and fixture, plus its commit and tree |
| `evidence/baseline/` | the defects on B: `CAPTURE.txt`, `vc1c-baseline-probe.patch`, logs |
| `evidence/dev/` | development boots `dev1` to `dev8` and the hosted runs; each failed one has a `DISPOSITION.txt` |
| `evidence/qual-N/summary.txt` | every step: command, worktree head and porcelain, start, rc, duration, free disk; then the run's DISPOSITION |
| `evidence/qual-N/<step>.log[.gz]` | one log per step |
| `evidence/qual-3/W_fd_wide_smp1_<port>/`, `W_fd_narrow_smp1_aarch64/` | the fd witness boots: build log, artifact binding, raw boot log, core smoke, `grade-fd.txt`, `grade-correct.txt`, `sha256.txt` |
| `evidence/qual-3/W_1b_wide_<port>/` | the VFS-CORRECT1B witness rerun: 1A's 300 bytes, 1B's 192 bytes and wide capabilities, PM's FILE_GRANT_RO → NR 29 |
| `evidence/qual-3/R_race_smp2_<port>/` | the predeclared finding boots: the fd witness at SMP2, where it reaches `D2_PUBLISH_RACE_UNWIND` |
| `evidence/qual-3/NCL_<H\|P\|S>/` | the live negative controls (AArch64, SMP1, `wide`), each from its own worktree |
| `evidence/qual-N/kernels/SHA256SUMS`, `initramfs/SHA256SUMS` | the C and B default kernels and core initramfs images (hashes only) |
| `MANIFEST.sha256` | every file in this directory except itself |
| `OMITTED.sha256` | the files kept out, with their hashes |

**Conventions:**
- Files over 1 MB are gzipped (`gzip -9n`).
- `boot.norm.log` is derived (`tr '\r' '\n' < boot.log`) and is omitted.
- Kernel, initramfs and init binaries are omitted; their hashes stay in `SHA256SUMS`, `sha256.txt`
  and `OMITTED.sha256`.
- Development boot logs over 5 MB are omitted, with their hashes kept.

## Reading the results

- **RISC-V core smoke.** Outside `QEMU_SMOKE_STRICT=1` it prints failed checks and exits 0. The
  qual-3 disposition lists and attributes every failed check, boot by boot.
- **`R_race_smp2_*`.** These steps end with rc 1 by design; they are findings, not passes.

## Reproduction

From a checkout of `76c74efb`:

```
python3 -I scripts/grade-vfs-fd-witness.py --self-test
scripts/qemu-vfs-fd-witness-smoke.sh aarch64 1 wide     # or narrow; x86_64, riscv64
```

`wide` builds:
- the kernel with `cap-gen-witness`;
- init, VFS and ramfs with `vfs-fd-witness`;
- init with `vfs-correct-witness`.

The smoke runs both graders on the one boot. To check this pack, run `sha256sum -c MANIFEST.sha256`
from this directory. The manifest hashes the files as stored, compressed ones included.
