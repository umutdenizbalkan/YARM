# SEND-TIMEOUT-ARB1 — predeclared qualification schedule

Candidate F = d3bc11a0 (wip/send-timeout-arb1: 2ad2145f code + re-derived guards, 6c178c91 and
d3bc11a0 regressions), worktree sta1-wt, clean. Baseline B = 414afb86 (canonical dev head,
IPC-SENDER-ID1), worktree isi1-wt, clean. Runner: sta1_matrix.sh. Sequential; every step runs
regardless of earlier results; nothing is retried in place (a rerun is appended, never
substituted). Raw logs, rcs, durations, HEAD, porcelain and every ARTIFACT_BINDING line are kept
under sta1-evidence/qual.

| id | step | expected |
|---|---|---|
| P0 | provenance | HEAD=F, porcelain 0 |
| P1 | source evidence: the withdrawal, the claim, BlockedInLock, the refusal unwind, the test hooks | captured lines |
| H1 | cargo fmt --check | rc 0 |
| H2 | clippy --lib --features hosted-dev, then class comparison against B | 427 warnings, identical classes |
| H3 | hosted lib, serial, CARGO_INCREMENTAL=0 | 0 failed |
| H4 | integration (`--test '*' --no-fail-fast`: broad-lock census, witness-scope and doc-fragmentation guards) | 0 failed |
| H5 | subsets: send_timeout_arb1 ipc_sender_id1 u3_ordinary_cap_sender_wake u7_production_timeout_promotion u6_offlock_blocking_send_commit u6_frame_exact_envelope_preservation stage169 stage199d_wa2 ipc_displace1 sender_wake send timeout exit reap transfer shared_region u9f_split notif_waiter1 notif_lifetime1 irq_auth1 irq_delivery1 | 0 failed each |
| H6 | lock-order warnings per (test, pair), F vs B, on the common test set (new tests skipped) | every changed (test, pair) is in (a) a re-derived test (ISI1 s2/s3/s6, the claim owner test, the U6 reply-cap and two endpoint-destruction tests), (b) the run-to-run variant `c09_reply_vs_timeout_terminal_cas_race`, (c) a test that drains send-timeout work (B logs `U7_SEND_TIMEOUT_SETTLED`/`REFUSED_STALE`): the drain's added read-only rank-2 precheck and its reordered rank-3→rank-2 claims, or (d) a test in which F ends a cycle by removal (`SEND_ARB_PUBLICATION_REFUSED_UNWIND`, `ENDPOINT_DESTROY…`): the added claim + wake. Nothing else changes; nothing is nested |
| D1-D3 | default freestanding builds, three ISAs | rc 0 |
| O1-O3 | IPC recv-v2 oracle, three ISAs | rc 0 |
| S1-S6 | CI-exact strict build + QEMU_SMOKE_STRICT=1 core smoke, three ISAs | rc 0 |
| S7 | CI strict x86 timer grep | inherited debt #6 |
| W1-W3 | IPC-DISPLACE1 live witness SMP1, three ISAs | witness=ok core_smoke=ok |
| M1 | reply-direct matrix seal | rc 0 |
| M2-M4 | server-dies x86_64 / AArch64 / RISC-V | rc 0 |
| C1/C3 | default x86 rebuild on F / on B | rc 0 |
| C2/C4 | CI profiles on F / on B: sender-wake d2-send smp2-core smp4-core smp2-sender-wake smp4-sender-wake sched-timeout | sender-wake, smp*-core pass; #2/#3/#4 inherited; d2-send the inherited IPC-SENDER-ID1 observation, identical on F and B |
| C5 | reply-timeout matrix and SMP-request script on F and on B | inherited #1/#5 on both |
| Z | every QEMU log of F grepped for `SCHED_WAKE_FAIL`, `SCHED_TIMEOUT_STRANDED_WAITER`, `IPC_RECV_BLOCKED_COMPLETE_FAILED`, `U6_SEND_COMPLETION_REFUSED_STALE`, `U7_SEND_TIMEOUT_CLAIM_AFTER_WITHDRAW_REFUSED`, `SEND_ARB_PUBLICATION_REFUSED_UNWIND`, the ports' fatal channels | reported per file; any hit explained |

Classification: hosted proof (H*: every arbitration interleaving as the production phases in an
order two CPUs can run them, with test-only phase hooks for the broad block's Phase B→C and the
drain's two claims — serial hosted schedules, not concurrent execution); live regression evidence
(S*, W*, M*, C*: blocking-send, sender-wake, timeout, direct IPC and teardown paths on three ports
and under SMP). No live proof of the race itself is claimed: no existing witness can place a
timeout inside another CPU's take→claim window deterministically, and none is added (a new
synchronization point in production would be needed).

After the matrix: doc commit D on top of F (docs only). On D: H1, H3, H4 rerun; `git diff F..D`
touches only doc/*.md.
