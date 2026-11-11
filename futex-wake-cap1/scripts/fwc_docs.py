#!/usr/bin/env python3
"""FUTEX-WAKE-CAP1 documentation commit. Usage: fwc_docs.py <worktree> <record.md>"""
import pathlib, sys
wt = pathlib.Path(sys.argv[1]); record = pathlib.Path(sys.argv[2]).read_text()
def patch(rel, old, new, count=1):
    p = wt / rel; s = p.read_text()
    assert s.count(old) == count, (rel, old[:90], s.count(old))
    p.write_text(s.replace(old, new))

patch("doc/SYSCALL_ABI.md", '''- `FutexWake` wakes up to `count` waiters of the key `addr` resolves to in the caller's address
  space, oldest first, and returns how many it woke. `count = 0` wakes nobody.''', '''- `FutexWake` wakes up to `count` waiters of the key `addr` resolves to in the caller's address
  space, oldest first, and returns how many it woke. `count = 0` wakes nobody.
- **Every waiter counted is placed (FUTEX-WAKE-CAP1).** A waiter is claimed only after a run-queue
  slot on its home CPU is reserved for it, so a woken waiter is always queued exactly once (or is
  still current, finishing its own wait). When no slot can be reserved — the home CPU's queue for
  that priority is full (64 user entries), or the CPU cannot take work — the wake stops: that
  waiter and every later one stay parked with their records, are not counted, and are reached by a
  later wake. So the return value can be less than `count` while waiters remain; a caller that
  must wake everyone (a broadcast) re-issues the wake until it returns 0.''')

C = "doc/SCHEDULING_CONTRACT.md"
patch(C, "| FutexWake | `futex_wake_split_mut` → `futex_wake_key_split` | trapping CPU | 5 → 6 (key), then 2 (keyed claim), then 1 per woken | masked | caller continues | ≤ 512 record visits in one bucket, O(1) each; ≤ `count` enqueues (FUTEX-FOUND1) |",
         "| FutexWake | `futex_wake_split_mut` → `futex_wake_key_split` | trapping CPU | 5 → 6 (key); then per waiter 2 (find, plan), 1 (reserve), 2 (claim), 1 (reserved commit) — alternating, never nested | masked | caller continues | per waiter: ≤ 512 record visits in one bucket, O(1) each, and ≤ 4 acquisitions; ≤ `count` waiters (FUTEX-FOUND1, FUTEX-WAKE-CAP1) |")
patch(C, "| W10 | FutexWake | `futex_wake_split_mut` | **S** | one key resolution, then ≤ 512 record visits in one hash bucket (each an O(1) TCB-slot check) + ≤ `count` (≤ 512 live waiters) enqueues. FUTEX-FOUND1. |",
         "| W10 | FutexWake | `futex_wake_split_mut` | **S** | one key resolution, then per placed waiter one bucket walk (≤ 512 O(1) record visits) and four short acquisitions; ≤ `count` (≤ 512) waiters, plus at most 512 waiters taken concurrently by other owners. Each single acquisition is bounded as before. FUTEX-FOUND1, FUTEX-WAKE-CAP1. |")
patch(C, '''  * Today's 64-per-level ring can refuse an admissible wake (`QueueFull`) with 512 TCB slots.''',
         '''  * Today's 64-per-level ring can refuse an admissible wake (`QueueFull`) with 512 TCB slots.
  * FUTEX-WAKE-CAP1 makes the futex wake respect that refusal without losing a waiter: it
    reserves the slot before it claims, so a refused placement leaves the waiter parked rather
    than `Runnable` and unqueued. The queue is no larger; the population bound is still this
    item's to resolve.''')
patch(C, '''  while TCB slots remain free. Overflow is the explicit `SchedulerError::QueueFull` (SC-POOL-1).''',
         '''  while TCB slots remain free. Overflow is the explicit `SchedulerError::QueueFull` (SC-POOL-1).
  FUTEX-WAKE-CAP1: a queue's user room counts *reservations* — slots promised to a wake that has
  not yet made its waiter `Runnable` (`PriorityScheduler::reserve_user_slot`); every user push
  treats them as occupied, and the reserved fill cannot be refused for capacity.''')

