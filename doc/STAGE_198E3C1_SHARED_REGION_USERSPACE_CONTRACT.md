<!-- SPDX-License-Identifier: Apache-2.0 -->
# Stage 198E3C2A — Direct Shared-Region Userspace Contract (source-grounded, live-proven)

The exact live data flow for `IpcSendSharedRegionDirect`, traced from the production sources and
confirmed by the sealed x86_64 QEMU boot (`SECOND_COHORT_SHARED_REGION_DIRECT_LIVE_SEAL … result=ok`).

**CORRECTION (Stage 198E3C2A):** the shared-region transfer is **NOT** selected by an inline
`OPCODE_SHARED_MEM` message. The first live boot proved that a small inline cap-transfer message is
decoded by the kernel as an *ordinary* inline cap transfer (its producer then page-faults writing the
payload into the unmapped oracle window). The kernel selects the shared-region path purely by the
**large-transfer form of `IpcSend`**: `arg(LEN) > Message::MAX_PAYLOAD`. The `OPCODE_SHARED_MEM`
value is *produced by the kernel* on that path (`handle_ipc_send`), not supplied by userspace.

## Authoritative large-length `IpcSend` ABI (production, from `handle_ipc_send`, `src/kernel/syscall/ipc.rs`)

| Item | Value / source |
| --- | --- |
| syscall number | `IpcSend` = `SYSCALL_IPC_SEND_NR` = **1** (`crates/yarm-user-rt/src/lib.rs`) |
| endpoint cap arg | `arg(SYSCALL_ARG_CAP)` = **arg 0** — the SEND cap to the endpoint the receiver recv-v2-blocks on (`ipc.rs:261`) |
| source offset arg | `arg(SYSCALL_ARG_PTR)` = **arg 1** = `user_ptr_or_offset` — the byte OFFSET into the source shared-region object (`ipc.rs:268`) |
| requested length arg | `arg(SYSCALL_ARG_LEN)` = **arg 2** = `len` — the region byte length (`ipc.rs:269`) |
| source shared-region cap arg | `arg(SYSCALL_ARG_TRANSFER_CAP)` = **arg 5** (`= TRAPFRAME_ARG_REGS − 1 = 6 − 1`, all 3 arches) — the init-local source cap (`transfer_cap_arg`, `ipc/ipc_abi.rs:45`) |
| args 3 / 4 | `SYSCALL_ARG_INLINE_PAYLOAD0` (unused here) / `SYSCALL_ARG_INLINE_PAYLOAD1` = send-timeout ticks (0 = non-blocking) |
| **path selector** | `len > Message::MAX_PAYLOAD` (**128**, `crates/yarm-kernel/src/ipc.rs:90`) selects the large shared-region transfer branch (`ipc.rs:317`). `len ≤ 128` takes the ELSE branch → an ordinary `OPCODE_INLINE` cap transfer that copies the inline payload to `arg(PTR)` (NOT shared region). This is why the region length MUST exceed 128. |
| source object forms | `CapObject::MemoryObject { .. }` or `CapObject::DmaRegion { .. }` only (`ipc.rs:323-326`); any other object → `WrongObject`. `validate_shared_mem_transfer_rights(&grant)` gates the required rights. |
| region validation | `validate_user_region(offset, len)` (`syscall/helpers.rs:42`): `offset < KERNEL_SPACE_BASE`, `offset + len` no overflow and `≤ KERNEL_SPACE_BASE`. |
| descriptor built by kernel | `SharedMemoryRegion { offset, len }` → `region.encode()` becomes the message payload |
| **descriptor layout** | `SharedMemoryRegion::ENCODED_LEN` = **16 bytes**, little-endian: `offset: u64` at bytes **0..8**, `len: u64` at bytes **8..16** (`crates/yarm-kernel/src/ipc.rs:45`). Decoded by `SharedMemoryRegion::decode` on the same field order. |
| envelope | `stash_transfer_handle(kernel, transfer_cap, endpoint, Some(TransferSharedRegion{offset,len}))` creates exactly **one** `TransferEnvelope`; the source cap is **delegated/duplicated, not moved** — it remains valid in the sender's CNode (`ipc.rs:333-341`). |
| kernel message | `Message::with_header(sender_tid, OPCODE_SHARED_MEM, FLAG_CAP_TRANSFER, transfer_handle, &region.encode())` (`ipc.rs:343-350`). Opcode + flags are set by the KERNEL, not userspace. |
| pre-ack behavior | when the receiver is not yet an authoritatively-committed recv-v2 waiter, the direct producer fails closed with the canonical retryable `SyscallError::WouldBlock` (no mutation; source cap + envelope preserved; parent retries). |

## End-to-end flow (production)

