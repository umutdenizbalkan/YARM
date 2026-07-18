<!-- SPDX-License-Identifier: Apache-2.0 -->
# Stage 199A1 — Direct `IpcCall` and Reply-Lifecycle Hosted Audit

Source-grounded audit of the DIRECT synchronous `IpcCall` path (server already blocked in
receive; client sends a request bearing a one-shot reply authority; server replies; the exact
caller resumes once; the reply authority becomes stale). Hosted-only; no QEMU; no production
behavior changed. Does not modify any accepted Stage 198F class.

Scope excluded this increment: queued/no-waiter `IpcCall`, call/receive/reply timeouts,
notifications, shared-region call payloads, reply-cap enqueue, D3, D6.

## 1. Production `IpcCall` contract (source-grounded)

| item | production fact | source |
| --- | --- | --- |
| syscall number | `IpcCall = SYSCALL_IPC_CALL_NR = 6` | `src/kernel/syscall.rs:20,289` |
| endpoint arg | `arg0 = SYSCALL_ARG_CAP` — SEND cap to the server endpoint | `ipc.rs:923-929` |
| request payload | `arg1 = user_ptr/offset`, `arg2 = len` (≤ `Message::MAX_PAYLOAD = 128`) | `ipc.rs:978-982` |
| reply-recv cap | `arg5 = SYSCALL_ARG_TRANSFER_CAP` = caller's private reply-endpoint RECV cap (must hold `RECEIVE`) | `ipc.rs:931-932` |
| request representation | `Message::with_header(sender_tid, OPCODE_INLINE, FLAG_REPLY_CAP, transfer_handle=reply_cap, payload)` — opcode/flags set by the KERNEL | `ipc.rs:997-1017` |
| caller blocking state | `IpcCall` is **request-send only**: it returns `Ok` immediately (`frame.set_ok(0,0,0)`); the caller then blocks by an explicit `recv` on its reply endpoint → `Blocked(EndpointReceive)`. The block is the caller's recv, keyed by the endpoint waiter identity. | `ipc.rs:1123-1152` |
| caller identity authority | the reply endpoint waiter is `ReceiverWaiterIdentity { tid, asid }` (generation-bearing, Stage 198E); a reused numeric TID always carries a different ASID | `defs.rs:262`, `ipc_state.rs:49,314` |
| reply-record allocation + generation | `create_reply_cap_for_caller(ThreadId(caller_tid), reply_recv_cap, responder_tid)` reserves `reply_caps[slot]`, bumps `reply_cap_generations[slot]`, mints a `CapObject::Reply { index=slot, generation }` cap into the caller's cnode | `ipc.rs:952-966`, `defs.rs:273-274` |
| reply-cap object | `CapObject::Reply { index, generation }` — the sole one-shot invocation authority; generation-guarded | `defs.rs:224-242` |
| server receiver-local reply cap | `waiter_cap_id` — minted by the Phase-C executor (`produce_blocked_waiter_reply_cap_delivery`) into the **server's** cnode when the `FLAG_REPLY_CAP` request is delivered | `defs.rs:233-241`, `ipc.rs:1052-1070` |
| reply payload/metadata | reply carries `OPCODE_INLINE` + `FLAG_CAP_TRANSFER_PLAIN` (verbatim payload, no 2-byte opcode strip) when forwarding a cap, else a plain `Message` | `ipc.rs:1259-1271` |
| reply-record consume/revoke | `ipc_reply`: `resolve_reply_index` (generation-checked) → atomic one-shot claim `reply_caps[slot]=None` → `fast_revoke_reply_cap_in_cnode` for **both** replier + caller slots (no heap alloc, no delegation walk) | `ipc_state.rs:2309-2423` |
| caller wake + return regs | the reply is delivered to the reply-endpoint waiter `{tid, asid}`; the copy + slot-clear + wake run in Phase C (executor) after the broad borrow drops, or via the legacy `complete_blocked_recv_for_waiter` (copy outside locks) → `apply_scheduler_wake_plan` (wake outside locks) | `ipc_state.rs:2440-2523` |
| duplicate-reply result | a second invocation finds `reply_caps[slot]=None` → `StaleCapability`; the cnode slot was fast-revoked → the reply cap resolves to `InvalidCapability`; **wakes nobody** | `ipc_state.rs:2321-2325`, tests `reply_cap_record_is_single_use…` |
| caller exit ownership | `revoke_reply_caps_for_caller(caller_tid)` clears records by `caller_tid` at caller exit/death (rank-3 `ipc_state_lock`, no wake) | `ipc_state.rs:251-262` |
| server exit ownership | `revoke_reply_caps_for_replier(tid)` clears records by `responder_tid` at replier exit/death (rank-3, no wake) | `ipc_state.rs:283-298` |

