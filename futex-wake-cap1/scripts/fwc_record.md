#### FUTEX-WAKE-CAP1 (from delivered `7522b866`)

**The defect, reproduced.** `MAX_TASKS = 512`; a per-CPU priority queue holds
`MAX_RUN_QUEUE = 64` user entries (kernel threads have 32 more of their own). A FutexWake claimed
every waiter first — `Runnable`, futex record retired, in one rank-2 section — and only then
enqueued each one, logging and skipping any refusal (`FUTEX_WAKE_ENQUEUE_DEFERRED` /
`FUTEX_WAKE_ENQUEUE_SKIPPED`). On one CPU with no backlog, 65 Normal-priority waiters and
`FutexWake(65)`: the queue already holds the idle task, so the 64th and 65th waiters were
`Runnable`, on no queue and in no current slot, with no record, and the wake returned 65. With 10
tasks already queued and 60 woken, 7 were stranded. Both the broad and the split path, on
`7522b866` (`futex_wake_cap1_baseline` w1–w3, all failing there).

**Every refusal class, separately.**

| refusal | reachable for a claimed futex waiter on `7522b866`? | now |
|---|---|---|
| `QueueFull` | yes — the defect | reserved before the claim; a refused reservation ends the wake with the waiter parked |
| `CpuOffline` / `InvalidCpu` | only if the home CPU is not online (no CPU goes offline today) | refused at the reservation, before the claim (c9) |
| `WakeOnly` | only if the home CPU is wake-only (homes are fixed at a placement a wake-only CPU refuses) | refused at the reservation |
| `AlreadyQueued` | yes: a waiter still current mid-publication; its own read-back owns its placement | reserved commit reports `AlreadyPlaced`; counted (c6) |
| enqueue-policy refusal | only for a TID with no live TCB or a spawn reservation (never `Running`, so never registered) | the plan precedes the claim; a refusal ends the wake |
| concurrent exit / death | between claim and enqueue, an `Exited` task was enqueued (stale, discarded at dispatch) | before the claim: the claim fails, the reservation is released, the next waiter is woken (c7); after the claim: the reserved commit lands, nothing leaks (c8) |
| another owner moves the waiter without retiring its record | not distinguished | the token-exact claim refuses; reservation released (c11) |
| split park versus wake | handled by the park's read-back | unchanged; the reserved commit never refuses capacity in the window |

**The contract.** Every waiter a FutexWake claims has exactly one continuation: queued exactly
once, or already current and owned by its own park. The count is exactly those. A waiter whose
placement cannot be guaranteed is never claimed: it keeps its record and its `Blocked(Futex)`
status, and a later wake reaches it. No waiter is `Runnable` and unplaced.

**Why not a larger queue.** Measured: a `CpuCell` is 5.6 KiB and the 64-CPU cell array 352 KiB,
which `SchedDomain::new` builds as a local array (the boot stacks are 16 MiB; the hosted test
threads 2 MiB). Rings covering 512 user tasks are 1024 slots each: about 27 KiB per CPU and
1.7 MiB for the array — static memory up by ~1.4 MiB per image, an in-place construction needed for
the hosted builds, and the membership table's linear fallback and `remove_tid` shifts growing
8×. And it would not be a proof: a task that exits while queued stays `Queued` until a dispatch
discards it (probed: `Exited(0)` / `Queued(CpuId(0))`), so occupancy is not bounded by the live
population. A pending-ready list would need either the same per-CPU storage or a drain hook in
every dispatch path on three ports. A re-park after a refused enqueue races the split park's own
recovery (it could re-park a waiter whose read-back is in flight).

