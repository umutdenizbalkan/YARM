<!-- SPDX-License-Identifier: Apache-2.0 -->
# Stage 199A2D2C2 — Live CPU-1 recv-v2 Blocked Continuation and Cross-CPU NR6 Request Seal

Goal: run a real recv-v2 server on CPU 1, save its blocked userspace continuation, wake it through
the accepted CPU-0 NR6 transaction, restore that saved continuation on CPU 1, and earn
`STAGE_199_IPCCALL_DIRECT_SMP_REQUEST_SEAL ... cross_cpu=1 result=ok`.

## Outcome — substrate + invariants delivered; LIVE continuation scoped, seal HELD

The saved-continuation model, the wake-finalization transition, and the canonical
RFLAGS/CS/SS invariants are implemented and proven by hosted tests. The **LIVE recv-v2 blocked
continuation is not yet wired**, so the request smoke honestly holds at `cross_cpu=0
result=blocked` (never a false `result=ok`). The Stage 199A2D2C1 generic fresh-entry seal
(`STAGE_199_X86_AP_GENERIC_RETURN_SEAL … result=ok`) remains genuine and valid.

Feasibility confirmed (P0): the AP syscall path already routes through the SAME
`dispatch_trap_entry_with_shared_kernel` → `handle_trap` as the BSP, so a recv-v2 on CPU 1 blocks
the task and saves its continuation (`user_context`) via the normal block path; the AP trap-EXIT
already context-restores for the `task_switched` case (`write_task_gprs_to_saved_regs` + iretq). The
remaining LIVE work is bounded and precise (below).

## Delivered (verifiable)

### Saved-continuation model + wake finalization — `src/arch/x86_64/ap_sched.rs`
- `SavedUserReturnFrame` (from C1) reuses the canonical BSP `TrapFrame` (saved_pc/saved_sp/user_gprs
  + ret0/1/2/error) + canonical user CS=0x23 / SS=0x1b / RFLAGS=0x202 — no second AP-only ABI.
- `finalize_wake_to_runnable_saved` — the `BlockedUnfinalized → RunnableSaved` transition: it
  completes the blocked recv-v2 syscall's result registers (`ret0/1/2`, `err`) INTO the saved frame
  and commits it, returning `RunnableSaved` ONLY when the result is completed AND the frame carries a
  valid RIP/RSP; otherwise `NotFinalized` (never dispatchable). The CPU-1 dispatcher builds a
  `SavedUserFrame` plan ONLY from a `RunnableSaved` state (`select_return_source`).

### Canonical RFLAGS/CS/SS invariants (the spec's explicit concern)
`stage199a2d2c2_guards`: the fixed RFLAGS=0x202 is NOT a silent substitution — it mirrors the
canonical x86 return policy `flush_trap_context_to_iret_frame` (source-guarded: `frame.rflags =
0x202;`). CS=0x23 / SS=0x1b are the single user code/data selector pair YARM guarantees
(source-guarded against `USER_CODE_SELECTOR` + the iret-frame asm), so synthesizing them in the saved
frame is invariant-safe.

### Tests (`ap_sched::tests` 15 + `stage199a2d2c2_guards` 5, plus the C1/C2 shared substrate)
saved RIP/RSP/RFLAGS/CS/SS + all GPRs preserved (not zeroed); syscall-result state preserved;
BlockedUnfinalized not dispatchable; wake finalization completes the result THEN publishes
RunnableSaved; RIP/RSP/GPRs survive the block/wake cycle; wrong `{tid,asid}` / wrong-home-CPU
rejected; reschedule flag set/coalesce/consume + per-CPU isolation + no-lost-wake; the generic
fresh-entry seal remains valid; the `cross_cpu=0 result=blocked` diagnostic cannot satisfy success;
Stage 199 functional cells stay 6.

## Remaining LIVE work (the request smoke's ok gate now requires ALL of it)

The smoke seals `cross_cpu=1 result=ok` ONLY when a clean boot shows the full ordered sequence
(`X86_AP_ONLINE cpu=1` → `X86_AP_GENERIC_DISPATCH_OK … mode=fresh` → `IPCCALL_DIRECT_SMP_SERVER_BLOCKED
server_cpu=1` → `X86_AP_RECV_V2_CONTINUED cpu=1 … continuations=1` → `IPCCALL_DIRECT_SMP_REQUEST_OK
sender_cpu=0 receiver_cpu=1 cross_cpu=1`). Until then it holds at `result=blocked
reason=ap_saved_frame_restore_and_recv_v2_server_not_wired`. The bounded remaining pieces:

1. **Real recv-v2 server on CPU 1** — extend the C1 proof-task provisioning to a real server:
   a request endpoint whose RECEIVE cap is in the AP server's CNode (shared with a BSP client's SEND
   cap), payload/meta destination pages in its ASID, and one reply-cap CNode slot. Its ring-3 stub
   issues a real recv-v2 (which blocks). Reuses C1's scheduler-selected fresh-entry to reach ring 3.
2. **Authoritative block marker** — after the AP recv-v2 commit, validate (server_cpu=1, exact
   waiter identity, BlockedRecvState + BlockedServerAck committed, absent from runnable queues) and
   emit `IPCCALL_DIRECT_SMP_SERVER_BLOCKED … saved_frame=1 result=ok`.
3. **CPU-0 NR6 + RunnableSaved completion** — the accepted `ipc_call_direct_request_txn` (no fork)
   delivers the request, then `finalize_wake_to_runnable_saved` completes the recv-v2 result in the
   saved frame and publishes `RunnableSaved`, enqueues the server on CPU 1 LAST, and sets the
   reschedule-pending flag + sends the wake IPI.
4. **AP saved-frame context-restore entry** — the ONE genuinely new asm: an idle-loop-driven
   dispatch that, for a `RunnableSaved` selection, installs per-CPU CR3/TSS-RSP0/GS/FS and restores
   the FULL saved user frame (16 GPRs + saved RIP/RSP/RFLAGS/CS/SS) through the canonical
   trap-return trampoline — NOT `enter_user_mode_iret` with a fresh frame, and NOT a restart at the
   entry point. The AP idle loop consumes the reschedule-pending flag and selects from CPU 1's real
   run queue after the wake.
5. **Userspace continuation proof** — the resumed server validates the request bytes + reply cap +
   `continuations==1` + `cpu==1` and emits `X86_AP_RECV_V2_CONTINUED cpu=1 … result=ok`; the kernel
   emits `IPCCALL_DIRECT_SMP_REQUEST_OK … cross_cpu=1`.

## Preserved
SYSCALL_COUNT=32, VARIANT_COUNT=22, NR27 absent, DebugLog=192, REPLY_CAP_QUEUEING_SUPPORTED=false,
Stage 198F live cells=30, Stage 199 functional live cells=6, queued IpcCall unsupported. Not begun:
cross-CPU NR7, timeouts, notifications, server-death caller wake, AArch64/RISC-V SMP, D3.

## Cross-CPU NR7 plan (after the request continuation lands)
NR7 mirrors this on the reply endpoint: the BSP client blocks in recv-v2 on its reply endpoint
(already CPU 0) with a saved continuation; the AP server replies (NR7) delivering cross-CPU; the
client is remotely woken + resumed on CPU 0 via the SAME saved-frame context-restore (step 4) applied
to the BSP-bound caller. The reschedule flag, idle dispatcher, `finalize_wake_to_runnable_saved`, and
the context-restore entry are all reused symmetrically.