## 2. Generation-bearing identities

- **Caller** (reply endpoint waiter): `ReceiverWaiterIdentity { tid, asid }` — the wake targets this
  exact incarnation; a reused TID with a new ASID never matches (`clear_endpoint_waiters_for_identity`
  uses the full identity).
- **Server** (request endpoint waiter): same `ReceiverWaiterIdentity { tid, asid }` — the direct
  request requires the committed generation-bearing waiter.
- **Reply record**: `CapObject::Reply { index, generation }` + `reply_cap_generations[index]`. The
  record itself stores `caller_tid` (numeric) + `reply_endpoint` + `responder_tid` + cnode CapIds.

**Numeric-TID-only authority = NONE.** Reply delivery/wake/consume is gated by (1) the reply-cap
object generation (`resolve_reply_index` → `StaleCapability` on an empty/mismatched slot) and (2) the
reply-endpoint waiter `{tid, asid}` identity. The record's numeric `caller_tid` is used only for cnode
cleanup, itself guarded by object + generation match and task presence. Numeric TID alone authorizes
neither delivery, wake, cancellation, cleanup, nor stale-record reuse.

## 3. Reply-record lifecycle

```
IpcCall → create_reply_cap_for_caller: reserve reply_caps[slot], bump generation,
          mint CapObject::Reply{index,generation} into caller cnode (caller_cap_id)
      → request delivered to the blocked server (Phase C): mint receiver-local reply cap
          (waiter_cap_id) into server cnode, copy request, clear waiter, wake server once
Reply → resolve_reply_index (generation-checked) → validate responder_tid==replier
      → ATOMIC one-shot claim reply_caps[slot]=None  (before any copy/wake)
      → fast-revoke replier + caller cnode slots
      → deliver to reply-endpoint waiter {tid,asid}: copy → clear waiter → wake (enqueue last)
Duplicate Reply → slot None → StaleCapability / cap revoked → InvalidCapability → wakes nobody
Caller exit  → revoke_reply_caps_for_caller  (invalidate by caller_tid)
Server exit  → revoke_reply_caps_for_replier (invalidate by responder_tid)
```

## 4. Lock classification

Neither `IpcCall` (NR 6) nor `IpcReply` (NR 7) is in the pre-lock split-dispatch bridge
(`syscall_split.rs`); both handlers run under the broad `&mut KernelState`. The **target-side**
delivery (copy to server/caller + mint + wake) is deferred off the broad borrow to the Phase-C
dispatch-return executor (`produce_blocked_waiter_reply_cap_delivery` / `try_ipc_reply_boundary_split`,
Stage 188E/188F) — the same accepted off-lock mechanism as the retired Stage 198F `IpcSendReplyCap`
direct class. The reply-record reservation/consume and cnode revokes are narrow ranked mutations
(`with_ipc_state_mut`, rank 3 / cnode rank 4). The scheduler wake is `apply_scheduler_wake_plan` /
`apply_split_receiver_wake_plan` **after** every lock is released (enqueue last).

| path | off-lock (Phase C) | under broad lock | classification |
| --- | --- | --- | --- |
| **request direct** (`IpcCall`→blocked server) | server mint + request copy + waiter-clear + server wake | **caller's request-payload read** (`copy_from_current_user`, `ipc.rs:986`); reply-record reserve; caller-cap mint | **NEEDS_BOUNDED_FIX** |
| **reply direct** (server `Reply`→caller) | caller copy + waiter-clear + caller wake | **replier's reply-payload read** (`copy_from_current_user`, `ipc.rs:1204`); one-shot claim; cnode revokes | **NEEDS_BOUNDED_FIX** |

Both target-side deliveries already satisfy the off-lock/ordering requirements (claim → fallible
copy/revalidate → clear blocked state → make Runnable → scheduler enqueue last; no fallible op after
enqueue). The single blocking gap for a LIVE retirement is that the **sender/replier payload read**
runs under the broad lock because NR 6 / NR 7 are not split-dispatched.

