# FUTEX-TIME1 — §19 lock-order, footprint and performance measurements

Candidate F = `04a09b2b`, baseline B = `6ba03af9`. Measured values come from `m01_F.log` /
`m01_B.log` (a temporary hosted test, in throwaway worktrees `ft1m-wt` / `ft1mb-wt`, never
committed; hosted debug profile, x86_64 Linux host, so times are relative, not ISA cycles). Section
sizes per ISA are the matrix's X / FP steps (`qual/X_sections.log`, `qual/FP_footprint.log`).

## Lock order

Ranks: scheduler 1 (per-CPU 1L, admission 1G), task 2, VM 5, memory 6. No new lock, no new
multi-lock helper, no global-lock fallback, no all-CPU scheduler acquisition.

| path | acquisitions (in order; never nested unless shown) | change vs B |
|---|---|---|
| timed wait admission (NR 9 + flag) | clock: one atomic load (`SchedDomain.clock`, no lock); then the SAME single 2 → 5 → 6 compare-and-register hold as an untimed wait, which now also attaches the deadline; then the unchanged compare-and-clear (1L) and read-back (2) | +0 acquisitions; +1 atomic load |
| untimed wait | unchanged | 0 |
| wake of a timed waiter | unchanged FUTEX-WAKE-CAP1 sequence: 2 (find, plan), 1L (reserve), 2 (claim — now also publishes the completion), 1L (reserved commit) | +0 |
| timeout of a due waiter | 2 (scan) once per pass; per candidate 2 (plan), 1L of its home CPU (reserve), 2 (claim, or expire on a refused reservation), 1L (reserved commit) — the wake's own shape | new path, 4 acquisitions per candidate + 1 per pass |
| collector with no timed waiter | one relaxed atomic load (`FUTEX_TIMED_RECORDS`), no lock | new: 1 load per trap tail |
| resume of a timed waiter | the existing IpcSend completion take (2), widened to the FutexWait class | +0 |
| exit / reap | the existing `retire_futex_waiters_for_task_locked` (2); retirement also decrements the timed count | +0 |

There is no rank-2 → rank-1 nesting anywhere in the new code: every scheduler acquisition is made
after the task lock is released (`with_task_*_split_mut` returns before `with_cpu_sched_split`).

## Work bounds

* **Collector per pass:** at most one wrap of the 512 waiter slots, stopping after 64 TIMED records
  examined or 8 due candidates, whichever first; then at most 8 × 4 short acquisitions. Measured
  with N due timed waiters (a dispatch stand-in drains placed waiters between passes):

| timed waiters | examined, not yet due | max examined per pass | max due per pass | passes to settle all | max pass (µs, hosted debug) |
|---|---|---|---|---|---|
| 1 | 1 | 1 | 1 | 1 | 25 |
| 64 | 64 | 8 | 8 | 8 | 37 |
| 256 | 64 | 8 | 8 | 32 | 142 |
| 440 | 64 | 8 | 8 | 55 | 183 |

  The pass time grows with N though the work per pass is capped: the per-candidate plan
  (`plan_balanced_enqueue_locked` → `refuse_reservation_locked`) and the claim's TCB lookups walk the
  TCB table, as `FutexWake`'s identical per-waiter plan does (pre-existing O(MAX_TASKS) per placed
  waiter, unchanged). 512 timed waiters are bounded the same way (window 64, due 8).
* **O(MAX_TASKS) scans added:** the timed branch of compare-and-register checks the waiter's
  incarnation and that its `futex_wait_generation` can advance (two TCB walks, inside the hold the
  untimed path already holds; the untimed path's own registration already walks the TCBs). No scan
  in the wake or exit paths was added.
* **Interrupt-disabled duration:** the collector runs in the post-lock trap tail with interrupts
  masked, like the IPC timeout collector it sits beside; its bound is the pass bound above. With the
  witness feature only (default off), a sequence-G window may spin up to 400 M polls while a peer is
  current on another CPU, rechecking the peer every 65 536 polls and ending after 8 misses.

## Footprint

| item | B | F | delta |
|---|---|---|---|
| `FutexWaiterTable` (static, one per kernel) | 26 888 B | 35 088 B | +8 200 B (512 × 16 B `Option<u64>` deadline, + `timed`, `timeout_cursor`) |
| `ThreadControlBlock` (per task, 512) | 1 904 B | 1 920 B | +16 B (`futex_wait_generation: u64` + alignment) — +8 KiB total |
| `BlockedSyscallCompletion` | 32 B | 32 B | 0 (the class enum gained `FutexWait`) |
| collector stack | — | 192 B (`[Option<FutexTimeoutCandidate>; 8]`) + locals | new |
| deadline registration | — | 16 B in the record, 1 u16 count, 1 global atomic hint | new |
| cleanup on exit | O(512) record sweep (existing) | same; each retired timed record also decrements two counters | O(1) per record |

`.text` per ISA: see `qual/X_sections.log` / `qual/FP_footprint.log`.
