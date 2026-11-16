# PRE-SCHEDULER-CLEANUP1 — qualification addendum Q2

This adds to the original evidence at `4cfe49dd` (the parent commit; `../README.md`) for the two
qualification items that were left open:
- BOOT-COMPLETE1: the AArch64 default-profile completion condition. D2s's earlier PASS was false.
- SCHED-SAT-HANDOFF1: in the RISC-V sched-time witness, W's deadline is derived from the verified
  saturation, and spare fillers refill the home queues.

Candidate C2 = `1eb75614`, record commit `a1b06b2c` on `wip/pre-scheduler-cleanup`.
Verdict: **QUALIFIED**. The record is `doc/KERNEL_UNLOCKING.md`, "Qualification addendum Q2".

| path | contents |
|---|---|
| `evidence/summary.txt` | every step of the C2 run (rc, seconds, the checkout's commit and porcelain count) |
| `evidence/predeclared.sha` | the runner's sha256, declared before C, then again for C2 |
| `evidence/C*_*.log` | syntax/format, completion self-test, the D2s/S4 replay, grader self-tests, W3/W3b replays |
| `evidence/L1*` | AArch64 SMP 2 default-profile build, core smoke, boot log, ordered completion markers |
| `evidence/L2_riscv64/`, `evidence/L3_x86_64/`, `evidence/L4_aarch64/` | the sched-time witnesses: boot log, grade, artifact binding, hashes |
| `evidence/C1_1e4e88be/` | the first candidate's run: L2 failed at build (E0432, a faulty edit); preserved, not rerun |
| `evidence/Q1_script_text_tests.log`, `evidence/Q2_replay_W3_W3b.txt` | hosted script-text tests, and the replays run during development |
| `scripts/` | the runner, and the module-scoped edit that rebuilt the cell for `1eb75614` |

Files over 1 MB are gzipped, and build outputs are excluded. `MANIFEST.sha256` covers this directory.
