#### FUTEX-TIME1 (from delivered `6ba03af9`)

**The question.** Can `FutexWait` take a timeout whose three possible owners — a wake (including
the wake portion of `FutexCmpRequeue` and a robust exit wake), the deadline, and exit / teardown —
claim each wait cycle exactly once, with the result ring 3 sees on every ISA naming the owner that
actually won, with no lost wake, no wake reported after a timeout, no timeout lost on a full run
queue, no recycled slot or TID answered with another cycle's result, and no user thread having to
run for a timeout to fire? No user-visible clock is added (ABI v11 has none; a relative timeout
needs none); `clock_gettime`, `nanosleep`, `clock_nanosleep` and a vDSO are out of scope.

**The ABI** (`SYSCALL_ABI.md`, *Timed waits*). `FutexWait(addr, expected, flags | FUTEX_FLAG_TIMED
(bit 1), timeout_ns)`; `timeout_ns` is a RELATIVE `u64` duration in nanoseconds from admission.
The kernel converts it once, at admission, on the CPU's own scheduler tick: `0` → deadline now (a
matching word times out at once, a differing word still answers `0`); otherwise `ceil(ns / period)
+ 1` ticks (period 0.8 s x86_64, 50 ms AArch64, 10 ms RISC-V, each from the constant that programs
the timer); the `+ 1` makes the timeout a lower bound. An overflowing deadline is `InvalidArgs`
(`checked_add`). Results: `0` mismatch, `1` a wake claimed the cycle, `TimedOut` (9) the deadline
claimed it. The bit was reserved (`InvalidArgs` on `6ba03af9`) and slot 3 is not read without it,
so the ABI version stays 11 and untimed waits are unchanged. `yarm-user-rt` gains
`futex_wait_timeout(addr, expected, scope, timeout: Duration) -> Result<FutexTimedWait,
SyscallError>` (`ValueChanged` / `Woken` / `TimedOut`; durations beyond `u64::MAX` ns clamp).

**The arbitration.** The deadline is attached to the waiter's record in the SAME compare-and-register
hold (task 2 → VM 5 → memory 6) as the untimed registration, after the comparison and after one
atomic clock read; a reached deadline is `TimedOut` with nothing published. From then on every
claimant acts on the record under the task lock by its exact `{slot, generation}` token and the
waiter's `Blocked(Futex(token))` status: a wake (the FUTEX-WAKE-CAP1 sequence: plan, reserve on
the home CPU, claim, reserved commit), the timeout (the same four steps, from a collector), a
requeue (moves the record WITH its deadline; a requeue whose wake portion has committed to the
waiter owns it, and the timeout skips a `Pending` key), and exit / reap / teardown (retire). The
claim retires the record and publishes exactly one `FutexWaitEnding` completion; every later
claimant finds nothing. Each timed cycle advances the TCB's `futex_wait_generation`, which the
completion carries, so a stale completion never answers a later cycle and a reused slot or numeric
TID never matches.

**Capacity (Option B, reserve then claim).** A timeout claims only after reserving a run-queue slot
on the waiter's home CPU. If the reservation is refused, the timeout still wins: under the task lock
the record moves to the kernel-private key `FutexKey::Expired { tid }` (no user word, so no wake or
requeue can reach it; logged `FUTEX_TIMEOUT_EXPIRED_UNPLACED`), the task stays `Blocked` (never
`Runnable` without a slot), and a later collector pass reserves again and places it with
`TimedOut`. A reserved commit that is refused is logged `FUTEX_WAKE_RESERVED_COMMIT_REFUSED`.

**The collector.** `run_due_futex_timeouts_at` runs on every CPU's trap tail (after the trap's own
locks are released) and in the kernel timer thread, including on idle CPUs (each CPU's timer keeps
firing while it idles; CPU 0 owns the scheduler clock). With no timed waiter it costs one relaxed
atomic load. Otherwise one pass examines at most 64 timed records from a wrapping cursor and hands
out at most 8 due candidates, then per candidate: plan, reserve, expire-on-refuse, claim, reserved
commit. Every timed record is therefore examined within a bounded number of passes; no user thread
has to run or poll.

