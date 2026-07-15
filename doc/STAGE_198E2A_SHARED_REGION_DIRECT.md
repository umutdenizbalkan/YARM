<!-- SPDX-License-Identifier: Apache-2.0 -->
# Stage 198E2A — Shared-Region Post-Lock Transaction & Direct Hosted Path

Builds the architecture-neutral post-global-lock shared-region DIRECT delivery transaction and proves
it through hosted production-path tests. No QEMU, no live architecture class, no arch retirement
gate; queued shared-region delivery stays on its current fallback path. Implements the bounded fix
the Stage 198E1 audit called for. Source: `src/kernel/boot/shared_region_txn.rs`.

## Snapshot ownership & object-pin transfer

`RecvBoundarySharedRegionSnapshot` is owned/copyable state captured under the broad lock in
`shared_region_direct_phase_a`: frozen `object` + `object_generation`, attenuated destination
`rights`, `descriptor` (offset/len), `source_tid`/`source_cap` (bookkeeping only), `receiver_tid`/
`receiver_pid`/`receiver_asid`, `endpoint`, `map_va`/`meta_ptr`, `map_write`, `pin_owned`,
`origin_direct`, and the `msg`. Phase A consumes the envelope via `take_transfer_envelope_keep_pin`
— the shared-region `+1` MemoryObject pin is **not** dropped, it is transferred into the snapshot
(`pin_owned = true`), so the pin never transiently reaches zero (no reference gap). Sender CSpace is
resolved exactly once, here; it is **never** re-resolved after the lock drops.

## Transaction states

`SharedRegionTxnState`: `Reserved → CapMinted → Mapped → Published` (success) or `→ Cancelled`
(failure). Exactly one terminal transition per one-shot `SharedRegionDirectTxn`, which also owns the
intermediate `minted_cap` and `mapped` `(base,len)` that rollback unwinds.

## Provisional lifecycle registration

The `ActiveTransferMapping` is registered (`register_active_transfer_mapping(receiver_tid,
minted_cap, map_va, mapped_len)`) **BEFORE the first page is mapped**, keyed by the receiver TID +
the generation-bearing minted CapId. Process-exit cleanup (`purge_active_transfer_mappings_for_pid`)
therefore owns reclamation of any partially-mapped region — there is no interval with a live mapping
and no registry owner. The receiver is identified by generation-bearing authority: the captured
`receiver_asid` + liveness (`task_asid(tid) == receiver_asid` and not Exited/Dead). A replacement
task reusing the numeric TID receives a different ASID, so a stale transaction can never publish.

## Lock & shootdown phase order

1. **(under lock)** Phase A snapshot: consume envelope keep-pin, resolve+attenuate rights, capture
   receiver generation authority.
2. **(post-lock)** Revalidate receiver generation-authority + object generation.
3. Bounds/rights gates (region len > 0, `map_va` page-aligned, `MAP` required, `WRITE` only with
   canonical WRITE).
4. Mint one fresh receiver-local cap (attenuated rights). → `CapMinted`.
5. Register the provisional active mapping, then map ONLY the authorized region into
   `receiver_asid` (`execute:false` always; `write:!read_only`). Fresh maps need no TLB shootdown
   (only rollback unmaps do). → `Mapped`.
6. User metadata copy (`copy_to_user`) **outside all locks**.
7. Final revalidation of receiver generation + object liveness **before** any publish.
8. Publish: wake the receiver **exactly once**, release the transferred object pin (the receiver-
   local cap now owns the reference), clear post-work state. → `Published`.

The wake never precedes mapping + writeback + final revalidation.

## Direct-path production wiring

`shared_region_direct_phase_a` (producer) + `shared_region_direct_execute` (executor) are real
`KernelState` methods using existing seams (`mint_capability_in_cnode`, `map_user_page_in_asid_raw`,
`register/remove_active_transfer_mapping`, `copy_to_user`, `apply_split_receiver_wake_plan`,
`unmap_range_two_phase`, `adjust_memory_object_pin_refcount`, `revoke_capability_in_cnode`). No new
syscall/ABI/lock/capacity; RecvSharedV3 ABI unchanged; not wired to an arch retirement gate.

## Rollback order & idempotence

Single `rollback_shared_region_direct_txn`, safe from every state, reverse order: prevent
publication/wake (never performed on this path) → `unmap_range_two_phase` (two-phase, tolerates
absent pages) then drop `mapped` → `remove_active_transfer_mapping` → `revoke_capability_in_cnode`
then drop `minted_cap` → release the object pin (`pin_owned` → false) → `state = Cancelled`. Each
undo is guarded by `Option::take` / the `pin_owned` flag, so it can never unmap or revoke twice; a
`Published` txn is never rolled back. The consumed direct transfer is dropped after failure, not
reconstructed.

## Exit / restart race results

| Race | Result |
|---|---|
| receiver exits before cap mint | phase-2 check → `ReceiverGone`, no mapping |
| receiver generation replaced (ASID changed) | phase-2 check → `ReceiverGone`, no mapping |
| receiver exits / gen-replaces after map, before publish | phase-7 final revalidation → `StalePublish`, exactly one unmap/revoke |
| source process exits after snapshot | delivery still succeeds (identity frozen; sender CSpace never re-resolved) |

## Hosted cases & seal

`stage198e2a_shared_region_direct` (18 cases) + `scripts/qemu-shared-region-direct-hosted-seal.sh`
→ `SECOND_COHORT_SHARED_REGION_DIRECT_HOSTED_SEAL cases=18 leaked_caps=0 leaked_mappings=0
leaked_transactions=0 duplicate_wakes=0 result=ok`. Covers: MemoryObject/DmaRegion snapshot
identity; pin transfer; source-exit-after-snapshot; receiver-exit-before-mint; generation
replacement; cnode-full; missing-MAP; write-without-WRITE; map-fault rollback; meta-copy-fault
rollback; successful one-cap/one-mapping/one-wake delivery; rollback idempotence; stale cleanup
token; reply/ordinary exclusion; stale-after-map single-unmap; NX-always guard; bad-region.

## Preserved

`SYSCALL_COUNT=32`, `VARIANT_COUNT=22`, NR27 absent, DebugLog=192,
`REPLY_CAP_QUEUEING_SUPPORTED=false`; no new syscall/ABI/lock/capacity. Full hosted suite 2922/0.

## Bounded Stage 198E2B plan (queued reuse)

Reuse the SAME snapshot + transaction + rollback for the enqueue path: a no-waiter shared-region
send stashes the envelope (as today); at recv-v2/RecvSharedV3 dequeue, take the envelope keep-pin
into a `RecvBoundarySharedRegionSnapshot` and run `shared_region_direct_execute` unchanged (the
receiver is current there, so `receiver_asid` = current ASID). No second transfer mechanism; classify
+ hosted-prove the enqueue path independently (do not assume it safe because direct is); keep all
gates, `REPLY_CAP_QUEUEING_SUPPORTED=false`, and the no-QEMU/no-arch-gate scope.

## Hard-stops honored

No untracked mapping window (active-mapping registered before the first page maps); no user copy
under any lock (metadata copy is post-lock); no sender-CSpace re-resolution (identity frozen in the
snapshot); no publication before final revalidation (phase 7 gates the wake); no reclaim before
required shootdown completion (rollback uses two-phase unmap which completes the shootdown before
freeing).
