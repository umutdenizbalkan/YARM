# Qualification run 2 — FAILED (retained)

Candidate acbf79c1 (tree 9d3c6760), baseline c4cac895. Runner sha256 71676468… (predeclared.sha).
Every step ran; one graded step outside the intended controls failed:

- W_time_riscv64 rc=1 (the reused FUTEX-ABS-BITSET1 futex-time witness cell, sequence E). The kernel
  behaved legally; two witness assumptions were wrong:
  1. E's deadline lead (10 s) was shorter than the helper's re-fill under saturation. With both
     queues full the helper ran about once per rotation of its queue (~4 s under TCG) and was
     preempted after almost every call: W registered at boot.log line 114251, settled at 120151,
     and the helper's re-fill spawn of the slot W vacated came at 126280. The timeout found that
     slot free and was placed, so no FUTEX_TIMEOUT_EXPIRED_UNPLACED was emitted (rule E: "the queue
     was not full").
  2. The helper's closing ack was refused for capacity (line 133641: FUTEX_WAKE_STOPPED tid=10010
     cpu=1 reason=no_placement err=SchedulerFull woke=0) while the released fillers still filled
     W's home queue; ack() did not retry, W stayed parked, the boot reached its 1800 s wall timeout
     (U_DONE / U_WAITER_DONE missing).
  Correction: f3a806f2 (witness-only; 60 s E lead on every port, ack retried until W has seen it).
  No grader rule changed. A fresh qualification (run 3) follows on f3a806f2.

All other steps passed (hosted, gates, cross checks, builds, nine core smokes, the six seal boots,
W_time aarch64/x86_64, W_sync on all three ports). The four controls were caught as intended
(hosted ncC and ncB rc=101; live ncA by PB "rq arm made no NR 17 call" with correctness passing;
live ncB by RB own=0 / FSB_END fails=2).
