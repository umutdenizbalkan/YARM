# IPC-D2-RULE1 evidence

This pack adopts and qualifies the route-specific `D2_PUBLISH_RACE_UNWIND` rule. The rule is in
`doc/AI_AGENT_RULES.md` §14.3, the checker is `scripts/check-d2-publish-race.py`, and the stage
record is `doc/KERNEL_UNLOCKING.md`, *IPC-D2-RULE1*.

It starts from qualified `4b7b71e0` and builds on the decision brief at `9515a943`, whose evidence
is `a4cbcce4` on `wip/ipc-d2-publish1-evidence`. The archived 1C race boots it replays are
VFS-CORRECT1C run 3 (`35267905` on that pass's evidence branch).

## Commits

| commit | tree | what | run |
|---|---|---|---|
| `f6b2dce3` | `99d73a76` | rule, checker, smokes, guards, fd grader, docs | `qual-1`: superseded (disposition in its `summary.txt`) |
| `ccf582fe` | `29a4bfa8` | strict x86_64 witness feature; x86_64 tick parse without SIGPIPE | `qual-2`: every step |
| record | | `doc/KERNEL_UNLOCKING.md` only | |

## Layout

| path | what |
|---|---|
| `scripts/d2r1_qual.sh` | the runner (`C=`, `RUN=`, `FROM=` resumes at a step) |
| `scripts/dev*.sh` | the development boot scripts |
| `evidence/predeclared.sha` | written before each run: its commit and tree, and the hashes of the runner, checker, fixtures, graders and smoke scripts |
| `evidence/qual-N/summary.txt` | every step: command, worktree head and porcelain, start, rc, duration, free disk; then the run's DISPOSITION |
| `evidence/qual-N/<step>.log[.gz]` | one log per step |
| `evidence/qual-N/W_fd_strict_smp2_<port>_{a,b}/` | strict SMP2 fd witness boots: build log, artifact binding, raw boot log, core smoke (with the checker's verdict), `grade-fd.txt`, `grade-correct.txt`, `sha256.txt` |
| `evidence/qual-N/W_1b_strict_smp2_<port>/` | strict SMP2 VFS-CORRECT1B witness |
| `evidence/qual-N/S_core_*` | strict SMP2 default core smokes |
| `evidence/qual-N/I_ipcd1_smp2_<port>/` | IPC-DISPLACE1 witness (non-strict) |
| `evidence/qual-N/GC_gate_controls_<port>/` | the IPC-DISPLACE1 gate fixtures replayed through the core smoke and witness grader |
| `evidence/qual-N/replay/<port>/` | the archived 1C race boot (`boot.log.gz`, its manifest line), its original 1C `core-smoke.rc`, `grade-fd.txt` and `grade-correct.txt`, unchanged |
| `evidence/dev/` | development boots; a failed one has a `DISPOSITION.txt` |
| `refs_before.txt` | the remote refs before this pass |
| `MANIFEST.sha256` | every file here except itself, as stored |
| `OMITTED.sha256` | the files kept out, with their hashes |

**Conventions:**
- Files over 1 MB are gzipped (`gzip -9n`).
- `boot.norm.log` is derived (`tr '\r' '\n' < boot.log`) and is omitted.
- Binaries (`rootfs-init.bin`) are omitted. Their hashes are kept.

## Reading the results

- **The D2 verdict** of each boot is the `D2_PUBLISH_RACE_CHECK verdict=…` line in its core-smoke
  log, followed by one `race line=…` line per race.
- **RISC-V core smoke.** Under `QEMU_SMOKE_STRICT=1` a failed check fails the step. Its printed
  `[fail]` lines are listed in `qual-2/summary.txt`'s DISPOSITION.

## Reproduction

From a checkout of `ccf582fe`:

```
python3 -I scripts/check-d2-publish-race.py --self-test scripts/fixtures/d2-publish-race/*.txt
python3 -I scripts/check-d2-publish-race.py <boot log>
QEMU_SMOKE_STRICT=1 scripts/qemu-vfs-fd-witness-smoke.sh aarch64 2 wide
```

To check this pack, run `sha256sum -c MANIFEST.sha256` from this directory.
