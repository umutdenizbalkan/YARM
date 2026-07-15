<!-- SPDX-License-Identifier: Apache-2.0 -->
# Stage 198C Part 1 — Reply-Cap Semantics Audit (authoritative kernel model)

Traced against the current branch head (`ae38561`), x86_64 reference path. This audit fixes the
canonical model BEFORE any architecture wiring, per the Stage 198C Part 1 requirement. It does NOT
infer reply semantics from ordinary-cap behavior.

## 1. Objects and tables

- **Reply object**: `CapObject::Reply { index: usize, generation: u64 }` (`src/kernel/capabilities.rs:21`).
  The `index` is a slot into a global registry; `generation` is the slot's monotonic reuse counter.
- **Global registry** (in `IpcState`): `reply_caps: [Option<ReplyCapRecord>; MAX_REPLY_CAPS]` plus a
  parallel `reply_cap_generations: [u64; MAX_REPLY_CAPS]`. A slot is *live* iff `reply_caps[idx]`
  is `Some(_)` AND the cap's `generation` equals `reply_cap_generations[idx]`.
- **`ReplyCapRecord`** (`ipc_state.rs`): `{ caller_tid, reply_endpoint, responder_tid,
  caller_cap_id, waiter_cap_id }`.
  - `caller_tid` — the client C that made the outstanding call.
  - `reply_endpoint` — the caller's reply receive endpoint (where the reply payload is delivered).
  - `responder_tid` — optional pinned responder; if `Some`, only that TID may invoke the reply.
  - `caller_cap_id` — the CapId minted into C's cnode at call time (Phase 3 of creation).
  - `waiter_cap_id` — the receiver-local CapId materialized when the reply cap is *transferred*
    to a server via IPC (`None` until then).

## 2. Creation — `create_reply_cap_for_caller[_in_cnode]` (`ipc_state.rs:1706/1727`)

Called on the `IpcCall` path (and to provision the oracle cap into init's cnode at boot).

1. Resolve the caller's reply *receive* endpoint cap (must carry `CapRights::RECEIVE`; object must
   be `Endpoint`, else `WrongObject`).
2. **Phase 1** — reserve the first free slot; bump `reply_cap_generations[idx]` (skipping 0);
   install a placeholder `ReplyCapRecord` with `caller_cap_id = CapId(0)`, `waiter_cap_id = None`.
   Full registry → `CapabilityFull`.
3. **Phase 2** — mint `Capability::new(CapObject::Reply { index, generation }, CapRights::SEND)`
   into the destination cnode (active cnode by default). Mint failure rolls the reservation back.
4. **Phase 3** — write the real minted CapId back into `record.caller_cap_id`.

**Rights on a reply cap are exactly `CapRights::SEND`** — no READ, MAP, TRANSFER, MINT/COPY.

## 3. Transfer (send → deliver) — the 188D/193D split path

**Send side** (`syscall/ipc.rs::stash_transfer_handle`, used by IpcSend for all cap transfers):
resolves the source cap to validate it exists, then `stash_transfer_envelope(sender, source_cap_id,
endpoint, receiver, None)`. **It does NOT revoke or consume the source cap.** The envelope captures
the *source object identity* (the `Reply{index,generation}`) bound to `(endpoint, receiver)`.

**Deliver side** (`cap_transfer_split.rs::materialize_split_reply_cap_equivalent`), classified as
the reply arm only when the resolved source object is `Reply`:
- **Phase A** `phase_a_take_reply_envelope`: take the envelope; require `source_object ==
  CapObject::Reply` (else `WrongObject`); require the reply object still *live*
  (`capability_object_live`, else `InvalidCapability`); resolve receiver cnode.
- **Phase B** `phase_b_mint_reply_cap`: mint `Capability::new(Reply{index,generation},
  CapRights::SEND)` into the receiver's cnode → **fresh receiver-local CapId** pointing at the SAME
  reply object.
