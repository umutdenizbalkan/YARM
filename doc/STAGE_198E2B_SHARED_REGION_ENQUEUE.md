<!-- SPDX-License-Identifier: Apache-2.0 -->
# Stage 198E2B — Queued Shared-Region Reuse & Hosted Enqueue Seal

Extends the post-lock shared-region transaction (`src/kernel/boot/shared_region_txn.rs`) so the
QUEUED (no-waiter enqueue → receiver-side dequeue) delivery path reuses the **same** executor as the
DIRECT path, and proves the cancellation-request table is **fail-closed**. Hosted-only; no queued
delivery is wired into a live syscall, no architecture gate, no QEMU. Extends Stages 198E2A / 198E2A1.

## Cancellation-capacity proof — option C (fail-closed)

The cancellation-request table stays at its existing capacity
(`IpcState.shared_region_cancel_requests: [Option<SharedRegionCancelReq>; 4]`) — **no capacity bump**.
Silent cancellation loss is closed by two mechanisms:

1. **Stale eviction.** `shared_region_request_cancel(tid, asid)` first tries a free slot, then evicts a
   **stale** occupant — one whose `(tid, asid)` can no longer name any live receiver (task gone /
   Exited / Dead, or a different ASID now), so it could never be consumed by any transaction.
2. **A permanent fail-closed fuse.** If (and only if) every occupant is still live, the request cannot
   be recorded: `shared_region_request_cancel` returns `false` and sets the
   `IpcState.shared_region_cancel_overflow` latch. While the latch is set, **every** executor
   checkpoint (`shared_region_cancel_now`) treats cancellation as authoritative, so no transaction can
   map further, write back, publish, or wake. The latch is a permanent per-kernel-instance safety
   fuse: it never auto-clears, because the cancellation that overflowed was never recorded, so clearing
   it could let that receiver publish (silent loss). It resets only with the whole `IpcState` at init.

Capacity was **not** increased to pass the tests: the existing 4-slot table is honest — it is an
internal signal store, not coupled 1:1 to a transaction capacity, so proof C (fail-closed) is used
rather than proof A (capacity coupling).

Cancellation-capacity cases (`cap_*`, 5): all slots occupied → one more fails closed & latches; a
stale entry is evicted so a live cancellation records (capacity unchanged); a replacement-ASID request
stays distinct from the stale old-ASID request; no transaction publishes after an unrecordable
cancellation; the fuse is global and permanent.

## Origin-neutral executor (no fork, no duplicate mechanism)

`shared_region_direct_phase_a` / `shared_region_direct_execute` were **renamed** to the origin-neutral
`shared_region_phase_a(…, origin_direct: bool)` / `shared_region_execute` — the implementation is
unchanged and shared verbatim by both origins. `origin_direct` sets ONLY the snapshot's proof marker;
it never influences classification, rights attenuation, mapping, rollback, lifecycle, or wake. There is
exactly **one** shared-region transfer mechanism.

## Queued dequeue path

At the receiver-side dequeue the queued Message (carrying the envelope handle) is popped, the handle is
taken from the message, and the same Phase A runs with `origin_direct=false`, capturing the CURRENT
dequeuing task's TID/PID/ASID/generation. The sender's CSpace is resolved exactly once in Phase A and
**never** after dequeue. The envelope is consumed via `take_transfer_envelope_keep_pin` (the pin
transfers into the snapshot with no reference gap), and the same `shared_region_execute` runs.

## Atomic dequeue ownership

The queued message is popped exactly once (`Endpoint::recv`), and the envelope is consumed exactly once
(generation-checked `take_transfer_envelope_keep_pin`). Proven states that cannot occur: message
removed but envelope/pin owner lost; envelope consumed but message replayable; two receivers consuming
the same envelope; a stale-handle dequeue leaving a dangling entry. Every outcome ends in exactly
**Published** (one receiver cap + one active mapping) OR **Cancelled/failed** (mapped prefix unmapped,
provisional record removed, cap revoked, pin released, no wake). A failed queued transfer is dropped,
not reconstructed.

## Lifecycle & teardown (queued)

