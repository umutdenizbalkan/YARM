<!-- SPDX-License-Identifier: Apache-2.0 -->
# Stage 199A2D2B — x86_64 AP Dispatch-on-Wake and Blocked-Task Continuation

Goal: complete the generic x86_64 AP execution mechanism so CPU 1 can run a scheduler-selected
userspace task, be woken by a reschedule IPI, and **resume a blocked task's saved continuation**
(not a fresh restart), then earn the request-only cross-CPU NR6 seal.

## Outcome summary

The **generic, architecture-neutral core** of AP dispatch-on-wake is implemented and proven by
hosted tests: the per-CPU reschedule-pending coalescing flag, the lost-wakeup-safe idle decision,
and the OWNED scheduler-selected dispatch plan that distinguishes `FreshUserEntry` from
`BlockedUserResume`. The **remaining LIVE piece is the arch ASM half** — the AP context-restore
`iretq` that installs per-CPU CR3/TSS-RSP0/GS/FS and resumes a blocked task's saved trap frame,
plus a real recv-v2 server provisioned on CPU 1. That asm is a genuine SMP-bringup milestone: the
AP has **never** performed a context-restore return (its only ring-3 entry is the accepted
fresh-entry probe; AP syscalls end in block + park). Until it is wired and proven under QEMU, no
server can block in recv-v2 on CPU 1 and be woken + resumed there, so the request smoke honestly
reports `result=blocked` (never a false `result=ok`).

## Delivered (verifiable this stage) — `src/arch/x86_64/ap_sched.rs`

Generic, asm-free, NOT oracle-specific (no hardcoded probe TID/RIP/stack/syscall):

### Part 1 — reschedule-pending flag (bounded interrupt work)
`AP_RESCHEDULE_PENDING` per-CPU `AtomicBool` with `set_reschedule_pending` (Release, **coalescing** —
repeated IPIs collapse to one pending request, no lost request, no per-IPI queue growth),
`reschedule_pending` (Acquire observe), and `take_reschedule_pending` (AcqRel consume-once). The LIVE
IPI handler's role is only: identify CPU → set this flag → EOI → return to the interrupted idle
context. It selects no task and never `iretq`s to userspace. The flag is per-CPU (an IPI to CPU 1
never marks CPU 0).

### Part 2 — lost-wakeup-safe idle decision
`ap_idle_should_dispatch(reschedule_pending, has_runnable)` is the pure decision the LIVE idle loop
runs with interrupts DISABLED (`cli`): dispatch iff a reschedule is pending OR a local runnable task
exists; otherwise `sti; hlt` as one atomic sequence. An enqueue landing after the check but before
`hlt` both sets the pending flag and sends the IPI, which stays pending in the LAPIC across the
`cli` window and fires immediately at `sti; hlt` — the classic no-lost-wakeup pattern. The pending
flag's Release/Acquire carries the happens-before, so the IPI is never relied on as the sole
memory-ordering primitive. After wake the AP acquires the CPU-1 scheduler state, thereby observing
the request payload/meta writes, the reply record `Available`, the task `Runnable`, and the CPU-1
run-queue insertion published by the NR6 transaction's enqueue.

### Part 3 — generic scheduler-selected dispatch plan
`ApUserDispatchPlan` is an OWNED, `Copy` value (no `&Task`/scheduler/capability reference escapes a
guard): `{mode, tid, asid, home_cpu, cr3, entry_rip, user_rsp, user_gprs[16], kernel_rsp0,
fs_base}`. `ApReturnMode` distinguishes `FreshUserEntry` (never-run task: fresh entry, zeroed GPRs)
from `BlockedUserResume` (ran-then-blocked task: restore saved GPRs + saved post-syscall RIP/RSP).
`build_dispatch_plan(cpu, selected)` returns `Idle` for the idle task / no selection and REFUSES
(`Idle`) a task whose `home_cpu != cpu` — CPU 1 never dispatches a CPU-0-homed task, and never
migrates a task to prove the wake. `plan_install_permitted` is the fail-closed identity check the
LIVE install runs immediately before the user return (exact `{tid, asid}` + CPU, else refuse).