**The repair: the run queue is the reservation again.** `PriorityScheduler` gains a per-priority
`reserved` count. Every user push treats reservations as occupied (`len + reserved <
MAX_RUN_QUEUE`, applied at the one push site, `enqueue_with_priority`); `reserve_user_slot` is
refused exactly when a push would be; `enqueue_reserved_with_priority` consumes the reservation
and cannot be refused for capacity — at the reservation `len + reserved < 64`, later user pushes
keep `len + reserved <= 64`, and kernel threads add at most 32 entries, so `len < 96 =
MAX_QUEUED_PER_PRIORITY` (the ring's physical bound) when the fill runs. The owners live with the
existing commit (`task_enqueue::reserve_enqueue_on_cpu_locked`,
`release_enqueue_reservation_on_cpu_locked`, `commit_reserved_enqueue_on_cpu_locked`), and the
futex claim becomes two owners: `next_futex_waiter_locked` (the oldest live waiter, without
claiming it) and `claim_futex_waiter_locked` (exactly that waiter, token-exact). Both wake paths
run the same protocol per waiter, oldest first: find and plan (rank 2, THE planner and home rule),
reserve on the home CPU (rank 1), claim (rank 2; a failed claim releases the reservation), the
reserved commit (rank 1; the split path then announces it, `notify_enqueued`, and the SCHED-LOCK1
witness claims it). A refused reservation stops the wake, which keeps FIFO: no later waiter is
woken past an earlier one that could not be. The loop is bounded by `count` plus the waiters
other owners take concurrently (≤ 512).

**Footprint.** Memory: `PriorityScheduler` +24 bytes (`[usize; 3]`), 1.5 KiB across the 64-CPU
array; no new allocation. Stack: both wake paths drop their 4 KiB `[u64; MAX_FUTEX_WAITERS]`
claimed-TID array. Contention: per woken waiter, the split path takes six short acquisitions
(find, plan, home, reserve, claim, commit) where it took three after one batch claim; none nests,
each is bounded as before, and a refused reservation costs one more acquisition and ends the
wake. The broad path takes the whole-domain scheduler view twice per waiter (reserve; commit)
where its `enqueue_task` took it once. Scheduler complexity: one comparison added to each user
push; no queue, membership or dispatch change.

**User-visible semantics.** `FutexWake` can return less than `count` while waiters remain, when
their home queue is full. Those waiters are parked, not lost; a broadcast that must reach everyone
re-issues the wake (documented in `SYSCALL_ABI.md`). Run-queue capacity below the live population
remains SC-POOL-3 (RT-SCHED1's).

**Re-derived guards.** 191b (both wakes reserve, claim, then fill, in that order; the batch claim
may not return), AP-SCHED1 (the reserved commit is an enqueue commit and is announced), the 199D
census (the claim's new name and neighbourhood; the CANNOT proof's closure fact is now that the
candidate must name the current incarnation of a record on the woken key), and the SCHED-LOCK1
grader (the reserved commit is a claim site of its own: 5 → 6).

**Tests.** `futex_wake_cap1_baseline` w1–w3 (fail on `7522b866`, pass now) and `futex_wake_cap1`
c1–c11: 65 waiters on one CPU (63 placed, FIFO, 2 parked with records, count exact); a partly
filled queue (53 placed); repeated wakes never double-place, and the rest are woken once there is
room; the shared key; broad and split parity; a wake in the publication window (counted, caller
restored); death after the reservation (released, next waiter woken); death after the claim
(nothing leaks); a refused placement (offline home CPU) claims nothing; the reservation
arithmetic including the kernel-thread headroom; a waiter moved by another owner is not claimed.

**Hosted negative controls** (`fwc_nc.py`, disposable worktree). The first run, at `84ed5cd4`, is
kept: NC7 — a claim that checks only the TID — passed every test, because exit and death retire
the record and so the token check never decided anything there. `c11` was added for the case the
check exists for (`d64a2f2e`), and all eight controls were rerun there. All are detected, each by
behavioural tests; the unmodified tree passes all 146 tests in the subset (`futex`, `robust`).

| Control | Defect reintroduced | Detected by |
|---|---|---|
| NC1 | claim before the placement is secured (the baseline order) | w1–w3, c1–c5, c7, c9, c11 |
| NC2 | the reservation ignores the capacity bound | c1–c4, c10 |
| NC3 | a failed claim keeps its reservation | c7, c11 |
| NC4 | a reserved fill does not consume its reservation | c1–c8, c10, c11 |
| NC5 | ordinary user pushes ignore reservations | c10 |
| NC6 | a waiter already placed (mid-publication) is not counted | c6, F5, F7 |
| NC7 | the claim checks the TID only, not the waiter's state | c11 |
| NC8 | the wake takes the newest waiter, not the oldest | c1, c3, c7, c11, F8, F11 |

**Qualification of `d64a2f2e`** against `7522b866`, on a predeclared schedule (`fwc_schedule.md`,
hashed with the runner); steps sequential; nothing retried.

| Step | Result |
|---|---|
| fmt; clippy `--features hosted-dev` | clean; 424 warnings, identical classes and counts to `7522b866` |
| hosted lib (serial) | 6442 passed, 0 failed, 3 ignored |
| integration: broad-lock census, doc-fragmentation guard, SCHED-LOCK1 grader | 41 binaries, 396 passed |
| subsets: futex_wake_cap1, futex_found1, futex, robust, scheduler, sched_, enqueue, ap_sched1, stage199d, stage191b, stage191d, u9qa, kthread, exit, reap, spawn, thread, fork, ipc_displace1, sender_wake | all pass |
| lock order, common test set | 15 changed (test, pair) entries: 13 in futex-wake tests (class a), 2 in `c09` (class b); nothing nested |
| default builds ×3; recv-v2 oracles ×3; CI-exact strict builds + strict core smokes ×3 | pass |
| first-cohort futex seal on the CI-exact artifacts | F: 12/12 live cells; its RISC-V idle cross-check missed. B (same run, its own CI-exact artifacts): 12/12, missing BOTH the AArch64 and RISC-V idle cross-checks. In F's RISC-V idle boot another task (tid 2) was runnable at the wait, so the drain correctly switched to it rather than idling and the idle-only marker could not be emitted — the same signature `a7a27fb4` showed in FUTEX-FOUND1's rerun. The wait path is unchanged here; run-to-run variance, not a regression |
| IPC-DISPLACE1 SMP1 ×3 | `witness=ok core_smoke=ok` |
| overtaken witness x86_64, AArch64 (cross-CPU, cross-process shared-key wake) | `result=ok` on F and B |
| AP-scheduling SMP2 ×3; KTHREAD1 SMP2 ×3 | `result=ok` ×3; `result=pass` ×3 |
| SCHED-LOCK1 witness SMP2 ×3 — no lost or duplicated wake, one placement per task, claims matched with notifications, now including the futex reserved commit | `SL1W_SEAL result=pass` ×3 |
| UART / PL011 / UART IRQ witnesses; reply-direct matrix; server-dies ×3 | pass |
| CI profiles, F and B | identical status and identical error lines; inherited #2, #3, #4 and d2-send |
| reply-timeout matrix; SMP-request script; CI strict timer grep | inherited #1, #5, #6, identical on F and B |
| every QEMU log of F for the marker list (incl. `FUTEX_WAKE_RESERVED_COMMIT_REFUSED`, `FUTEX_WAKE_STOPPED`, SCHED-LOCK1 `result=fail`) | no hits |

**Limits.**

* *Not a capacity change.* A queue still holds 64 user entries per priority (SC-POOL-3, RT-SCHED1).
  The wake now respects that limit without losing a waiter; it cannot create room.
* *Return value.* `FutexWake` may report fewer than `count` while waiters remain; they are
  parked and reachable, and a broadcast must re-issue the wake. Documented in `SYSCALL_ABI.md`.
* *Not changed:* exited tasks still linger in run queues until dispatched (recorded as an open
  observation); the other wake owners' handling of `QueueFull` is outside this stage.
* *Still open:* the six baseline debts and the inherited `d2-send` failure, unchanged; the
  FUTEX-FOUND1 and ENDPOINT-ROLLBACK1 observations, unchanged.

Verdict: **FUTEX_WAKE_CAP1_ACCEPTED_WITH_BASELINE_DEBT**. Refs: `claude/yarm-kernel-unlock-u0-f2zh8p`
and `wip/futex-wake-cap1` at the commit that adds this record; `main` `b81e0317` and every other
ref unchanged.
