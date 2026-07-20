<!-- SPDX-License-Identifier: Apache-2.0 -->
# Stage 199A2D2C1 — x86_64 Generic Scheduler-Selected AP User Return (LIVE)

Goal: prove in a genuine `QEMU_SMP=2` boot that CPU 1 enters a SCHEDULER-SELECTED userspace task
through the generic AP return path (fresh entry). The recv-v2 blocked continuation + cross-CPU NR6
seal are deferred to Stage 199A2D2C2.

## Outcome — GENUINE LIVE seal

One fresh `QEMU_SMP=2` boot (`scripts/qemu-x86_64-ap-generic-return-smoke.sh`,
`--features x86-ipccall-direct-smp-oracle`, `yarm.x86_64_ipccall_direct_smp_oracle=1
yarm.ap_user_dispatch=1`) produces, in order:

```
X86_AP_ONLINE cpu=1
X86_AP_WORKLOAD_BUILT base_tid=20205 count=1 asid=5 cr3=0x1000b000 entry=0x20000000 stack_top=0x20010ff0
X86_AP_RING3_ENTER cpu=1 tid=20205 n=1 entry=0x20000000 stack=0x20010ff0 rsp0=0x... cr3=0x1000b000
X86_AP_GENERIC_DISPATCH_OK cpu=1 mode=fresh scheduler_selected=1 tid=20205 result=ok        (kernel)
USER_LOG tid=20205 msg=X86_AP_GENERIC_USER_ENTRY cpu=1 scheduler_selected=1 result=ok       (userspace)
STAGE_199_X86_AP_GENERIC_RETURN_SEAL arch=x86_64 smp=2 cpu=1 scheduler_selected=1 fresh_entries=1 duplicate_entries=0 wrong_cpu_entries=0 result=ok
```

The proof is genuine: the task (tid 20205) is picked from CPU 1's REAL run queue
(`dispatch_next_on_cpu`, not a hardcoded probe TID), enters ring 3 through the canonical
`enter_user_mode_iret` with its own CR3/stack/kernel-RSP0, and executes a REAL `DebugLog` syscall in
ring 3 on CPU 1 (the `USER_LOG tid=20205 msg=…` line is the kernel's DebugLog handler echoing the
userspace-supplied bytes — authentic userspace execution). Exactly one entry, on CPU 1 only, no
panic/fault, no CPU-0 generic marker.

## Delivered

### Substrate (Parts 1, 2, 5, 8) — `src/arch/x86_64/ap_sched.rs`, `arch::x86_64::ap_sched::tests`
- **Explicit return source** (Part 1): `ApUserReturnSource::{FreshEntry(FreshUserEntryState),
  SavedUserFrame(SavedUserReturnFrame)}`, chosen by `select_return_source` from an authoritative
  `TaskDispatchState` — NeverEntered→FreshEntry, RunnableSaved(committed valid)→SavedUserFrame,
  BlockedUnfinalized→NotFinalized (not dispatchable), Invalid/no-source→fail closed. Never
  `has_run_before`. A previously-run task WITHOUT a committed saved frame is rejected, not treated
  as a blocked continuation.
- **Canonical saved frame** (Part 2): `SavedUserReturnFrame::from_trap_frame` adapts the canonical
  BSP `TrapFrame` (saved_pc/saved_sp/user_gprs + ret0/1/2/error) + canonical user CS=0x23 / SS=0x1b /
  RFLAGS=0x202 — no second AP-only ABI. Fresh vs saved are distinguished by the explicit state.
- **Owned plan + revalidation** (Part 5): `ApUserDispatchPlan` (Copy; `source`/tid/asid/home_cpu/
  cr3/kernel_rsp0/fs_base), `build_dispatch_plan` (refuses wrong-home-CPU + idle-task),
  `plan_install_permitted` (exact `{tid,asid}`+CPU+still-Runnable). No borrow escapes a guard.
