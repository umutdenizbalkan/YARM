<!-- SPDX-License-Identifier: Apache-2.0 -->
# Stage 199A2D2C2B1 — Live CPU-1 recv-v2 Server Block and Blocked-Server Ack

Goal: run a REAL scheduler-selected userspace IPC server on CPU 1, have it invoke the ACTUAL recv-v2
syscall through the normal x86 syscall entry + shared trap dispatch, and genuinely QEMU-prove it
reaches a COMPLETE authoritative blocked state — a committed saved continuation, an installed
`BlockedRecvState` + exact endpoint waiter, absence from every runqueue, home CPU 1, and a published
`BlockedServerAck` — earning `STAGE_199_X86_AP_RECV_V2_BLOCK_SEAL … result=ok`. This stage does NOT
send an NR6 request and does NOT wake or continue the server: it proves the server-block half only.

## Outcome — GENUINE LIVE seal

One fresh `QEMU_SMP=2` boot (`scripts/qemu-x86_64-ap-recv-v2-block-smoke.sh`, `--features
x86-ipccall-direct-smp-oracle`, knobs `yarm.x86_64_ipccall_direct_smp_oracle=1
yarm.x86_64_ipccall_direct_smp_recv_v2_server=1 yarm.ap_user_dispatch=1`) produces, in order:

```
X86_AP_ONLINE cpu=1
X86_AP_RECV_V2_SERVER_PROVISIONED base_tid=20205 asid=5 endpoint_index=6 recv_cap=65536 payload_va=0x20030000 meta_va=0x20040000 home_cpu=1
X86_AP_GENERIC_DISPATCH_OK cpu=1 mode=fresh scheduler_selected=1 tid=20205 result=ok
USER_LOG tid=20205 msg=X86_AP_RECV_V2_SERVER_ENTERED cpu=1 result=ok
IPCCALL_DIRECT_SMP_SERVER_BLOCKED server_cpu=1 recv_v2_committed=1 saved_frame=1 waiter_exact=1 ack_published=1 absent_from_runqueue=1 server_tid=20205 server_asid=5 endpoint_index=6 endpoint_generation=… result=ok
STAGE_199_X86_AP_RECV_V2_BLOCK_SEAL arch=x86_64 smp=2 server_cpu=1 real_syscall=1 blocked_commits=1 ack_publications=1 premature_wakes=0 premature_continuations=0 wrong_cpu_blocks=0 result=ok
```

The proof is genuine: task 20205 fresh-enters ring 3 (scheduler-selected via `dispatch_next_on_cpu`
on the SEALED C1 fresh-entry path), emits a real userspace DebugLog marker, then issues a GENUINE
recv-v2 syscall (NR 2) on a request-endpoint RECEIVE cap held in its OWN process CNode, with real
payload+meta destinations. That syscall routes through the SAME `yarm_x86_lstar_entry` →
`dispatch_trap_entry_with_shared_kernel` → `handle_ipc_recv` path as any other x86 syscall, finds no
message waiting, and BLOCKS — installing `BlockedRecvState`, linking the exact endpoint waiter,
descheduling the task off CPU 1's run queue, and (at the fully-committed recv-v2 point) publishing
the authoritative `BlockedServerAck`. The server then STAYS `BlockedUnfinalized` (no wake, no
saved-frame resume, no continuation) for the rest of the boot.

## What was built

### Real recv-v2 server provisioning — `exec_state.rs::build_ap_workload`
Behind the DEFAULT-OFF sub-selector `yarm.x86_64_ipccall_direct_smp_recv_v2_server=1` (only meaningful
under the SMP oracle), the single AP workload becomes a real IPC server instead of the C2A Yield
proof. On first build it: creates a request endpoint (depth 1 = one reply-cap slot capacity); mints
its RECEIVE cap into the server's OWN process CNode via an attenuated grant (no shared-CNode
shortcut); maps a payload page (`0x2003_0000`) and an IpcRecvMetaV2 page (`0x2004_0000`); arms ONLY
the oracle request endpoint (`set_ipccall_direct_oracle_endpoints(idx, usize::MAX)`); binds the server
to home CPU 1 (`set_task_home_cpu`); and copies a recv-v2 server stub with the minted `recv_cap` CapId
patched in. The stub: `DebugLog(X86_AP_RECV_V2_SERVER_ENTERED) → recv-v2(recv_cap, payload_va, 8,
meta_va, 40, 0) → park`. x86-64 SYSCALL passes recv-v2 arg3 (meta ptr) in R10, arg4 (meta len) in R8,
arg5 (flags) in R9 — matching the LSTAR entry which copies R10 into the RCX/arg3 slot.

