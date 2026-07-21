<!-- SPDX-License-Identifier: Apache-2.0 -->
# Stage 199A2D3 — x86_64 Direct IPC Exact-Commit Regression and Freeze Seal

Freezes the completed x86_64 direct-IPC implementation by proving, from ONE exact clean commit, that
the Stage 199A2D2C2C BSP saved-resume + nested-trap-depth changes preserve all four behaviors, then
emitting the freeze seal.

## Deterministic QEMU lifecycle (`scripts/lib/qemu-x86-deterministic.sh`)
`qemu_run_deterministic` makes the SMP smoke — not `timeout`, not an external killer — own QEMU
termination: it truncates a fresh log (so no stale/manual terminal marker can predate the run),
launches QEMU in the background, polls the log until ALL `QEMU_TERMINAL_MARKERS` appear, scans a fatal
regex every poll, and only then terminates QEMU from the script and waits for it to exit. A SIGTERM
exit is accepted ONLY because the complete terminal condition was observed first. It fails closed on:
QEMU exit before proof (`qemu_exited_before_proof`), a fatal marker before proof
(`fatal_before_termination`), and a ceiling hit without proof (`timeout_before_completion`). The
bidirectional smoke (`qemu-x86_64-ap-cross-cpu-reply-smoke.sh`) uses it and only seals on rc 0. This
closes the Stage 199A2D2C2C gap where QEMU had been terminated externally.

`X86_BSP_RESCHEDULE_IPI_SENT sender_cpu=1 receiver_cpu=0` is the authoritative, deterministic reverse
IPI proof (exactly one). `X86_BSP_RESCHEDULE_IPI_RECEIVED` is BEST-EFFORT: the BSP resume is driven by
CPU-0's trap-return poll of the committed reply, so a timer tick can win the race and produce the
terminal markers before the 0xF1 handler runs — the smoke accepts 0 or 1 (never >1), and requires
`dispatch_in_handler=0` whenever it is present.

## Trap-depth correctness (guards: `stage199a2d3_freeze_guards`)
The normal trap-return epilogue keeps its balanced per-CPU depth reset (`store(0)` at every
return/idle point). The diverging saved-frame `iretq` clears the nested-trap guard EXACTLY ONCE via
`clear_trap_dispatch_depth(cpu)`, immediately before the resume `iretq` (the abandoned kernel-stack
frame never unwinds through the normal epilogue). The clear is confined to the diverging resume — its
`(cpu)` call form appears zero times in `descriptor_tables.rs` and exactly once in `smp.rs`.

## Authoritative current-task selection (guards + `scheduler::tests::on_preempt_prefer_on_*`)
The resume performs, in order: authoritative selection via `on_preempt_prefer_on_cpu(cpu, client_tid)`
→ publish per-CPU current → load CR3/FS → prepare frame → diverging `iretq`; it aborts without mutation
if the client cannot be made current. Regression cases: an absent replacement TID is rejected (FIFO
fallback), a task assigned to another CPU is never stolen, a duplicate runnable entry is never
dispatched twice (single-entry invariant), and current is published before the userspace return — so
the resumed task's first DebugLog resolves ITS ASID, not the previous idle/BSP task.

## Exact-commit x86 regression runner (`scripts/qemu-ipccall-reply-direct-x86_64-final-seal.sh`)
Captures one SHA + clean tree, runs four fresh runs serially with fresh logs/artifacts, and RE-CHECKS
the SHA + clean tree after each:
- **RUN_A** x86 feature-off core smoke, SMP=1 — no direct-oracle retirement markers.
- **RUN_B** x86 direct request/reply functional smoke, SMP=1 — request=1, reply=1, server_wakes=1,
  caller_wakes=1, duplicate_reply=rejected.
- **RUN_C** x86 AP dispatch/saved-resume regression, SMP=2 — AP saved-dispatch=1,
  request_user_consumed=1, no ring-3 fault.
- **RUN_D** x86 bidirectional cross-CPU direct IPC, SMP=2 (deterministic lifecycle) — cross-CPU
  request/reply=1, user-consumed both directions, IPIs 1/1, continuations 1/1, duplicate refused, no
  overwrite fuse.

## Final freeze seal
Emitted ONLY after all four fresh runs succeed from the same clean commit:
```
STAGE_199_X86_DIRECT_IPC_FINAL_SEAL functional_smp1=1 ap_dispatch_smp2=1 cross_cpu_request_smp2=1 cross_cpu_reply_smp2=1 request_user_consumed=1 reply_user_consumed=1 trap_depth_errors=0 wrong_current_task=0 duplicate_replies=0 duplicate_wakes=0 overwrite_fuse_trips=0 result=ok
```

## Preserved
No NR6/NR7 transaction change; no multi-pair support. SYSCALL_COUNT=32, VARIANT_COUNT=22, NR27 absent,
DebugLog=192, REPLY_CAP_QUEUEING_SUPPORTED=false, Stage 198F cells=30, Stage 199 functional cells=6,
single-pair acknowledgement store, multi-pair concurrency unclaimed, queued IpcCall unsupported,
timeouts / notifications / server-death caller-wake unretired. AArch64 / RISC-V QEMU not re-run (no
shared arch-neutral production code changed).
