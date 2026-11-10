# IPC-SENDER-ID1 — predeclared qualification schedule

Candidate F = 811b4867 (wip/ipc-sender-id1: 9347cb2d code + re-derived guards, 811b4867
regressions), worktree isi1-wt, clean. Baseline B = 16e5c04 (canonical dev head, IRQ-DELIVERY1),
worktree id1-wt, clean. Runner: isi1_matrix.sh. Sequential; every step runs regardless of
earlier results; nothing is retried in place (a rerun is appended, never substituted). Raw logs,
rcs, durations, HEAD, porcelain and every ARTIFACT_BINDING line are kept under
isi1-evidence/qual.

| id | step | expected |
|---|---|---|
| P0 | provenance | HEAD=F, porcelain 0 |
| P1 | source evidence: every sender-waiter writer and remover, the claim's callers, the three wake seams | captured lines |
| H1 | cargo fmt --check | rc 0 |
| H2 | clippy --lib --features hosted-dev, then class comparison against B | 427 warnings, identical classes |
| H3 | hosted lib, serial, CARGO_INCREMENTAL=0 | 0 failed |
| H4 | integration (`--test '*' --no-fail-fast`, includes the broad-lock census, witness-scope and doc-fragmentation guards) | 0 failed |
| H5 | subsets: ipc_sender_id1 u3_ordinary_cap_sender_wake u7_production_timeout_promotion u6_offlock_blocking_send_commit u6_frame_exact_envelope_preservation stage199d_wa2 ipc_displace1 sender_wake send timeout exit reap transfer shared_region u9f_split notif_waiter1 irq_delivery1 | 0 failed each |
| H6 | lock-order warnings per (test, pair), F vs B, on the common test set (new tests skipped) | every changed (test, pair) is (a) in a re-derived test (`u3_ordinary_cap_sender_wake::*`, `u7_production_timeout_promotion::the_completion_is_published_before_the_enqueue`, the status census), (b) the run-to-run variant `c09_reply_vs_timeout_terminal_cas_race`, or (c) a test in which B logged `U6_SEND_COMPLETION_REFUSED_STALE` — a refused claim that B followed with a bare wake and F does not; nothing else changes and nothing is nested |
| D1-D3 | default freestanding builds, three ISAs | rc 0, kernel_features=none |
| O1-O3 | IPC recv-v2 oracle, three ISAs | rc 0 |
| S1-S6 | CI-exact strict build + QEMU_SMOKE_STRICT=1 core smoke, three ISAs | rc 0 |
| S7 | CI strict x86 timer grep | inherited debt #6 (0/0/0), same as before |
| W1-W3 | IPC-DISPLACE1 live witness SMP1, three ISAs | witness=ok core_smoke=ok |
| M1 | reply-direct matrix seal | rc 0 |
| M2-M4 | server-dies x86_64 / AArch64 / RISC-V | rc 0 |
| C1 | default x86 rebuild on F | rc 0 |
| C2 | CI profiles on F: sender-wake d2-send smp2-core smp4-core smp2-sender-wake smp4-sender-wake sched-timeout | sender-wake, d2-send, smp*-core pass; #2/#3/#4 inherited |
| C3 | default x86 rebuild on B | rc 0 |
| C4 | same profiles on B | same set and signatures as C2 |
| C5 | reply-timeout matrix and SMP-request script on F and on B | inherited #1/#5 on both |
| Z | every QEMU log of F grepped for `U6_SEND_COMPLETION_REFUSED_STALE` followed by a wake of the same TID, `SCHED_WAKE_FAIL`, `SCHED_TIMEOUT_STRANDED_WAITER`, `IPC_RECV_BLOCKED_COMPLETE_FAILED`, `x86 shared trap dispatch failed`, `YARM_AARCH64_TRAP_HANDLE halting`, `RISCV_TRAP_HANDLE_FAILED` | reported per file; any hit explained |

Classification: hosted proof (H*: the claim, both removals, the three wake seams, the timeout
pipeline and the receive take, in the two-CPU orders the interleavings need — a serial hosted
schedule, not concurrent execution); live regression evidence (S*, W*, M*, C*: the blocking-send,
sender-wake, timeout and teardown paths still behave on all three ports and under SMP). No live
proof of the race itself is claimed: it needs a timeout settled on one CPU inside another CPU's
receive window, which no existing witness drives deterministically.

After the matrix: doc commit D on top of F (docs only). On D: H1, H3, H4 rerun; `git diff F..D`
touches only doc/*.md.