- **Part-8 saved-frame tests** (10): preserve RIP/RSP/RFLAGS/CS/SS, all GPRs (not zeroed), syscall
  result state; reject missing/malformed/uncommitted frames; reject identity replacement; reject
  wrong-home-CPU; distinguish fresh vs saved without `has_run_before`; same canonical BSP layout.

### Reschedule flag + idle decision (Parts 3, 4 — mechanism, hosted-tested)
`set/reschedule/take_reschedule_pending` (per-CPU, coalescing, consume-once) and
`ap_idle_should_dispatch` are implemented and hosted-tested. **Honest scope:** the LIVE boot drives
CPU 1's wake through the EXISTING, real AP remote-wake IPI (`AP_REMOTE_WAKE_VECTOR`) + the managed AP
idle loop (`smp_trampoline.rs` labels 75–79) + the dispatch hook — it does NOT yet route through a
newly-rewritten IPI-handler-sets-`AP_RESCHEDULE_PENDING` + idle-consumes-flag path (that needs a
Rust AP IDT handler, deferred). The flag + idle decision are the tested mechanism the full path will
use; the existing wake path is a genuine reschedule IPI + persistent managed idle loop, and the AP
never `iretq`s to userspace from inside the IPI handler (no direct-from-handler dispatch).

### LIVE fresh-entry wiring (Parts 5, 6, 7)
- `build_ap_workload` (`exec_state.rs`), when the SMP oracle is armed, provisions ONE ordinary proof
  task whose userspace stub emits the `X86_AP_GENERIC_USER_ENTRY` marker via a real `DebugLog`
  syscall (marker bytes in a dedicated user-readable page) before yielding/parking.
- `ap_workload_task_count()` runs EXACTLY ONE proof task for the SMP oracle (one fresh entry, no
  duplicate); the legacy 2-task `ap_user_dispatch` scaffold is unchanged.
- `ap_enter_task_ring3` emits `X86_AP_GENERIC_DISPATCH_OK cpu=1 mode=fresh scheduler_selected=1
  tid=<selected>` once, after the scheduler selection, reusing the canonical CR3/TSS-RSP0/GS/FS
  install + `enter_user_mode_iret` (no forked oracle-only iretq).

## Not done this stage (per scope)
recv-v2 blocked AP continuation + cross-CPU NR6 seal (199A2D2C2), cross-CPU NR7, timeouts,
notifications, server-death caller wake, AArch64/RISC-V SMP. This is explicitly the x86_64
AP-return/D6 subset — D6 (AP userspace scheduling) is now partially touched (fresh-entry return).

## Preserved
SYSCALL_COUNT=32, VARIANT_COUNT=22, NR27 absent, DebugLog=192, REPLY_CAP_QUEUEING_SUPPORTED=false,
Stage 198F live cells=30, Stage 199 functional live cells=6, queued IpcCall unsupported. This seal
proves the generic AP return path only; it does NOT increase the NR6/NR7 live-cell count.

## Stage 199A2D2C2 blocked-resume plan
The saved-frame planning path (Part 8) is fully substrate-tested. The remaining LIVE work for the
blocked continuation:
1. AP context-restore `iretq` that consumes an `ApUserReturnSource::SavedUserFrame` (restore all 16
   GPRs + saved RIP/RSP/RFLAGS/CS/SS through the canonical trap-return trampoline), vs the fresh
   `enter_user_mode_iret` used here.
2. A real recv-v2 server on CPU 1 that blocks (publishing `IPCCALL_DIRECT_SMP_SERVER_BLOCKED
   server_cpu=1`) and, when the CPU-0 NR6 transaction remotely enqueues it + wakes CPU 1, is
   selected by this generic dispatcher with a `RunnableSaved` state and resumed via (1).
3. A Rust AP IDT reschedule handler that sets `AP_RESCHEDULE_PENDING` + the idle loop consuming it
   (fully realising Parts 3/4 in the LIVE path), so the wake is driven by the flag rather than the
   legacy `remote_wake_count`.