**Per-ISA results.** The completion is delivered by the resume boundary that already consumes IPC
completions (`take_resume_completion`, widened to the `FutexWait` class) and by the thread-restore
facts. RISC-V and AArch64 write `a0` / `x0` from it; a parked timed wait's provisional result is
never `1` (the split route's precommit applies to untimed waits only). *x86_64* (found live, dev
run r1, repaired in `e4f6c0bb`): a timed wait resumed on ANOTHER task's trap was encoded into the
frame's return fields only, while the vector epilogue restores the continuation's registers, so ring
3 saw `InvalidNumber` / `Internal`. `encode_x86_blocked_completion` now writes the result lanes and
`commit_x86_syscall_returns` installs them into the registers the epilogue restores (t42, nc28,
L-nc28).

**Tests** (`futex_requeue1::time1`, 45, `t01`…`t45`): untimed NR 9 unchanged; flag / argument
refusals and overflow; zero and reached deadlines; deadline exactly reached and not reached; wake
before, after and racing the timeout's reservation, claim and the scheduler enqueue; full home
queue (timeout commits without placement, then places with `TimedOut`); requeue before and after
expiry, across two and three keys, with a `Pending` wake; private / shared key transitions and
hash collisions; timeout and wake during the split park publication; exit before expiry and after
the claim, reap and numeric-TID reuse, slot and generation reuse, address-space generation reuse,
mapping teardown; simultaneous collectors, idempotent overdue scans, cursor wrap and refusal; all
user threads blocked; timer progress; multi-CPU delivery; the parked return value on each ISA; the
x86_64 switched resume in registers; the in-lock resume take and the thread-restore facts each
delivering once; a stale cycle never answering a later one; no duplicate enqueue or completion; no
deadline, reservation or record leak; and the collector's bound with up to 512 timed waiters.
Plus `yarm-user-rt` unit tests for the wrapper.

**Negative controls** (each reintroduces one defect into the candidate; the full hosted lib per
control at F'' = `fb140666`; every control built — the one `^error` line in each log is cargo's
closing `error: test failed`; the unmodified tree passes 6605, 0 failed):