```
userspace: raw IpcSend(NR=1, [ep_cap, offset=0, len=REGION_BYTES>128, 0, 0, mem_cap])
  → handle_ipc_send: len > MAX_PAYLOAD → large-transfer branch
  → resolve arg5 = source cap → MemoryObject|DmaRegion + validate_shared_mem_transfer_rights
  → validate_user_region(offset, len)
  → SharedMemoryRegion{offset,len}; stash_transfer_handle(Some(TransferSharedRegion)) → ONE envelope
  → Message::with_header(_, OPCODE_SHARED_MEM, FLAG_CAP_TRANSFER, handle, region.encode()[16B])
  → boundary: ReceiverWaiterFound → is_task_recv_v2_blocked → try_ipc_send_boundary_split_any_pub
    (plain/reply/ordinary-cap all DECLINE OPCODE_SHARED_MEM) → Ok(false)
  → produce_blocked_waiter_shared_region_delivery (knob + drainer gated)
       Stage 198E3C1B-H AUTHORITATIVE ACK GATE: if no matching committed blocked-recv ack for this
       receiver+endpoint → return WouldBlock (fail closed, no mutation); else proceed and CONSUME the
       ack only AFTER the post-work is published (consume-after-publish, exactly-once).
  → shared_region_phase_a(origin_direct=true, map_write=false): consume envelope ONCE, move object
    pin into RecvBoundarySharedRegionSnapshot, capture receiver ASID + map_va (= receiver
    payload_user_ptr) + meta_ptr (= receiver meta_user_ptr)
  → stash ONE DispatchPostWork::BlockedWaiterSharedRegionDelivery (origin Direct)
  → trap-entry drain (broad borrow dropped): SharedRegionOffLockCtx + run_shared_region_txn
      mint receiver-local cap (rank 4) → map region READ-ONLY at map_va per page (rank 5→6)
      → recv-v2 meta writeback OFF-lock → register ActiveTransferMapping → generation-bearing waiter
      CLAIM → clear blocked-return regs → clear waiter → set Runnable → enqueue once → publish
  → receiver resumes out of its recv-v2 syscall frame
```

## Receiver (recv-v2) contract — unchanged from Stage 198E3C1B

| Item | Value / source |
| --- | --- |
| receiver map VA | `BlockedRecvState.payload_user_ptr` — the recv-v2 payload pointer (recv arg 1). The region maps HERE (over the receiver's payload window); it must be a dedicated **unmapped, page-aligned two-page** window (`SHARED_REGION_ORACLE_VA`). |
| receiver payload len | `payload_user_len` (recv arg 2) ≥ two pages. |
| recv-v2 metadata | `meta_user_ptr`/`meta_user_len` (recv args 3/4). Layout = `IpcRecvMetaV2`: `status, opcode, flags, payload_len, cap_id, recv_meta_flags, sender_tid`. On the direct delivery: `opcode = OPCODE_SHARED_MEM`, `payload_len = 16` (the encoded region descriptor length), `cap_id` = the fresh receiver-local cap, `recv_meta_flags & SYSCALL_RECV_META_TRANSFERRED_CAP`. |
| map intent | NONE from userspace on the blocked-waiter path — the direct delivery hardcodes read-only (`shared_region_phase_a(..., map_write=false, ...)`). `recv_shared_mem_map_intent_flags` (arg4 reader) applies ONLY to the immediate inline-transfer path. |
| delegated cap / cleanup token | `IpcRecvMetaV2.cap_id` (present with `TRANSFERRED_CAP`) is BOTH the fresh receiver-local cap AND the cleanup authority; nonzero, distinct from the sender's source cap. No separate token field. |
| release | `TransferRelease(cap, 0, 0)` (0/0 ⇒ kernel resolves base+len from the active mapping). First → `Ok(map_len)`; duplicate → `SyscallError::InvalidArgs` (cap + mapping already gone). |

## Userspace helper (canonical, architecture-neutral)

- `send_shared_region_large(send_ep, mem_cap, offset, region_len)`: the ABI-exact helper. Rejects
  `region_len ≤ IPC_MESSAGE_MAX_PAYLOAD (128)` and overflowing `offset+region_len` locally with
  `InvalidArgs`; otherwise issues `raw_syscall(SYSCALL_IPC_SEND_NR, [send_ep, offset, region_len, 0,
  0, mem_cap])`. It builds **no** inline `Message` and depends on **no** `OPCODE_SHARED_MEM` framing —
  the kernel produces the opcode + 16-byte descriptor. Returns the canonical `SyscallError`
  unchanged (`WouldBlock` on the pre-ack retry path). The source cap is preserved (delegated, not
  moved).
- `send_shared_region(send_ep, mem_cap)`: thin oracle convenience = `send_shared_region_large(send_ep,
  mem_cap, 0, SHARED_REGION_ORACLE_LEN)` (offset 0, the whole two-page region, `8192 > 128`).
- `recv_shared_region_v2` / `release_shared_region_mapping`: unchanged (see the receiver contract).

## Constant agreement (userspace ↔ kernel)

| Constant | userspace (`yarm-user-rt`) | kernel |
| --- | --- | --- |
| max inline payload / selector threshold | `IPC_MESSAGE_MAX_PAYLOAD = 128` | `Message::MAX_PAYLOAD = 128` |
| descriptor length | `SHARED_REGION_DESCRIPTOR_LEN = 16` | `SharedMemoryRegion::ENCODED_LEN = 16` |
| descriptor offset field | bytes `0..8` (`SHARED_REGION_DESCRIPTOR_OFFSET_AT = 0`) | `encode()` writes `offset` at `0..8` |
| descriptor length field | bytes `8..16` (`SHARED_REGION_DESCRIPTOR_LEN_AT = 8`) | `encode()` writes `len` at `8..16` |
| IpcSend NR | `SYSCALL_IPC_SEND_NR = 1` | `Syscall::IpcSend` = 1 |