- **Source exit before dequeue** → `purge_transfer_envelopes_for_pid` reclaims the envelope and
  releases the pin; the queued message goes inert (its handle no longer resolves).
- **Receiver exit before dequeue** → the receiver-bound envelope association is removed and the pin
  released; no transaction can be built from the queued message.
- **Receiver exit after snapshot** → the executor cancels (`ReceiverGone`); the single rollback
  releases the pin.
- **Endpoint teardown before dequeue** → the endpoint (and its queued message) is dropped and the
  envelope + pin are reclaimed together — no half-torn state.
- **Endpoint teardown after snapshot** → teardown marks a generation-bearing cancellation request; the
  executor observes it and cancels — no publication.
- **Reused numeric TID with a new ASID** → the stale (old-ASID) transaction fails the
  generation-bearing liveness check and cannot publish/cancel/inherit the old transaction.

## Rights & mapping invariants (preserved exactly)

Classification is object-authoritative (MemoryObject / DmaRegion required; Endpoint / Reply / ordinary
excluded); the receiver cap is freshly minted with attenuated rights (source ∩ recv-intent; WRITE
dropped without intent); MAP is required; WRITE requires the canonical WRITE right; the mapping is
ALWAYS non-executable (`execute: false`); DmaRegion bounds are enforced at stash; the user copy is
outside all locks; the wake is once, after the final revalidation. Reply caps remain direct-only
excluded (`REPLY_CAP_QUEUEING_SUPPORTED=false`).

## Cases & seal

`stage198e2b_shared_region_enqueue` = 23 queued cases + 5 cancellation-capacity cases.
`scripts/qemu-shared-region-enqueue-hosted-seal.sh` runs the 23 queued cases and emits:

```
SECOND_COHORT_SHARED_REGION_ENQUEUE_HOSTED_SEAL cases=23 leaked_messages=0 leaked_envelopes=0 \
  leaked_caps=0 leaked_mappings=0 leaked_transactions=0 leaked_pins=0 duplicate_wakes=0 result=ok
```

Both prior seals stay green: `SECOND_COHORT_SHARED_REGION_DIRECT_HOSTED_SEAL` (18/18) and
`SECOND_COHORT_SHARED_REGION_TXN_RACE_SEAL` (12/12).

## Preserved

`SYSCALL_COUNT=32`, `VARIANT_COUNT=22`, NR27 absent, DebugLog=192, `REPLY_CAP_QUEUEING_SUPPORTED=false`;
no new syscall / ABI / lock / queue-type / CNode-capacity / mapping-permission / capability-transfer
variant. No D2/IpcCall/Reply/timeout/notification/D3/D6 work. Full hosted suite 2962/0.

## Stage 198E3 live plan (precise)

1. Wire the queued dequeue reuse into the real recv-v2 / RecvSharedV3 syscall path behind the existing
   oracle-proof knob (default-off), calling `shared_region_phase_a(…, origin_direct=false)` +
   `shared_region_execute` at the post-lock boundary — no new mechanism.
2. Add a live queued-shared-region oracle (userspace producer enqueues with no waiter; a later receiver
   dequeues and maps) plus its slot-provisioned smoke toggle, mirroring the direct oracle.
3. Route endpoint-teardown / process-exit of an in-flight queued dequeue through
   `shared_region_request_cancel` so the executor owns cleanup (the fail-closed fuse already covers the
   unrecordable edge).
4. Prove the live path on all three architectures under the full QEMU battery, emit a live enqueue
   seal, and keep the direct + race + enqueue hosted seals and the first-cohort seal green.
5. No syscall/ABI/lock/capacity change, `REPLY_CAP_QUEUEING_SUPPORTED=false` preserved, no
   D2/IpcCall/timeout/notification/D3/D6 work.

## Hard-stops honored

No silent cancellation loss (stale eviction + permanent fail-closed fuse); message and envelope
ownership never separate (single pop + generation-checked single consume); no duplicate dequeue; no
sender-CSpace re-resolution after dequeue; no page mapped after cancellation (the shared checkpoints
gate every map); the user copy is outside all locks; and there is exactly one shared-region transaction
mechanism (the direct executor, renamed origin-neutral — not forked).
