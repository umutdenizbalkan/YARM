<!-- SPDX-License-Identifier: Apache-2.0 -->
# Stage 199A2D2A — x86_64 AP-Bound Server and Cross-CPU NR6 Request

Goal: run one userspace IPC server on CPU 1, block it in recv-v2, deliver one NR6 direct request
from a CPU 0 client, remotely wake it, and resume it on CPU 1 — proving ONLY the cross-CPU
`IpcCallDirectRequest` direction (no NR7 reply, no complete Stage 199 SMP seal).

## Outcome summary

The **kernel mechanism** for the cross-CPU request is implemented and proven deterministically in
hosted tests. The **LIVE QEMU cross-CPU seal is BLOCKED** by a missing piece of the accepted x86 AP
infrastructure — there is no AP dispatch-on-wake and no context-restore path — so a blocked recv-v2
server cannot be woken and resumed on CPU 1. No `result=ok` SMP seal is emitted; the smoke reports
`result=blocked` with the precise reason. This is the honest hard-stop the spec requires (same-CPU
must never be presented as cross-CPU; no seal without a clean cross-CPU boot).

## Delivered (verifiable this stage)

### Part 1 — dedicated SMP oracle gate
- Feature `x86-ipccall-direct-smp-oracle` (Cargo.toml), separate from the SMP=1 functional feature.
- Knob `yarm.x86_64_ipccall_direct_smp_oracle=1` (`boot_command_line.rs`).
- Selector `X86_IPCCALL_DIRECT_SMP_ORACLE_SELECTOR = 9`, distinct from the functional selector (3).
- Activation predicate `ipccall_direct_smp_oracle_active(online_cpus)` requires the selector AND
  `online_cpus >= 2` (same-CPU can never spoof cross-CPU). `target_arch = x86_64` + the feature are
  enforced at the marker emitters.
- **Mutual exclusion**: `set_x86_ipccall_direct_smp_oracle_enabled` refuses while the functional
  selector is armed, and the functional setter refuses while the SMP selector is armed. Feature-on
  without the selector is inert.
- The single-slot acknowledgement overwrite fuse stays enabled (one outstanding pair).

### Part 6 — CPU-targeted remote-enqueue mechanism (the core of the cross-CPU request)
Audit result: the accepted NR6 transaction `SharedKernel::ipc_call_direct_request_txn` already
performs a CPU-targeted enqueue — it commits the blocked server, captures its `cpu_affinity`, and
calls `sr_enqueue_committed_receiver_split(tid, affinity)`, which enqueues on that CPU's run queue.
So the request transaction is NOT forked; the cross-CPU behaviour is obtained by binding the server
to CPU 1 first:
- `KernelState::set_task_home_cpu(tid, cpu)` / `task_home_cpu(tid)` — INTERNAL placement (not a
  public affinity syscall) assigning the server's authoritative home/target CPU.
