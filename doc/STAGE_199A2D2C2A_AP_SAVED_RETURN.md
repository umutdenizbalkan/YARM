<!-- SPDX-License-Identifier: Apache-2.0 -->
# Stage 199A2D2C2A — Live x86_64 AP Saved-Frame Resume

Goal: implement and genuinely QEMU-prove the idle-dispatcher → saved-user-frame return on CPU 1 —
a scheduler-selected task freshly enters ring 3, runs a real syscall, its post-syscall continuation
is saved, and the AP idle dispatcher restores that saved frame through canonical assembly so the
task continues after the syscall exactly once. No recv-v2 / NR6 / NR7 / endpoint orchestration.

## Outcome — GENUINE LIVE seal

One fresh `QEMU_SMP=2` boot (`scripts/qemu-x86_64-ap-saved-return-smoke.sh`,
`--features x86-ipccall-direct-smp-oracle`) produces, in order, each marker exactly once, on CPU 1
only, no fault:

```
X86_AP_ONLINE cpu=1
X86_AP_GENERIC_DISPATCH_OK cpu=1 mode=fresh scheduler_selected=1 tid=20205 result=ok
USER_LOG tid=20205 msg=X86_AP_SAVED_RESUME_BEFORE
X86_AP_SAVED_FRAME_COMMITTED cpu=1 syscall=Yield task_exact=1 rip_after_syscall=1 frame_complete=1 tid=20205 asid=5 result=ok
X86_AP_SAVED_DISPATCH_OK cpu=1 mode=saved scheduler_selected=1 continuations=1 tid=20205 result=ok
USER_LOG tid=20205 msg=X86_AP_SAVED_FRAME_RESUMED cpu=1 syscall=Yield continuations=1 stack_ok=1 registers_ok=1 result=ok
STAGE_199_X86_AP_SAVED_RETURN_SEAL arch=x86_64 smp=2 cpu=1 fresh_entries=1 saved_dispatches=1 continuations=1 duplicate_entries=0 duplicate_continuations=0 wrong_cpu_continuations=0 result=ok
```

The proof is genuine: task 20205 fresh-enters ring 3 (scheduler-selected via `dispatch_next_on_cpu`),
emits two real userspace DebugLog markers, runs a real `Yield`, is descheduled to a RunnableSaved
state on CPU 1, and its committed post-Yield continuation (RIP = the instruction after the syscall,
RSP = its user stack, all GPRs) is restored through the NEW `yarm_x86_resume_ring3` asm — NOT the
fresh-entry trampoline — so it continues at the `X86_AP_SAVED_FRAME_RESUMED` DebugLog exactly once.

## What was built

### Real saved-frame assembly return — `descriptor_tables.rs`
`yarm_x86_resume_ring3(frame: *const ApSavedResumeFrame)` — the counterpart to the fresh-entry
`yarm_x86_enter_ring3`. It builds the canonical BSP iret frame (SS/RSP/RFLAGS/CS/RIP) from the
committed saved frame and restores ALL 15 user GPRs (`user_gprs` order rax..r15, `rdi` restored
last), then `iretq`s to the post-syscall RIP. `ApSavedResumeFrame` is `repr(C)` with compile-time
`offset_of!` asserts matching the asm (gprs@0, rip@120, rsp@128, rflags@136, cs@144, ss@152). No
reduced AP-only frame ABI — the SS=0x1b / CS=0x23 / RFLAGS=0x202 values are the canonical ones.

### Authoritative capture + selection — `exec_state.rs`, `smp.rs`
`KernelState::ap_saved_resume_context(tid)` reads the committed saved continuation
`(asid, cr3, rip, rsp, gprs[15], runnable_with_saved)` under the lock; `runnable_with_saved` is true
ONLY for a Runnable task with a valid (non-null RIP+RSP) saved frame — never a partial/uncommitted
frame. `ap_saved_frame_resume` (smp.rs) is the idle dispatcher: it sets+consumes the CPU-local
reschedule-pending flag (Release/Acquire), selects the RunnableSaved task from CPU 1's REAL run
queue (`dispatch_next_on_cpu`, not a hardcoded proof TID), validates it, emits the authoritative
`X86_AP_SAVED_FRAME_COMMITTED` + `X86_AP_SAVED_DISPATCH_OK` markers, installs per-CPU
CR3/TSS-RSP0/syscall-RSP0/GS(FS=0) for the selected task, and diverges through
`resume_user_mode_iret` — every Rust guard released before the asm. A one-shot per-CPU latch
(`AP_SAVED_RESUME_DONE`) makes it run EXACTLY ONCE (one continuation, no duplicate).

### Multi-syscall proof stub + inline-resume gate — `exec_state.rs`, `descriptor_tables.rs`
The SMP proof stub is `DebugLog(GENERIC) → DebugLog(BEFORE) → Yield → [saved-frame resume lands
here] → DebugLog(RESUMED) → park` (64 bytes; the post-Yield continuation RIP = code+38, reachable
ONLY via the saved-frame return). To let a multi-syscall stub run, the AP trap dispatch was gated:
for the SMP oracle a RETURNING syscall (nr != 0, e.g. DebugLog) resumes the task INLINE (keeping the
seal probe active), and ONLY `Yield` (nr == 0) runs the deschedule so the task becomes RunnableSaved
and the AP scheduler performs the saved-frame resume. The legacy probe (blocks after every syscall)
is unchanged.

### Tests
`ap_sched::tests` (canonical saved-frame + selection substrate from C1/C2) + `stage199a2d2c2a_guards`
(4): the restore asm exists + restores the full GPR set + is distinct from fresh entry; the
orchestration is scheduler-selected + one-shot + wires the reschedule flag + validates RunnableSaved;
returning syscalls resume inline while only Yield deschedules; the seal is gated behind the full
ordered live sequence and fails on a CPU-0 continuation. The `ApSavedResumeFrame` offsets are
compile-time asserted against the asm.

## Preserved
SYSCALL_COUNT=32, VARIANT_COUNT=22, NR27 absent, DebugLog=192, REPLY_CAP_QUEUEING_SUPPORTED=false,
Stage 198F live cells=30, Stage 199 functional live cells=6, queued IpcCall unsupported. This seal
proves the generic AP saved-frame return mechanism; it does NOT increase the NR6/NR7 functional
live-cell count. Not begun: recv-v2 AP server, cross-CPU NR6/NR7, timeouts, notifications,
server-death wake, AArch64/RISC-V SMP, D3. This is the x86_64 AP-return/D6 subset (D6 AP userspace
scheduling now does both fresh entry AND saved-frame resume).

## Remaining recv-v2 cross-CPU NR6 plan (199A2D2C2)
The saved-frame resume mechanism proven here IS the crux the recv-v2 continuation needed. To land
the cross-CPU NR6 request seal: replace the proof stub with a real recv-v2 server (request endpoint
RECEIVE cap + payload/meta pages + one reply-cap slot) whose recv-v2 blocks; on the CPU-0 NR6
transaction's delivery, run `finalize_wake_to_runnable_saved` to complete the recv-v2 result in the
saved frame and publish RunnableSaved; then this exact idle-dispatcher saved-frame resume restores
the server's recv-v2 continuation on CPU 1 with the delivered request. The only new integration is
the endpoint/cap orchestration + the CPU-0→CPU-1 wake — the AP saved-frame return itself is done.
