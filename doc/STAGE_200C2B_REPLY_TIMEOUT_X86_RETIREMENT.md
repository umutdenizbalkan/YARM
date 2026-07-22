<!-- SPDX-License-Identifier: Apache-2.0 -->
# Stage 200C2B — x86_64 Off-Lock Reply-Timeout Retirement

Removes the broad `KernelState` dependency from token-bearing reply-timeout collection
and completion, and earns ONE x86_64 retirement cell: **`IpcReplyTimeout`**. Ordinary
non-reply receive timeouts stay on their existing in-lock path (NOT retired). No
server-death handling, notifications, multi-pair IPC, non-x86 live port, or new deadline
queue.

## 1. Commit / baseline

Starting commit `e4adcde` (Stage 200C2A live functional seal).

## 2. Reply-copy / terminal / deadline rollback ordering

The NR7 reply-win is now RESERVE-then-resolve, so an irreversible terminal completion
never precedes the fallible caller copy. In `handle_ipc_reply`, immediately BEFORE the
caller copy/delivery (`kernel.ipc_reply`), `reserve_reply_win_before_copy` takes only
REVERSIBLE holds:

```text
owned reply source snapshot (server payload already read)
→ claim terminal Reserved(Reply)                 [try_claim_reply_terminal_slot]
→ obtain reversible ownership of the exact deadline registration
                                                 [claim_deadline_reply_lease: Armed→ClaimedByReply]
→ copy payload/meta to caller                    [frozen ipc_reply: validate → claim waiter →
                                                  wake → ReplyCapRecord Consumed → enqueue]
→ commit terminal Completed(Reply)               [commit_reply_win_after_delivery]
→ deadline permanently completed                 [complete_deadline_reply_lease: ClaimedByReply→Completed]
```

On a caller-copy fault (`ipc_reply` error), `rollback_reply_win` restores the exact
state: terminal `Reserved(Reply) → Open`, deadline `ClaimedByReply → Armed`, reply record
still `Available` (cap usable), caller still blocked, **copies committed = 0, wakes = 0**.
The new reversible lease is a distinct token-cell lifecycle
(`Armed → ClaimedByReply → Completed | Armed`) — registration OWNERSHIP only, never a
terminal-result authority. A leased token is NOT fire-claimable, so a concurrent due
timeout fire fails until the lease resolves (`t12`, `t14`). All four required races —
reply-copy fault vs due timeout, vs caller exit, vs endpoint replacement, and reply
completion vs already-claimed timeout fire — resolve to exactly one terminal outcome.

## 3. Narrow collector design

`SharedKernel::collect_due_reply_timeout_work(now, cpu)` scans ONLY token-bearing
reply-receive deadlines through the rank-2 task split-mut seam (`with_task_tcbs_split_mut`).
It uses NO broad `&mut KernelState`, NO `with` / `with_cpu`, and NO broad runtime lock
(`h1`). For each exact DUE entry (`now >= deadline`) it publishes one owned bounded
`ReplyTimeoutPostWork { handle, deadline }` — the `DeadlineTokenHandle` embeds the full
generation-bearing identity (token slot+generation, terminal epoch, caller `{tid,asid}`,
reply record index+generation, reply endpoint index+generation, blocked-recv generation).
The collector makes NO timeout decision, mutates NO waiter and wakes NO task. A FULL queue
leaves the deadline armed + due (the TCB entry is NOT cleared) to be retried on a later
scan (`t04`); a duplicate token yields only ONE work owner (`t03`, dedup by token
slot+generation).

## 4. Deferred-work ownership

A per-CPU bounded `ReplyTimeoutPostWork` queue (`RT_POST_WORK_SLOTS = MAX_DEADLINE_TOKENS`)
guarded by its own IRQ-safe lock — never the broad `SpinLock<KernelState>`:

```text
deadline collector → exact work publication → post-lock drain owner
→ token fire claim → terminal timeout claim → completion
```

Duplicate collection of the same token produces one work owner; a stale work item fails at
the exact token fire claim BEFORE any endpoint-waiter mutation (`t06`).

## 5. Timer-path integration

