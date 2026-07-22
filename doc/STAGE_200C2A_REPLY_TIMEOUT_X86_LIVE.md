<!-- SPDX-License-Identifier: Apache-2.0 -->
# Stage 200C2A — x86_64 Live Reply-Timeout Functional Seal

Wires the accepted Stage 200C1 reply-timeout transaction into the REAL production
recv-v2 deadline registration and `process_ipc_timeout_deadlines` scan, and proves
two live outcomes on x86_64 `-smp 1`, each from a fresh boot of the same clean tree:
**A. timeout wins before NR7**, and **B. NR7 reply wins before timeout**. This is a
live FUNCTIONAL seal, not a lock-retirement seal — the deadline scan still enters
through the broad `KernelState`. No server-death, notifications, multi-pair, second
queue, or non-x86 live cell.

## 1. Commit / baseline

Starting commit `f9665d3` (Stage 200C1).

## 2. Feature / selectors

Default-off feature `x86-ipc-reply-timeout-oracle` (compiles the live marker paths
only). Selector `yarm.x86_64_ipc_reply_timeout_oracle=timeout-wins|reply-wins`
(`1`/`2` accepted). Activation requires `target_arch = x86_64` + the feature + a
valid selector; feature-on without a valid selector is inert. The oracle uses
slot-5 value `10` (timeout-wins) / `11` (reply-wins), mutually exclusive with every
existing slot-5/13/14 oracle (fires only when they are all zero). Feature-off
artifacts contain none of the live marker literals (build-time asserted by the
smoke; source-guarded by `g11`).

## 3. Real registration evidence

`register_reply_receive_deadline` is wired into the committed recv-v2 reply block
(`block_current_on_receive_with_deadline`, after `IPC_RECV_BLOCK_REGISTER`), oracle-
gated to the confined reply endpoint. timeout-wins registers from a finite
recv-timeout deadline; reply-wins blocks with an infinite recv-v2 (so the reply
delivers through the recv-v2 waiter path) and the kernel injects a fixed later
deadline. On a committed block it emits (once):

```text
IPC_REPLY_TIMEOUT_ARMED arch=x86_64 caller_tid=… caller_asid=… record_index=…
  record_generation=… terminal_epoch=… token_slot=… token_generation=… deadline=… result=ok
```

Registration validates the committed blocked state (caller `Blocked(EndpointReceive)`
+ ASID, exact endpoint waiter, terminal Open + identity exact, finite deadline) and
mutates nothing on failure.

## 4. Production scan integration

`process_ipc_timeout_deadlines` first runs a token-bearing pre-pass
(`run_due_reply_timeout_completions`, oracle-gated) for DUE reply-receive deadlines,
then the ordinary loop — which now SKIPS token-bearing TCBs, so ordinary and
reply-receive deadlines are cleanly distinguished. Each due reply registration runs
the SINGLE Stage 200C1 completion body (`run_reply_timeout_completion_locked`) — the
same body the hosted `SharedKernel::run_reply_timeout_completion` wrapper calls, so
there is NO duplicated completion body. A stale/mismatched entry fails at the exact
token fire claim, before any waiter mutation or wake.

## 5. Timeout-wins result

`STAGE_200C_REPLY_TIMEOUT_X86_LIVE_SEAL` boot A (fresh) emits, exactly once each:

```text
IPC_REPLY_TIMEOUT_ARMED arch=x86_64 … result=ok
IPC_REPLY_TIMEOUT_OK arch=x86_64 terminal=Timeout timeout_result=TimedOut caller_wakes=1
  reply_aliases_invalid=1 late_reply_successes=0 result=ok
IPC_REPLY_TIMEOUT_LOCK_STATUS arch=x86_64 scan_broad_lock=1 completion_transaction_narrow=1 result=ok
USER_LOG … X86_IPC_REPLY_TIMEOUT_DONE caller_result=TimedOut caller_continuations=1 late_reply=rejected result=ok
```

