<!-- SPDX-License-Identifier: Apache-2.0 -->
# Stage 198E3C1B — x86_64 Direct Shared-Region Userspace Contract (source-grounded)

The exact live data flow for `IpcSendSharedRegionDirect`, traced from the production sources (no
RecvSharedV3 draft — the live direct path uses the ordinary `IpcRecv` (recv-v2) syscall with a
map-intent, NOT `RecvSharedV3`).

## End-to-end flow

```
parent IpcSend(OPCODE_SHARED_MEM, FLAG_CAP_TRANSFER, transferred_cap=mem_cap)
  → ipc.rs boundary: ReceiverWaiterFound → is_task_recv_v2_blocked → try_ipc_send_boundary_split_any_pub
  → Ok(false) arm → produce_blocked_waiter_shared_region_delivery  (knob + drainer gated)
  → shared_region_phase_a(origin_direct=true): consume TransferEnvelope ONCE, move object pin into
    RecvBoundarySharedRegionSnapshot, capture receiver ASID + map_va (= blocked_state.payload_user_ptr)
    + meta_ptr (= blocked_state.meta_user_ptr)
  → stash ONE DispatchPostWork::BlockedWaiterSharedRegionDelivery (origin Direct)
  → trap-entry drain (broad borrow dropped): SharedRegionOffLockCtx + run_shared_region_txn
      mint receiver-local cap (rank 4) → map region at map_va into receiver ASID (rank 5→6, per page)
      → user metadata writeback (recv-v2 meta) OFF-lock → register ActiveTransferMapping
      → generation-bearing waiter CLAIM → clear blocked-return regs → clear waiter → set Runnable
      → enqueue once → publish
  → receiver resumes out of its recv-v2 syscall frame
```

## Contract table

| Item | Value / source |
| --- | --- |
| opcode | `OPCODE_SHARED_MEM` (`kernel/syscall.rs`), sender sets it in the `Message` header |
| required flags | `Message::FLAG_CAP_TRANSFER` + a transferred cap (`shared_region_live_eligible`, syscall.rs:903) |
| source endpoint cap | the send cap to the endpoint the child recv-v2-blocks on (parent-held) |
| source MemoryObject cap | init-local `READ\|MAP` cap (from `provision_init_shared_region_oracle`); preserved by the send |
| descriptor offset/length | envelope descriptor; the real IpcSend send path stashes `region=None`, so the delivery maps the whole object prefix at `map_va` for `payload_user_len` bytes (two pages) |
| receiver map VA | `BlockedRecvState.payload_user_ptr` — the recv-v2 **payload pointer** (arg `SYSCALL_ARG_PTR`, index 1). The region maps HERE (over the receiver's payload window). Must be a dedicated **unmapped, 2-page-aligned** window. |
| receiver payload ptr/len | `payload_user_ptr` / `payload_user_len` (recv-v2 args 1 / 2). len ≥ two pages. |
| recv-v2 metadata ptr/layout | `meta_user_ptr` / `meta_user_len` (recv-v2 args 3 / 4). Layout = `IpcRecvMetaV2` (`yarm_ipc_abi`): `status, opcode, flags, payload_len, cap_id, recv_meta_flags, sender_tid`. |
| **map intent (blocked-waiter path)** | NONE from userspace. The recv-v2 BLOCK path (`from_legacy_ipc_recv`, recv_core.rs:171) sets `map_intent = RecvMapIntent::None` and captures only `payload_ptr(arg1)/payload_len(arg2)/meta_ptr(arg3=INLINE_PAYLOAD0)/meta_len(arg4=INLINE_PAYLOAD1)`. The DIRECT delivery to a blocked waiter hardcodes **read-only** (`produce_blocked_waiter_shared_region_delivery` → `shared_region_phase_a(..., map_write=false, ...)`, syscall.rs:964). `recv_shared_mem_map_intent_flags` (ipc.rs:233, reads arg4) applies ONLY to the *immediate* inline-transfer delivery path (ipc.rs:1462), NOT the blocked-waiter oracle path. So the child needs no map-intent arg — just a recv-v2 whose `payload_ptr` is the dedicated 2-page window. |
| receiver-local transferred cap | `IpcRecvMetaV2.cap_id`, exposed when `recv_meta_flags & SYSCALL_RECV_META_TRANSFERRED_CAP`. Fresh receiver-local cap (differs from the sender's `mem_cap`). |
| **cleanup token** | the receiver-local transferred cap itself IS the cleanup authority — `handle_transfer_release` looks the active mapping up by `active_transfer_mapping_for(owner, transfer_cap)` (syscall/cap.rs:21). Nonzero cap ⇒ nonzero cleanup token. NO separate token field is invented. |
| release operation | `Syscall::TransferRelease` with `arg(CAP)=receiver_local_cap`, `arg(PTR)=0`, `arg(LEN)=0` (0/0 ⇒ the kernel resolves base+len from the active mapping). Unmaps the range (two-phase shootdown), revokes the cap, removes the ActiveTransferMapping, `note_shared_mem_released`. Returns `Ok(map_len)`. |
| first release | success — `frame.set_ok(map_len, 0, 0)`. |
| duplicate release | canonical rejection: the cap is revoked + the mapping removed, so `active_transfer_mapping_for` returns `None` ⇒ `SyscallError::InvalidArgs`. |

## Consequences for the userspace helpers

- `send_shared_region`: build a `Message` (opcode `OPCODE_SHARED_MEM`, `FLAG_CAP_TRANSFER`,
  transferred_cap = the init-local `mem_cap`, a small inline payload), then `ipc_send(send_ep, &msg)`.
  The source cap is preserved (the kernel duplicates through the envelope). Returns the exact
  `SyscallError`.
- `recv_shared_region_v2`: recv on the endpoint with `payload_ptr = ORACLE_MAP_VA` (dedicated
  unmapped 2-page window), `payload_len = 2*PAGE_SIZE`, a separate valid `IpcRecvMetaV2` buffer, and
  `map_intent = SYSCALL_RECV_MAP_INTENT_READ`. On return, decode `meta.cap_id` (receiver-local cap)
  when `recv_meta_flags & TRANSFERRED_CAP`; the two mapped pages are readable at `ORACLE_MAP_VA`.
- `release_shared_region_mapping`: `TransferRelease(cap, 0, 0)`; first → Ok(len), duplicate →
  `InvalidArgs`.

## Cleanup-authority verdict

The accepted kernel design DOES expose the cleanup authority through the live recv-v2 path: the
receiver-local transferred cap (`meta.cap_id`) is the cleanup token, released via `TransferRelease`.
No cleanup field is invented, and the RecvSharedV3 draft (whose `cleanup_token` is documented as
always 0) is NOT on this path. Contract satisfied — no hard-stop.