At the production trap-entry post-lock area (`handle_trap_entry_shared`, AFTER the
`with_cpu` broad guard is dropped, mirroring the D2 drains), the oracle-gated hook runs:

```text
collect_due_reply_timeout_work(now, cpu)   → publish DUE token-bearing work (off-lock)
drain_reply_timeout_post_work(cpu, now)    → run the completion transaction (off-lock)
```

The ordinary in-lock scan (`process_ipc_timeout_deadlines`) still SKIPS token-bearing TCBs
exactly as in Stage 200C2A and no longer completes any of them (`h6`, `t17`). Classification:

| deadline class | path |
|---|---|
| reply receive timeout | NARROW collector + NARROW off-lock completion |
| ordinary receive timeout | existing BROAD in-lock scan, unchanged (`t16`) |

No ordinary `IpcRecvTimeout` retirement is claimed.

## 6. Timeout-wins off-lock result

The off-lock drain runs the SINGLE Stage 200C1 completion transaction
(`complete_reply_timeout_over`, the SAME body the hosted `run_reply_timeout_completion`
wrapper calls — no duplication) through `OffLockReplyTimeout`, which reaches state ONLY via
the per-domain split-mut seams (`with_ipc_split_mut`, `with_task_tcbs_split_mut`,
`with_scheduler_split_mut`) and NEVER forms a broad `&mut KernelState`. Boot A emits:

```text
IPC_REPLY_TIMEOUT_ARMED arch=x86_64 … result=ok
IPC_REPLY_TIMEOUT_LOCK_STATUS arch=x86_64 scan_broad_lock=0 completion_transaction_narrow=1 result=ok
IPC_REPLY_TIMEOUT_DEFERRED arch=x86_64 published=1 drained=1 result=ok
GLOBAL_LOCK_RETIRE_CLASS_DONE arch=x86_64 class=IpcReplyTimeout result=ok
IPC_REPLY_TIMEOUT_OK arch=x86_64 terminal=Timeout timeout_result=TimedOut caller_wakes=1 reply_aliases_invalid=1 late_reply_successes=0 result=ok
USER_LOG … X86_IPC_REPLY_TIMEOUT_DONE caller_result=TimedOut caller_continuations=1 late_reply=rejected result=ok
```

The composed transaction installs the canonical `SyscallError::TimedOut` before the enqueue
(the LAST, non-fallible step); no fallible operation follows the enqueue (`h4`, `t09`,
`t10`). The server's late NR7 is rejected by the frozen reply-cap resolve (record
`Available → Cancelled`, `t11`).

## 7. Reply-wins preservation result

Boot B (separate fresh boot) emits:

```text
IPC_REPLY_TIMEOUT_ARMED arch=x86_64 … result=ok
IPC_REPLY_BEATS_TIMEOUT_OK arch=x86_64 terminal=Reply reply_copies=1 deadline_disarmed=1 late_timeout_claims=0 caller_wakes=1 result=ok
IPC_REPLY_TIMEOUT_LOCK_STATUS arch=x86_64 scan_broad_lock=0 completion_transaction_narrow=1 result=ok
IPC_REPLY_TIMEOUT_LATE_SCAN arch=x86_64 outcome=reply_won late_timeout_claims=0 result=ok
USER_LOG … X86_IPC_REPLY_BEATS_TIMEOUT_DONE reply_ok=1 caller_continuations=1 late_timeout_wakes=0 result=ok
```

The reversible reply win commits (terminal `Reply`, deadline lease `Completed`) after the
delivery; the production collector genuinely scans PAST the reply-wins deadline and finds
no claimable timeout registration (the token is retired), so NO `IPC_REPLY_TIMEOUT_OK`
(timeout wake) appears — reply preservation is intact.

## 8. Exact counts (per fresh boot)

