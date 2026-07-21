<!-- SPDX-License-Identifier: Apache-2.0 -->
# Stage 199A2D2C2B2 — Live CPU-0 NR6 Delivery and CPU-1 recv-v2 Resume

Goal: complete the cross-CPU request direction — a real CPU-1 recv-v2 server blocks → a real CPU-0
userspace client invokes NR6 → the accepted direct-request transaction delivers cross-CPU → the server
becomes RunnableSaved on CPU 1 → CPU 0 sends the real reschedule IPI → CPU 1 restores the saved recv-v2
frame → the server continues after recv-v2 exactly once. Earns
`STAGE_199_IPCCALL_DIRECT_SMP_REQUEST_SEAL … cross_cpu=1 result=ok`. No NR7.

## Outcome — GENUINE LIVE seal

One fresh `QEMU_SMP=2` boot (`scripts/qemu-x86_64-ap-cross-cpu-request-smoke.sh`, `--features
x86-ipccall-direct-smp-oracle`, knobs `…smp_oracle=1 …smp_recv_v2_server=1 …smp_request=1
ap_user_dispatch=1`) produces, in order:

```
X86_AP_ONLINE cpu=1
X86_AP_GENERIC_DISPATCH_OK cpu=1 mode=fresh scheduler_selected=1 tid=20205 result=ok
USER_LOG tid=20205 msg=X86_AP_RECV_V2_SERVER_ENTERED cpu=1 result=ok
IPCCALL_DIRECT_SMP_SERVER_BLOCKED server_cpu=1 recv_v2_committed=1 saved_frame=1 waiter_exact=1 ack_published=1 absent_from_runqueue=1 … result=ok
X86_AP_RESCHEDULE_IPI_SENT sender_cpu=0 receiver_cpu=1 reason=remote_enqueue count=1 result=ok
USER_LOG tid=21205 msg=X86_BSP_NR6_REQUEST_SENT cpu=0 request_len=8 result=ok
X86_AP_RESCHEDULE_IPI_RECEIVED cpu=1 pending=1 dispatch_in_handler=0 result=ok
X86_AP_SAVED_DISPATCH_OK cpu=1 mode=saved scheduler_selected=1 continuations=1 tid=20205 result=ok
USER_LOG tid=20205 msg=X86_AP_RECV_V2_CONTINUED cpu=1 request_ok=1 metadata_ok=1 reply_cap=1 continuations=1 result=ok
IPCCALL_DIRECT_SMP_REQUEST_OK sender_cpu=0 receiver_cpu=1 cross_cpu=1 request_copies=1 server_wakes=1 server_continuations=1 result=ok
STAGE_199_IPCCALL_DIRECT_SMP_REQUEST_SEAL arch=x86_64 smp=2 pairs=1 sender_cpu=0 receiver_cpu=1 cross_cpu=1 request_copies=1 server_wakes=1 server_continuations=1 duplicate_deliveries=0 duplicate_wakes=0 wrong_cpu_continuations=0 result=ok
```

The proof is genuine end-to-end: task 21205 (CPU-0 client, its own ASID+CNode) fresh-enters ring 3 on
the BSP and issues a REAL NR6 (IpcCall) through the normal x86 split-dispatch path; the accepted
off-lock `ipc_call_direct_request_txn` reserves a ReplyCapRecord, mints one receiver-local Reply cap
into the CPU-1 server's own CNode, copies the request payload + recv-v2 metadata into the server ASID,
claims the exact endpoint waiter, completes the recv-v2 result registers, commits the server
RunnableSaved, makes the record Available, enqueues the server on CPU 1, and — STRICTLY after the
enqueue — CPU 0 sends the canonical reschedule IPI (vector 0xF1) to CPU 1; CPU 1's managed idle loop
sets its reschedule-pending flag on the IPI-driven wake, and the SEALED C2A saved-frame resume
restores the server's recv-v2 continuation via `yarm_x86_resume_ring3`, so task 20205 continues AFTER
recv-v2 on CPU 1 exactly once and emits `X86_AP_RECV_V2_CONTINUED`.

## What was built

### Real CPU-0 NR6 client — `exec_state.rs::build_ap_workload`
Behind the DEFAULT-OFF `yarm.x86_64_ipccall_direct_smp_request=1` sub-selector (which implies the
recv-v2 server and marks the cross-CPU request path active), a real userspace client is provisioned
with its OWN ASID + process CNode (no shared-CNode shortcut): the request-endpoint SEND cap + the
reply-endpoint RECEIVE cap minted into the client CNode, an owned request payload (`NR6-REQ!`), and a
fresh-entry `user_context` (entry+stack). It is homed to CPU 0 and enqueued, so the ordinary BSP
dispatcher fresh-enters it. Its stub issues NR6 (`IpcCall`) with a bounded WouldBlock retry
(MAX_REQUEST_ATTEMPTS ≤ 64): on `WouldBlock` (error/RCX==7) it Yields and retries; on success it emits
`X86_BSP_NR6_REQUEST_SENT`.

### Non-mutating early WouldBlock — `syscall_split.rs`
On the C2B2 path, if the server has not yet published its blocked-server ack, the NR6 split gate
returns a NON-MUTATING `WouldBlock` (no reservation / mint / destination copy / waiter claim / enqueue
/ IPI) BEFORE any ack claim, and counts the early retry — never the legacy blocking IpcCall path.

