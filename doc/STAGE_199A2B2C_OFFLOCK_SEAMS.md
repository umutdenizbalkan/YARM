# Stage 199A2B2C — Off-Lock NR6 Reservation Seams (composition step)

Goal of the stage: complete the production NR6 `IpcCallDirectRequest` transaction.

Honest status: this increment lands the **off-lock (`_split`) reply-record
reservation seams** the composed transaction body calls — the exact rank-3 seams
that let the transaction reserve / bind / commit / cancel the single reply-record
slot **without taking the broad `&mut KernelState` lock** (Part 6 mandate). These are
proven at parity with the accepted KernelState primitives + the invocation gate. The
**composed transaction body** and the `STAGE_199_IPCCALL_DIRECT_REQUEST_TRANSACTION_SEAL
result=ok` are **not** emitted — that seal asserts an end-to-end off-lock delivery
(`no_ack_mutations`, `duplicate_deliveries=0`, `duplicate_wakes=0`,
`leaked_reply_records=0`, `record_available_before_enqueue=1`) that requires the full
transaction to exist. Emitting it now would be a false claim.

## Landed and tested

Four `SharedKernel` seams (`src/runtime.rs`), each operating the single `reply_caps`
slot via the rank-3 `with_ipc_split_mut` seam ONLY (no broad borrow, no
`with`/`with_cpu`):

* `reserve_direct_reply_record_split(caller, replier, reply_endpoint)` → installs a
  `Reserved` (not invokable) record binding both `{tid,asid}` identities + the reply
  endpoint index+generation; returns the slot `(index, generation)` (the sole
  authority) or `CapabilityFull`.
* `bind_direct_reply_record_server_cap_split(index, gen, cap)` → binds the provisional
  server-local reply CapId into a `Reserved` record.
* `commit_direct_reply_record_split(index, gen)` → `Reserved → Available`; **infallible
  for an exact live reservation** (flips one field), runs strictly before the rank-1
  enqueue so the server is never enqueued while `Reserved`.
* `cancel_direct_reply_record_split(index, gen)` → `Reserved → Cancelled → Vacant`
  (atomic rollback reclaim).

Six hosted tests (`stage199a2b2c_offlock_seams`) prove off-lock reserve→commit gates
invocation (Reserved not invokable, Available invokable, identities+endpoint bound),
cancel reclaims to Vacant, bind only into Reserved, exact-generation gating, the
source constraint (seams use `with_ipc_split_mut`, not the broad lock), single store,
and preserved invariants.

Preserved: `SYSCALL_COUNT=32`, `VARIANT_COUNT=22`, NR27 absent, DebugLog=192,
`REPLY_CAP_QUEUEING_SUPPORTED=false`, Stage 198F 10 classes / 30 cells. No live NR6
split arm; NR6/NR7 behavior byte-for-byte unchanged.

## Remaining for the transaction seal — precise seam-level plan

All seams below already exist; the remaining work is composing them (a `SharedKernel`
method) + the two publish points, plus the 24-test behavioral suite.

1. **x86 trap publish (Part 1).** In the pre-lock x86 seam: snapshot NR6 args →
   validate `len<=128` → capture caller `{tid,asid}` → `copy_from_user_asid_split_read`
   (off-lock) → build `IpcCallDirectSnapshot` → publish one post-work item, behind a
   default-off proof gate. Copy fault / oversize mutate nothing.
2. **Ack publish (Part 2).** At the recv-v2 commit point publish `BlockedServerAck`
   only after `Blocked(EndpointReceive)` + exact `ReceiverWaiterIdentity` installed +
   endpoint index/gen match + committed `BlockedRecvState` + RecvV2 + valid payload
   dest + non-null meta dest; re-read the waiter identity immediately before publish
   and require an exact match.
3. **Transaction body (Part 3/5, a `SharedKernel` method).** revalidate caller
   (`sr_prevalidate_blocked_receiver_split` / task split-read) → resolve SEND +
   reply-endpoint RECEIVE caps (cap split-read) → require the committed
   `BlockedServerAck` (else canonical `WouldBlock`, **before** any reserve/mint/copy/
   waiter mutation; never queued fallback) → `reserve_direct_reply_record_split` →
   `sr_mint_split` (one provisional server-local Reply cap) →
   `bind_direct_reply_record_server_cap_split` → `copy_slice_to_user_asid_split_write`
   (request payload + recv-v2 meta to server, off-lock) → final revalidation →
   `sr_claim_endpoint_waiter_split` (exact waiter) → `commit_direct_reply_record_split`
   (record `Available`, before enqueue) → `sr_commit_blocked_receiver_split` (clear
   blocked-return regs + Runnable) → `sr_enqueue_committed_receiver_split` (rank-1,
   last, non-fallible).
4. **Rollback (Part 6).** Any failure before enqueue: `sr_restore_endpoint_waiter_split`
   (if the exact server is still blocked) + `cancel_direct_reply_record_split` +
   `sr_revoke_split` (provisional server cap) + transfer-envelope reclaim + retain/
   restore the ack + free the post-work slot — covering source-copy fault, caller/
   server replacement, endpoint generation change, invalid reply endpoint, record
   capacity exhaustion, server CNode full, payload/meta copy fault (server stays
   blocked + retryable, no reply authority exposed, zero wake), changed/missing
   waiter, server exit, duplicate drain.
5. **24 hosted/source tests + `STAGE_199_IPCCALL_DIRECT_REQUEST_TRANSACTION_SEAL
   result=ok`**, emitted only when they genuinely pass.

## Stage 199A2B3 (NR7) preview

Reuses the already-landed `ReplyReservation` FSM (`Available→Reserved→Consumed`,
`ipccall_direct.rs`) and these same off-lock record seams: copy replier payload
off-lock → resolve reply index+gen → validate bound replier `{tid,asid}` → require
exact caller reply-endpoint waiter → reserve → copy reply to caller off-lock → final
revalidation → consume (`Available→Consumed` on the same slot) → revoke aliases →
claim/commit/enqueue caller last; caller-copy fault → release (`Reserved→Available`),
caller stays blocked, zero wake. No new authority store.