| | timeout-wins | reply-wins |
|---|---|---|
| deadline registrations | 1 | 1 |
| deferred work publications | 1 | 1 (late, stale) |
| deferred work drains | 1 | 1 (late, stale) |
| timeout terminal claims / commits | 1 / 1 | 0 / 0 |
| reply terminal successes / destination copies | 0 / 0 | 1 / 1 |
| exact deadline completions (lease/fire) | 1 (fire) | 1 (lease) |
| caller wakes / continuations | 1 / 1 | 1 / 1 |
| late NR7 successes | 0 | — |
| late timeout wakes | — | 0 |
| duplicate wakes | 0 | 0 |
| records left Reserved | 0 | 0 |
| stale authority restores / wrong-waiter mutations | 0 / 0 | 0 / 0 |

## 9. Ordinary-timeout preservation

Ordinary receive-timeout deadlines are unchanged: `process_ipc_timeout_deadlines` still
marks them `Runnable` + clears the waiter in-lock (`t16` green), and token-bearing entries
are skipped there byte-for-byte as in Stage 200C2A (`h6`, `t17`). Ordinary receive timeout
remains UNRETIRED.

## 10. Broad-lock source audit

For the token-bearing reply-timeout path (collector + drain + `OffLockReplyTimeout`):
broad `&mut KernelState` uses = 0, `with` / `with_cpu` uses = 0, user copies = 0, scheduler
enqueue under an object lock = 0 (the enqueue is the LAST step, taken with no ipc/task lock
held), fallible operations after enqueue = 0. `TerminalIdentity` / `DeadlineTokenIdentity`
remain race-free: the token cell writes its identity ONLY under `&mut self` at arm time,
Release-published last, and the collector copies the whole immutable `DeadlineTokenHandle`
by value (`h5`). Source-guarded by `h1`–`h6`.

## 11. Clean retirement seal

`scripts/qemu-ipc-reply-timeout-x86_64-retirement-smoke.sh` captures the SHA + clean tree,
builds feature-on (+ a class-specific feature-off marker-clean assertion), boots
timeout-wins fresh, re-checks SHA/clean, boots reply-wins fresh, re-checks, and emits ONLY
when both pass:

```text
STAGE_200C_REPLY_TIMEOUT_X86_RETIREMENT_SEAL
arch=x86_64
classes=1
live_cells=1
timeout_wins=1
reply_wins=1
scan_broad_lock=0
completion_transaction_narrow=1
late_reply_successes=0
late_timeout_wakes=0
duplicate_wakes=0
stale_authority_restores=0
wrong_waiter_mutations=0
result=ok
```

It fails closed on: a token-bearing deadline handled by the old broad loop, missing
deferred-work evidence, duplicate work publication/drain, a late reply success, a late
timeout wake, a record left Reserved, a wrong waiter/ASID/generation, an ordinary-timeout
regression, or a panic/fatal trap/early QEMU exit.

Preserved: `SYSCALL_COUNT = 32`, `VARIANT_COUNT = 22`, NR27 absent, `DebugLog = 192`,
`REPLY_CAP_QUEUEING_SUPPORTED = false`, Stage 198F live cells = 30, Stage 199 functional
live cells = 6, x86 Stage 199 frozen, ordinary receive timeout unretired, single-pair
acknowledgement stores, queued `IpcCall` unsupported, notifications not live, server-death
caller wake not live. Hosted guards: `stage200c2b_guards` (13 updated + 6 hard-stops) +
`stage200c2b_offlock` (20 deterministic) + the frozen Stage 199 reply-timeout guards; all
three freestanding kernels compile.

## 12. Stage 200C2C AArch64 / RISC-V port plan

1. **AArch64 live cell** — port the oracle + off-lock collector/drain to the AArch64
   trap-entry post-lock area (its FutexWait/Yield drains are the template), reusing the
   frozen AArch64 SMP saved-resume path for the timeout wake. Prove the same two outcomes.
2. **RISC-V live cell** — the same, atop the RISC-V queue-switch foundation drain.
3. **Shared body reuse** — both ports reuse the arch-neutral `complete_reply_timeout_over`
   + `OffLockReplyTimeout` verbatim; only the trap-entry wire + userspace oracle are
   arch-specific, keeping the completion transaction single-source across all three arches.
4. **SMP** — extend to `-smp 2` (a cross-CPU timer scan waking a remote blocked caller) as
   a later stage, reusing the affinity-targeted enqueue.