| id | defect | failed by behaviour |
|---|---|---|
| nc1 | a wake that wins a timed cycle publishes `TimedOut` | 5, incl. `t08`, `t11`, `t17` |
| nc2 | the timeout publishes `Woken` (success after the timeout won) | 24, incl. `t06`, `t09`, `t10` |
| nc3 | the timeout claim leaves the record registered (duplicate collection; both claim) | 5, incl. `t06`, `t14`, `t22` |
| nc4 | a wake of a timed cycle publishes no completion | 5, incl. `t08`, `t11`, `t17` |
| nc5 | a reached deadline parks (zero timeout) | `t03`, `t07` |
| nc6 | the timeout ignores an NR 17 `Pending` key | `t17` |
| nc7 | the collector times out waits that are not due | 6, incl. `t05`, `t21` |
| nc8 | capacity refused: the waiter made `Runnable` anyway (retired before placement) | `t13`, `t14` |
| nc9 | ticks without the partial-period `+ 1` | `t02`, `t34` |
| nc10 | a retired timed record is not uncounted | 4, incl. `t06`, `t14`, `t24` |
| nc11 | the arm does not advance the wait generation (cycle identity = bare TID + ASID) | `t45` |
| nc12 | the split route precommits `1` into a timed parked frame | `riscv_park_ret1::g04` |
| nc13 | the resume boundary never consumes a `FutexWait` completion | `t43` |
| nc14 | the collector cursor never advances | `t41` |
| nc15 | the timeout read without `FUTEX_FLAG_TIMED` | `t02` |
| nc16 | the deadline wraps instead of refusing | `t02` |
| nc17 | the timeout claim without the exact `Blocked(Futex(token))` check | only the census guard `stage199d_wa2b_wake_owner_census` — structural, NOT counted (masked by `timeout_claimable`'s live check) |
| nc18 | expiry leaves the record on its user word (wake and timeout both claim) | `t13` |
| nc19 | the scan window unbounded | `t41` |
| nc20 | the deadline attached after the registration lock is released | 32, incl. `t05`, `t06`, `t08` |
| nc21 | a timeout lost when its home queue is full | `t13`, `t14` |
| nc22 | cancel by the original key after requeue | `t15`, `t19` + 1 |
| nc23 | resurrect an exiting task | 5, incl. `t24`, `t25`, `t26` |
| nc24 | an unplaceable due waiter skipped forever | `t14` |
| nc25 | the wrong parked result on RISC-V (`a0` success whatever ended it) | `t36`, `t37`, `t43` |
| nc26 | the timeout result lost in the thread-restore facts | `t44` |
| nc27 | the clock stops advancing while the system is idle | `u9tm_timer_entry`, `sl1_only_the_clock_owner_advances_time…` |
| nc28 | x86_64: the completion reaches the frame fields, not the restored registers | `t42` |
| nc29 | a retired record's deadline kept with its slot, taken by the next registration | 7, incl. `t18`, `t24`, `t26` |

28 counted (nc17 not). At F (`04a09b2b`, before the test commit `27956ed9`) nc11, nc13 and nc26
survived the full hosted lib — test gaps, not defects; Addendum B added `t43`–`t45` and nc29, and
all four are caught at F''. Both runs are kept.

**The live witness** (`futex-time-witness`, default off; selector 18 in the FUTEX-WAIT witness
init-cell pattern; one waiter thread W issues every timed wait; the kernel logs the deadline
computation, the registration, each WAKE / TIMEOUT / LOST claim and the G windows from inside the
deciding transactions; `scripts/grade-futex-time-witness.py` fails closed on any damaged marker and
requires exactly one terminal claim per cycle matching W's ring-3 decode):

* *A / F* — three timeouts with every other cell thread parked, the clock advancing ≥ `ticks`, ≥ 1
  collected on an idle CPU (`current=0`);
* *B* — woken, then a second wait times out after the first deadline (the woken cycle never ends
  again);
* *C* — requeued, times out on the TARGET word, neither word's wake finds it after;
* *D* — requeue-and-wake: woken on the target, then a timed-out second wait;
* *E* — 64+ fillers saturate W's home queue to a `SchedulerFull` spawn refusal; W's timeout is
  `EXPIRED_UNPLACED`, no wake finds it, and it is later placed with `TimedOut`;
* *G* — the race, causally synchronized: after a G timeout claims the cycle, the collector opens a
  window only while another thread of W's address space is CURRENT on another CPU (otherwise `WINDOW_SKIPPED` / `no_peer`, a
  bounded refusal, never a window held open for a task that cannot run), rechecks the peer every
  65 536 polls and ends after 8 misses; every in-window wake must find nothing (`woke=0`); coverage
  needs ≥ 1 window ended by a cross-CPU wake, ≥ 1 G cycle won by a wake and ≥ 1 by the timeout.

| run | tree | result |
|---|---|---|
| LT-riscv64-1..3 | F `04a09b2b` | `result=ok` ×3 (`g_woken` 5 / 8 / 9, `g_timed_out` 27 / 24 / 23, windows woke 11 / 11 / 12, E 128 fillers, `SchedulerFull`) |
| LT-aarch64-1..3 | F | `result=ok` ×3 (`g_woken` 11 / 12 / 11, windows woke 10 / 15 / 16, refused 6 / 3 / 2) |
| LT-x86_64-1 | F | **FAILED** — call 41 (E) never returned (the inherited last-slot starvation, below) |
| LT-x86_64-2, -3 | F | `result=ok` ×2 |
| L-nc2 (RISC-V), L-nc28 (x86_64) | F + nc | FAIL, as required: 21 G + 3 A `kernel TIMEOUT but ring 3 saw woken`; 32 G `kernel WAKE but ring 3 saw error InvalidNumber` |
| LW ×3 ISAs, LE | F | **FAILED to build** — `futex-wait-witness`'s outcome marker lacked the `TimedOut` arm (E0004), a defect of F (Addendum C) |
| LS ×3 ISAs | F | IRQ saturation witness `result=ok` |
| ID-<isa> ×3 | F'' `fb140666` | time-witness kernel and initramfs byte-identical to LT-<isa>-1's — the LT and L-nc evidence holds for F'' |
| LW-<isa>-C ×3 | F'' | `witness=ok core_smoke=ok`, `parked_err=0` (`parked_ok` 23 / 2 / 13) |
| LE-C | F'' | `result=ok`, `lost_a3=0 lost_a7=0 lost_other=0` |
| LT-x86_64-4..6 | F'' | `result=ok` ×3, each with an `EXPIRED_UNPLACED` recovery |
| LS-<isa>-D ×3 | F'' | LIVED_RESULT |

The x86_64 time witness passed 5 of 6 runs; run 1 stays FAILED. Development runs before the freeze
(`dev-*`, kept, not qualification) include x86_64 r1, which found the resume-lane defect above.

*LT-x86_64-1, attributed* (Addendum C). E's timeout was claimed on CPU 1 with its reservation
GRANTED: it took the last free slot of CPU 1's queue (`MAX_RUN_QUEUE` 64) while CPU 1's current
task (a filler) was outside it. From then on CPU 1 cannot rotate: re-enqueueing the current task is
a user push under the same limit, refused, and `on_preempt_selection` / `preempt_reenqueue_only`
keep `current` (47 120 `YIELD_SPLIT_REFUSED reason=reenqueue_failed`, 416
`TIMER_SPLIT_PREEMPT_DEFERRED … settlement=continue_current`); nothing queued on CPU 1 is dispatched
again. FUTEX-TIME1 changes none of `scheduler.rs`, `yield_txn.rs`, `scheduler_timer.rs`,
`task_enqueue.rs`, `spawn_reservation.rs`. Reproduced deterministically on `6ba03af9` with an
UNTIMED `FutexWake` into the last free slot (scratch test, never committed), and at F' with both the
untimed wake and a timed-out placement. Classified as an inherited scheduler defect, separate from
the six baseline debts, d2-send and the idle-oracle variance (`KERNEL_UNLOCK_DEFERRED_DEFECTS.md`);
not repaired here (scheduler policy).

*The feature sweep* (Addendum C): every kernel feature check-compiled per ISA at F' (132 checks) and
at `6ba03af9` (129). F' adds `futex-time-witness` ×3, all pass; the only F'-fails / B-passes entry
was `futex-wait-witness` ×3, repaired in `fb140666` (feature-gated code only). 22 umbrella-feature
checks (`kstack-probe*`, `kthread1-witness`, `lock-witness`, `sched-lock1-witness`, x86_64
`apsched-witness`) fail identically on `6ba03af9`: inherited, recorded, not touched.

QUALIFICATION_BLOCK

**Locks and footprint.** Ranks: scheduler 1 (per-CPU 1L, admission 1G), task 2, VM 5, memory 6. No
new lock, multi-lock helper, global fallback or all-CPU acquisition; every scheduler acquisition in
the new code is made after the task lock is released (no 2 → 1 nesting). Timed admission: +1 atomic
load, otherwise the untimed path's acquisitions. A wake of a timed waiter: unchanged. A timeout: one
task-lock scan per pass, then per candidate 2, 1L, 2, 1L — a wake's own shape. Collector with no
timed waiter: one relaxed load per trap tail. Per pass ≤ 64 examined, ≤ 8 due; measured (hosted
debug) 25 µs at 1 waiter to 183 µs at 440 (the per-candidate plan walks the TCB table, as
`FutexWake`'s does). `FutexWaiterTable` 26 888 → 35 088 B (+512 × 16 B deadlines, the count and the
cursor); `ThreadControlBlock` 1 904 → 1 920 B (`futex_wait_generation`); `BlockedSyscallCompletion`
unchanged; collector stack 192 B + locals. FOOTPRINT_TEXT

**Limits.**
* *Relative timeouts only*, on the scheduler tick: a lower bound, ending up to a tick (0.8 s on
  x86_64) plus scheduling latency later. No absolute deadline, no clock id, no user-visible clock —
  the unified timekeeping API is a later stage.
* *A timed-out waiter on a full home queue* waits, `Blocked`, until a later pass finds room; with the
  inherited last-slot starvation above, a CPU whose queue is full while its current task is outside
  it may never give that room.
* *Not in this stage:* bitsets, PI futexes, `FUTEX_WAIT` on an absolute clock, RT scheduling.
* *Still open:* the six baseline debts and d2-send, unchanged; the idle-oracle variance
  (IDLE_ORACLE_LINE); the IPC timeout drain's discarded `QueueFull` and the x86_64 blocked-completion
  consumers question (recorded 2026-11-13, unchanged).

VERDICT_LINE
