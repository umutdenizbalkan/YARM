<!-- SPDX-License-Identifier: Apache-2.0 -->
# Stage 200C1 — Production Reply-Receive Deadline Completion Transaction

Connects the accepted Stage 200A terminal ownership and Stage 200B deadline tokens
to YARM's real IPC receive-timeout infrastructure, and hosted-proves the production
timeout completion transaction for a caller blocked in recv-v2 on a reply endpoint.
No QEMU; no live architecture-cell retirement. No server-death handling,
notifications, multi-pair direct-call support, or second deadline queue.

## 1. Commit / baseline

Starting commit `6938c66` (Stage 200B). Baseline frozen invariants preserved (§ below).

## 2. Existing timeout-path audit

Marker `STAGE_200C_EXISTING_TIMEOUT_AUDIT queue=existing generation_identity=incomplete
broad_lock_completion=yes duplicate_wake_risk=no result=ok`
(`stage200c_existing_timeout_audit`). Findings:

* **Queue** — the per-TCB `ipc_timeout_deadline: Option<u64>` field in the fixed
  `MAX_TASKS`-wide TCB array (fixed slots, `TIMEOUT_SCAN_CHUNK` chunked scan). NOT a
  heap or sparse tombstone list. It is registration + scheduling infrastructure and
  is **adapted, not replaced**, to reference the Stage 200B `DeadlineTokenHandle`.
* **Registration** — `ipc_recv_with_deadline` / `ipc_recv_until_deadline` set the
  per-TCB deadline and block `Blocked(EndpointReceive(cap))`.
* **Timer/tick** — `process_ipc_timeout_deadlines(now)` runs from the timer IRQ
  handler (`fault_state.rs`). Phase 1 (task rank 2): mark Runnable + clear deadline +
  `ipc_timeout_fired`. Phase 2 (ipc rank 3): clear waiters by COMPLETE `{tid, asid}`
  identity. Phase 3 (scheduler rank 1): enqueue once.
* **Timeout identity** — `{tid, asid}` + absolute tick only. **Generation identity
  is INCOMPLETE** for reply-terminal purposes (no reply-record / endpoint /
  blocked-recv generation) — Stage 200C1 supplies those via the referenced token.
* **Result publication** — `ipc_timeout_fired` → `consume_ipc_timeout_fired_for_tid`
  → `SyscallError::TimedOut` (the existing canonical result; no new ABI).
* **Broad-lock completion = yes** — the live scan runs under the broad
  `&mut KernelState`; internally it uses phased per-domain sub-locks. Reported
  honestly; no global-lock retirement claimed.
* **Duplicate wake risk = no** — the deadline is cleared in the waking pass; each
  expired task is enqueued exactly once.

The existing queue is NOT unsuitable — it is adapted, not replaced.

## 3. Deadline registration point

`KernelState::register_reply_receive_deadline` (composed). Validates the committed
blocked state (caller `Blocked(EndpointReceive)` + exact ASID; exact endpoint waiter
installed at the reply endpoint generation; terminal cell Open + identity exact;
finite deadline), then **arms the Stage 200B token BEFORE** publishing the blocked
receive as timeout-capable (the per-TCB deadline + the exact `DeadlineTokenHandle`
reference + the captured blocked-recv generation). Ordering: prepare complete
blocked state → reserve/arm token (validated inside `arm_deadline_token`) → publish
blocked receive as timeout-capable (LAST). On any failure it mutates NOTHING (no TCB
deadline, no token, no reply-authority change), so the block path rolls back cleanly
(`c02`). The token identity is built from authoritative live state; no timing-based
inference.

## 4. Queue / token integration

The existing per-TCB deadline entry now REFERENCES the exact `DeadlineTokenHandle`
(TCB field `reply_timeout_token`), plus a `blocked_recv_generation`. The queue does
not decide that timeout won: the lifecycle is `queue entry due → exact token fire
claim → try_claim_timeout_terminal → completion transaction`. A stale queue entry
fails at the token fire claim (`c17`), before any waiter mutation or wake.

## 5. Timeout completion ordering

`SharedKernel::run_reply_timeout_completion` — one owned, ordered plan of short
bounded domain claims:

```text
claim exact due token → claim Timeout terminal ownership
  → revalidate caller {tid,asid} → revalidate reply endpoint generation
  → revalidate blocked-recv generation → claim exact endpoint waiter
  → prepare canonical timeout recv-v2 result (ipc_timeout_fired)
  → invalidate reply authority (Available/Reserved → Cancelled)
  → complete TerminalCell as Timeout → mark token Completed
  → commit caller blocked-state completion → Runnable → scheduler enqueue LAST
```

The caller can never dispatch before the timeout result is installed, the terminal
is `Completed(Timeout)`, and the reply aliases are non-invokable — all happen before
the final, non-fallible enqueue (`c10`). No user-memory copy is performed. Outcomes:
`Woken`, `CleanupNoWake` (caller/endpoint/waiter changed after the Timeout claim),
`LostToTerminal` (another terminal already won — fire disarmed), `StaleToken`.

## 6. Reply-wins cancellation behavior

`SharedKernel::disarm_reply_deadline_on_reply_win` reads the caller's TCB token
handle and disarms the EXACT token (slot + generation + epoch) when reply obtains
terminal ownership, so a stale queued fire can no longer claim (`c06`, `c07`).
Cancellation cannot affect a newer registration (`c18`), is not held across any user
copy (there is none), and cannot reopen terminal authority. A losing timeout fire
mutates no result and wakes nobody. The frozen NR7 transaction is otherwise
unchanged (source copy before reply claim, record Consumed before dispatch,
duplicate reply rejection all preserved).

