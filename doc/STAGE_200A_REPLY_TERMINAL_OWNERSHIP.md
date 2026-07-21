<!-- SPDX-License-Identifier: Apache-2.0 -->
# Stage 200A — Reply, Timeout and Peer-Death Terminal Ownership Model

One authoritative, **architecture-neutral, hosted-proven** state machine governing
the outcome of a caller blocked on its reply endpoint after an `IpcCall` (NR6).
No production wiring: no timer queue, no task-exit wake path, no QEMU oracle, no
notification system, no endpoint-indexed acknowledgement store is wired in this
stage. This is a **hosted mechanism seal**, not a live retirement marker.

## 1. Commit / baseline

Starting frozen baseline (parent `b73a6b3`): Stage 198F 10 classes / 30 live cells;
Stage 199 direct NR6/NR7 functional matrix 6/6; x86_64 SMP=2 bidirectional
cross-CPU direct IPC sealed; direct request/reply transactions frozen.

## 2. Terminal state representation

`src/kernel/terminal_ownership.rs` defines the mechanism (portable, `core`-only —
compiled into all three freestanding kernels):

* `TerminalClaimant` — `Reply | Timeout | PeerDeath | CallerExit | EndpointGone`.
* `TerminalIdentity` — the generation-bearing identity (see §4).
* `TerminalCell` — the single per-record authority. Its packed `AtomicU64` state
  word is `[ epoch (59) | claimant (3) | phase (2) ]`, phase ∈ {Open, Reserved,
  Completed}. The immutable identity is written at `arm`, then `Open` is published
  with a single `Release` store.
* `TerminalOwner` — the exclusive token minted only by a successful claim (private
  constructor); only its holder can commit or release.

Lifecycle (a fan-in — exactly one claimant may reserve; only that owner completes
or releases):

```text
   Open (Live/Available) ──► Reserved(<one claimant>) ──► Completed (terminal, one-shot)
                                    │
                                    └── release_if_retryable ──► Open (same generation)
```

The accepted NR7 reply reservation maps onto
`Open(Available) → Reserved(Reply) → Completed(Reply)`.

## 3. Integration with the existing reply reservation

The single authority store is `IpcSubsystem::reply_terminal_ownership:
[TerminalCell; MAX_REPLY_CAPS]`, **co-located with and indexed identically to
`reply_caps`** (`authority_stores = 1`). Reply, timeout, peer death, caller exit
and endpoint destruction ALL claim through the same cell for a record slot — there
is **no** second timeout or peer-death table. The existing `ReplyRecordReservation`
(`Available → Reserved → Consumed`) is retained unchanged for reply-cap
invokability, but is now subordinate: it advances to `Consumed` only when the reply
also owns the terminal cell (proven in `stage200a_terminal_integration`). If a
timeout or peer death owns the terminal, the reply can neither claim nor consume,
and a caller-copy rollback cannot restore reply authority. The seams
(`reply_terminal_identity`, `arm_reply_terminal`, `try_claim_reply_terminal_slot`,
`commit_reply_terminal_slot`, `release_reply_terminal_slot_if_retryable`) live on
`KernelState` and are dormant in production; later stages wire the live paths onto
them.

## 4. Generation-bearing identity

Every terminal operation is tied to `TerminalIdentity`: reply record index +
generation, caller `{tid, asid}`, replier `{tid, asid}`, reply endpoint index +
generation, blocked-recv generation, and an optional deadline-token generation. A
claim first `Acquire`-loads the state, then requires an **exact `==` identity
match** before the CAS. Numeric TID alone never authorizes: a restarted caller or
server reusing the same numeric TID carries a different ASID (and/or generation),
so its claim is refused (`race_09`, `race_10`, `stale_generation_never_authorizes`).
The internal `epoch` is a separate ABA nonce (bumped every `arm`) so a stale owner
token can never commit/release a re-armed cell even if the claimant tag repeats.

## 5. Reply-versus-timeout result

`race_01_reply_vs_timeout` (barrier-aligned, 200×) and the behavioral cases: a
single CAS decides one winner. **Reply wins** → record `Consumed`, reply copied
exactly once, caller wakes once with success, late timeout rejected. **Timeout
wins** → terminal `Completed(Timeout)`, no reply destination copy, the late reply
cannot claim/consume (reply cap non-invokable for delivery), caller wakes once with
the canonical timeout result.

## 6. Reply-versus-peer-death result

`race_02_reply_vs_server_death` and `peer_death_wins_before_reply_late_reply_rejected`:
**Peer death wins** → terminal `Completed(PeerDeath)`, no reply copy, reply aliases
non-invokable, caller wakes once with the canonical peer-death/cancellation result.
No new syscall or error code is added — the cancellation reuses existing ABI.

## 7. Caller-exit and endpoint-destruction behavior

* **Caller exit wins** (`caller_exits_first_no_wake_authority_reclaimed`): terminal
  `Completed(CallerExit)`, `wake = 0`, the record's reply/deadline authority is
  reclaimed (discarded → non-invokable), no stale authority restored.
