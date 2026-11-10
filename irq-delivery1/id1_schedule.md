# IRQ-DELIVERY1 — predeclared qualification schedule

Candidate F = 3b7aa56 (wip/irq-delivery1: 1611de3 code, ba0296a regressions, edf4ac9 + 60abd7f +
3b7aa56 default-off saturation witness), worktree id1-wt, clean. Baseline B = c703226 (canonical
dev head, IRQ-AUTH1), worktree ia1-wt, clean. Runner: id1_matrix.sh. Sequential; every step runs
regardless of earlier results; nothing is retried in place (a rerun is appended, never
substituted). Raw logs, rcs, durations, HEAD, porcelain and every ARTIFACT_BINDING line are kept
under id1-evidence/qual.

| id | step | expected |
|---|---|---|
| P0 | provenance | HEAD=F, porcelain 0 |
| P1 | source evidence: every `send_irq` caller, the `Coalesced` outcome, the one completion site, the ports' fatal channels | captured lines |
| H1 | cargo fmt --check | rc 0 |
| H2 | clippy --lib --features hosted-dev, fresh target | 427 warnings, identical classes to B |
| H3 | hosted lib, serial, CARGO_INCREMENTAL=0 | 0 failed |
| H4 | integration (`--test '*' --no-fail-fast`, includes census, witness-scope and doc guards) | 0 failed |
| H5 | subsets: irq_delivery1 u9irq1 u9irqfinal irq_auth1 irq notification notif_waiter1 notif_lifetime1 irq_generation1 stage21 stage22 stage23 ipc_displace1 selected_arch_trap_entry u9f_split revoke reap | 0 failed each |
| H6 | lock-order warnings, per test and pair, F vs B, on the common test set (`--skip irq_delivery1`) | every changed (test, pair) is in one of the three reworked queue-full cases (which now also receive) or is a test already known to vary run to run on B (`c09_reply_vs_timeout_terminal_cas_race`); nothing else changes |
| D1-D3 | default freestanding builds, three ISAs | rc 0, kernel_features=none |
| O1-O3 | IPC recv-v2 oracle, three ISAs | rc 0 |
| S1-S6 | CI-exact strict build + QEMU_SMOKE_STRICT=1 core smoke, three ISAs | rc 0 |
| S7 | CI strict x86 timer grep | inherited debt #6 (0/0/0), same as B |
| U1-U3 | UART/PL011 IRQ witness x86_64 / AArch64 / RISC-V (default profile, unchanged contract) | rc 0, 8/8 deliveries, outcome=delivered ×8 only |
| X1-X3 | IRQ saturation witness x86_64 / AArch64 / RISC-V (`irq-saturation-witness`) | rc 0, IRQSAT_GRADE result=ok: 16 delivered, 2 coalesced, 9 received, claims == completions, no fatal marker |
| K1-K3 | kernel-thread IRQ witness x86_64 / AArch64 / RISC-V | rc 0 |
| W1-W3 | IPC-DISPLACE1 live witness SMP1, three ISAs | witness=ok core_smoke=ok |
| M1 | reply-direct matrix seal | rc 0 |
| M2-M4 | server-dies x86_64 / AArch64 / RISC-V | rc 0 |
| C1 | default x86 rebuild on F | rc 0 |
| C2 | CI profiles on F: smp2-core smp4-core smp2-sender-wake smp4-sender-wake sched-timeout | smp*-core pass; #2/#3/#4 inherited |
| C3 | default x86 rebuild on B | rc 0 |
| C4 | same profiles on B | same set and signatures as C2 |
| C5 | reply-timeout matrix and SMP-request script on F and on B | inherited #1/#5 on both |
| Z | every QEMU log of F grepped for `outcome=failed`, `x86 shared trap dispatch failed`, `YARM_AARCH64_TRAP_HANDLE halting`, `RISCV_TRAP_HANDLE_FAILED`, `KTHREAD_DEVICE_IRQ_DELIVERY_REFUSED`, `NOTIF_REF_OVER_RELEASE`, `IRQ_ROUTE_RETRACTED`, `IRQ_ROUTE_STALE_RECLAIMED`, `IRQ_ROUTE_RELEASED` | 0 files |

Classification: hosted proof (H*: the object owner, both producers, the bridge and the shared
trap dispatch for user and idle origin, exact completion on both tokens, interleavings at the
rank-3 linearization point); live proof (X*: real device interrupts through each port's
interrupt controller saturating the witness Notification — x86_64 and AArch64 from the idle hart,
RISC-V on user returns — with the overflowing interrupts coalesced, completed and later
observed; U*/K*: unchanged ordinary and kernel-thread delivery); the live baseline (the
saturation witness applied to B without the repair) is recorded separately in
id1-evidence/live-baseline. Production-default reachability: none — no default image binds an
IRQ line.

After the matrix: doc commit D on top of F (docs only). On D: H1, H3, H4 rerun; `git diff F..D`
touches only doc/*.md.
