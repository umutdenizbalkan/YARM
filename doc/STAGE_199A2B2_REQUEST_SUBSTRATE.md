# Stage 199A2B2 — x86_64 Off-Lock IpcCall Direct Request: Reply-Authority Substrate

Goal of the stage: move the successful direct NR6 request path off the broad
runtime lock on x86_64 (production request-side transaction; no live class, no
QEMU).

Honest status: this increment lands the **reply-authority representation (Part 4)**
and the **committed blocked-server acknowledgement type (Part 2)** — the correct,
hosted-verifiable substrate the request transaction composes — plus the record-side
reservation primitives. The **composed off-lock request transaction** (require-ack →
mint → off-lock delivery → claim/commit/enqueue → commit, with full rollback) and
the **x86 trap-entry snapshot publication** are the remaining work. The stage's
`STAGE_199_IPCCALL_DIRECT_REQUEST_OFFLOCK_SEAL … result=ok` is therefore **not
emitted**: it asserts an end-to-end off-lock request delivery (`duplicate_deliveries=0`,
`duplicate_wakes=0`, `leaked_reply_records=0`) that requires the transaction to
exist. Emitting it now would be a false claim.

---

## Landed and tested

### Part 4 — reply-authority representation (single store, reservation in the slot)

`ReplyRecordReservation { Available, Reserved, Consumed, Cancelled }` is a **field of
`ReplyCapRecord`** (`src/kernel/boot/defs.rs`), so the reservation lifecycle lives IN
the single existing `reply_caps` slot. There is exactly one persistent reply
authority store and one authoritative generation (`reply_cap_generations`) — no
second table, no second generation.

* `resolve_reply_index` now gates on `reservation.is_invokable()`: only an
  `Available` record resolves for `ipc_reply`. A `Reserved` (in-flight direct
  transaction) record is `StaleCapability` — a reply can never be delivered against a
  record whose server delivery + reply-cap materialization have not committed.
* Record-side reservation primitives (`src/kernel/boot/ipc_state.rs`, each a single
  rank-3 ipc critical section the live transaction wraps via `with_ipc_split_mut`):
  * `reserve_direct_reply_record(caller, replier, reply_endpoint)` → installs a
    `Reserved` record binding both `{tid,asid}` identities + the reply endpoint
    (index+generation in `CapObject::Endpoint`), returns the slot `(index, generation)`.
  * `commit_direct_reply_record(index, gen)` → `Reserved → Available` (the last
    record mutation, only after consistent delivery).
  * `cancel_direct_reply_record(index, gen)` → `Reserved → Cancelled → Vacant`
    (rollback; atomic slot reclaim).
  * `bind_direct_reply_record_server_cap(index, gen, cap)` → records the server-local
    reply CapId into a `Reserved` record.
  * All require the exact generation; a stale generation mutates nothing.

Required record identity is complete: record index+generation, caller `{tid,asid}`,
replier `{tid,asid}`, reply endpoint index+generation.

### Part 2 — committed blocked-server acknowledgement type

`BlockedServerAck` (`src/kernel/ipccall_direct.rs`) — bounded, owned, by-value:
server `ReceiverWaiterIdentity {tid,asid}`, endpoint index+generation, RecvV2
committed flag, and payload/metadata destinations. `is_committed()` rejects a
non-committed / destination-less ack (→ treated as "no acknowledgement" → canonical
`WouldBlock`, never queued fallback); `waiter_claim_key()` yields the exact
`(index, generation, identity)` for `sr_claim_endpoint_waiter_split`.

### Tests

`kernel::ipccall_direct::tests` (9) + `stage199a2b2_request_substrate` (8) prove:
reserved-not-invokable-until-committed, cancel-reclaims-to-vacant, identities +
endpoint bound, exact-generation gating, server-cap binds only into Reserved,
single-authority-store (no second table), and the ack fields/predicate. All existing
reply-cap behavior is preserved (legacy records default `Available`).

### Preserved

`SYSCALL_COUNT=32`, `VARIANT_COUNT=22`, NR27 absent, DebugLog=192,
`REPLY_CAP_QUEUEING_SUPPORTED=false`, Stage 198F 10 classes / 30 cells. No live NR6
direct split arm; NR6/NR7 behavior byte-for-byte unchanged; direct class default-off.

---

## Remaining for the `result=ok` request seal (next portion of 199A2B2 / into 199A2B3)

Compose the off-lock transaction (all via `with_*_split_mut` / the Stage 198E
primitives — no `&mut KernelState`, no `with`/`with_cpu`, no user copy under a lock,
enqueue last, no fallible work after enqueue), reusing the substrate above:

1. **x86 trap publish (Part 1).** Snapshot NR6 args → validate `len<=128` → capture
   caller `{tid,asid}` → copy request payload off-lock (`copy_from_user_asid_split_read`)
   → publish one `IpcCallDirectSnapshot` split-work item, behind a default-off proof
   gate. Source-copy failure mutates nothing.
2. **Require the exact ack (Part 2/3).** No committed `BlockedServerAck` → canonical
   `WouldBlock` **before** `reserve_direct_reply_record` / mint / delivery / waiter
   mutation; never queued fallback. Consume the ack only after the split-work item is
   installed; restore it on every pre-publication failure.
3. **Transaction (Part 3).** revalidate caller → resolve SEND + reply-endpoint RECEIVE
   caps → require the exact committed blocked server → `reserve_direct_reply_record`
   → mint exactly one server-local Reply cap (`phase_b_mint_reply_cap` /
   `sr_mint_split`) → `bind_direct_reply_record_server_cap` → copy request payload/meta
   to the server outside locks (186E seam) → final revalidation →
   `sr_claim_endpoint_waiter_split` (exact waiter) → `sr_commit_blocked_receiver_split`
   (clear blocked-return regs + Runnable) → `sr_enqueue_committed_receiver_split`
   (rank-1, last) → `commit_direct_reply_record`.
4. **Rollback (Part 5).** Any failure before enqueue restores the blocked server
   (`sr_restore_endpoint_waiter_split`) and reclaims the reserved record
   (`cancel_direct_reply_record`), the server-local reply cap, the transfer envelope,
   and the ack when retry remains valid — covering caller replacement, endpoint
   generation change, invalid reply endpoint, record capacity exhaustion, server CNode
   full, server-copy fault (server stays blocked + retryable, no reply authority
   exposed), changed/missing waiter, and task exit.
5. **22 hosted tests + `STAGE_199_IPCCALL_DIRECT_REQUEST_OFFLOCK_SEAL … result=ok`**,
   emitted only when the transaction genuinely passes them.

### Stage 199A2B3 (NR7 off-lock reply) preview

The reply side reuses `ReplyReservation` (already landed in `ipccall_direct.rs`):
copy replier payload off-lock → resolve reply object index+generation → validate
bound replier `{tid,asid}` → require exact caller reply-endpoint waiter → `reserve`
→ copy reply to caller off-lock → final revalidation → `consume` (`Available →
Consumed` via `commit`/state on the same slot) → revoke aliases → claim/commit/enqueue
caller last. Caller-copy fault: `release` (`Reserved → Available`), caller stays
blocked, zero wake. The `Consumed` reservation state added this increment is the NR7
terminal state; no new authority store is introduced.
