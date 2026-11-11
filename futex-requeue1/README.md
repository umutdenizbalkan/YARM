# FUTEX-REQUEUE1 evidence

Raw logs behind the FUTEX-REQUEUE1 stage record (`doc/KERNEL_UNLOCKING.md`, "FUTEX-REQUEUE1 (from
delivered `3ff5a628`)"). Candidate F = `15e02f59` (record commit `279eb4f5` adds only the record),
baseline B = `3ff5a628`.

| directory | contents |
|---|---|
| `nc/` | the ten hosted negative controls, run at `08a639ac` with filter `futex`: for each, the patch applied (`ncN.patch.txt`) and the test log; `F_control.log` is the unmodified tree (177 passed); `summary.txt` lists the failing tests per control |
| `witness/` | the ring-3 compare-and-requeue witness on the frozen F, SMP 2, x86_64 / AArch64 / RISC-V: serial logs, grader output, `summary.txt` with the three `FUTEX_REQUEUE_WITNESS_SEAL` lines |
| `qual/` | the predeclared qualification of F against B (`scripts/frq_schedule.md`, run by `scripts/frq_matrix.sh`): `summary.txt` is the step list with exit codes; one log per step; `H6_attribution/` holds the per-test lock-order comparison and the `c09` baseline variance reruns |
| `hosted/` | the full hosted library run on the record commit |
| `scripts/` | the runners, the negative-control patcher and the schedule |

Build outputs (kernel images, initramfs archives, build directories) are excluded; files over 1 MB
are gzipped. `MANIFEST.sha256` covers every other file.

Failing steps, all reproduced identically on B: the first-cohort seal's idle cross-check (AArch64,
RISC-V; idle-oracle variance), the CI profiles d2-send, smp2-sender-wake, smp4-sender-wake and
sched-timeout, the reply-timeout matrix (`class=IpcReplyTimeout`), the SMP-request script (line
67), the CI strict timer grep (0/0/0). `H2b` exits 1 because it prints the clippy class diff: one
extra `never used` warning for the witness setter, the class every witness setter has in hosted
builds.
