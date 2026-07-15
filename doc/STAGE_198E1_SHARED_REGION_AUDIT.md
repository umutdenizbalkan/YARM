<!-- SPDX-License-Identifier: Apache-2.0 -->
# Stage 198E1 — Shared-Region IpcSend Audit & Hosted Preparation

Hosted-only audit, traced against branch head `263624f`, x86_64 reference path. No live class is
enabled and no QEMU is run. This prepares the shared-region `IpcSend` path for cross-architecture
retirement (Stage 198E2 implements; Stage 198F produces the complete supported-IpcSend seal).

Roadmap position: plain (direct+enqueue), ordinary-cap (direct+enqueue), and reply-cap-direct are
accepted; reply-cap enqueue is **unsupported by policy** (`REPLY_CAP_QUEUEING_SUPPORTED = false`,
preserved). Stage 198E covers shared-region IPC.

## 1–2. Shared-region object types

A shared-region transfer is exactly a transfer cap whose resolved `CapObject` is one of:

- **`CapObject::MemoryObject { id }`**
- **`CapObject::DmaRegion { id, offset, len }`**

`handle_ipc_send` builds an `OPCODE_SHARED_MEM` message ONLY when the transfer grant resolves to one
of these two variants (`ipc.rs` `match grant.object { MemoryObject | DmaRegion => {} , _ => WrongObject }`,
at both the user-ASID and kernel-ASID `len > IPC_REGISTER_BYTES` arms). No other object variant is a
shared-region transfer; a Reply object is never one.

## 3–4. Rights (sender required / receiver installed)

- **Sender:** `validate_shared_mem_transfer_rights(&grant)` is required on the grant; a **writable**
  receiver mapping additionally requires the source cap to carry `CapRights::WRITE`. `CAP_RIGHT_MAP`
  is required for any mapping.
- **Receiver:** the receiver-local cap is minted by the transfer materialize (delegation), then
  `attenuate_transfer_cap_for_recv_intent` **drops `WRITE`** unless the receiver's `map_intent`
  requested (and the source carried) write: a read-only recv intent yields `READ | MAP`. The mapping
  permission is `write: !read_only, execute: false` — **never executable**, and writable only with
  canonical `WRITE` authority (`compute_recv_v3_mapping_plan`:
  `wants_write && !(cap_rights & CAP_RIGHT_WRITE) → InsufficientRights`).

## 5. Transfer contract