* **Endpoint destruction** — one explicit policy: caller still valid and blocked →
  complete with canonical cancellation and wake **once** (`race_07`, which asserts
  the still-valid caller wakes exactly once regardless of winner); caller gone →
  cleanup only, **no wake** (`wakes_caller(EndpointGone, caller_alive=false) = 0`).

## 8. Deterministic race totals

Twelve deterministic races (`stage200a_terminal_ownership`); contended ones repeat
200× and hold on every run (barrier-forced), asymmetric ones use bounded steps:

| # | Race | Mechanism |
|---|------|-----------|
| 1 | reply vs timeout | barrier |
| 2 | reply vs server death | barrier |
| 3 | timeout vs server death | barrier |
| 4 | reply vs caller exit | barrier |
| 5 | timeout vs caller exit | barrier |
| 6 | peer death vs caller exit | barrier |
| 7 | endpoint destruction vs reply | barrier |
| 8 | endpoint generation replacement vs late timeout | re-arm + stale claim |
| 9 | server restart (reused TID) vs late reply | stale-ASID claim |
| 10 | caller restart (reused TID) vs late timeout | stale-ASID claim |
| 11 | two duplicate timeout events | barrier |
| 12 | two reply aliases racing with timeout | barrier |

Every race asserts EXACTLY: terminal winners = 1, caller wakes ≤ 1, reply
destination copies ≤ 1, record left reserved = 0, stale authority restored = 0,
wrong waiter mutated = 0 (proven with an untouched decoy record), cleanup owners = 1.

## 9. Memory-ordering findings

`stage200a_terminal_memory_ordering` (source guards + functional):

```text
identity init → state Release(Open) → state Acquire/AcqRel claim
             → owner AcqRel commit → committed winner visible → caller dispatch
```

* `arm` publishes `Open` with a `Release` store **last** (after the immutable
  identity); claims `Acquire`-gate on the state first (Release→Acquire ⇒ the
  complete identity is visible).
* claim / commit / release are `compare_exchange(_, _, AcqRel, Acquire)` — only one
  claimant wins, and only the exact `{claimant, epoch}` owner transitions.
* `Completed` has no outgoing transition ⇒ a stale generation cannot reopen a
  completed record; timeout/peer-death losers cannot obtain ownership after a reply
  completes, so they cannot wake the caller a second time.
* The existing SpinLock-guarded reply-record ordering (`Reserved → Consumed`) is
  **not weakened** — it is retained verbatim.

## 10. Hosted seal

`stage200a_terminal_seal` runs every race, aggregates exact totals, and emits only
when all invariants hold:

```text
STAGE_200A_REPLY_TERMINAL_OWNERSHIP_SEAL
authority_stores=1
generation_bearing_identity=1
terminal_winners_per_record=1
duplicate_wakes=0
duplicate_result_copies=0
stale_authority_restores=0
wrong_waiter_mutations=0
records_left_reserved=0
result=ok
```

## 11. Stage 199 preservation

No syscall ABI, variant, or frozen-transaction change. Preserved:
`SYSCALL_COUNT = 32`, `VARIANT_COUNT = 22`, NR27 absent, `DebugLog = 192`,
`REPLY_CAP_QUEUEING_SUPPORTED = false`, Stage 198F live cells = 30, Stage 199
functional live cells = 6, x86 Stage 199 final freeze, single-pair acknowledgement
stores, queued `IpcCall` unsupported, timeouts not live, notifications not live,
server-death caller wake not live. All three freestanding kernels compile (shared
record layout portable). `cargo check -p yarm-control-plane-servers --bins` and
`cargo test -p yarm-ipc-abi pm_restart` pass.

## 12. Precise Stage 200B deadline-token plan

Stage 200B introduces the deadline token that this stage left as
`TerminalIdentity::deadline_token_generation: Option<u64>`:

1. **Deadline token store** — a bounded, generation-bearing deadline table keyed by
   `{reply_record_index, reply_record_generation}`, minting a monotonic
   `deadline_token_generation` on arm. It is NOT a second terminal authority: firing
   a deadline only produces a `try_claim_timeout_terminal` attempt against the
   existing cell, carrying the token generation in the identity.
2. **Arm/disarm** — `arm_reply_terminal` populates `deadline_token_generation` when a
   deadline is registered; a reply/peer-death/caller-exit terminal win disarms the
   token (its generation is stale thereafter), so a late fire mismatches identity and
   claims nothing.
3. **Late-fire safety** — a deadline that fires after any terminal win presents a
   token generation that no longer matches the (completed or re-armed) record, so the
   timeout claim is refused — exactly the `race_08`/`race_10` invariant, now driven by
   the deadline token rather than a synthetic stale identity.
4. **Still hosted** — 200B keeps the timer queue and production wake path out; it adds
   the token lifecycle + its hosted races (deadline-fire vs reply, deadline-disarm vs
   late-fire, token-generation replacement) before any live timer registration in a
   later stage.