- **Phase B'** `phase_b_prime_record_reply_cap`: atomically `try_set_reply_cap_waiter_cap(index,
  generation, minted)`. On any stale outcome (index range / generation mismatch / slot empty) it
  rolls back the Phase-B mint via `rollback_materialized_recv_cap(receiver, minted,
  is_reply_cap=true)` and returns `WrongObject`; the reply object stays live and re-deliverable.
  On success the record's `waiter_cap_id` now points at the receiver-local cap.

## 4. Invocation — `ipc_reply(reply_cap, msg)` (`ipc_state.rs:2204`)

1. Resolve the invoked cap task-locally; require `CapRights::SEND`.
2. `resolve_reply_index(object)` → slot (validates index + generation against the live record).
3. If `responder_tid` is set, require the current task == that TID (else `MissingRight`).
4. **Consume**: `reply_caps[slot] = None` (this is the operation that marks the Reply object
   consumed — one-shot).
5. Fast-revoke (no-alloc, no delegation traversal — reply caps are never delegated) the replier's
   cap (`record.waiter_cap_id`, falling back to the passed `reply_cap`) from the replier cnode,
   bumping that cnode slot generation.
6. Fast-revoke `record.caller_cap_id` from the caller's cnode.
7. Deliver the reply payload and wake the caller C exactly once.

## 5. The ten required answers

1. **Does successful transfer invalidate/consume the sender's reply cap?**
   **No — not at transfer time.** `stash_transfer_handle` does not revoke the source; the source
   CapId remains resolvable in the sender's cnode after a successful transfer. It becomes *stale*
   only once the underlying reply object is consumed (by whichever holder invokes `ipc_reply`
   first, or when the slot is reused and its generation bumped).

2. **Can a Reply cap exist in two live CSpaces simultaneously?**
   **Yes, transiently** — after transfer, the sender still holds its source cap and the receiver
   holds the freshly-minted `waiter_cap_id`, both referencing the same `Reply{index,generation}`.
   **Single-use is enforced at the object/record level, not by cap uniqueness**: only one
   `ipc_reply` can succeed because step 4 empties the slot; the losing holder's cap then fails
   `resolve_reply_index` with `StaleCapability`. So this is **delegation with one-shot object
   authority**, NOT move/consume — the source-cap disposition must be attested as this exact model
   (Part 5), not asserted as "sender cap invalidated by transfer".

3. **Which operation marks the Reply object consumed?**
   `ipc_reply` step 4: `reply_caps[slot] = None`.

4. **When is the receiver-local reply cap removed?**
   On `ipc_reply` (fast-revoke of `waiter_cap_id` from the replier cnode, step 5), OR on Phase-B'
   stale rollback (`rollback_materialized_recv_cap … is_reply_cap=true`) if the mint→record race
   loses.

5. **What if the original caller C exits before transfer?**
   The reply record persists until consumed/reused, but Phase A's `capability_object_live` check
   and (on invocation) the caller-cnode revoke tolerate a missing caller cnode
   (`task_cnode` → `None` → best-effort). Delivery of the *reply payload* to a dead/reused caller is
   the lifecycle hazard Part 9 must gate via generation/object identity, not numeric TID.

6. **What if the receiver B exits after receiving but before replying?**
   The `waiter_cap_id` cap dies with B's cnode; the reply object slot remains live (never consumed)
   until reused. No caller wake occurs. This is an *orphaned reply object* case (Part 8/9): it must
   not leak or be double-counted, and must not later reply to a reused caller TID.

7. **Which rights are legal on a reply cap?**
   Exactly `CapRights::SEND` (created at §2.3, minted at §3 Phase B). No transfer/mint/copy/map/read.

8. **Can reply caps be copied, minted, moved, or transferred again?**
   The reply cap carries only SEND (no MINT/COPY/TRANSFER right), and `ipc_reply` does not traverse
   or create delegation links (reply caps are "never delegated"). The only sanctioned movement is
   the single IPC transfer that mints one receiver-local cap; a second retransfer is a negative case
   (Part 8) that must be rejected by the missing transfer right / stale object.

9. **Which metadata flag tells recv-v2 the delivered cap is a reply cap?**
   The recv-v2 metadata flag set on the reply arm (distinct from
   `SYSCALL_RECV_META_TRANSFERRED_CAP` used for ordinary caps) — carried via `Message::FLAG_REPLY_CAP`
   on the wire and re-derived by the receiver-side classifier; the delivered meta marks the cap as a
   reply cap so the receiver invokes it with `ipc_reply`, not as an ordinary object.

10. **Is that metadata derived exclusively from object identity?**
    The authoritative selection is object-derived: the delivery arm is chosen by
    `envelope.source_object == CapObject::Reply` (Phase A), and the delegation-split materialize
    "classifies reply caps by the authoritative object kind" (`matches!(snapshot.object,
    CapObject::Reply { .. })`). The user-controlled `FLAG_REPLY_CAP` wire flag must NOT be the
    authority (Part 2); it is a hint re-validated against the resolved object.

## 6. Source-cap disposition (Part 5 determination)

**Model = delegation with restricted one-shot authority (NOT move/consume).** Concretely:
- Transfer mints a second SEND-only cap to the same one-shot `Reply{index,generation}` object and
  does not revoke the sender's source cap.
- One-shot is enforced by the single global `reply_caps[slot]` record: the first `ipc_reply`
  consumes it; all other holders' caps become stale.
- Therefore the one-shot oracle (Part 5) must attest THIS model: after a successful transfer +
  receiver reply, the **object** is consumed (second invocation via any cap → rejected, no second
  caller wake), and the source-side attestation records that the sender's cap is not moved-away by
  the transfer itself but is rendered stale by the object's consumption. The Part-5 marker's
  `second_reply=rejected caller_wakes=1 duplicate_reply=0` fields are the object-level one-shot
  proof; the "sender cap invalid after transfer" phrasing in the task's *move* branch does not apply
  — the *other-model* attestation branch does.

## 7. Hard-stop relevance

The "no duplicate Reply authority / no second successful reply / caller woken twice" invariants are
satisfied by the object-level one-shot record, NOT by cap-count uniqueness. Any Stage 198C wiring
must preserve `reply_caps[slot] = None` as the single consume point and must keep Phase-B' stale
rollback intact, or duplicate-reply becomes possible. The transient two-CSpace coexistence is
SAFE under the current model but means the oracle must prove one-shot at the **object** layer.

## 8. Stage 198C3 — Reply-Cap Direct Negative Seal & Final Acceptance

The positive reply-cap direct-delivery topology and the one-shot oracle (198C2/198C2B, sealed
`SECOND_COHORT_REPLY_CAP_DIRECT_SEAL arches=3 classes=1 live_cells=3 result=ok`) are **unchanged**
— no delivery/topology change was made because no real defect was found. 198C3 adds the negative
half and reconciles stale guards.

### Negative / rollback seal
`scripts/qemu-reply-cap-direct-negative-seal.sh` drives 18 hosted production-path cases through the
real producer/executor + reply-cap lifecycle helpers, each asserting its own rollback invariant:
invalid source cap; stale source generation; non-Reply source; invalid receiver payload/metadata
dest; provisional-cap rollback on executor copy fault; Phase-B' stale-record rollback; duplicate
transfer attempt (no double mint); one-shot record arbiter / duplicate invoke; caller exit/reap
revoke; caller generation replacement + reused-TID stale reply; rollback clears slot + waiter cap;
record slot cleared on revoke; receiver-cnode teardown; server exit holding delegated cap;
unbound-responder rejection. Emits:
`SECOND_COHORT_REPLY_CAP_DIRECT_NEGATIVE_SEAL cases=18 leaked_reply_caps=0 leaked_reply_objects=0
duplicate_wakes=0 duplicate_replies=0 result=ok` — the zeros are by construction (the seal fails
unless every case passes, and every case asserts no leak / no wake / no duplicate).

### AArch64 InvalidCapability exemption scope
`aarch64_invalidcapability_exemption_is_narrow` proves the core-smoke's blocker exemption is
restricted to the exact gated one-shot second-invoke line
(`IPC_REPLY_FAIL tid=<n> reply_cap=<n> err=InvalidCapability`): `InvalidCapability` remains in
`BLOCKER_REGEX`, and the exclusion does NOT hide `IPC_RECV_CAP_MATERIALIZE_FAILED`, `CAP_LOOKUP`,
or `IPC_CALL_FAIL`. The exemption cannot suppress any other reply-cap failure.

### Reconciled stale guards
The 198C2/198C2B cross-arch wiring left four hosted guards asserting the *old* x86-only reply-cap
oracle (they were not caught because the full suite was not re-run then). They are reconciled to the
accepted reality (topology unchanged): the retirement marker is now arch-tagged for all three arches;
init dispatches on the `if let Some(reply_recv_cap) = ctx.pm_request_recv_cap` slot-17 discriminator;
and `reply_cap_oracle_provisioned_on_all_arches` replaces the two `no_reply_cap_oracle_on_non_x86`
guards.

### Combined preservation
The reply-cap-direct cohort and the supervisor crash-restart baseline are added to
`scripts/qemu-combined-retirement-seal.sh`, which now proves, strictly serialized:
first-cohort 12/12, plain 6/6, ordinary-cap 6/6, reply-cap-direct 3/3, crash-restart result=ok, and
emits `SECOND_COHORT_PROGRESS first=12 plain=6 ordinary_cap=6 reply_cap_direct=3 result=ok`.

### Preserved invariants (unchanged in 198C3)
SYSCALL_COUNT=32, VARIANT_COUNT=22, NR27/InitramfsReadChunk absent, DEBUG_LOG_MAX_BYTES=192, no new
global lock, no ABI/capacity change, reply-cap **enqueue** path still not provisioned
(`provision_init_ipc_send_reply_cap_enqueue` absent), shared-region and D2 still unretired.
