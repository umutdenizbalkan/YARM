"""SCHED-LOCK-FAIR1 documentation edits: contract §22 + status lines, and the stage record.
Usage: python3 -I fair1_docs.py <worktree> <contract22.md> <record.md>"""
import sys

wt, c22, rec = sys.argv[1:4]
p = wt + "/doc/SCHEDULING_CONTRACT.md"
s = open(p).read()


def rep(a, b):
    global s
    assert s.count(a) == 1, a[:80]
    s = s.replace(a, b)


rep("* Every trap row may spin on an unfair `SpinLockIrq`, with an unbounded wait (§6 W1).",
    "* Every trap row may spin on an unfair `SpinLockIrq`, with an unbounded wait (§6 W1). The\n"
    "  scheduler locks 1L and 1G are FIFO since SCHED-LOCK-FAIR1 (§22): at most N − 1 earlier\n"
    "  entrants, still no time bound.")
rep("* **SC-INV-1 — Kernel spinlock blocking.** **UNRESOLVED.**\n",
    "* **SC-INV-1 — Kernel spinlock blocking.** **UNRESOLVED.**\n"
    "  * SCHED-LOCK-FAIR1 (§22) makes 1L and 1G FIFO ticket locks: a waiter has at most N − 1\n"
    "    earlier entrants. Every other lock, including ranks 2 and 3 and `PRINTK_DRAIN_LOCK` nested\n"
    "    inside 1L, is still the TTAS kind below, and no section length is bounded (§22.3 A3).\n")
rep("TTAS with no fairness. A bound needs a fair lock **and** every other CPU's maximum hold time. "
    "A weak-CAS spurious failure adds unbounded retries on LL/SC ports in theory. |",
    "TTAS with no fairness. A bound needs a fair lock **and** every other CPU's maximum hold time. "
    "A weak-CAS spurious failure adds unbounded retries on LL/SC ports in theory. **1L and 1G** "
    "(SCHED-LOCK-FAIR1, §22): FIFO ticket lock, ≤ N − 1 earlier entrants; the hold times (A3) "
    "remain unbounded. |")
rep("| SC-LOCK-2 (§12) | **partial**: lock order excludes the scheduler-lock cycles; no progress "
    "argument (§17.3, corrected by SCHED-LOCK1-ACCEPTANCE) | fair lock or bounded-contention proof | S + N | RT-SCHED1 |",
    "| SC-LOCK-2 (§12) | **partial**: lock order excludes the scheduler-lock cycles; FIFO admission "
    "with A1, A2, A4 (SCHED-LOCK-FAIR1, §22) | section bounds (A3): nested rank-2/3 and printk "
    "waits; time on hardware | S + N | RT-SCHED1 |")
rep("| SC-INV-1, W1 for 1L / 1G | TTAS `SpinLockIrq`, now per CPU | fair lock or bounded-contention proof | S + N | RT-SCHED1 |",
    "| SC-INV-1, W1 for 1L / 1G | **FIFO ticket lock** (SCHED-LOCK-FAIR1, §22): ≤ N − 1 earlier "
    "entrants | per-section bounds (A3) | S + N | RT-SCHED1 |")
rep("  shootdown waits. No progress argument is established: the 1L/1G wait is the unfair W1 kind, and\n"
    "  sections can wait on the task-table and printk locks. No starvation-freedom or time bound is\n"
    "  claimed; a fair lock or a bounded-contention proof is still owed (RT-SCHED1).",
    "  shootdown waits. SCHED-LOCK-FAIR1 (§22) makes the 1L/1G wait FIFO: a waiter has at most\n"
    "  N − 1 earlier entrants and, under A1, A2 and A4 (§22.3), is served once each of their sections\n"
    "  completes. Sections can still wait on the TTAS task-table, IPC and printk locks, so section\n"
    "  completion (A3) and any time bound are not established (RT-SCHED1).")
rep("  * SC-LOCK-1, SC-LOCK-3…7 (SCHED-LOCK1). SC-LOCK-2 is PARTIAL: the lock order excludes the\n"
    "    scheduler-lock deadlock cycles, but no progress argument is established (§17.3).",
    "  * SC-LOCK-1, SC-LOCK-3…7 (SCHED-LOCK1). SC-LOCK-2 is PARTIAL: the lock order excludes the\n"
    "    scheduler-lock deadlock cycles (§17.3) and admission is FIFO (SCHED-LOCK-FAIR1, §22), but\n"
    "    section completion and time are not bounded.")
rep("* W1 holds for every 1L and for 1G: TTAS, no fairness, no bound.",
    "* W1 holds for every 1L and for 1G: TTAS, no fairness, no bound. *(Superseded for 1L and 1G\n"
    "  by SCHED-LOCK-FAIR1, §22: FIFO, ≤ N − 1 earlier entrants.)*")
rep("* **Fairness.** None (§6 W1, unchanged).",
    "* **Fairness.** None (§6 W1, unchanged). *(Superseded by SCHED-LOCK-FAIR1, §22: 1L and 1G are\n"
    "  FIFO ticket locks.)*")
s = s.rstrip("\n") + "\n\n" + open(c22).read().rstrip("\n") + "\n"
open(p, "w").write(s)

p = wt + "/doc/KERNEL_UNLOCKING.md"
k = open(p).read()
anchor = "#### SCHED-LOCK2B (from qualified `c9fbc7ef`)"
i = k.index(anchor)
j = k.index("**Verdict: QUALIFIED.**", i) + len("**Verdict: QUALIFIED.**")
k = k[:j] + "\n\n" + open(rec).read().rstrip("\n") + k[j:]
open(p, "w").write(k)
print("docs edited")
