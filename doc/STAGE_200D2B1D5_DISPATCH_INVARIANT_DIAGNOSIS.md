<!-- SPDX-License-Identifier: Apache-2.0 -->

# Stage 200D-2B1D5 — x86 runnable-task dispatch: diagnosis (fix NOT landed)

Diagnosed from the Stage 200D-2B1D4 live log. **The fix was not implemented in this stage** —
see "Status" at the end. Nothing in the ServerDies path, runner, counters or grading was
touched.

## `D6_LOCAL_DISPATCH_STEP_SPLIT tid=None runnable=2` is not the bug

That marker comes from `d6_genuine_local_dispatch_observe` (`runtime.rs:567`), which is
explicitly **non-mutating** — it reads `current_tid_on(cpu)` and `runnable_count_on(cpu)` and
logs them. `tid=None` therefore means "this CPU is currently running nothing", not "selection
returned nothing". It is an accurate observation of an already-idle CPU, and it is a symptom,
not the cause. Chasing it as a selection failure would have been the wrong repair.

## The actual defect: the restore owner is chosen before the drains that create work

From the live log, in order:

```text
11570  EXIT_TASK_RESTORE_OWNER_PREPARED arch=x86_64 owner=idle exiting_tid=10008 cpu=0 broad_lock=1
11582  IPC_SERVER_DEATH_CALLER_ENQUEUED caller_tid=1 caller_asid=1 enqueues=1        broad_lock=0
11588  EXIT_TASK_POST_LOCK_DRAIN_DONE   arch=x86_64 tid=10008 cpu=0 drains=all       broad_lock=0
11589  SCHED_ENTER_IDLE_HLT cpu=0
11590  EXIT_TASK_COMMON_EPILOGUE_OWNER  arch=x86_64 owner=idle clears=1 frame_committed=1
```

1. The x86_64 exit consumer runs **in-lock** (`broad_lock=1`) and correctly picks `owner=idle`:
   at that instant nothing is runnable.
2. The **post-lock drains** then run — and the server-death drain makes the caller runnable and
   enqueues it (`enqueues=1`).
3. The epilogue commits the decision taken at step 1. The CPU halts holding an idle frame while
   a runnable task exists.

The CPU then wakes on every timer tick, observes `runnable=2`, and re-idles — 220 times until
the boot times out.

**The violated invariant:** *a restore-owner decision taken before the post-lock drains must be
re-validated after them, because the drains are exactly where wakes are published.* Any
post-lock drain that makes a task runnable hits this — the reply-timeout collector and the
server-death drain both can. ServerDies is simply the first path that exercised it, because it
is the first that wakes a task from a drain on a CPU whose owner was already resolved to idle.

Note this is x86_64-specific in an interesting way: AArch64 and RISC-V consume the disposition
**after** the drains (Stage 200D-2B1C, guard `a07`), so their owner selection already sees the
drain's wakes. x86_64 consumes in-lock by design (Stage 200D-0B3), which is correct for
identity coherence but leaves this re-validation gap.

## The shape of the correct fix

Re-validate the owner between the last drain and the epilogue's frame commit, on the x86_64
post-lock path only:

* if the prepared owner is `idle` **and** a task is runnable on this CPU after the drains,
  dispatch it instead of committing the idle frame;
* if the prepared owner is a replacement task, leave it alone — it was chosen from live state
  and the drains cannot invalidate it;
* preserve queue ownership (the dispatch must go through the same
  `dispatch_next_on(cpu)` the in-lock path uses, never a second queue), affinity (select only
  from this CPU's own run queue), and one-at-a-time dispatch (exactly one task selected, no
  re-entry of the drain loop).

The re-check belongs where `EXIT_TASK_COMMON_EPILOGUE_OWNER` is emitted, so the marker reports
the owner that is actually committed rather than the one prepared earlier.

## Status

The diagnosis above is complete and evidence-backed, but **the fix, its regression tests and
its mutation guards were not written**. This stage ran out of working budget after the
diagnosis, and landing a partially validated change to the scheduler/epilogue path — the code
that decides what a CPU returns to — was not a safe trade. No production code, test, runner,
counter or grading was modified; the tree is diagnosis-only.

The next stage should implement the fix as described, with regression tests covering:

* a task made runnable by a post-lock drain is dispatched rather than idled past;
* a prepared replacement owner is not displaced by the re-check;
* exactly one task is selected (no double dispatch, no queue double-advance);
* affinity is respected (a task runnable on another CPU is not stolen);
* the idle path still idles when the drains genuinely produced nothing.
