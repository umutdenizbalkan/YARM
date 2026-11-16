
## 19. Scheduler design inputs (PRE-SCHEDULER-CLEANUP1)

Facts the next scheduler stage starts from. They are observations and constraints, not decisions;
RT policy, the scheduler lock design and the clock source are out of this record's scope.

### 19.1 Every placement owner reserves before it claims

With SR-PLACE1 the last discard of a refused enqueue is gone. Every owner that makes a blocked task
runnable reserves the task's home slot first (`reserve_enqueue_on_cpu_locked`, under that CPU's 1L
only), claims second, and fills the promised slot last (`commit_reserved_enqueue_on_cpu_locked`,
which capacity cannot refuse); a refused reservation claims nothing and the obligation stays with
its owner (an armed deadline, a parked death, an `Expired` futex key, or, for the shared-region
delivery, the sender's `WouldBlock` before anything is taken). Owners: FutexWake, the futex/sleep
timeout collector, the four IPC timeout / server-death owners, and the shared-region receiver wake
(both producers and the finalizer, broad and split). A new placement owner joins this contract; a
`QueueFull` that is merely returned after a commit is not a placement.

### 19.2 Capacity is occupancy, not population (from the 2026-11-11 ledger observation)

A task that exits while queued stays queued (`Exited`, placement `Queued`) until a dispatch reaches
and discards it. Its entry counts against `MAX_RUN_QUEUE` in the meantime, so a queue's occupancy
is not bounded by its live population, and a reservation can be refused on a queue whose live tasks
would fit. Consequences for the design:

* any capacity proof (e.g. "a push cannot be refused while live tasks ≤ N") must count dead
  entries, or exit must remove them — a removal from the exiting task's home queue, which may be
  another CPU's, under that CPU's 1L;
* a policy that sizes queues to `MAX_TASKS` does not by itself make placement infallible;
* the witnesses now report exactly what a refusal saw (§19.4), which is the measurement to use
  when evaluating either remedy.

### 19.3 Kernel-thread fairness under a user yield storm (KTHREAD1 observation)

While a CPU's user tasks rotate in a yield storm, a READY kernel thread homed there is not
dispatched until the storm ends: the rotation's unfiltered dequeue (`dequeue_highest`, behind
`dispatch_next_selection` and the rotation selections) skips kernel threads in place by design,
and only the acceptance-filtered selection owner runs one. Observed in the sched-time witness on
RISC-V and AArch64 (the kernel thread stops beating for the whole saturation window and resumes
when the fillers park); nothing is lost and it does not depend on a full queue. A scheduler that
gives kernel threads any service guarantee must make the rotation path able to select them (or
bound how long the filtered path can be bypassed); the sched-time witness's `STW_KTHREAD_BEAT`
markers measure the gap.

### 19.4 Saturation is measured at the refusal (SCHED-SAT-EVIDENCE1)

A spawn refusal names only its own target CPU, and rotation changes queue lengths every quantum, so
"spawning refused" or a filler count is no evidence that a given waiter's home queue was full. In
the witness builds (`sched-time-witness`, `futex-time-witness`) and hosted tests,
`reserve_enqueue_on_cpu_locked` records — inside its refusal, under the home CPU's 1L — that
queue's entries and promised slots at the waiter's priority (`kernel::sat_witness`), and the
refused owner logs `SATW_REFUSED class=sleep|futex|recv … queued reserved cap full … facts`. It
changes no decision. Measured on QEMU: a saturated home queue reads `queued=64` or `65` (the
returning occupant's rotation slot), `reserved=0`.

### 19.5 Scheduler ticks are not wall time under saturation (QEMU TCG)

Tick-denominated deadlines (IPC receive timeouts, FutexWait's tick bound) advanced about 1.4× slower
in wall time under a saturated RISC-V TCG guest than when calibrated idle (≈ 42 ms vs ≈ 30 ms per
10 ms nominal tick). Nothing in the kernel assumes otherwise, but any test or policy that aligns a
tick deadline with `CLOCK_MONOTONIC` must allow for it; the sched-time witness enters its timed
receive only after saturation and holds for a 10× tick margin.

### 19.6 printk capacity floor

The default printk ring is 64 records (`PRB_SLOTS_DEFAULT`), the structural floor `MAX_CPUS`: below
it, a simultaneous record from every CPU could lap one still held in flight. Marker-heavy witnesses
keep 1024 (`printk-witness-ring`). A scheduler change that raises the default boot's burst rate (for
example, per-dispatch markers in a default image) must re-measure it.
