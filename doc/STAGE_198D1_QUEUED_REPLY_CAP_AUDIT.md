<!-- SPDX-License-Identifier: Apache-2.0 -->
# Stage 198D1 — Queued Reply-Cap Semantic Audit & Redesign

> **STATUS — REJECTED BY POLICY: DIRECT-ONLY REPLY AUTHORITY (Stage 198D-S).**
> This document is retained as **design history**. Its queued reply-cap implementation path
> (design A / Stage 198D2) is **not adopted**: queued reply-cap transfer is not part of YARM's
> supported capability model. Reply capabilities are **direct-delivery only, one-shot, and are
> never stored in an endpoint message queue**. Stage 198D-S enforces this in production
> (`REPLY_CAP_QUEUEING_SUPPORTED = false`): IpcSend refuses to enqueue a Reply-object transfer
> when no compatible receiver is blocked, and the recv-side materialize fails closed if a Reply
> object ever reaches dequeue via an internal invariant violation. **Stage 198D2B (the live queued
> class) is cancelled.** The object-derived rollback, ReplyCapRecord-present validation, and
> generation/caller checks introduced while exploring the queued path are retained as hardening.
> The audit below remains accurate as an analysis of *why* queued reply-cap delivery is hazardous.

Audit/design only, traced against branch head `b6df618`, x86_64 reference path. This increment
does **not** enable or retire the queued reply-cap class. It fixes the architecture-neutral design
that Stage 198D2 will implement. It builds on the accepted **direct** reply-cap delivery (198C2B/198C3)
and the Stage 193G "both shapes NO-GO" audit.

Path under audit:

```
IpcSend with Reply source cap
  → no blocked receiver
  → queued transfer envelope (endpoint buffer + stashed TransferEnvelope)
  → later recv-v2 dequeue
  → object classification            ← the 193G misclassification seam
  → receiver-local materialization
  → payload/meta copy
  → wake
  → reply invocation
```

## 1. What the queued envelope currently stores

Two kernel-side artifacts survive the queue interval:

- **The buffered `Message`** in the endpoint queue: `sender_tid`, `opcode`, `flags`, a
  `transferred_cap()` numeric **transfer handle**, and inline payload. For an **IpcSend** reply
  cap the send ABI carries **no reply flag**; the kernel tags the message `FLAG_CAP_TRANSFER`
  (via `transfer_flag_bits`). Only the `ipc_call` reply path sets `FLAG_REPLY_CAP` explicitly.
- **The stashed `TransferEnvelope`** (`src/kernel/boot/defs.rs:137`), keyed by the handle
  `= (generation << 16) | idx`:
  ```rust
  struct TransferEnvelope {
      source_tid:    ThreadId,
      source_cap:    CapId,          // sender-local CapId — bookkeeping only
      source_object: CapObject,      // kernel-resolved at STASH time (authoritative)
      endpoint:      CapObject,
      receiver_tid:  Option<ThreadId>,
      state:         TransferState,  // Created→MappedReceiver…→Released/Revoked
      shared_region: Option<TransferSharedRegion>,
      generation:    u64,            // slot reuse counter (handle validation)
  }
  ```
  plus the parallel `transfer_envelope_generations[idx]`.

`stash_transfer_envelope` (`transfer_state.rs:7`) resolves the source cap to `source_object` **once,
at enqueue**, and stores it. `peek_transfer_envelope_source_object` (`transfer_state.rs:110`) is a
pure, non-consuming read of that field.

## 2. Reply { index, generation } vs source CapId vs user flag

- **`source_object`** holds the kernel-derived `CapObject::Reply { index, generation }`, captured at
  stash time. **This is the authoritative identity** and it survives the queue interval by value.
- **`source_cap`** is the sender-local CapId, carried **only** as delegation-parent bookkeeping — it
  is never resolved-to-mint on the receiver side (the receiver-local cap is minted fresh).
- **`FLAG_REPLY_CAP`** on the message is *not* the envelope's authority. For an IpcSend reply cap it
  is not even set (the message is `FLAG_CAP_TRANSFER`).