**Delegation** (identical to ordinary cap-transfer): the receiver-local cap is minted fresh via
`grant_task_to_task_with_rights` / the D1 transfer arm (`materialize_split_transfer_cap_equivalent`).
The sender's source cap is not moved/consumed. The shared-region descriptor (`TransferSharedRegion {
offset, len }`) rides in the stashed `TransferEnvelope` and pins the underlying MemoryObject
(`adjust_memory_object_pin_refcount(+1)`) for the queue interval.

## 6. Direct vs enqueue — same typed object snapshot?

Both paths carry the SAME kernel-resolved `TransferEnvelope.source_object` (+ `shared_region`), so
classification identity is shared. But **delivery mechanics differ and the mapping is not split**:

- **Enqueue (no waiter):** `stash_transfer_envelope(..., Some(TransferSharedRegion))` pins the object
  and queues the `OPCODE_SHARED_MEM` message; the receiver later dequeues via `ipc_recv` (recv-v2
  full path, `handle_ipc_recv_result_with_empty_error`) or `RecvSharedV3` (NR30) and maps.
- **Direct (blocked receiver):** the IpcSend boundary splits (plain → reply → ordinary) all
  **decline** shared-region (`OPCODE_SHARED_MEM` excluded), so `complete_blocked_recv_for_waiter`
  delivers the message; the mapping still happens at the receiver's recv completion.

In **both** cases the cap materialize + `map_shared_region_into_receiver` (or the RecvSharedV3
mapping) + `register_active_transfer_mapping` + the user metadata copy run **entirely under the broad
`&mut KernelState`** (the global lock). There is **no** post-lock resolve→snapshot→map/copy boundary
for shared-region (the plain/ordinary/reply-direct classes have one; shared-region was explicitly
excluded from the ordinary split — "RecvSharedV3/VM territory", Stage 193G FINDING 2).

## 7. Flag vs object classification

Security decisions are object-authoritative:

- The **opcode** `OPCODE_SHARED_MEM` routes to the shared-region handler but is **descriptive**: it
  is derived by the kernel on send (from `len > IPC_REGISTER_BYTES` + a MemoryObject/DmaRegion grant);
  a user cannot spoof it onto an inline message.
- The **mapping gate** keys on the resolved cap's rights (`CAP_RIGHT_MAP`, `CAP_RIGHT_WRITE`) and the
  descriptor length, never on a user flag.
- The ordinary-cap enqueue split excludes `OPCODE_SHARED_MEM` **and** peeks the envelope object; a
  Reply object is fail-closed at dequeue (198D-S). So no routing decision is security-authoritative
  on a user flag.

## 8. Consume vs retryable

The transfer envelope is **one-shot**: `take_transfer_envelope` consumes it (Created→Released, slot
`None`, unpins the object). A copy/mapping fault AFTER materialize rolls back
(`rollback_materialized_recv_cap` + `unmap_range_two_phase` + `remove_active_transfer_mapping`) and
the message is dropped — not retryable with the same envelope. This matches the accepted ordinary-cap
drop semantics.

## 9. Mapping/cleanup ↔ generation & receiver identity

- **Active mapping registry** (`ActiveTransferMapping { owner_tid, transfer_cap, base, len }`) is
  keyed by `(receiver owner_tid, receiver-local CapId)`.
- **Cleanup token** = the receiver-local `CapId.0`, which **encodes slot + generation** (bits 63:16).
  A revoked-then-reused slot yields a **different** CapId, so a stale token cannot match the stored
  mapping → stale releases fail. (`TransferRelease` looks up `active_transfer_mapping_for(owner, cap)`;
  a mismatch is `InvalidArgs`.)
- The helper-only `RecvV3CleanupRegistry` (recv_core.rs) proves the same token-lifecycle contract in
  isolation: `allocate → release=Released`, duplicate `release=AlreadyReleased`, post-reuse
  `release=StaleGeneration`, zero/oob `release=InvalidToken`. **No live syscall path uses it** — the
  live path uses `ActiveTransferMapping` + the generation-encoded CapId.

## 10. Process-exit / cancel / teardown reclamation

- **Transfer envelopes:** `purge_transfer_envelopes_for_pid(pid)` clears every envelope whose source
  OR receiver pid matches, and **unpins** the MemoryObject (`adjust_memory_object_pin_refcount(-1)`)
  for shared-region envelopes → no envelope or object-reference leak.
- **Receiver caps:** revoked with the receiver cnode teardown; the transfer materialize mint is a
  delegated cap reclaimed on receiver exit.
- **Active mappings:** `purge_active_transfer_mappings_for_pid(pid)` two-phase-unmaps the range,
  decrements map-refcount, revokes the transfer cap, and clears the registry slot → **no mapping
  survives receiver/process teardown**.
- **Cleanup-token registry:** the generation-encoded CapId key means numeric-TID or ASID reuse cannot
  revive a released mapping (a new mapping has a new CapId/generation).
- **Timeout/cancellation:** the send error path (`take_transfer_envelope` on the bound receiver) and
  `clear_ipc_waiters_for_tid` reclaim in-flight state; the envelope is consumed exactly once.

## Locking phase audit (current, pre-retirement)

| Phase | Work | Lock rank (global `&mut KernelState`) |
|---|---|---|
| Resolve | send-side cap resolve + rights validate | IPC/cap read, in-lock |
| Snapshot/stash | `stash_transfer_envelope` (+pin) | IPC rank 3, in-lock |
| Enqueue / direct deliver | endpoint queue or `complete_blocked_recv_for_waiter` | IPC rank 3/4, in-lock |
| Materialize | `grant_task_to_task_with_rights` (receiver-local mint) | cap rank 4, in-lock |
| **Map** | `map_user_page_in_asid*` + **TLB shootdown** | **VM rank 5 / memory rank 6, held UNDER the broad lock** |
| Register | `register_active_transfer_mapping` | IPC rank 3, in-lock |
| **Writeback** | **`copy_to_current_user` / `write_v3_output_to_user`** | **user-memory copy held UNDER the broad lock** |
| Cleanup | `TransferRelease`: two-phase unmap + revoke + remove | in-lock |

**Forbidden-condition findings (all present today, expected pre-retirement):** user-memory copy while
locks held; page-table modification + TLB shootdown while the broad IPC mutation is active. This is
the single-global-lock baseline. The desired retired model —
`resolve/snapshot → publish typed post-lock work → drop locks → materialize/map/copy → revalidate →
publish or rollback` — **does not yet exist for shared-region** (it exists for plain/ordinary/reply-
direct via the 187A/187B boundary snapshots). No second transfer mechanism should be created; 198E2
must reuse the existing envelope/mapping/registry types on a new post-lock boundary snapshot.

## Supported-class decision

| Class | Verdict | Rationale |
|---|---|---|
| **IpcSendSharedRegionDirect** | **NEEDS_BOUNDED_FIX** | Classification + lifecycle + rights/attenuation + cleanup are sound and object-authoritative, but the map + TLB shootdown + user-copy run under the broad lock. Retirement requires the post-lock map/copy boundary snapshot (rank 5/6 seam), which does not exist for shared-region. Not blocked by policy; not ready as-is. |
| **IpcSendSharedRegionEnqueue** | **NEEDS_BOUNDED_FIX** | Same object snapshot, same clean pin/purge lifecycle (envelope pins + `purge_*` unpins), but the same in-lock map/copy. The enqueue path additionally needs the boundary snapshot to defer the RecvSharedV3/recv-v2 mapping past the lock. |

Neither is `UNSUPPORTED_BY_POLICY` (shared regions are a supported capability model, unlike queued
reply caps) and neither is `READY_FOR_HOSTED_IMPLEMENTATION` (the map/copy split is missing).
Ordinary-cap transfer working is **not** sufficient — shared-region adds VM mapping + TLB shootdown +
cleanup-token obligations the ordinary class never had.

## Mapping & cleanup invariants (verified by the hosted seal)

no writable mapping without canonical WRITE authority; no executable mapping; no mapping survives
receiver/process teardown; no stale cleanup token releases a replacement mapping; no duplicate
release; no duplicate receiver cap; no transfer-envelope leak; no MemoryObject/reference leak; no
numeric-TID or ASID reuse revives stale state. Existing `MAP_WRITE` gates are left intact (the
lifecycle is not yet fully split-proven).

## Hosted proof

`stage198e1_shared_region_audit` (18 production-path cases, existing primitives only, no production
change) + `scripts/qemu-shared-region-hosted-seal.sh` emitting
`SECOND_COHORT_SHARED_REGION_HOSTED_SEAL direct=blocked enqueue=blocked cases=<n> leaked_caps=0
leaked_mappings=0 leaked_envelopes=0 stale_releases=0 result=ok` (audit seal, NOT a live retirement
seal). `direct/enqueue = blocked` encodes NEEDS_BOUNDED_FIX.

## Bounded Stage 198E2 implementation plan

1. **Post-lock shared-region boundary snapshot** — add a `RecvBoundarySharedRegionSnapshot` (mirroring
   `RecvBoundaryOrdinaryCapSnapshot`) capturing, under the lock: receiver cnode, resolved object +
   attenuated rights, source tid/cap (bookkeeping), descriptor (offset/len), receiver ASID, map
   intent. Consume the envelope ONCE under the lock.
2. **Post-lock materialize + map + copy** — after the broad borrow drops, mint the receiver-local cap
   via the existing seam, compute the mapping plan, map via the rank-5 VM seam, run the TLB shootdown
   off-lock, register the active mapping, then the rank-6 user-copy writeback — order
   `materialize → (wake) → map → writeback`, with revalidation of the object generation immediately
   before publish.
3. **Rollback reuse** — route map/copy/register failures through
   `unmap_range_two_phase` + `remove_active_transfer_mapping` + `rollback_materialized_recv_cap`
   (existing helpers); no new rollback code.
4. **Keep gates** — preserve the `MAP_WRITE`/`CAP_RIGHT_WRITE` gate and `execute:false`; keep the
   cleanup token = generation-encoded CapId.
5. **Classify direct vs enqueue separately** — implement + hosted-prove each independently; do not
   assume enqueue safe because direct is.
6. **Scope guard** — no new syscall/ABI/lock/capacity; `REPLY_CAP_QUEUEING_SUPPORTED = false` stays;
   no D2/IpcCall/Reply/IpcRecvTimeout/notifications/D3/D6 work; no second transfer mechanism.

## Hard-stops honored

Safe retirement here relies on none of: trusting user flags (opcode is descriptive; rights are
object-derived), leaving mappings alive after receiver exit (`purge_active_transfer_mappings_for_pid`),
re-resolving stale sender CSpace (identity is the frozen envelope object; cleanup keys on the
generation-encoded receiver CapId), or an ABI change. If 198E2 finds any unavoidable, it must stop.
