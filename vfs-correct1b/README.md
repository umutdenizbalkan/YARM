# VFS-CORRECT1B evidence

Full-width capability IDs (audit finding F07) and authorized mount registration (F04). Mount
administration is a capability (the VFS admin endpoint's SEND side); VFS routes only a backend
identity it received in its own CNode; every capability on the mount and grant paths keeps its
generation bits. Proved on x86_64, AArch64 and RISC-V SMP2, with every identity above 2^32 under the
kernel's `cap-gen-witness`. Canonical contract: `doc/VFS.md` §3.2; stage record:
`doc/KERNEL_UNLOCKING.md`, *VFS-CORRECT1B*.

- **Baseline B:** `cfc14b33c537084e2add72df5f1743b0a50f5622` (the delivered VFS-CORRECT1A head).
- **Tested code commit C:** `408ae6e798747359fbabbd98b13e2a767ac2e5e0`, tree
  `5901cfbc101e7d8dc26d747c939dca9f702dca6f`, on `wip/vfs-correct1b`.
- **Record-only commit D:** `fbc8ff49`, the stage record in `doc/KERNEL_UNLOCKING.md`. It touches
  only that file.
- **Verdict: QUALIFIED** on C. F07 closed, F04 closed.

## Layout

| path | what |
|---|---|
| `scripts/vc1b_qual.sh` | the qualification runner, steps in order |
| `scripts/vc1b_qual_cont.sh` | the continuation: only the steps run 1 had not reached (the two live controls, the epilogue) |
| `scripts/nc.py` | the control mutations: `ncT_recv_truncates`, `ncT_vfs_truncates`, `ncS_sender_slot`, `ncK_no_slot17` |
| `scripts/dev/feature_sweep.sh` | `cargo check` of every kernel and server feature, one target each |
| `evidence/baseline/` | the defects on B: `CAPTURE.txt` (F04: VFS routes `/ram` through init's slot number 65551, refused `InvalidCapability`, then blocks on the reply; F07: with generations above 65,535 the supervisor's and PM's startup capabilities read as absent), the two probe patches, logs |
| `evidence/predeclared.sha` | written before run 1: hashes of the runner, `nc.py`, both graders, the smoke script and the fixture, plus C's commit and tree; the continuation runner's hash appended after the interruption, before it ran |
| `evidence/qual-1/summary.txt` | every step: command, worktree head and porcelain, start, rc, duration, free disk; the interruption note at the end |
| `evidence/qual-1/*.log[.gz]` | one log per step |
| `evidence/qual-1/W_<wide|narrow>_<port>/` | the witness boots: build log, artifact binding, raw boot log, core smoke, `grade-mount.txt`, `grade-correct.txt`, `sha256.txt` |
| `evidence/qual-1/kernels/SHA256SUMS`, `initramfs/SHA256SUMS` | C and B default kernels and core initramfs images (hashes only) |
| `evidence/qual-1-cont/` | the continuation: `NCL_T` (VFS keeps 32 bits of the backend) and `NCL_S` (VFS routes the sender's number), live AArch64 wide, freshly built in their own worktrees |
| `evidence/dev/` | development boots (`gen1-aarch64` the first F07 checkpoint; `int1`–`int3-aarch64` the integrated bring-up, `int1` the transfer-copies finding; `int1-x86_64`, `int1-riscv64`), the dev hosted pass `hosted1` (its 9 + 2 stale guard failures, fixed before C), the feature sweeps `sweep1` (kernel + servers, with the disk-full stretch) and `sweep2` (servers, user target), and `sweep1-base` (the still-failing features on B) |
| `MANIFEST.sha256` | every file in this directory except itself |
| `OMITTED.sha256` | files kept out, with their hashes: kernel ELFs and initramfs images, each boot's `rootfs-init.bin`, every `boot.norm.log` / `n.log` (derived: `tr '\r' '\n' < boot.log`) |

Files over 1 MB are gzipped (`gzip -9n`).

## Expected results

- **qual-1 steps.** All `rc=0` except `H4_crate_yarm-control-plane-servers` (rc=101: 277 passed, 2
  failed — `sup_l5b_crash_test_binary_and_staging_are_test_gated` and
  `sup_l6b_runtime_handoff_markers_and_guardrails_are_wired`, which fail identically on B) and the
  four hosted controls (rc=101), each failing its named test (see the stage record).
- **Live boots.** Each `W_*/grade-mount.txt` reports `VFS_MOUNT_WITNESS_SEAL … result=ok`, and each
  `grade-correct.txt` `VFS_CORRECT_WITNESS_SEAL … result=ok bytes=300 grants=5 late_service_tid=10003
  nr29_spawns=5`; `wide` boots report `generation=65537`.
- **qual-1-cont.** Both live controls rc=1, each failing IDENTITY "authorized: expected status 0, saw
  ['7']" (the kernel refused the handshake through the wrong identity: `handshake=Some(Refused(3))`),
  plus BYTES and WIDTH; `post-NC porcelain=0 base-porcelain=0`.

## Reproduction

From a checkout of C:

```
python3 -I scripts/grade-vfs-mount-witness.py --self-test
scripts/qemu-vfs-mount-witness-smoke.sh aarch64 2 wide     # or narrow; x86_64, riscv64
```

`wide` builds the kernel with `cap-gen-witness`, init / VFS / ramfs with `vfs-mount-witness`, and
init with `vfs-correct-witness`; the smoke runs both graders on the one boot. Verify this pack with
`sha256sum -c MANIFEST.sha256`.
