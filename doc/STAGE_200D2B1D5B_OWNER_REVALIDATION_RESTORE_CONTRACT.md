<!-- SPDX-License-Identifier: Apache-2.0 -->

# Stage 200D-2B1D5B — owner-revalidation restore-failure contract

Scoped to the restore-failure contract of the Stage 200D-2B1D5A seam. **No QEMU, no live cell,
no ServerDies counter/runner/grading change.** One-advance, CPU-local and prepared-replacement
behaviour are preserved; the D5A guards were updated to the new shapes, not weakened.

## The hole in `Option<u64>`

`dispatch_next_on_cpu` **commits** its selection as the CPU's `current` before the arch restore
is attempted. Stage 200D-2B1D5A reported a restore failure as `None`, which the epilogue could
not tell apart from genuine idle. Taking the ordinary idle path in that state halts the CPU
while the scheduler still believes the committed task is running on it — on no run queue and on
no CPU. That is the Stage 200D-2B1D4 strand, one level down and caused by the fix for it.

## The typed outcome

```rust
pub(crate) enum OwnerRevalidation {
    Idle,                                            // nothing was committed
    Replacement(u64),                                // committed AND restored
    RestoreFailed { tid: u64, rolled_back: bool },   // committed, NOT restored
}

pub(crate) enum OwnerCommit { Idle, Replacement(u64), FailClosed(u64) }

impl OwnerRevalidation { pub(crate) fn disposition(self) -> OwnerCommit }
```

`disposition()` is a pure function, so the fail-closed rule is one testable rule rather than the
incidental shape of a `match` inside the trap handler. Only `RestoreFailed { rolled_back: false }`
maps to `FailClosed`.

## The rollback

On failure the seam undoes its own advance, CPU-locally:

```rust
let cleared  = kernel.block_current_on_cpu(cpu) == Some(next);
let requeued = !restorable || kernel.enqueue_on_cpu(cpu, next).is_ok();
OwnerRevalidation::RestoreFailed { tid: next, rolled_back: cleared && requeued }
```

`block_current_on_cpu` is the same primitive Stage 190A uses to return a CPU to idle; it also
clears scheduler membership, so the re-enqueue cannot hit `AlreadyQueued`. A task whose TCB is
gone is **not** resurrected into a run queue — there is nothing to run — but `current` is still
cleared, which is what the idle path depends on.

`rolled_back` requires **both** halves. An incomplete rollback drives the epilogue through the
existing fatal architecture path (`fatal_trap_read_snapshot` →
`log_decoded_fatal_trap_from_snapshot` → `debug_uart_trap_breadcrumb` → `halt_forever`) — the
same path the trap-dispatch error already uses, not a second policy. `halt_forever` diverges, so
that arm cannot fall through to the frame commit.

## A silent-success hole closed along the way

`restore_arch_thread_state` maps `KernelError::TaskMissing` to `Ok(())` so early boot (no user
task scheduled yet) restores nothing and still returns cleanly. That is correct for its other
callers and **wrong** at this call site: a task still in a run queue whose TCB has been reaped
would have reported success with the frame still holding the *previous* task's context, and the
epilogue would have `iret`ed into ring 3 on the exited task's frame.

Tracing the failure modes shows this was the *only* reachable one — `take_tls_restore_request`
never returns `Err`, so under D5A the `Err(_) => None` arm was effectively dead and the real
failure took the success path. The seam now establishes restorability
(`kernel.thread_user_context(next).is_some()`) before trusting the result.
`restore_arch_thread_state` itself is unchanged.

## Tests — `stage200d2b1d5b_restore_contract` (11)

The seam is `#[cfg(target_arch = "x86_64")]` and the hosted suite is an x86_64 host, so h03–h07
run the **real production seam** on a real `SharedKernel` rather than inspecting source.

| # | Property |
|---|---|
| h01 | `disposition()` is the fail-closed rule — all four outcome shapes |
| h02 | a restore failure is type-level distinguishable from genuine idle |
| h03 | successful restore → `Replacement`, frame populated with *that* task's context, one advance |
| h04 | genuine idle → `Idle`, frame untouched, nothing became current |
| h05 | reaped TCB → `RestoreFailed { rolled_back: true }`, frame untouched, current cleared, not resurrected |
| h06 | **no outcome leaves a stranded `current`**: idle ⇒ unowned, replacement ⇒ that tid is current |
| h07 | a rolled-back failure is idle-equivalent *by the epilogue's own `None \| Some(0)` predicate* |
| h08 | restorability established → restore attempted → success reported, in that order; `.is_ok()` gates the commit |
| h09 | rollback clears `current` and requeues only live tasks; `rolled_back` needs both halves |
| h10 | the `FailClosed` arm uses the existing fatal path and never `idle_halt_loop` / frame commit |
| h11 | the epilogue branches on the typed disposition, once, with all three arms |

h07 deliberately does **not** assert byte-identity with genuine idle: genuine idle leaves the
scheduler's idle sentinel current (`Some(0)`) while the rollback clears the slot (`None`). Both
satisfy the predicate the idle gate actually applies, and asserting more than that would be
asserting something untrue.

## Mutation guards — 10/10 killed

| # | Mutation | Guard |
|---|---|---|
| N1 | restore failure collapsed back into `Idle` (the D5A bug) | h05 |
| N2 | restorability check removed — silent-success hole reopened | h05 |
| N3 | rollback never clears `current` | h05 |
| N4 | incomplete rollback reported as safe | h09 |
| N5 | fail-closed rule inverted (stranded → idle) | h01 |
| N6 | rolled-back failure escalated to fatal | h01 |
| N7 | reaped task resurrected into the run queue | h09 |
| N8 | epilogue falls through to idle instead of halting | h10 |
| N9 | epilogue bypasses the typed disposition | h11 |
| N10 | replacement committed without a successful restore | h08, h03, g03 |

N10 initially **survived** h08, which checked only that restorability preceded the success
return. h08 was strengthened to require the restore call itself and `.is_ok()` gating it; the
mutation is now killed by h08 (order/gating), h03 (frame never populated) and g03 (call absent).

## Reachability, stated plainly

With today's code `block_current_on_cpu` always returns the task just dispatched and the only
restore failure is the unrestorable one (which is not requeued), so `rolled_back` is always
`true` and the `FailClosed` arm is **not currently reachable**. It is a backstop for a future
failure mode, and `disposition()` keeps the rule under test regardless of reachability. The
reachable and now-fixed case is N2's: a reaped TCB silently reported as a successful
replacement.