### Authoritative blocked-server marker — `mod.rs::maybe_emit_ipccall_direct_smp_server_blocked`
Emitted from the fully-committed recv-v2 block point (right after the `BlockedServerAck` publishes),
EXACTLY once per boot, and ONLY once every authoritative condition is INDEPENDENTLY re-verified here
against committed state (never trusting the caller): the server carries a committed saved frame
(`task_has_saved_frame`), it is absent from EVERY runqueue (`task_present_in_any_runqueue` false), its
home CPU is 1, the exact endpoint waiter identity still equals the server, and the ack sequence is
live. A one-shot `EMITTED` latch makes it fire once. It is a KERNEL marker; it never emits
`IPCCALL_DIRECT_SMP_REQUEST_OK` (this stage delivers no request).

The saved continuation is captured on kernel ENTRY: the x86 `handle_trap` calls
`sync_current_thread_from_frame` before dispatch, so the post-recv-v2 RIP (the `jmp .`), RSP, and GPRs
are committed into `tcb.user_context` before the syscall blocks the task — the earliest complete
saved frame. Because the blocked server is not Runnable, `ap_saved_resume_context` reports
`runnable_with_saved=false`, so it is never selectable for a saved-frame resume.

### No premature wake — `smp.rs::ap_sched_next_or_idle`
The one-shot saved-frame resume (the SEALED C2A mechanism) is SKIPPED entirely while the
recv-v2-server sub-selector is armed — the server must stay blocked, absent from every runqueue, with
no premature dispatch or continuation. The C2A Yield proof (sub-selector off) keeps that resume
unchanged, so its seal is untouched.

### Runqueue-membership query — `scheduler.rs`, `scheduler_state.rs`
`Scheduler::task_present_anywhere` / `KernelState::task_present_in_any_runqueue` return true iff a TID
appears in ANY CPU's run queue or as ANY CPU's dispatched `current` — the authoritative
absent-from-runqueue proof the marker asserts.

### Tests — `stage199a2d2c2b1_guards` (15)
Real recv-v2 syscall in the stub; canonical SERVER_ENTERED marker; RECEIVE cap in the server's own
CNode; request endpoint armed; home CPU 1; payload+meta buffers provisioned; the marker re-verifies
every committed condition; one-shot emission; never emits REQUEST_OK; the recv-v2 server declines the
saved-frame resume; the absent-from-runqueue query is authoritative (blocked task absent, enqueued
task present — a live functional test); the sub-selector is default-off and requires the oracle; the
knob is parsed; the seal requires the full ordered live sequence; the C1/C2A/C2B seals + the C2A Yield
stub are intact.

## Preserved
SYSCALL_COUNT=32, VARIANT_COUNT=22, NR27 absent, DebugLog=192, REPLY_CAP_QUEUEING_SUPPORTED=false,
Stage 198F live cells=30, Stage 199 functional live cells=6, queued IpcCall unsupported. C1
(`STAGE_199_X86_AP_GENERIC_RETURN_SEAL`), C2A (`STAGE_199_X86_AP_SAVED_RETURN_SEAL`) re-run green.
This seal proves the CPU-1 recv-v2 SERVER-BLOCK half; it does NOT deliver an NR6 request, wake the
server, or increase the NR6/NR7 functional live-cell count. Not begun: the CPU-0 NR6 client + delivery
+ cross-CPU wake + recv-v2 resume (C2B2), cross-CPU NR7, timeouts, notifications, server-death wake,
AArch64/RISC-V SMP, D3.

## Remaining cross-CPU NR6 plan (199A2D2C2B2)
The server-block half proven here is the receiver end of the cross-CPU request. To land the full
request seal: a real CPU-0 userspace client issues an NR6 syscall (trap-gate split path) after the
server ack exists; the accepted `ipc_call_direct_request_txn` copies request+meta, mints the reply
cap, claims the waiter, runs `finalize_wake_to_runnable_saved` (completing the recv-v2 result in the
saved frame), makes the record Available, enqueues the server on CPU 1 LAST, and sends a canonical
reschedule IPI to CPU 1; the SEALED C2A idle-dispatcher saved-frame resume then restores the server's
recv-v2 continuation on CPU 1 with the delivered request, emitting `IPCCALL_DIRECT_SMP_REQUEST_OK …
cross_cpu=1`. Every mechanism that resume needs is already sealed — the only new integration is the
CPU-0 client + delivery + wake.