## 5. Cancellation and teardown ownership

| case | single cleanup owner | reply authority |
| --- | --- | --- |
| caller exits before reply | caller exit/death handler → `revoke_reply_caps_for_caller` | invalidated (slot cleared; gen bumped on reuse) |
| server exits holding reply cap | replier exit/death handler → `revoke_reply_caps_for_replier` | invalidated proactively |
| endpoint destroyed | `destroy_endpoint` bumps the endpoint generation | outstanding waiter identities/reply endpoints stale |
| reply-record generation reused | `create_reply_cap_for_caller` bumps `reply_cap_generations[slot]` | old `CapObject::Reply` fails `resolve_reply_index` |
| stale caller TID reused with new ASID | endpoint waiter `{tid, asid}` never matches the old identity | no wake of the replacement incarnation |
| transaction cancelled before publication | the request-delivery error arm consumes the stashed envelope (`take_transfer_envelope`) | reply record left reserved → freed at caller/replier teardown |
| copy fault after one-shot claim | `ipc_reply` legacy arm returns `UserMemoryFault`; the record is already consumed (irrevocably committed) — the caller is not double-woken | record consumed; envelope cleanup owner = the reply error arm |

**No hard-stop:** every consumed/reserved reply record has a single cleanup owner and no path leaves a
leaked record. NOTE (out of scope, Stage 199A3+): server-exit invalidates the record but does not by
itself WAKE a caller already blocked awaiting the reply — caller liveness on server-death depends on
the call-timeout / death-notification mechanism (timeouts + notifications are excluded here). This is a
liveness bounded-fix for a later stage, not a leaked-record defect.

## 6. Bounded fixes required before a Stage 199A2 live retirement

1. **Split-dispatch `IpcCall` (NR 6) and `IpcReply` (NR 7)** off the trap entry (like `IpcSend` NR 1)
   so the sender/replier payload read happens off the broad lock. The target-side delivery is already
   off-lock; only the payload read must move.
2. **Bind the caller incarnation directly in `ReplyCapRecord`** — add `caller_asid` / task-generation
   so the record carries the full `{tid, asid, index, generation}` authority and the caller-incarnation
   check at reply time is explicit in the record, not only indirect via the reply-endpoint waiter.
3. (Later, Stage 199A3+) **caller liveness on server-death** via the call-timeout / death-notification
   path (out of this increment's scope).

## 7. Stage 198F preservation

No Stage 198F kernel marker or policy is changed. Supported classes remain 10, live cells remain 30,
`IpcSendReplyCapEnqueue` and `IpcSendSharedRegionEnqueue` remain unsupported. `SYSCALL_COUNT=32`,
`VARIANT_COUNT=22`, `NR27` absent, `DebugLog=192`, `REPLY_CAP_QUEUEING_SUPPORTED=false` unchanged.

## 8. Hosted audit seal

```
STAGE_199_IPCCALL_DIRECT_HOSTED_AUDIT request_direct=needs_bounded_fix reply_direct=needs_bounded_fix numeric_tid_only_authority=0 duplicate_replies=0 duplicate_wakes=0 leaked_reply_records=0 result=ok
```

Hosted-only; NOT a retirement marker.

## 9. Precise Stage 199A2 plan (x86_64 live oracle)

Implement bounded fix (1) for x86_64 first: split-dispatch `IpcCall` (NR 6) so the request-payload
read is off the broad lock, keeping the existing Phase-C server delivery; and split-dispatch
`IpcReply` (NR 7) so the reply-payload read is off the broad lock, keeping the existing
`try_ipc_reply_boundary_split` caller delivery. Add bounded fix (2) (record `{tid, asid}`). Then a
default-off `yarm.x86_64_ipccall_direct_oracle=1` two-task oracle (client `IpcCall` → server recv →
`Reply` → client resumes once; duplicate reply rejected; no wake of a replacement incarnation) earning
`IPCCALL_DIRECT_*` attestations + a `GLOBAL_LOCK_RETIRE_CLASS_*` `class=IpcCallDirect` retirement,
under the same authoritative-ack + fail-closed provenance model, before the AArch64/RISC-V ports.
