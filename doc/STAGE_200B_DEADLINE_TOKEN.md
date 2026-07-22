<!-- SPDX-License-Identifier: Apache-2.0 -->
# Stage 200B — Generation-Bearing Deadline Token Store

A bounded, generation-bearing deadline-token mechanism for reply receives, plus an
explicit reply-slot terminal re-arm (slot reuse) protocol. A deadline token may
*request* a timeout terminal claim, but it is NEVER a second terminal-result
authority: `TerminalCell` (Stage 200A) remains the sole arbiter among Reply,
Timeout, PeerDeath, CallerExit and EndpointGone. Hosted registration/ownership
mechanism only — no real timer, production deadline queue, caller wake, task-exit
scan, QEMU oracle or live timeout marker.

## 1. Commit / baseline

Starting commit `783d6b9` (Stage 200A). Baseline frozen invariants preserved (§11).

## 2. Reply-slot terminal re-arm protocol

`TerminalCell::try_rearm(&mut self, previous_record_generation, new_identity) ->
Option<epoch>` and the store seam `KernelState::rearm_terminal_for_reply_record`.
Succeeds only when: the slot is recycled (reply-cap generation strictly ADVANCED),
the old terminal phase is `Completed` or a canonical unused `Open` (never
`Reserved` — no live `TerminalOwner`), and the new identity names the slot at the
advanced generation. Publication order: reply-cap generation advances → complete
new `TerminalIdentity` installed → new terminal epoch installed → `Open` published
with a `Release` store LAST. A claimant `Acquire`-loads state → observes epoch →
compares the complete identity → CAS `Open → Reserved`.

Hard requirements proven (`stage200b_reply_slot_reuse`, 7 tests + module unit
tests): a `Completed` cell cannot reopen within one epoch; reuse advances BOTH the
reply generation and the terminal epoch; an old `TerminalOwner` is rejected after
reuse (epoch bump); an old identity / reused numeric TID (new ASID) / stale reply
alias are all rejected after reuse; and a new claimant never observes a partially
published identity.

## 3. Identity publication / data-race safety

The identity fields are ordinary (non-atomic) data written ONLY under `&mut self`
(exclusive). In the in-kernel store, `&mut` access is serialized by the KernelState
lock, and every claim (`&self`) also runs under that lock — so an identity rewrite
can never run concurrently with an identity read. This is the Rust-data-race-safe
mechanism (borrow checker + serializing lock), not a bare `AtomicU64` mutating
adjacent ordinary fields. `stage200b_reply_slot_reuse::t5` runs a barrier-aligned
re-arm-vs-claim race on the in-kernel store and asserts no torn identity is ever
observed (0 across 64×). The trailing `Release` publication of `Open`/`Armed` (after
the identity write) plus the `Acquire` gate on the state gives the happens-before so
a claimant that observes the new phase observes the complete identity.

## 4. Deadline-token representation

`src/kernel/deadline_token.rs` (portable, `core`-only — compiled into all three
freestanding kernels):

* `DeadlineTokenIdentity` — token slot/index, token generation, terminal epoch, and
  the full `TerminalIdentity` (reply-record index+gen, caller/replier {tid,asid},
  reply endpoint index+gen, blocked-recv gen, deadline-token gen). Must match the
  current `TerminalIdentity` exactly; numeric TID / reply-record index / endpoint
  index alone never authorize a timeout claim.
* `DeadlineTokenCell` — packed `AtomicU64` state `[ epoch (61) | phase (3) ]`,
  phase ∈ {Vacant, Armed, ClaimedForFire, Disarmed, Completed}; immutable-per-arming
  identity.
* `DeadlineTokenHandle` (mint at arm) / `DeadlineFireOwner` (mint at fire) — private
  constructors, so only the rightful holder can fire / complete / disarm / restore.
* `DeadlineArmError` — `AlreadyArmed | StoreFull | TerminalNotOpen`.

## 5. Arm / fire / cancel lifecycle

```text
   Vacant → Armed → ClaimedForFire → Disarmed | Completed
              └── cancel / terminal-disarm ──► Disarmed
```

`arm_deadline_token` (Part 5 ordering): validate current terminal identity/epoch is
`Open` and exact → enforce one active registration per reply record → reserve a free
slot (bounded; else `StoreFull`) → fill the complete token identity → publish
`Armed` with `Release` → return a generation-bearing handle. `claim_deadline_fire_exact`
is the synthetic hosted fire (`Armed → ClaimedForFire`, one owner). `cancel_deadline_exact`
and `disarm_deadline_after_terminal_completion` are `Armed → Disarmed` (exact
slot+generation+epoch). `complete_deadline_fire` / `disarm_deadline_fire` /
`restore_fire_claim_if_retryable` are the fire-owner terminal transitions.

## 6. Terminal-claim integration

A fire owner does NOT decide that timeout won; it calls
`try_claim_timeout_terminal` against the same `TerminalCell`:

* timeout terminal claim **wins** → `TerminalCell = Reserved(Timeout)`, token
  `Completed`. The terminal result is NOT committed and no caller is woken in this
  stage (the reservation is released during test cleanup).
* reply / peer-death / caller-exit / endpoint-gone **already won** → the timeout
  claim fails, the token disarms, no result mutation, no wake.
* token identity **stale** → rejected, mutates nothing.

A duplicate fire fails before any second terminal claim (the `Armed →
ClaimedForFire` CAS admits one owner).

## 7. Slot and generation reuse results