### Hosted tests
`arch::x86_64::ap_sched::tests` (7): reschedule set/coalesce/consume, per-CPU isolation, idle
decision never loses a wake, fresh-vs-resume distinct + scheduler-selected, refuse another CPU's
task / idle task / no selection, wrong-identity/CPU install rejected, per-task CR3/RSP0/FS.
`stage199a2d2b_guards` (3): plan is fully owned (no borrow escapes user return); the historical
`result=blocked` diagnostic can never satisfy the success seal (ok gated behind distinct-CPU
markers); Stage 199 functional cells stay 6 and Stage 198F stays 30. The CPU-targeted remote enqueue,
single-slot ack, one-pair fuse, and capability topology were proven in Stage 199A2D2A
(`stage199a2d2a_smp_request`, 14 tests) and 199A2D1.

## BLOCKED (LIVE seal) — Parts 4, 5, 7, 9

The remaining work is the arch ASM half plus the live server, each requiring QEMU iteration:

1. **Real recv-v2 server on CPU 1** — a `build_ap_workload` variant whose ring-3 stub issues a real
   recv-v2 syscall (request-endpoint RECEIVE cap in the server CNode; payload/meta pages mapped in
   its ASID; one reply-cap slot). CPU 1 selects it via its run queue and enters ring 3. When recv-v2
   finds no message the AP trap dispatch must leave it `Blocked(EndpointReceive)`, publish the exact
   `BlockedServerAck` (server_cpu=1), and emit `IPCCALL_DIRECT_SMP_SERVER_BLOCKED ... result=ok`
   after authoritative state validation (not timing).
2. **AP context-restore `iretq`** (the crux) — a per-CPU return that loads the plan's CR3, installs
   TSS RSP0 / syscall RSP0 / kernel GS / user FS, and for `BlockedUserResume` restores the full user
   GPR set + saved RIP/RSP/RFLAGS before `iretq`, reusing the canonical x86 userspace return
   trampoline and the existing blocked-syscall completion policy. This is the piece the AP has never
   had.
3. **AP idle dispatcher loop** — replace `ap_idle_halt_loop`'s bare `sti; hlt` with the loop that
   runs `ap_idle_should_dispatch`, and route the remote-wake IPI stub to `set_reschedule_pending` +
   EOI + return (no dispatch in the handler).
4. **Resume proof** — the resumed server validates the exact request bytes, receiver-local Reply
   cap, metadata, `continuation_count == 1`, `current_cpu == 1`, and emits
   `IPCCALL_DIRECT_SMP_REQUEST_OK sender_cpu=0 receiver_cpu=1 cross_cpu=1 ...`; the smoke then seals
   `result=ok`.

Because the LIVE server does not yet run, the request smoke
(`scripts/qemu-ipccall-direct-x86_64-smp-request-smoke.sh`) boots QEMU_SMP=2 clean (both CPUs
online, no ack overwrite / NR7 marker / panic) and emits:

```
STAGE_199_IPCCALL_DIRECT_SMP_REQUEST_SEAL arch=x86_64 smp=2 pairs=1 sender_cpu=0 receiver_cpu=1 cross_cpu=0 duplicate_deliveries=0 duplicate_wakes=0 wrong_waiter_mutations=0 result=blocked reason=ap_context_restore_asm_not_wired
```

It emits `result=ok` ONLY when the ordered LIVE sequence (`X86_AP_ONLINE cpu=1` →
`IPCCALL_DIRECT_SMP_SERVER_BLOCKED server_cpu=1` → `IPCCALL_DIRECT_SMP_REQUEST_OK
sender_cpu≠receiver_cpu`) appears in a genuine clean boot.

## Preserved
SYSCALL_COUNT=32, VARIANT_COUNT=22, NR27 absent, DebugLog=192, REPLY_CAP_QUEUEING_SUPPORTED=false,
Stage 198F live cells=30, Stage 199 functional live cells=6, queued IpcCall unsupported. Not begun:
cross-CPU NR7, timeouts, notifications, server-death caller wake, AArch64/RISC-V SMP, D3, D6.

## Remaining NR7 SMP plan (post-request)
Cross-CPU NR7 (199A2D2C) mirrors the request path on the reply endpoint: the BSP client blocks in
recv-v2 on its reply endpoint (already CPU 0), the AP server replies (NR7) delivering cross-CPU, and
the client is remotely woken + resumed on CPU 0 — reusing this stage's reschedule flag, idle
dispatcher, and context-restore `iretq` symmetrically for the BSP-bound caller.
