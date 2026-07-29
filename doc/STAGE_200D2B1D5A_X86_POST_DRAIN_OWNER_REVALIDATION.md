<!-- SPDX-License-Identifier: Apache-2.0 -->

# Stage 200D-2B1D5A — x86 post-drain restore-owner revalidation

Implements the fix diagnosed (but deliberately not landed) in
`STAGE_200D2B1D5_DISPATCH_INVARIANT_DIAGNOSIS.md`. **No live seal is claimed, no QEMU was
run, and no ServerDies counter, runner or grading was touched.**

## The invariant that was violated

The x86_64 trap consumer picks the restore owner **in-lock** (Stage 200D-0B3). That is correct
for identity coherence — the `{tid, asid}` incarnation is read under the broad guard that owns
it — but it happens strictly *before* the post-lock drains, and the drains are exactly where
wakes are published. Stage 200D-2B1D4 caught the consequence live:

```text
EXIT_TASK_RESTORE_OWNER_PREPARED  owner=idle exiting_tid=10008 cpu=0   broad_lock=1
IPC_SERVER_DEATH_CALLER_ENQUEUED  caller_tid=1 caller_asid=1 enqueues=1 broad_lock=0
EXIT_TASK_POST_LOCK_DRAIN_DONE    tid=10008 cpu=0 drains=all            broad_lock=0
SCHED_ENTER_IDLE_HLT              cpu=0
EXIT_TASK_COMMON_EPILOGUE_OWNER   owner=idle clears=1 frame_committed=1
```

The CPU halted holding an idle frame while a runnable task existed, then woke on every timer
tick, observed `runnable=2`, and re-idled — 220 times until the boot timed out.

**The invariant:** *a restore-owner decision taken before the post-lock drains must be
re-validated after them.* This is x86_64-specific: AArch64 and RISC-V consume the disposition
*after* the drains (Stage 200D-2B1C, guard `a07`), so their owner selection already sees the
drains' wakes.

## The seam — `SharedKernel::revalidate_idle_owner_after_drains`

`src/runtime.rs`, `#[cfg(target_arch = "x86_64")]`:

```rust
pub(crate) fn revalidate_idle_owner_after_drains(
    &self,
    cpu: CpuId,
    frame: &mut crate::kernel::trapframe::TrapFrame,
) -> Option<u64>
```

It runs with the broad guard already dropped — the brief `with_cpu` re-acquire is itself the
lock-dropped proof, since a still-held guard would deadlock. Inside it:

* calls the **existing** `dispatch_next_on_cpu(cpu)` — the run queue advances through the same
  authority the in-lock path uses, never through a second queue;
* **exactly one** advance: a single call whose result is returned;
* **CPU-local**: `dispatch_next_on_cpu(cpu)` pops from *this* CPU's run queue, so a task
  runnable elsewhere is never stolen;
* returns `None` for the scheduler's idle/supervisor sentinel (TID 0), which owns no user
  context — this matters because `PriorityScheduler::dispatch_next` returns `Some(0)` when the
  idle sentinel is current and nothing is runnable;
* restores the selected task's arch state into the caller's `TrapFrame` via the existing
  `restore_arch_thread_state`, and **fails closed** — a restore error is reported as "still
  idle" rather than committing a frame that was never populated.

## The wiring — `src/arch/x86_64/descriptor_tables.rs`

Placed after the last post-lock drain and before any frame commit:

```rust
let revalidated_owner = if matches!(exiting_tid, None | Some(0)) {
    shared.revalidate_idle_owner_after_drains(cpu, &mut trap_frame)   // (hosted-dev: None)
} else {
    None
};
```

* **A prepared replacement owner is never displaced.** The gate is on the prepared owner being
  idle. A replacement was chosen from live state under the broad guard, and the drains cannot
  invalidate it.
* **`Some(next)` joins the existing replacement path**, in this order: GPRs written to the
  saved regs (`if task_switched || revalidated_owner.is_some()`), `flush_trap_context_to_iret_frame`,
  a **single** `TRAP_DISPATCH_DEPTH[..].store(0, Release)`, then
  `maybe_attest_exit_common_epilogue(cpu, "replacement")`. No duplicate cleanup, no second
  depth clear, and the epilogue's existing ownership attestation reports the owner actually
  committed.
* **`None` keeps the existing idle body byte-for-byte** — the AP `ap_sched_next_or_idle` hook,
  `SCHED_ENTER_IDLE_HLT`, the depth clear, `maybe_attest_exit_common_epilogue(cpu, "idle")` and
  `idle_halt_loop()` are unchanged; only the guard gained `&& revalidated_owner.is_none()`.

One additional marker distinguishes a revalidated commit from a normally prepared one:

```text
EXIT_TASK_OWNER_REVALIDATED arch=x86_64 cpu={} prepared=idle committed=replacement
                            next_tid={} advances=1 broad_lock=0 result=ok
```

## Tests — `stage200d2b1d5a_owner_revalidation` (9)

| # | Property |
|---|---|
| g01 | uses the existing `dispatch_next_on_cpu`; **exactly one** advance; no hand-rolled selection |
| g02 | CPU-local: pops from the caller's own queue; the primitive dispatches on the requested CPU |
| g03 | fail-closed: TID-0 sentinel → idle; restore error → idle; restores into the caller's frame |
| g04 | prepared **replacement** owner preserved — gate on idle, exactly one call site |
| g05 | genuine idle body unchanged (AP hook, `SCHED_ENTER_IDLE_HLT`, attest, halt loop) |
| g06 | revalidated owner joins the replacement path: flush → **single** depth clear → attest |
| g07 | behavioural: a drain-woken task is selected from an idle CPU; one queue advance; an already-owned CPU does not advance again |
| g08 | behavioural: CPU 0 never steals a task queued on CPU 1; CPU 1's own revalidation does select it |
| g09 | behavioural: a genuinely idle CPU manufactures no owner and advances no queue |

## Mutation guards

Each mutation is applied to production source, the named guard is run, and the mutation must
kill it.

| # | Mutation | Guard | Result |
|---|---|---|---|
| M1 | seam never selects (`dispatch_next_on_cpu` → `current_tid`) | g01 | KILLED |
| M2 | seam advances the queue twice | g01 | KILLED |
| M3 | affinity broken (`dispatch_next_on_cpu(CpuId(0))`) | g02 | KILLED |
| M4 | fail-open (`Err(_) => Some(next)`) | g03 | KILLED |
| M5 | gate removed — a prepared replacement is displaced | g04 | KILLED |
| M6 | idle body loses `&& revalidated_owner.is_none()` | g05 | KILLED |
| M7 | revalidated owner does not join the replacement path | g06 | KILLED |

## Scope

Not done, by instruction: no QEMU run, no live cell claimed, no change to the ServerDies
counters, runner or grading, and no work on the Stage 200D-2B1D4 accounting defect
(`LINK_LEAK created=54 detached=1`), which remains an open design decision in its own stage.