The production scan delivers the canonical `SyscallError::TimedOut` (installed into
the caller's saved recv-v2 return register before the enqueue).

## 6. Late-reply result (timeout-wins)

The timeout completion invalidates the reply record (`Available → Cancelled`), so the
server's late NR7 through the original Reply cap is rejected by the FROZEN reply-cap
resolve — no oracle-specific shortcut (`X86_IPC_REPLY_TIMEOUT_DONE late_reply=rejected`).

## 7. Reply-wins result

Boot B (separate fresh boot) emits, exactly once each:

```text
IPC_REPLY_TIMEOUT_ARMED arch=x86_64 … result=ok
IPC_REPLY_BEATS_TIMEOUT_OK arch=x86_64 terminal=Reply reply_copies=1 deadline_disarmed=1
  late_timeout_claims=0 caller_wakes=1 result=ok
IPC_REPLY_TIMEOUT_LATE_SCAN arch=x86_64 outcome=reply_won late_timeout_claims=0 result=ok
USER_LOG … X86_IPC_REPLY_BEATS_TIMEOUT_DONE reply_ok=1 caller_continuations=1 late_timeout_wakes=0 result=ok
```

The NR7 reply-win hook (the ONLY NR7 integration) claims + commits the Reply terminal
and disarms the exact token BEFORE any user copy; the caller resumes with the reply
payload.

## 8. Late-timeout result (reply-wins)

`IPC_REPLY_TIMEOUT_LATE_SCAN outcome=reply_won` is emitted once, when the production
scan genuinely runs PAST the recorded reply-wins deadline and finds the token already
disarmed — positive evidence of harmless late expiry, not inferred. No
`IPC_REPLY_TIMEOUT_OK` (timeout wake) appears in boot B.

## 9. Exact counts (per fresh boot)

| | timeout-wins | reply-wins |
|---|---|---|
| deadline registrations | 1 | 1 |
| token fire owners | 1 | 0 (reply won) |
| timeout terminal commits | 1 | 0 |
| reply terminal commits | 0 | 1 |
| deadline disarms | 0 | 1 |
| reply destination copies | 0 | 1 |
| caller wakes / continuations | 1 / 1 | 1 / 1 |
| late NR7 successes | 0 | — |
| late timeout wakes | — | 0 |
| records left Reserved | 0 | 0 |
| stale authority restores / wrong-waiter mutations | 0 / 0 | 0 / 0 |

## 10. Broad-lock status

The production deadline scan still enters through the broad `&mut KernelState`; the
composed completion is extracted into narrow domain seams (short bounded claims,
enqueue last, no user copy). Reported honestly via
`IPC_REPLY_TIMEOUT_LOCK_STATUS arch=x86_64 scan_broad_lock=1 completion_transaction_narrow=1`.
No `GLOBAL_LOCK_RETIRE_CLASS_DONE` is emitted for the reply-timeout class, and no
report describes it as globally unlocked (`g12`).

## 11. Clean two-boot seal

`scripts/qemu-ipc-reply-timeout-x86_64-smoke.sh` captures the SHA + clean tree, builds
feature-on (+ feature-off marker-clean assertion), boots timeout-wins fresh, re-checks
SHA/clean, boots reply-wins fresh, re-checks SHA/clean, and emits ONLY when both pass:

```text
STAGE_200C_REPLY_TIMEOUT_X86_LIVE_SEAL
arch=x86_64
scenarios=2
timeout_wins=1
reply_wins=1
canonical_timeout_result=1
late_reply_successes=0
late_timeout_wakes=0
duplicate_wakes=0
stale_authority_restores=0
wrong_waiter_mutations=0
scan_broad_lock=1
result=ok
```

Preserved: `SYSCALL_COUNT = 32`, `VARIANT_COUNT = 22`, NR27 absent, `DebugLog = 192`,
`REPLY_CAP_QUEUEING_SUPPORTED = false`, Stage 198F live cells = 30, Stage 199
functional live cells = 6, x86 Stage 199 frozen. Ordinary receive-timeout behavior is
byte-for-byte unchanged (only token-bearing TCBs are diverted). Hosted guards
(`stage200c2a_guards`, 13) + the frozen Stage 199 guards (incl.
`feature_and_selector_both_required`) pass; all three freestanding kernels compile.

## 12. Stage 200C2B narrow-scan extraction plan

1. **Narrow the scan entry** — extract `process_ipc_timeout_deadlines` so the
   token-bearing reply-receive pre-pass runs OFF the broad `KernelState` lock: defer
   each due `DeadlineTokenHandle` to a per-CPU bounded slot under the broad lock, then
   drain + run `run_reply_timeout_completion_locked` after the broad guard drops (the
   existing `D2_RECV_DISPATCH_DEFERRED` trap-entry drain is the template).
2. **Ordinary-deadline seam** — move the ordinary receive-timeout scan onto the same
   narrow per-domain seams, preserving its exact behavior, so the whole scan no longer
   requires a broad critical section.
3. **Honest lock retirement** — only once the scan genuinely runs without the broad
   `KernelState` lock may an `IPC_REPLY_TIMEOUT_LOCK_STATUS scan_broad_lock=0` marker
   and a class retirement seal be emitted; until then the honest `scan_broad_lock=1`
   stands.
4. **SMP** — extend the oracle to `-smp 2` (cross-CPU timer scan waking a remote
   blocked caller) as a later stage, reusing the frozen x86 SMP saved-resume path.