So the envelope already retains the strong (index, generation) identity — this is the raw material
for design **A**.

## 3. Where the current receiver-side misclassification occurs (Stage 193G FINDING 1)

Recv-side reply-vs-transfer classification is **flag-based**, not object-based:

- `extract_cap_transfer_plan` (`recv_core.rs:573`) → `RecvCapTransferPlan.is_reply_cap =
  (msg.flags & FLAG_REPLY_CAP) != 0`.
- `materialize_received_message_cap` (`ipc_recv_core.rs:200`) routes on
  `if (msg.flags & FLAG_REPLY_CAP) != 0 { "reply" } else if FLAG_CAP_TRANSFER { "transfer" }`.
- `recv_shared_v3.rs:348` recomputes `is_reply_cap` from the flag as well.

Because an **IpcSend reply cap is tagged `FLAG_CAP_TRANSFER`** (object-routed on the send side via
`peek_transfer_envelope_source_object`, since the ABI has no reply flag), on drain it takes the
**"transfer" branch** → `materialize_received_transfer_cap` → `grant_task_to_task_with_rights`
(delegation-link mint). A one-shot Reply object is thereby materialized as an **ordinary delegatable
cap**: it never routes to the reply direct-mint path, never records `waiter_cap_id`, and loses the
one-shot record coupling. This is precisely the misroute that makes queued reply-cap enqueue
un-retire-able today.

Note the asymmetry: the **"reply" branch itself is already object-authoritative** — it takes the
envelope, requires `envelope.source_object == CapObject::Reply` (else `WrongObject`), and re-checks
`capability_object_live` (else `InvalidCapability`) *before* minting SEND-only. The defect is the
**routing gate in front of it**, which trusts the flag instead of the object.

## 4. Must the source cap remain valid until dequeue?

**No — and the design must not require it.** The authoritative identity is captured into
`source_object` at stash time. Finalization revalidates against the **global Reply registry**
(`capability_object_live(Reply{index,generation})`, which compares `generation` against
`reply_cap_generations[index]`), never by re-resolving the sender's `source_cap` in the caller's
CSpace. Requiring the source CSpace entry to stay live would be a **hard-stop** (§Hard-stops).

## 5. Caller/receiver lifecycle effects on the queued envelope

- **Caller (source) exit / reap:** `exit_task` / `mark_task_dead` calls
  `revoke_reply_caps_for_caller`, which sets `reply_caps[slot] = None` **but does NOT bump the slot
  generation** (`ipc_state.rs:175`; the generation is bumped only on the next slot reuse by
  `create_reply_cap_for_caller`). Proven by `reply_caps_are_revoked_when_caller_exits` /
  `_marked_dead`. **Consequence for the queued class:** the finalize revalidation must be the
  full record check `resolve_reply_index` (slot `is_some()` **AND** generation match), not merely
  `capability_object_live` (which only compares the generation and would still return `Some` after a
  caller exit that left the generation untouched). Independently, `purge_transfer_envelopes_for_pid`
  (`cnode_state.rs:310`) clears any queued envelope whose source **or** receiver pid matches the
  exiting process → **no envelope leak**.
- **Generation replacement (restart + re-mint):** slot reuse *does* bump the generation, so the old
  (index, generation) fails **both** the generation check and, until reused, the record-present
  check. A reused numeric TID does **not** revive it (generation is the discriminator, not the TID).
- **Cancellation:** consuming/cancelling the record (e.g. `ipc_reply`, revoke) sets
  `reply_caps[slot] = None`; a later dequeue of a queued envelope for that slot fails the
  record-present half of the finalize check.
- **Endpoint teardown:** the buffered message is dropped with the endpoint queue; the stashed
  envelope is reclaimed by `purge_transfer_envelopes_for_pid` on the owning process's teardown. (198D2
  adds a targeted guard test that a torn-down endpoint leaves no live envelope reachable.)

## 6. Where the Reply record is validated

