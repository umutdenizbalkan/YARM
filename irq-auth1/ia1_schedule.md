# IRQ-AUTH1 — predeclared qualification schedule

Candidate F = 578daf4 (wip/irq-auth1: 82995cf code + 61164db, 578daf4 regressions), worktree
ia1-wt, clean. Baseline B = 41403e1 (canonical dev head, NOTIF-WAITER1), worktree nw1-wt, clean.
Runner: ia1_matrix.sh. Sequential; every step runs regardless of earlier results; nothing is
retried in place (a rerun is appended, never substituted). Raw logs, rcs, durations, HEAD,
porcelain and every ARTIFACT_BINDING line are kept under ia1-evidence/qual.

| id | step | expected |
|---|---|---|
| P0 | provenance | HEAD=F, porcelain 0 |
| P1 | authority source evidence: every `CapObject::Irq` mint, fork exclusion, the attenuating transfer mint, the route writers | captured lines |
| H1 | cargo fmt --check | rc 0 |
| H2 | clippy --lib --features hosted-dev, fresh target | 427 warnings, identical classes to B |
| H3 | hosted lib, serial, CARGO_INCREMENTAL=0 | 0 failed |
| H4 | integration (`--test '*' --no-fail-fast`, includes census, witness-scope and doc guards) | 0 failed |
| H5 | subsets: irq_auth1 irq notification notif_waiter1 notif_lifetime1 irq_generation1 stage21 stage22 stage23 u9f_split ipc_displace1 driver deleg revoke reap transfer fork spawn | 0 failed each |
| H6 | lock-order warnings, per pair, F vs B, on the common test set (`--skip irq_auth1`) | pairs may change only where both domains are among task (2), ipc (3), capability (4) and boot_config (11) — the domains the authorized bind (task → capability snapshot → ipc decision → capability recheck), the test helper's IRQ mint, and a removal report's revalidation (ipc → capability → ipc) touch; every changed pair is attributed to one of those acquisitions; pairs outside that set are equal; the closure guard still proves nothing nested |
| D1-D3 | default freestanding builds, three ISAs | rc 0, kernel_features=none |
| O1-O3 | IPC recv-v2 oracle, three ISAs | rc 0 |
| S1-S6 | CI-exact strict build + QEMU_SMOKE_STRICT=1 core smoke, three ISAs | rc 0 |
| S7 | CI strict x86 timer grep | inherited debt #6 (0/0/0), same as B |
| U1-U3 | UART/PL011 IRQ witness x86_64 / AArch64 / RISC-V | rc 0, result=ok, 8/8 deliveries, `IRQ1_WITNESS_PROVISION_OK`, `IRQ_ROUTE_BOUND` on the port's DTB/ACPI line (x86 GSI 4, AArch64 INTID 33, RISC-V source 10) to notification 0 generation 1 |
| W1-W3 | IPC-DISPLACE1 live witness SMP1, three ISAs | witness=ok core_smoke=ok |
| M1 | reply-direct matrix seal | rc 0 |
| M2-M4 | server-dies x86_64 / AArch64 / RISC-V (live teardown + reap through the removal report) | rc 0 |
| C1 | default x86 rebuild on F | rc 0 |
| C2 | CI profiles on F: smp2-core smp4-core smp2-sender-wake smp4-sender-wake sched-timeout | smp*-core pass; #2/#3/#4 inherited |
| C3 | default x86 rebuild on B | rc 0 |
| C4 | same profiles on B | same set and signatures as C2 |
| C5 | reply-timeout matrix and SMP-request script on F and on B | inherited #1/#5 on both |
| Z | every QEMU log of F grepped for `NOTIF_REF_OVER_RELEASE`, `NOTIF_REF_RELEASE_FULL`, `IRQ_ROUTE_RETRACTED`, `IRQ_ROUTE_STALE_RECLAIMED`, `IRQ_ROUTE_RELEASED` | 0 files (no live revoke of line authority exists; provisioning binds once) |

Classification: hosted proof (H*: authority checks, the rank-3 decision, the lease, delivery
generation checks, with deterministic phase handshakes and owner tests); live witness coverage
(U*: the production provisioning through line authority, the route publication on real
hardware lines, interrupt delivery and acknowledgement on all three interrupt controllers;
M*: live teardown through the extended removal report, without an IRQ capability);
production-default reachability: none — no default image binds an IRQ line and no syscall can.

After the matrix: doc commit D on top of F (docs only). On D: H1, H3, H4 rerun; `git diff F..D`
touches only doc/*.md.
