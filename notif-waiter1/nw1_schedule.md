# NOTIF-WAITER1 — predeclared qualification schedule

Candidate F = 8c31d9e (wip/notif-waiter1: 2e88ac9 code + 7415a43 regressions + 8c31d9e take error precedence; supersedes the aborted 7415a43 run, kept as qual-aborted-7415a43), worktree nw1-wt,
clean. Baseline B = dc5c8fd (canonical dev head, NOTIF-LIFETIME1), worktree nl1-wt, clean.
Runner: nw1_matrix.sh. Sequential; every step runs regardless of earlier results; nothing is
retried in place (a rerun is appended, never substituted). Raw logs, rcs, durations, HEAD,
porcelain and every ARTIFACT_BINDING line are kept under nw1-evidence/qual.

| id | step | expected |
|---|---|---|
| P0 | provenance | HEAD=F, porcelain 0 |
| P1 | reachability source evidence: NR 2 / NR 5 validate an endpoint before any receive; the only notification door in witness builds is the non-blocking probe | captured lines |
| H1 | cargo fmt --check | rc 0 |
| H2 | clippy --lib --features hosted-dev, fresh target | 427 warnings, identical warning classes to B (427) |
| H3 | hosted lib, serial, CARGO_INCREMENTAL=0 | 0 failed |
| H4 | integration (`--test '*'`, includes census + doc guards) | 0 failed |
| H5 | subsets: notif_waiter1 notification notif_lifetime1 irq_generation1 stage22 stage23 u9f_split ipc_displace1 exit_task timeout fork deleg transfer revoke reap spawn | 0 failed each |
| H6 | lock-order warnings, per pair, F vs B, on the common test set (`--skip notif_waiter1 --skip exit_task_`) | equal per pair, except (revised after the aborted 7415a43 run observed them, each attributed): `current=task previous=3` may rise (the post-publication / stale-test rank-2 read, after the rank-3 closure returns); `current=scheduler previous=2` may rise (the unwind's `wake_tid_to_runnable`, task then scheduler, sequential); `current=ipc previous=11` may fall by exactly what `current=ipc previous=4` rises (the receive no longer reads the capacity config before the rank-3 take, so that acquisition now follows the capability lookup). Total ipc-pair count unchanged; nothing nested |
| D1-D3 | default freestanding builds, three ISAs | rc 0, kernel_features=none |
| O1-O3 | IPC recv-v2 oracle, three ISAs | rc 0 |
| S1-S6 | CI-exact strict build + QEMU_SMOKE_STRICT=1 core smoke, three ISAs | rc 0 |
| S7 | CI strict x86 timer grep | inherited debt #6 (0/0/0), same as B |
| U1-U3 | UART/PL011 IRQ witness x86_64 / AArch64 / RISC-V (production Notification creation, mint, bind, signal delivery, non-blocking probe) | rc 0, result=ok |
| W1-W3 | IPC-DISPLACE1 live witness SMP1, three ISAs | witness=ok core_smoke=ok |
| M1 | reply-direct matrix seal | rc 0 |
| M2-M4 | server-dies x86_64 / AArch64 / RISC-V (live process teardown + reap: the converted teardown bodies) | rc 0 |
| C1 | default x86 rebuild on F | rc 0 |
| C2 | CI profiles on F: smp2-core smp4-core smp2-sender-wake smp4-sender-wake sched-timeout | smp*-core pass; #2/#3/#4 inherited |
| C3 | default x86 rebuild on B | rc 0 |
| C4 | same profiles on B | same set and signatures as C2 |
| C5 | reply-timeout matrix and SMP-request script on F and on B | inherited #1/#5 on both |
| Z | every QEMU log of F grepped for `NOTIF_REF_OVER_RELEASE`, `NOTIF_REF_RELEASE_FULL`, `NOTIF_WAITER_` | 0 files (no live blocking notification receive exists) |

Classification: hosted proof (H*: the kernel receive primitive, signal, timeout, teardown and
lifetime owners, with deterministic phase handshakes); live witness coverage (U*: the production
notification object, route and signal delivery, unchanged by this package; W*/M*: the receive
family's unwind and the converted teardown bodies); production-default reachability: none — no
syscall reaches the blocking notification receive on any ISA or build.

After the matrix: doc commit D on top of F (docs only). On D: H1, H3, H4 rerun; `git diff F..D`
touches only doc/*.md.