| Point | Today | Design (198D2) |
|---|---|---|
| **At enqueue** | `stash_transfer_envelope` resolves `source_cap` → `source_object` (fails closed if unresolvable) | unchanged |
| **At dequeue** | `take_transfer_envelope` validates handle generation, endpoint match, bound-receiver match; one-shot Created→Released | unchanged |
| **After provisional mint** | reply branch: mint SEND-only, then `set_reply_cap_waiter_cap` | unchanged (mint is the provisional step; failure → rollback §7) |
| **Immediately before publication** | reply branch re-checks `capability_object_live` (generation only) **before** the mint; there is **no** object-based routing gate, so a `FLAG_CAP_TRANSFER` reply cap never reaches this check | **object-based routing gate** + **strengthen** the pre-mint revalidation to the full record check `resolve_reply_index` (slot present **AND** generation) so a caller that exited during the queue interval is rejected |

Two changes are required for the queued class: (a) the **object-based routing gate** (missing
entirely), and (b) **strengthening** the pre-mint revalidation from generation-only
(`capability_object_live`) to the full record check (`resolve_reply_index`). The direct path can rely
on `capability_object_live` alone because the caller is blocked in-flight and cannot exit between
call and immediate delivery; the queued path spans an arbitrary interval, so the record-present half
becomes load-bearing. The one-shot `take_transfer_envelope` already exists and is retained verbatim.

## 7. Rollback of a provisionally minted receiver cap

`rollback_materialized_recv_cap(receiver_tid, cap, is_reply_cap=true)` (`transfer_state.rs:149`):

1. `fast_revoke_reply_cap_in_cnode` clears the receiver cnode slot (no `cap_refcount` on a Reply
   cap);
2. `clear_reply_cap_waiter_cap(index, generation)` drops the now-stale `waiter_cap_id` so
   `ipc_reply` won't fast-revoke a cleared slot.

The global `ReplyCapRecord` itself is **not** consumed by the mint, so after rollback it stays
**live and re-deliverable** — a copy/CNode failure leaves the canonical drop state (message dropped,
receiver stays blocked, record intact), with no cap/waiter/record leak. The envelope was already
consumed by the one-shot `take`, so the faulted message cannot re-mint.

## 8. Can a queued envelope preserve stale authority after caller exit?

**No, provided finalization revalidates the full record** (which design A mandates). The envelope
by-value `source_object` can still *name* a dead `Reply{index, gen}`. After a **caller exit** the
slot is `None` while the generation is unchanged, so the **record-present** check
(`resolve_reply_index` → `reply_caps[index].is_some()`) is what rejects it — `capability_object_live`
alone (generation-only) would wrongly pass. After a **generation replacement** both halves reject.
The only ways stale authority could survive are a **flag-authoritative** classifier (skips the
object entirely) or a **generation-only** revalidation (misses caller exit) — design A forbids both
by routing on `source_object` and finalizing on the full record check.

## 9. Can duplicate dequeue / retry mint multiple receiver caps?

**No.** `take_transfer_envelope` is one-shot: it validates the slot generation and transitions
`Created→Released`, setting the slot to `None`. A second take of the same handle returns `None`
(generation now mismatches / slot vacant). One queue entry ⇒ at most one successful materialization
⇒ at most one receiver cap. A post-mint copy fault drops the message (the envelope is already gone)
and rolls back the single mint (§7); it never re-queues a live envelope.

## 10. Delegation semantics vs queue lifetime

Reply caps are **SEND-only, one-shot, non-delegatable**. The queued envelope stores the **object**
(`Reply{index,generation}`), not a delegation link. The one-shot property is enforced by the single
global `reply_caps[slot]` record (first `ipc_reply` sets it `None`), **independent of how many
CSpace referents name the object**. During the queue interval the object may transiently have two
referents (the sender's source cap + — after materialize — the receiver's minted cap), but there is
still exactly **one record ⇒ one successful invocation**. Queue lifetime therefore does not multiply
Reply authority. The 193G misroute is dangerous precisely because the **ordinary** transfer branch
*does* create a delegation link — which must never happen for a Reply object.