## 7. Timeout-wins late-reply behavior

When timeout wins, `TerminalCell = Reserved(Timeout) → Completed(Timeout)`, the reply
record is `Cancelled` (non-invokable), the timeout result is installed, and the
caller wakes once (`c08`, `c11`). A late reply's terminal claim and record
reservation are both rejected: caller copies = 0, additional caller wakes = 0, reply
post-work commits = 0 (it receives the existing canonical stale/invalid result).

## 8. Rollback and stale-entry behavior

Before terminal ownership, a stale/duplicate fire is refused at the token claim
(`c05`, `c17`). After Timeout owns the terminal, reply authority is never restored
and the old registration is never re-armed. If the exact caller disappears after the
Timeout claim (`c12`, `c13`), or the endpoint generation / waiter / blocked-recv
generation changes before the terminal claim (`c14`, `c15`, `c16`), the transaction
completes cleanup (invalidate record, Complete terminal + token) with NO wake and no
replacement-waiter mutation.

## 9. Lock-boundary findings

The composed transaction separates the deadline-queue claim, terminal-ownership
claim, endpoint-waiter claim, task-result completion and scheduler enqueue into short
bounded domain claims — the deadline store lock is never held while claiming the
waiter, mutating task state, invalidating reply aliases, or enqueuing; the enqueue is
last and non-fallible; no user copy occurs. The existing live `process_ipc_timeout_
deadlines` scan still runs under the broad `&mut KernelState` and is **not** rewired
in this hosted stage — reported honestly, no global-lock retirement claimed. Part 11
is source-guarded (`stage200c_terminal_reuse_sync_guard`): `TerminalIdentity` /
`DeadlineTokenIdentity` are ordinary data mutated ONLY under `&mut self` (exclusive;
in the in-kernel store additionally serialized by the KernelState lock) and read by
every claim under the same domain — no off-lock ordinary-data identity access.

## 10. Deterministic race totals

`stage200c_reply_timeout_transaction` — 22 composed-path cases (arm, arm-rollback,
ordinary-timeout-unchanged, due-fire, duplicate-fire, reply-before/after-fire,
timeout-before-reply, result-before-dispatch, late-reply-rejected, caller-exit
before/after claim, endpoint/waiter/brg change, token-slot reuse stale fire + stale
cancel, sparse-entry no-strand, two-caller isolation, canonical result, zero
left-Reserved) all pass. The central reply-vs-timeout terminal-CAS race (`c09`, 64×)
holds: terminal winners = 1; successful replies + successful timeouts = 1; caller
wakes = 1; reply destination copies ≤ 1; timeout result commits ≤ 1; late reply
successes = 0 when timeout wins; stale token accepts = 0; wrong waiter mutations = 0.

## 11. Hosted transaction seal

`stage200c_reply_timeout_seal` emits after the composed transaction is exercised:

```text
STAGE_200C_REPLY_TIMEOUT_TRANSACTION_SEAL
existing_deadline_queue_integrated=1
terminal_authority_stores=1
deadline_registration_stores=1
reply_timeout_terminal_winners=1
timeout_result_before_dispatch=1
late_reply_successes=0
duplicate_timeout_wakes=0
stale_token_accepts=0
wrong_waiter_mutations=0
records_left_reserved=0
result=ok
```

Preserved: `SYSCALL_COUNT = 32`, `VARIANT_COUNT = 22`, NR27 absent, `DebugLog = 192`,
`REPLY_CAP_QUEUEING_SUPPORTED = false`, Stage 198F live cells = 30, Stage 199
functional live cells = 6, x86 Stage 199 frozen, single-pair acknowledgement stores,
queued `IpcCall` unsupported, notifications not live, server-death caller wake not
live. All three freestanding kernels compile; `cargo check -p yarm-control-plane-
servers --bins` and `cargo test -p yarm-ipc-abi pm_restart` pass; the frozen Stage
199 guards (incl. `feature_and_selector_both_required`) pass.

## 12. Stage 200C2 x86_64 live timeout-oracle plan

1. **Wire the live scan** — `process_ipc_timeout_deadlines`, on an expired TCB whose
   `reply_timeout_token` is `Some`, dispatches `run_reply_timeout_completion(handle)`
   instead of the plain wake, under a selector-gated feature. Ordinary (token-less)
   deadlines keep the existing path untouched.
2. **Live registration** — the recv-v2 reply-block path (NR6 → recv-v2 timeout block)
   calls `register_reply_receive_deadline` when the caller supplies a finite reply
   timeout, and bumps `blocked_recv_generation` on each fresh block; NR7 calls the
   `disarm_reply_deadline_on_reply_win` hook on a reply win.
3. **x86_64 SMP oracle** — a default-off `yarm.x86_64_reply_timeout_oracle` selector
   drives one BSP caller blocked on its reply endpoint with a finite deadline, a
   controlled timer advance past the deadline, and asserts exactly one canonical
   `TimedOut` wake with the reply cap non-invokable — plus a reply-beats-timeout run
   proving the exact deadline disarm. Emits the live retirement marker (replacing this
   hosted seal) only after the distinct-outcome checks pass under live QEMU.
4. **Broad-lock boundary** — extract the live completion onto the narrow seams
   already built here so the timer scan no longer requires the broad `&mut
   KernelState`, then honestly seal the global-lock reduction.
