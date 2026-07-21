<!-- SPDX-License-Identifier: Apache-2.0 -->
# Stage 199A2D2C2C — x86_64 Cross-CPU NR7 Reply and Complete SMP Round-Trip

Goal: complete the reverse direction of the sealed cross-CPU exchange (CPU-0 client blocks on its
reply endpoint → CPU-1 server issues a genuine NR7 with the Reply cap it read in ring 3 → accepted
off-lock reply transaction targets CPU 0 → CPU-1→CPU-0 reschedule IPI → CPU-0 saved-frame resume →
CPU-0 ring-3 reply validation) and emit the complete request/reply SMP seal.

## Status — user-entry parity guard delivered (Part 11); reply round-trip NOT sealed

This increment delivers the verifiable, low-risk, permanent **user-entry EFER SCE|NXE parity guard**
(Part 11) and does NOT emit any cross-CPU reply seal. The full bidirectional NR7 round-trip — a large
integration on the scale of the B2 request direction — is scoped below and left for the next
increment; per the stage's own hard-stops, a `result=ok` SMP seal is only permitted after a genuine
clean two-direction boot, so none is emitted here.

## Delivered (verifiable)

### Permanent user-entry EFER parity guard — `descriptor_tables.rs`
`configure_syscall_msrs_for_self` — run by EVERY x86 CPU that can enter ring 3, before it loads user
mappings containing NX PTEs — now reads EFER back after writing `SCE | NXE` and REQUIRES both, failing
closed (`X86_EFER_USER_ENTRY_PARITY_FAIL` + `halt_forever`) otherwise. It emits a one-shot per-CPU
attestation `X86_EFER_USER_ENTRY_OK cpu=<n> sce=1 nxe=1 result=ok`. Live-proven on a fresh SMP=2 boot
for both the BSP (cpu=0) and the AP (cpu=1); the B3 user-consumption seal re-runs green. This makes the
Stage 199A2D2C2B3 defect (an AP reaching ring 3 without NXE, so NX data pages took reserved-bit #PFs)
un-reintroducible without a fail-closed halt, and does NOT mask faults by clearing NX from data pages.
BSP and AP now share the identical SCE/NXE requirement.

### Tests — `stage199a2d2c2c_efer_parity` (2)
The user-entry path requires both SCE and NXE and fails closed; the guard attests per-CPU and keeps NX
a hard requirement.

## Remaining reply round-trip (the SMP seal requires ALL of it)

The reply machinery from the SMP=1 NR7 oracle already exists and is reused unchanged:
`ipcreply_direct_ack` (BlockedCallerAck), `maybe_publish_ipcreply_direct_blocked_caller_ack`,
`try_split_ipcreply_direct_into_frame`, `DirectReplyPostWork`, and `ipc_reply_direct_txn` (the
reserve→copy→claim→commit→consume→enqueue ordering). The remaining cross-CPU integration, each piece
mirroring the sealed request direction:

1. **Arm the reply endpoint** — `set_ipccall_direct_oracle_endpoints(req_idx, reply_idx)` with the
   real reply endpoint index (today the reply slot is `usize::MAX`), so the caller-ack publishes.
2. **CPU-0 caller block** — extend the client stub: after NR6 success, issue recv-v2 on its reply
   endpoint (RECEIVE cap it already holds + a mapped reply payload/meta buffer). It blocks, committing
   a saved continuation and publishing the BlockedCallerAck; emit
   `IPCREPLY_DIRECT_SMP_CALLER_BLOCKED caller_cpu=0 …` from the committed block point (the reply-side
   analog of B1's `IPCCALL_DIRECT_SMP_SERVER_BLOCKED`).
3. **CPU-1 genuine NR7** — extend the server stub: after `X86_AP_RECV_V2_USER_VALIDATED`, issue NR7
   with the Reply CapId it read from its recv-v2 metadata in ring 3 (never a kernel-injected value),
   a reply buffer, and the exact reply length, with a bounded pre-ack WouldBlock retry (≤64).
4. **CPU-1→CPU-0 IPI** — on the accepted reply-txn success (client enqueued on CPU 0), CPU 1 sends the
   canonical 0xF1 IPI to CPU 0 (`X86_BSP_RESCHEDULE_IPI_SENT sender_cpu=1 receiver_cpu=0`); the CPU-0
   handler sets its own pending flag with no dispatch in the handler.
5. **CPU-0 saved resume** — the BSP restores the client's committed recv-v2 continuation. Unlike the
   AP, the BSP's normal timer-driven dispatch already resumes recv-v2 waiters via `user_context`
   (RIP/RSP/GPRs + cleared result regs), so this is largely the existing BSP recv-v2 wake, tagged with
   `X86_BSP_SAVED_DISPATCH_OK cpu=0 mode=saved`; validate no fresh re-entry.
6. **CPU-0 ring-3 reply validation** — the resumed client reads its reply buffer + metadata via direct
   ring-3 loads (now safe on both CPUs thanks to the NXE parity guard) and emits
   `X86_BSP_REPLY_USER_VALIDATED cpu=0 …`.
7. **Duplicate reply proof** — a second NR7 through the same userspace cap yields canonical
   WrongObject/StaleCapability with zero additional copies/claims/enqueues/IPIs/wakes (the Consumed
   record is the one-shot barrier).
8. **Markers + seals** — `IPCREPLY_DIRECT_SMP_REPLY_OK`, the reply user seal
   `STAGE_199_IPCREPLY_DIRECT_SMP_REPLY_USER_SEAL`, and the complete
   `STAGE_199_IPCCALL_REPLY_DIRECT_SMP_SEAL` (both directions, exact totals) — emitted ONLY after a
   clean genuine two-direction boot.

## Preserved
C2A / B1 / B2 / B3 seals re-run green with the parity guard on. SYSCALL_COUNT=32, VARIANT_COUNT=22,
NR27 absent, DebugLog=192, REPLY_CAP_QUEUEING_SUPPORTED=false, Stage 198F cells=30, Stage 199
functional cells=6, single-pair acknowledgement store, queued IpcCall unsupported, timeouts /
notifications / server-death caller-wake unretired, multi-pair concurrency unclaimed. NR7 cross-CPU
delivery not begun; no reply seal emitted.