---

## Chosen design — **A: typed queued transfer envelope carrying kernel-derived object identity**

**Rationale.** The envelope *already* carries `source_object: CapObject` resolved by the kernel at
enqueue, and the reply materialize branch is *already* object-authoritative (Reply-object check +
liveness revalidation before mint). The only defect is the **flag-based routing gate** in front of
it. Design A closes the gap with the minimum, architecture-neutral change: **classify the queued
message by the envelope's resolved `source_object`, not by the message flag.** No new envelope
field, no ABI change, no lock change, no capacity change.

**Why not B (source-cap reference + re-resolution at dequeue).** Re-resolving the sender's
`source_cap` at dequeue requires the caller's CSpace entry to remain valid across the queue
interval. The caller may have exited, been reaped, or had its generation replaced — leaving a stale
or reused CSpace slot. Relying on that is a **hard-stop** and could resurrect stale authority. B is
rejected. (The envelope's by-value `source_object` + global-registry liveness check is strictly
safer: identity is frozen at enqueue, authority is revalidated against the record, and neither
depends on the sender's live CSpace.)

**Why not "another design."** No architecture-neutral alternative gives a stronger lifetime proof
than "freeze kernel-derived identity at enqueue + revalidate the record at finalize" while reusing
the already-proven direct-path materialize. C is unnecessary.

### Design invariants satisfied

| Invariant | How design A satisfies it |
|---|---|
| classification derives from an authoritative resolved Reply object | routing gate reads `envelope.source_object`, not `msg.flags` |
| user `FLAG_REPLY_CAP` never authoritative | flag is not the discriminator; the object is |
| `Reply{index,generation}` survives the queue interval safely | stored by value in the envelope, keyed by slot generation |
| Reply record revalidated immediately before finalization | full record check `resolve_reply_index` (slot present **AND** generation) before mint — stronger than the direct path's generation-only check |
| caller generation replacement invalidates the queued transfer | old generation ≠ `reply_cap_generations[index]` → record check fails |
| caller exit (no generation bump) invalidates the queued transfer | `reply_caps[index]` is `None` → record-present check fails |
| one queue entry ⇒ at most one receiver cap | one-shot `take_transfer_envelope` |
| one Reply record ⇒ at most one successful invocation | single global `reply_caps[slot]` consume point |
| copy/CNode failure ⇒ canonical retry/drop | `rollback_materialized_recv_cap` clears slot + waiter; record stays live |
| no cap/waiter/envelope/Reply-object leak | one-shot take + rollback + `purge_transfer_envelopes_for_pid` |

## Enqueue / dequeue validation points (design A)

- **Enqueue:** `stash_transfer_envelope` resolves + freezes `source_object` (fail-closed).
  Reply-object messages continue to enqueue through the unchanged plain seam (they are *not* an
  ordinary-cap enqueue — the ordinary split already excludes `is_reply_object`).
- **Dequeue routing gate (NEW in 198D2):** peek `source_object`; if `CapObject::Reply{..}`, route to
  the reply direct-mint path **regardless of message flag**.
- **Finalization:** one-shot `take_transfer_envelope` → require `source_object == Reply` →
  full record revalidation `resolve_reply_index` (slot present AND generation) → mint SEND-only →
  `set_reply_cap_waiter_cap`.
- **Rollback:** on copy/CNode/meta failure after mint, `rollback_materialized_recv_cap` clears the
  slot + waiter; the record stays live; the message drops.

## Caller & receiver lifecycle rules (design A)

- Caller exit/reap/gen-replacement/cancel ⇒ record non-live ⇒ finalize rejects
  (`InvalidCapability`); `purge_transfer_envelopes_for_pid` reclaims the envelope.
- Receiver exit/reap ⇒ `purge_transfer_envelopes_for_pid` (receiver-pid match) reclaims the queued
  envelope; a not-yet-materialized reply cap simply never materializes.
- Reused numeric TID never revives a dead record (generation is authoritative).

## Rollback rules (design A)

Identical to the accepted direct path: `fast_revoke_reply_cap_in_cnode` + `clear_reply_cap_waiter_cap`;
the record is never consumed by the mint, so it remains re-deliverable; the one-shot envelope is
already consumed so no re-mint is possible.

## Hosted tests added (198D1)

New module `stage198d1_queued_reply_cap_lifetime` in `src/kernel/boot/tests.rs` — clarifies the
lifetime model using existing primitives only (no production change):

1. `queued_reply_envelope_classification_is_object_authoritative` — a stashed envelope for a Reply
   source cap exposes `CapObject::Reply{..}` via `peek_transfer_envelope_source_object`, with no
   message flag consulted (the class discriminator is the object).
2. `queued_reply_envelope_take_is_one_shot` — `take_transfer_envelope` yields the Reply
   `source_object` once, then `None` (one entry ⇒ one receiver cap).
3. `caller_exit_requires_record_present_check_not_just_generation` — after caller exit the envelope
   still names the Reply object AND `capability_object_live` still returns `Some` (generation not
   bumped), but `reply_cap_record_present` is `false`; this is the precise reason the queued finalize
   must use the full record check, not the direct path's generation-only gate.
4. `queued_reply_envelope_reclaimed_on_process_teardown` — `purge_transfer_envelopes_for_pid` clears
   the queued reply envelope on a participating process's teardown, source **or** receiver (no
   envelope leak).