A stale fire after reply-record reuse, endpoint-generation replacement,
blocked-recv-generation replacement, or a reused caller/server numeric TID (new
ASID) may still claim the (un-disarmed) token, but its timeout terminal claim
targets the OLD identity, which no longer matches the re-armed cell → refused
(`stale_tokens_accepted = 0`). When a terminal wins, the exact associated token is
disarmed, and a stale disarm/cancel cannot touch a newer registration
(`new_registration_cancellations = 0`).

## 8. Deterministic race totals

Fifteen races (`stage200b_deadline_races` 1–13 barrier/bounded-step;
`stage200b_deadline_store_races` 14–15 lock-serialized). Contended races repeat
200×. Every race asserts EXACTLY: token fire owners ≤ 1; terminal winners = 1 when a
terminal race exists; timeout terminal winners ≤ 1; caller wakes = 0; result copies
= 0; stale tokens accepted = 0; new registrations cancelled = 0; records left
Reserved = 0 after cleanup.

| # | Race | # | Race |
|---|------|---|------|
| 1 | fire vs reply claim | 9 | stale fire after reply-record reuse |
| 2 | fire vs peer-death claim | 10 | stale fire after endpoint-gen replacement |
| 3 | fire vs caller-exit claim | 11 | stale fire after blocked-recv-gen replacement |
| 4 | fire vs endpoint-gone claim | 12 | old token vs new caller (reused TID) |
| 5 | fire vs exact cancellation | 13 | old token vs restarted server (reused TID) |
| 6 | two simultaneous fires | 14 | terminal completion vs token publication |
| 7 | cancellation vs re-arm | 15 | store-full failure vs terminal claim |
| 8 | stale fire after token-slot reuse | | |

## 9. Memory-ordering findings

`stage200b_memory_ordering` (source guards + functional):

```text
token identity init → Armed Release publication → Acquire/AcqRel fire claim
                    → fire owner observes the complete token identity
new terminal identity init → Open Release publication → terminal Acquire claim
                           → claimant observes the complete identity
terminal completion → exact token disarm → late fire cannot reopen or reclaim
```

The per-cell `epoch` (ABA nonce) is embedded in every CAS, so an old cancel/restore
cannot re-arm a newly published token. Source guards pin the `Release` publication
of `Armed`/`Open`, the `Acquire` fire-claim gate, and the `AcqRel`/`Acquire` CASes.
Stage 200A and Stage 199 reply-record orderings are NOT weakened (the terminal claim
CAS is guarded unchanged; the `TerminalCell::try_rearm` publish uses the identical
`Release` discipline as `arm`).

## 10. Stage 199 guard resolution

`stage199a2b4_live_oracle_guards::feature_and_selector_both_required` failed on the
untouched baseline because a later stage (199A2D2A) added a mutual-exclusion comment
to `set_x86_ipccall_direct_oracle_enabled`, pushing the `set_ipccall_direct_proof_enabled(true)`
call past the guard's brittle fixed 400-char window. This was a STALE source-inspection
expectation, not a real activation bug — the frozen setter still arms the gate. The
guard now bounds its scan to the setter's function body (up to the next top-level
`pub` item) instead of a fixed char count. No frozen NR6/NR7 behavior was modified.

## 11. Hosted seal

`stage200b_deadline_seal` runs every fire-vs-terminal race, aggregates exact totals,
and emits only when all invariants hold:

```text
STAGE_200B_DEADLINE_TOKEN_SEAL
terminal_authority_stores=1
deadline_registration_stores=1
generation_bearing_tokens=1
reply_slot_reuse_epoch_safe=1
duplicate_fire_owners=0
duplicate_timeout_claims=0
stale_token_accepts=0
new_registration_cancellations=0
caller_wakes=0
result_copies=0
result=ok
```

Preserved: `SYSCALL_COUNT = 32`, `VARIANT_COUNT = 22`, NR27 absent, `DebugLog = 192`,
`REPLY_CAP_QUEUEING_SUPPORTED = false`, Stage 198F live cells = 30, Stage 199
functional live cells = 6, x86 Stage 199 frozen, single-pair acknowledgement stores,
queued `IpcCall` unsupported, timeouts not live, notifications not live, server-death
caller wake not live. All three freestanding kernels compile;
`cargo check -p yarm-control-plane-servers --bins` and `cargo test -p yarm-ipc-abi
pm_restart` pass.

## 12. Stage 200C production deadline-queue and reply-versus-timeout plan

1. **Production deadline queue** — replace the synthetic fire with a real
   monotonic-time min-heap (or timing-wheel) keyed by `{token_index,
   token_generation}`. The timer interrupt handler dequeues due entries and drives
   `claim_deadline_fire_exact` → `try_claim_timeout_terminal` under the existing
   lock-order rules (terminal ownership → deadline store → reply-record; short owned
   claims, never two domain locks across a terminal CAS).
2. **Live reply-versus-timeout** — the timeout terminal winner (Stage 200B leaves it
   `Reserved(Timeout)`) is now COMMITTED: prepare the canonical timeout result bytes,
   `commit_terminal`, then enqueue the blocked caller (the Stage 200A memory-ordering
   chain: result prepared → commit → enqueue → resumed caller observes the final
   result). Reply and peer-death continue to race the same cell; the loser disarms
   the token and never wakes the caller.
3. **Arm wiring** — the NR6/recv-v2 reply-block path registers a deadline (when the
   caller supplies a timeout) via `arm_deadline_token`, populating
   `deadline_token_generation`; the reply/peer-death/caller-exit/endpoint terminal
   winners disarm the exact token on completion.
4. **QEMU oracle** — a live selector-gated oracle proving one real timeout wake and
   one reply-beats-timeout round trip, emitting the live retirement marker only once
   the distinct-outcome checks pass — replacing this stage's hosted mechanism seal.