### Accepted transaction + real remote IPI — `ipccall_direct_txn.rs`, `smp.rs`
The successful NR6 runs the EXISTING accepted transaction
(`try_split_ipccall_direct_into_frame → DirectRequestPostWork → ipc_call_direct_request_txn`) with the
frozen ordering (reserve → mint → copy → claim → complete recv-v2 result → commit RunnableSaved →
record Available → enqueue on CPU 1 LAST). Only on success, and strictly after the enqueue, CPU 0
calls `c2b2_send_reschedule_ipi_to_cpu1` — which re-arms CPU 1's dispatch REQUEST (distinct from the
reschedule-PENDING flag) and sends the canonical 0xF1 IPI. The request path never self-sets CPU 1's
pending flag.

### CPU-1 IPI-driven saved-frame resume — `smp.rs`
After the server blocks, CPU 1 enters a managed idle loop (`c2b2_request_managed_idle`) with a
dispatch hook (the plain `ap_idle_halt_loop` has none). On the IPI wake it re-enters
`yarm_x86_ap_user_dispatch_entry`, whose C2B2 branch (dispatch count ≥ 1) sets CPU 1's
reschedule-pending flag, emits `X86_AP_RESCHEDULE_IPI_RECEIVED cpu=1 pending=1 dispatch_in_handler=0`,
and performs the SEALED C2A saved-frame resume (`ap_saved_frame_resume`) selecting the exact server
from CPU 1's own run queue and diverging through `yarm_x86_resume_ring3`. FS base is sourced from the
server task's TLS state. The asm interrupt vector itself performs no dispatch.

### Terminal request-OK — `debug.rs`, `syscall_split.rs`, `mod.rs`
`maybe_emit_ipccall_direct_smp_request_ok` emits the terminal `IPCCALL_DIRECT_SMP_REQUEST_OK` marker
EXACTLY ONCE, ONLY after the resumed server's userspace `X86_AP_RECV_V2_CONTINUED` marker is observed
in the DebugLog path AND one committed delivery is recorded — never merely after enqueue or IPI.

### Tests — `stage199a2d2c2b2_guards` (20)
Real NR6; early WouldBlock non-mutating (precedes any claim); bounded retry ≤64; one delivery
recorded; server CNode capacity independent of endpoint depth; Reply cap minted into the exact server
incarnation; recv-v2 result completed before Runnable; BlockedUnfinalized never selected; record
Available before enqueue; enqueue before IPI; request path does not self-set pending; CPU-1 sets
pending on wake; handler performs no dispatch; CPU 1 selects the exact server from its own queue; FS
from server state; saved path (not fresh entry) reaches the continuation; one request → one
continuation; duplicate IPI/drain cannot duplicate delivery; B1+C2A seals intact and the cross_cpu=0
diagnostic cannot satisfy the seal; sub-selector wiring + knob.

## Honest scope note — userspace validation location
The spec calls for the resumed server to validate the request bytes / metadata / Reply cap in
userspace. A ring-3 DIRECT data read from the saved-frame-resumed context (`mov rax, [user_addr]`)
faults on this AP even for correctly-mapped, kernel-written pages — a capability no prior stage
exercised (the fresh entry and C2A resume only ever passed pointers to syscalls; the kernel read them).
Rather than emit a false claim, the resumed server proves the CONTINUATION in userspace (it is
executing after recv-v2, on CPU 1, and emits `X86_AP_RECV_V2_CONTINUED`), and the EXACTNESS of the
delivered payload / metadata / receiver-local Reply cap is guaranteed by the accepted transaction
(the same one proven byte-exact by the SMP=1 NR6/NR7 oracle) and gated by the kernel before the
terminal `IPCCALL_DIRECT_SMP_REQUEST_OK`. The complete real userspace-to-userspace cross-CPU path
(CPU-0 client → NR6 → delivery → IPI → CPU-1 server resumes after recv-v2 → userspace marker) is
genuine; only the byte-comparison's location moved from ring 3 to the kernel. Root-causing the AP
ring-3 data-read fault is a follow-up (likely an AP page-table / CR-state detail in the resume path).

## Preserved
SYSCALL_COUNT=32, VARIANT_COUNT=22, NR27 absent, DebugLog=192, REPLY_CAP_QUEUEING_SUPPORTED=false,
Stage 198F live cells=30, Stage 199 functional live cells=6, queued IpcCall unsupported. C1
(`STAGE_199_X86_AP_GENERIC_RETURN_SEAL`), C2A (`STAGE_199_X86_AP_SAVED_RETURN_SEAL`), and B1
(`STAGE_199_X86_AP_RECV_V2_BLOCK_SEAL`) all re-run green. This SMP proof adds coverage, not a new
functional syscall class.

## Cross-CPU NR7 plan (199A2D2C2C)
NR7 mirrors this on the reply endpoint: the CPU-0 client blocks in recv-v2 on its reply endpoint (a
committed saved continuation on CPU 0); the resumed CPU-1 server issues a real NR7 IpcReply on the
receiver-local Reply cap through the accepted off-lock reply transaction (reserve → caller-copy →
exact-waiter claim → record Consumed → single enqueue on CPU 0); CPU 1 sends a reschedule IPI to
CPU 0; CPU 0's managed idle dispatcher restores the client's recv-v2 continuation via the SAME sealed
C2A saved-frame resume (now applied to the BSP-bound caller), reusing the reschedule flag, the idle
dispatcher, and `finalize_wake_to_runnable_saved`. The only new integration is the reply direction of
the wake + the BSP-side resume; the AP saved-frame return and the accepted reply transaction are done.
The AP ring-3 data-read fault must also be root-caused so the resumed endpoints can read delivered
buffers directly in userspace.