5. `provisional_reply_mint_rollback_leaves_record_live` — `rollback_materialized_recv_cap` clears the
   receiver slot + global `waiter_cap_id` while the `ReplyCapRecord` stays present (canonical drop
   state, no leak).

## Bounded implementation plan for Stage 198D2

1. **Object-based recv routing gate.** In the recv-side classifier
   (`extract_cap_transfer_plan` / `materialize_received_message_cap` routing), when
   `msg.transferred_cap()` is `Some(h)` and `peek_transfer_envelope_source_object(h) ==
   Some(CapObject::Reply{..})`, route to the reply direct-mint branch **regardless of `msg.flags`**.
   Keep the flag path working for `ipc_call` (which sets `FLAG_REPLY_CAP`) — object and flag agree
   there. Guard behind the same default-off oracle gating used for the other second-cohort classes;
   do **not** retire/enable by default in 198D2.
2. **Reuse the direct-path materialize, with one strengthened check** — keep the object check +
   SEND-only mint + `set_reply_cap_waiter_cap`, but replace the generation-only
   `capability_object_live` gate with the full record revalidation `resolve_reply_index` (slot
   present AND generation) so a caller that exited during the queue interval is rejected. No new mint
   code otherwise.
3. **Rollback reuse** — route copy/CNode/meta failures through `rollback_materialized_recv_cap`
   (reply flavor).
4. **Negative/lifecycle proof** — extend the reply-cap direct negative seal with queued-path cases:
   stale source generation across the queue interval, duplicate dequeue, caller-exit-before-dequeue,
   endpoint-teardown-before-dequeue, copy-fault-after-mint rollback. Reuse the 198C3 seal harness.
5. **One live oracle per arch** (gated) proving a no-waiter IpcSend reply cap enqueues, is dequeued
   by recv-v2, materializes one-shot, wakes the caller once, and rejects a second invocation — then
   a cross-arch `SECOND_COHORT_REPLY_CAP_ENQUEUE_SEAL`.
6. **Scope guard** — no lock, ABI, CNode/queue capacity, or DebugLog change; shared-region and D2
   remain out of scope; the ordinary-cap enqueue split's Reply exclusion stays.

## Hard-stops honored

Safe queued delivery under design A relies on **none** of: stale source CSpace entries (identity is
frozen in the envelope; liveness comes from the global registry), user flags (object is the
discriminator), duplicate Reply authority (one record ⇒ one invocation), or an ABI change (no wire
change). If 198D2 finds any of these unavoidable, it must stop and re-audit.
