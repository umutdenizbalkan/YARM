Verdicts are from qualification run 3 (candidate `9d5282c4`). "Live ×3" means the named rule
passed on x86_64, AArch64 and RISC-V SMP2 boots of that run; the seal boots ran both GP and
RT+budget (`gp`, `rtb`).

| Claim | Evidence (rule — boot) | Verdict |
|---|---|---|
| Selective wake: a nonmatching waiter stays parked, keeps its order and consumes no count | OS: A(0x1), B(0x2), C(0x1); wakes select B, then A, then C, identities from the kernel's WAKE markers; A passed over by BITSET_SKIP before B's wake — seal gp/rtb | PASS, live ×3 |
| NR 17 keeps mask and deadline | NR17: X (0x2, 4 s) moved N1→N2 times out ON N2 with its registered deadline and mask; FUTEX-ABS-BITSET1 C (masked requeue, then timeout, deadline and mask unchanged) — seal, time | PASS, live ×3 |
| Wake and expiry after requeue | NR17: V2 woken on N2 (mask 0x4), X expires on N2; time D (requeued, then woken on the target) | PASS, live ×3 |
| NR 17's wake portion is unfiltered | NR17: V (0x8) woken on N1 by NR 17's wake, never skipped | PASS, live ×3 |
| Robust cleanup preserved | FUTEX-ABS-BITSET1 hosted tests (`abs_bitset1` subset, H2/H4 in this run) | PASS, hosted only (inherited): no system call registers a robust list, so there is no live registration path |
| Wake-versus-timeout race, both winners | time G: 32 cycles, both WAKE- and TIMEOUT-won cycles observed, each with one terminal; winner named by the kernel's markers, never by a wake's return | PASS, live ×3 |
| Full-home-queue expiry with pending placement, then one completion | time E: SATW_REFUSED class=futex full=1 for W's home, one FUTEX_TIMEOUT_EXPIRED_UNPLACED, then exactly one final TIMEOUT and W returns timed_out | PASS, live ×3 |
| Timed lock and timeout | sync L1/L2/L3/L3b: one deadline per operation (R4), a timed-out lock leaves the word CONTENDED | PASS, live ×3 (FUTEX-SYNC-TIME1 cell, rerun) |
| Late sole-wake handoff with a follower | LH: rounds whose wake landed at or after W's deadline (late) acquired the mutex; F completed with ownership after W's return; at least one late round | PASS, live ×3 (seal gp/rtb) |
| Timed condvar waiter requeued onto its mutex; its deadline passes while another holds the mutex | sync C3: X registered on the sequence word, requeued by the broadcast, times out ON the mutex word with its registered deadline before the holder's release, returns holding the mutex | PASS, live ×3 |
| `wait_while`'s Satisfied rule and the original deadline | sync C1 (re-wait after a spurious signal returns Satisfied; every admission names the one deadline); runtime hosted `futex_sync_tests` | PASS, live ×3 + hosted |
| RT budget: settlement, Ready but budget-parked, replenishment, runtime return with the mutex; one completion and one rejoin | RB rtb: one INDUCED drain at the collector's commit, park stamped after the drain and before the commit, commit_parked=1, the first release stamped after the commit, one rejoin for that release, W returns timed_out holding and owning the mutex. Deadline graded in CLOCK_MONOTONIC (REGISTER/TIMEOUT now ≥ deadline), budget in global ticks (park tick < release ≤ rejoin tick) | PASS (INDUCED exhaustion), live ×3; gp boots: no budget context, nothing parked |
| Paired broadcast measurement | PB: one oracle for both arms, every round clean in both arms | MEASURED (no threshold), live ×3 × gp/rtb |