M = "doc/KERNEL_MULTITHREADING_DESIGN.md"
patch(M, '''A futex wake resolves the same key in the waker's address space and claims up to `count` waiters
of that key, oldest first: each is made `Runnable` and its record retired in one task-lock
critical section, then each is enqueued once.''', '''A futex wake resolves the same key in the waker's address space and wakes up to `count` waiters
of that key, oldest first. For each it first reserves a run-queue slot on the waiter's home CPU,
then claims exactly that waiter (made `Runnable` and its record retired in one task-lock critical
section), then fills the reserved slot (FUTEX-WAKE-CAP1). A waiter that cannot be placed is not
claimed and stays parked.''')

R = "doc/AI_AGENT_RULES.md"
patch(R, '''  `undo_futex_registration_locked`, `claim_futex_waiters_locked` — and every check of a waiter
  compares the exact token. Never wake a futex waiter by TID or by VA.''', '''  `undo_futex_registration_locked`, `claim_futex_waiter_locked` — and every check of a waiter
  compares the exact token. Never wake a futex waiter by TID or by VA.
- Never make a task `Runnable` before its placement is guaranteed (FUTEX-WAKE-CAP1). A wake that
  must mutate the task before it enqueues it reserves the slot first
  (`task_enqueue::reserve_enqueue_on_cpu_locked`), then mutates, then fills the reservation
  (`commit_reserved_enqueue_on_cpu_locked`), and releases it if the task was taken by another
  owner. Logging a refused enqueue is not recovery.''')

T = "doc/KERNEL_TEST_RULES.md"
patch(T, '''between registration and the compare-and-clear) arm
`futex::futex_phase_hook::arm_after_register`.''', '''between registration and the compare-and-clear) arm
`futex::futex_phase_hook::arm_after_register`. To force a wake's own windows (after a waiter's
slot is reserved, after it is claimed) arm `futex_phase_hook::arm_wake_step`; the hook gets the
step and the TID and returns whether to stay armed (FUTEX-WAKE-CAP1).''')

D = "doc/KERNEL_UNLOCK_DEFERRED_DEFECTS.md"
s = (wt / D).read_text().rstrip("\n") + "\n"
s += ("- 2026-11-11 | RESOLVED (FUTEX-WAKE-CAP1) — a FutexWake claimed every waiter (`Runnable`, "
      "record retired) before enqueueing it, and logged and skipped an enqueue refused with "
      "`QueueFull`: with 64 or more Normal-priority tasks runnable on one CPU, the overflow waiters "
      "were `Runnable` on no queue and in no current slot, with no record, and still counted. "
      "Reproduced on `7522b866` on the broad and split paths. Resolution: each waiter's slot is "
      "reserved on its home CPU before it is claimed; a refused reservation leaves it parked and "
      "uncounted | src/runtime.rs, src/kernel/boot/exec_state.rs, src/kernel/boot/futex.rs, "
      "src/kernel/scheduler.rs, src/kernel/task_enqueue.rs | found and resolved in FUTEX-WAKE-CAP1\n")
s += ("- 2026-11-11 | OPEN OBSERVATION (pre-existing, unchanged) — a task that exits while queued "
      "stays queued (`Exited`, placement `Queued`) until a dispatch reaches and discards it, so its "
      "entry occupies run-queue capacity in the meantime and a queue's occupancy is not bounded by "
      "its live population. This is why raising the queue to `MAX_TASKS` would not by itself "
      "prove that a push cannot be refused for capacity (see SC-POOL-3) | src/kernel/boot/"
      "restart_state.rs (exit_task), src/kernel/scheduler.rs | noted during FUTEX-WAKE-CAP1\n")
(wt / D).write_text(s)

U = "doc/KERNEL_UNLOCKING.md"
anchor = '''Verdict: **FUTEX_FOUND1_ACCEPTED_WITH_BASELINE_DEBT**. Refs: `claude/yarm-kernel-unlock-u0-f2zh8p` and `wip/futex-found1` at the
commit that adds this record; `main` `b81e0317` and every other ref unchanged.
'''
patch(U, anchor, anchor + '\n' + record.rstrip('\n') + '\n')
print("docs applied")
