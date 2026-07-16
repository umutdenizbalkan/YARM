<!-- SPDX-License-Identifier: Apache-2.0 -->
# Stage 198E2A1 — Shared-Region Transaction Cancellation & Partial-Map Hardening

Hardens the post-lock shared-region DIRECT transaction (`src/kernel/boot/shared_region_txn.rs`) so it
has **exactly one cleanup owner** under receiver-exit, cancellation, and partial multi-page mapping
races. Hosted-only; no queued delivery, no architecture gate, no QEMU. Extends Stage 198E2A.

## Selected cleanup-ownership protocol — A (executor-owned)

Teardown **marks** a generation-bearing cancellation request; the executor **observes** it at its
checkpoints and performs ALL unmap/revoke/pin-release cleanup itself. Teardown never directly cleans
an in-flight transaction, so there is never a second cleanup actor. `rollback_shared_region_direct_txn`
claims the unwind by transitioning to `CleanupOwned` (a one-shot transition); a `Published` txn is
never rolled back and a `Cancelled` txn is fully unwound, so any second/third invocation is a no-op —
nothing can be unmapped, revoked, or pin-released twice.

The request registry is a bounded `IpcState.shared_region_cancel_requests: [Option<SharedRegionCancelReq>;
4]` (`SharedRegionCancelReq { tid, asid }`) — an internal signal table, **not** a queue/CNode/ABI
capacity. `shared_region_request_cancel(tid, asid)` records; the executor's `shared_region_consume_cancel`
one-shot-consumes a request matching BOTH the numeric TID and the captured ASID.

## Transaction-state changes

`SharedRegionTxnState` gains `Mapping` (mapping in progress; `mapped_prefix_len` is authoritative),
`CancelRequested` (cancellation authoritative — no further page/writeback/wake), and `CleanupOwned`
(the executor's one-shot cleanup claim), alongside `Reserved → CapMinted → … → Mapped → Published |
Cancelled`.

## Partial-map progress tracking

`SharedRegionDirectTxn.mapped_prefix_len` (bytes) is updated **after each successful page, before the
next**. Rollback unmaps **exactly** `map_va .. map_va + mapped_prefix_len` (two-phase; the shootdown
completes before frames are freed). It does not depend on the txn reaching the terminal `Mapped`
state, so a failure on page N unmaps exactly pages `0..N-1` — no orphan pages. `map_user_page_in_asid_raw`
maps one page at a time, so there is no hidden atomic-internal rollback to prove elsewhere.

## Cancellation checkpoints

The executor calls `shared_region_cancel_now(snapshot, checkpoint)` (which folds a pending
generation-bearing request, a test hook, and receiver liveness) at:

1. before cap mint;
2. before the first map;
3. between page mappings (`i > 0`, before mapping page `i`);
4. after mapping, before writeback;
5. immediately before the user writeback;
6. immediately before publication and wake (phase-8 final revalidation).

After cancellation is authoritative: no further page is mapped, no writeback occurs, no wake occurs,
and no active mapping is published — the executor rolls back and returns `Cancelled` (or
`StalePublish` at checkpoint 6).

## Generation-bearing teardown matching

Transaction lookup, teardown matching, publication, and cancellation all use the captured **ASID**
in addition to the numeric TID. `shared_region_consume_cancel` requires `req.tid == tid && req.asid ==
asid`; `shared_region_receiver_alive` requires `task_asid(tid) == snapshot.receiver_asid`. A delayed
lifecycle action recorded for an old TID (old ASID) therefore cannot cancel or publish a replacement
process's transaction (new ASID) — proven by `delayed_old_tid_teardown_does_not_affect_replacement_asid`.

## Cleanup & shootdown ordering (single owner)

`rollback_shared_region_direct_txn`: block publication/wake (never performed) → **unmap the mapped
prefix** (`unmap_range_two_phase`, completing the required TLB shootdown before freeing) → clear the
prefix/`mapped` record → **remove** the active-mapping registry entry → **revoke** the provisional
receiver cap (guarded by `take()`) → **release** the object pin (guarded by `pin_owned`, NEVER before
the unmap+shootdown) → mark `Cancelled`. Every step is idempotent or guarded by a one-shot transition.

## Race cases & seal

`stage198e2a1_shared_region_txn_race` (12 cases) + `scripts/qemu-shared-region-txn-race-seal.sh` →
`SECOND_COHORT_SHARED_REGION_TXN_RACE_SEAL cases=12 orphan_pages=0 duplicate_unmaps=0
duplicate_revokes=0 duplicate_pin_releases=0 stale_publications=0 result=ok`. Covers all 16 required
behaviors: cancel before first / after first / between later pages; page-N failure unmaps the prefix;
teardown-request honored by the executor; executor-publishes-before-teardown; single-owner idempotent
cleanup (one unmap / one revoke / one pin release); no map/writeback/wake after cancellation; delayed
old-TID teardown vs replacement ASID; stale executor cannot publish; successful multi-page publish;
rollback idempotent across states.

## Preserved

`SYSCALL_COUNT=32`, `VARIANT_COUNT=22`, NR27 absent, DebugLog=192, `REPLY_CAP_QUEUEING_SUPPORTED=false`;
no ABI/syscall/lock/queue/CNode-capacity or mapping-policy change (the cancel-request table is an
internal signal store, not a queue/CNode/ABI). Ordinary-cap and reply-cap paths untouched. Full hosted
suite 2934/0.

## Stage 198E2B queued-reuse plan (precise)

1. On a no-waiter shared-region send, keep stashing the envelope (as today).
2. At recv-v2 / RecvSharedV3 dequeue, `take_transfer_envelope_keep_pin` into a
   `RecvBoundarySharedRegionSnapshot` where `receiver_tid`/`receiver_asid` are the CURRENT task's
   (the receiver is running its own recv), then call `shared_region_direct_execute` **unchanged** —
   the same checkpoints, `mapped_prefix_len`, cancel registry, and single rollback apply.
3. Reuse `shared_region_request_cancel` for endpoint-teardown/process-exit cancellation of a queued
   in-flight dequeue; the generation-bearing (tid, asid) match already handles TID reuse.
4. Classify + hosted-prove the enqueue path independently (do not assume safe because direct is);
   emit a distinct enqueue seal.
5. No second transfer mechanism, no new syscall/ABI/lock/capacity, `REPLY_CAP_QUEUEING_SUPPORTED=false`
   preserved, no D2/IpcCall/timeout/notification/D3/D6 work, no QEMU in the hosted increment.

## Hard-stops honored

No path maps a page after cancellation (checkpoints 2/3 gate every map); one cleanup owner (protocol
A + `CleanupOwned` one-shot); no untracked partial pages (`mapped_prefix_len` + provisional registry
registered before the first map); no numeric-TID-only lifecycle authority (ASID-bearing matching); no
pin release before unmap + required shootdown complete (rollback order).