- `SharedKernel::smp_assign_task_home_cpu` / `smp_request_wake_target_split_read` — the oracle-facing
  wrappers (the wake target is the server's home CPU, captured before it blocks).

**Memory ordering**: the scheduler-lock release inside `sr_enqueue_committed_receiver_split`
publishes the server's Runnable state + run-queue membership; the target CPU's dispatch
(scheduler-lock acquire) observes it. Ordering within the transaction (proven by the existing
`stage199a2b2d` tests and reused here): request bytes + metadata copied → reply record made
`Available` → server committed `Runnable` → remote CPU-1 enqueue LAST. So request bytes and record
availability happen-before the remote dispatch.

### Parts 9 + 3 — deterministic hosted tests (`stage199a2d2a_smp_request`, 14 tests)
The server is modelled on CPU 1 via `with_cpu(CpuId(1))` (blocked in recv-v2, home CPU = 1); the
client runs the accepted NR6 transaction. Proven:
1. AP server home CPU survives blocking;
2. the wake plan captures the CPU-1 target;
3. the NR6 transaction remotely enqueues the server on CPU 1 (never the BSP), with request bytes
   visible and the reply record `Available` before dispatch, and one server-local Reply cap minted;
4. two completion attempts (real threads + barrier racing the ack claim) → exactly one delivery →
   one CPU-1 enqueue;
5. a replacement task (same TID, different ASID) cannot inherit the AP wake;
6. request bytes + record availability happen-before remote dispatch;
7. CPU 1 cannot dispatch the server before the enqueue publication;
8. the acknowledgement carries the exact CPU-1 server `{tid, asid}` identity;
9. the overwrite fuse stays zero for one pair;
10. feature-off / selector-off is inert;
11. no NR7 cross-CPU reply marker exists in kernel source this stage;
12. Stage 199 functional invariants preserved (SYSCALL_COUNT=32, VARIANT_COUNT=22, functional
    selector=3);
plus the SMP gate (selector + smp≥2), mutual exclusion, and the capability topology (SEND for the
client, RECEIVE for the AP server; the BSP send cap grants no RECEIVE; process-local receiver
authority, no shared-CNode shortcut).

## BLOCKED (LIVE cross-CPU seal) — Parts 2, 4, 5, 7, 8-live, 10

A genuine cross-CPU NR6 request requires the server to (a) block in recv-v2 on CPU 1, (b) be
remotely woken, and (c) RESUME its recv-v2 continuation on CPU 1. Steps (b)/(c) are not achievable
with the accepted x86 AP infrastructure:

- `live_ap_user_dispatch` / `ap_enter_task_ring3` (`src/arch/x86_64/smp.rs`) enter ring 3 via a
  FRESH pre-built plan at a fixed `entry` — a hardcoded isolated probe (Yield + magic-park). They do
  NOT restore a blocked task's saved trap frame, so they cannot resume a blocked recv-v2 continuation.
- The AP idle loop `ap_idle_halt_loop` (`descriptor_tables.rs`) is a bare `sti; hlt`, and the
  AP remote-wake IPI handler `yarm_ap_remote_wake_stub` only counts + EOIs and `iretq`s back into
  that loop. There is NO AP dispatch-on-wake: an enqueued-on-CPU-1 runnable task is never picked up
  and dispatched by CPU 1.

So even though `sr_enqueue_committed_receiver_split` correctly places the woken server on CPU 1's run
queue (proven in hosted tests), CPU 1 has no mechanism to notice, dispatch, and context-restore it.
Building that is a new subsystem, not completable + QEMU-verifiable as an increment here.

`scripts/qemu-ipccall-direct-x86_64-smp-request-smoke.sh` boots a fresh `x86-ipccall-direct-smp-oracle`
kernel under QEMU_SMP=2, verifies a clean boot with both CPUs online and no ack overwrite / NR7 SMP
marker / panic, checks for the strictly-cross-CPU markers (`IPCCALL_DIRECT_SMP_SERVER_BLOCKED
server_cpu=1` + `IPCCALL_DIRECT_SMP_REQUEST_OK sender_cpu≠receiver_cpu`), and — absent them — emits:

```
STAGE_199_IPCCALL_DIRECT_SMP_REQUEST_SEAL arch=x86_64 smp=2 pairs=1 sender_cpu=0 receiver_cpu=1 cross_cpu=0 duplicate_deliveries=0 duplicate_wakes=0 wrong_waiter_mutations=0 result=blocked reason=ap_dispatch_on_wake_and_context_restore_not_wired
```

It NEVER emits `result=ok` unless the genuine cross-CPU markers appear with distinct CPU IDs.

## Precise Stage 199A2D2B (and completion of D2A LIVE) prerequisite plan

The blocker is an AP **dispatch-on-wake + context-restore** path. The narrow, ordered work:

1. **AP recv-v2 server workload** — a `build_ap_workload` variant whose ring-3 stub issues a real
   recv-v2 syscall on the request endpoint (endpoint RECEIVE cap in the AP server CNode; payload +
   meta destination pages mapped in its ASID), instead of the Yield + park stub.
2. **AP blocking-during-syscall** — when the AP server's recv-v2 finds no message, the AP trap
   dispatch must leave the task `Blocked(EndpointReceive)` (publishing the exact `BlockedServerAck`
   via the existing gated path with `server_cpu=1`) and route the AP to its idle loop WITHOUT
   re-entering ring 3. Emit `IPCCALL_DIRECT_SMP_SERVER_BLOCKED` only after the recv-v2 commit +
   ack publication (authoritative, not timing-based).
3. **AP dispatch-on-wake** — teach the AP idle path (or the remote-wake IPI handler) to, on wake,
   drain CPU 1's run queue under `with_cpu(CpuId(1))` and dispatch the next runnable task.
4. **AP context-restore** — replace the fixed `ap_enter_task_ring3` entry with a context-restoring
   dispatch that loads the resumed task's saved trap frame (user GPRs + saved PC/SP) and `iretq`s to
   the recv-v2 continuation with the delivered payload — reusing the shared per-CPU trap-return
   context-restore. This is the crux; it is the same context-restore the BSP already performs, made
   per-CPU-safe for the AP.
5. **Client on CPU 0** drives the accepted NR6 transaction (already implemented); its
   captured-affinity enqueue (already CPU-1-targeted) hands the server to CPU 1; a reschedule IPI to
   CPU 1 triggers step 3.
6. **Resume proof** — the resumed server validates the request payload/length + reply cap and emits
   `IPCCALL_DIRECT_SMP_REQUEST_OK ... sender_cpu=0 receiver_cpu=1 cross_cpu=1 ...`; the smoke then
   seals `result=ok`.

Stage 199A2D2B (cross-CPU NR7 reply) additionally requires the mirror on the reply endpoint: the
BSP client blocks in recv-v2 on its reply endpoint (already on CPU 0), the AP server replies (NR7)
delivering cross-CPU, and the client is remotely woken + resumed on CPU 0 — which reuses steps 3–4
symmetrically for the BSP-bound caller.

## Preserved
SYSCALL_COUNT=32, VARIANT_COUNT=22, NR27 absent, DebugLog=192, REPLY_CAP_QUEUEING_SUPPORTED=false,
Stage 198F cells=30, Stage 199 functional cells=6, queued IpcCall unsupported, timeouts /
notifications / server-death caller wake unretired, the ack store oracle-only + single-pair.
