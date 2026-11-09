# NOTIF-LIFETIME1 — predeclared qualification schedule

Candidate F = 675a32c (wip/notif-lifetime1: b481b3f code + 6a04065, 675a32c regressions), worktree nl1-wt,
clean. Baseline B = ff7cab55 (canonical dev head), worktree m1-wt, clean. Runner: nl1_matrix.sh.
Sequential; every step runs regardless of earlier results; nothing is retried in place. Raw
logs, rcs, durations, HEAD, porcelain and every ARTIFACT_BINDING line are kept under
nl1-evidence/qual.

| id | step | expected |
|---|---|---|
| P0 | provenance | HEAD=F, porcelain 0 |
| H1 | cargo fmt --check | rc 0 |
| H2 | clippy --lib --features hosted-dev, fresh target | 427 warnings; no new class vs B (428; the removed destroy body carried one) |
| H3 | hosted lib, serial, CARGO_INCREMENTAL=0 | 0 failed |
| H4 | integration (`--test '*'`, includes census + doc guards) | 0 failed |
| H5 | subsets: notif_lifetime1, irq_generation1, stage22, stage23, u9f_split, ipc_displace1, fork, deleg, transfer, revoke, reap, spawn | 0 failed each |
| H6 | lock-order warnings across the whole hosted suite (`--nocapture`), F vs B | F count == B count |
| D1-D3 | default freestanding builds, three ISAs | rc 0, kernel_features=none |
| O1-O3 | IPC recv-v2 oracle, three ISAs | rc 0 |
| S1-S6 | CI-exact strict build + QEMU_SMOKE_STRICT=1 core smoke, three ISAs | rc 0 |
| S7 | CI strict x86 timer grep | inherited debt #6 (0/0/0), same as B |
| U1-U3 | UART/PL011 IRQ witness x86_64 / AArch64 / RISC-V (production Notification creation, cross-process mint, bind, delivery) | rc 0; no NOTIF_REF_* markers |
| W1-W3 | IPC-DISPLACE1 live witness SMP1, three ISAs | witness=ok core_smoke=ok |
| M1 | reply-direct matrix seal | rc 0 |
| M2-M4 | server-dies x86_64 / AArch64 / RISC-V (process teardown + reap live) | rc 0 |
| C1 | default x86 rebuild on F (binding kernel_features=none) | rc 0 |
| C2 | CI profiles on F: smp2-core smp4-core smp2-sender-wake smp4-sender-wake sched-timeout | smp*-core pass; #2/#3/#4 inherited |
| C3 | default x86 rebuild on B | rc 0 |
| C4 | same profiles on B | same set and signatures as C2 |
| C5 | reply-timeout matrix and SMP-request script on F and on B | inherited #1/#5 on both |

Every QEMU log of F is also grepped for `NOTIF_REF_OVER_RELEASE` and `NOTIF_REF_RELEASE_FULL`
(must be 0). Classification: hosted proof (H*), live witness coverage (U*: creation, extra mint,
route, delivery; no live revoke/exit of a Notification holder exists), production-default
reachability (none: no default image creates a Notification).

After the matrix: doc commit D on top of F (docs only). On D: H1, H3, H4 rerun; `git diff F..D`
touches only doc/*.md.
