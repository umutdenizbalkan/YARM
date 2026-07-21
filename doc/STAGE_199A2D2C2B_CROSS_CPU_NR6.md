<!-- SPDX-License-Identifier: Apache-2.0 -->
# Stage 199A2D2C2B — Live Cross-CPU NR6 recv-v2 Continuation

Goal: a real recv-v2 server on CPU 1 blocks with a committed saved frame; a real CPU-0 client
delivers a request through the accepted NR6 off-lock transaction; CPU 1 is remotely woken and the
server is resumed after recv-v2 via `yarm_x86_resume_ring3`, earning
`STAGE_199_IPCCALL_DIRECT_SMP_REQUEST_SEAL … cross_cpu=1 result=ok`.

## Outcome — proof-shortcuts removed + ordering guards delivered; LIVE orchestration scoped, seal HELD

The two hardest AP *mechanism* pieces are sealed and genuine this session: **C1** (scheduler-selected
fresh entry, `STAGE_199_X86_AP_GENERIC_RETURN_SEAL`) and **C2A** (saved-frame context-restore resume,
`STAGE_199_X86_AP_SAVED_RETURN_SEAL`). C2B integrates them with real IPC — a real recv-v2 server + a
real CPU-0 client + a shared endpoint + cross-CPU delivery. That end-to-end orchestration is a large
multi-cycle effort; this increment delivers the verifiable Part-1 proof-shortcut removals + the
wake-finalization ordering, and HOLDS the request smoke at `cross_cpu=0 result=blocked` — never a
false `cross_cpu=1 result=ok`, and without destabilizing the C1/C2A seals.

## Delivered (verifiable)

### Part 1 — proof-shortcut removal
- **FS from task state** (`exec_state.rs`, `smp.rs`): `ap_saved_resume_context` now returns the
  selected task's saved FS base (`tcb.tls_ptr`), and the resume installs it via `wrmsr IA32_FS_BASE`
  — never a hardcoded constant (a task with a real TLS resumes with its own FS.base). Verified: the
  C2A saved-return seal still passes (the Yield-only proof task has no TLS → FS = 0).
- **Pending-flag origin** (`smp.rs`, `mod.rs`): a new `x86_ipccall_direct_smp_request_active()` flag
  (default-off) gates the resume's self-arm of the reschedule-pending flag. In the C2A Yield-only
  proof (flag false) the resume self-arms to exercise the consume path; on the C2B cross-CPU REQUEST
  success path (flag set) the flag ORIGINATES from the real CPU-0 remote-wake interrupt and the
  self-arm is skipped.
- **`AP_SAVED_RESUME_DONE`** stays a one-shot oracle latch/fuse gated behind the SMP oracle — never
  generic scheduler authority; the generic dispatcher runs later saved tasks without touching it.

### Wake-finalization ordering (Part 6, reused from C2)
`finalize_wake_to_runnable_saved` completes the recv-v2 result registers (`ret0/1/2`, `err`) INTO the
saved frame and commits it, returning `RunnableSaved` ONLY when the result is complete AND the frame
is valid — else `NotFinalized`. `select_return_source` builds a `SavedUserFrame` plan ONLY from
`RunnableSaved`; a `BlockedUnfinalized` task is never selectable.

### Guards (`stage199a2d2c2b_guards`, 5)
FS sourced from task state; the resume does not self-set the pending flag on the request path;
`AP_SAVED_RESUME_DONE` is an oracle fuse only; wake finalization completes the result before
RunnableSaved (BlockedUnfinalized never selectable); the request seal requires the full live recv-v2
sequence and the C1/C2A seals remain intact.

## Remaining LIVE orchestration (the request smoke's ok gate requires ALL of it)

The smoke seals `cross_cpu=1 result=ok` ONLY on a clean boot showing
`X86_AP_ONLINE cpu=1 → X86_AP_GENERIC_DISPATCH_OK mode=fresh → IPCCALL_DIRECT_SMP_SERVER_BLOCKED
server_cpu=1 → X86_AP_SAVED_DISPATCH_OK mode=saved → X86_AP_RECV_V2_CONTINUED cpu=1 →
IPCCALL_DIRECT_SMP_REQUEST_OK cross_cpu=1`. The bounded remaining pieces, each building on the SEALED
C1/C2A mechanisms:

1. **Real recv-v2 server on CPU 1** — extend the AP proof-task provisioning: create a request
   endpoint, grant its RECEIVE cap into the AP server's CNode + a SEND cap to a CPU-0 client, map
   payload/meta buffers, and one reply-cap slot. The server stub issues a real recv-v2 (blocks). It
   reaches ring 3 via the SEALED C1 fresh-entry.
2. **Authoritative block** — the AP recv-v2 commit installs BlockedRecvState + the exact endpoint
   waiter, leaves the task `BlockedUnfinalized` (absent from run queues), publishes the
   BlockedServerAck, and emits `IPCCALL_DIRECT_SMP_SERVER_BLOCKED server_cpu=1 … saved_frame=1`.
3. **Real CPU-0 client NR6** — a CPU-0 userspace client issues a real NR6 syscall (the trap-gate
   split path, NOT a boot-code call) after the server ack exists.
4. **Cross-CPU delivery + wake** — the accepted `ipc_call_direct_request_txn` copies request+meta,
   mints the reply cap, claims the waiter, runs `finalize_wake_to_runnable_saved` (complete recv-v2
   result in the saved frame), makes the record Available, enqueues the server on CPU 1 LAST, and
   sends a canonical reschedule IPI to CPU 1. The CPU-1 IPI handler only sets the pending flag + EOI.
5. **Recv-v2 saved-frame resume** — the SEALED C2A idle-dispatcher saved-frame resume restores the
   server's recv-v2 continuation on CPU 1 with the delivered request; the server validates the bytes
   + reply cap and emits `X86_AP_RECV_V2_CONTINUED cpu=1 …`; the kernel emits
   `IPCCALL_DIRECT_SMP_REQUEST_OK … cross_cpu=1`.

## Preserved
SYSCALL_COUNT=32, VARIANT_COUNT=22, NR27 absent, DebugLog=192, REPLY_CAP_QUEUEING_SUPPORTED=false,
Stage 198F live cells=30, Stage 199 functional live cells=6, queued IpcCall unsupported. Not begun:
cross-CPU NR7, timeouts, notifications, server-death wake, AArch64/RISC-V SMP, D3.

## Cross-CPU NR7 plan
NR7 mirrors the request on the reply endpoint: the BSP client blocks in recv-v2 on its reply endpoint
(already CPU 0) with a saved continuation; the AP server replies (NR7) delivering cross-CPU; the
client is remotely woken + resumed on CPU 0 via the SAME SEALED C2A saved-frame resume applied to the
BSP-bound caller, reusing the reschedule flag, idle dispatcher, and `finalize_wake_to_runnable_saved`.
