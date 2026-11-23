# FUTEX-SEAL1 — development boots after qualification run 2 (the futex-time witness cell)

| boot | candidate | disposition |
|---|---|---|
| time-fix-riscv64.ABORTED-hang-after-C | f3a806f2 | ABORTED (killed by hand after the cell stopped). f3a806f2's ack retry stopped on a flag that W cleared with its next command; H kept retrying M's ack, woke W parked for C's ack, then spun in C's requeue with W no longer on the source word. Corrected in 9bd304d6 (the retry is per command number). |
| time-fix2-riscv64 | 9bd304d6 | FAIL, kept with its grade. E passed (SATW_REFUSED full=1, FUTEX_TIMEOUT_EXPIRED_UNPLACED, one completion, FTW_DONE ok); rule F failed: all three A timeouts were collected with the supervisor current (current=2). CPU 0 was busy at 11 of the 13 ticks spanning A (runs 1/2: about half). Legal collection; the rule's three samples were too few. Corrected in 9d5282c4 (sixteen A samples, rule unchanged). |
| time-fix3-{riscv64,aarch64,x86_64} | 9d5282c4 | see fix3.summary and each grade.txt |
