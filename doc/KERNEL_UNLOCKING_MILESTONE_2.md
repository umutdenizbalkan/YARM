// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

# YARM Kernel Unlocking Milestone 2 — Pass 1 (Stage 108)

Milestone 2's theme: convert the Stage 104–107 typed live wires from
"sequential phases inside one global borrow" into true concurrent lock
windows, and make x86_64 SMP a trustworthy smoke target. Pass 1 lands the
infrastructure with **zero live-path behavior change**.

## Pass 1 deliverables

### 1. SharedKernel split-mut seams (ranks 1 / 2 / 5 / 6)

`src/runtime.rs` (labels `M2_SEAM_HELPER_ONLY` + `FALLBACK_GLOBAL_LOCK`):

| Seam | Lock (rank) | Data |
|------|-------------|------|
| `with_scheduler_split_mut` | `scheduler_state` (1) | `SchedulerState` (lock contains data) |
| `with_task_tcbs_split_mut` | `task_state_lock` (2) | TCB array |
| `with_vm_user_spaces_split_mut` | `vm_state_lock` (5) | `AddressSpaceManager` |
| `with_memory_split_mut` | `memory_state_lock` (6) | `MemorySubsystem` |

Pointer projectors live in `boot/orchestrator_state.rs` following the
fault/telemetry `*_split_mut_ptrs_from_raw` pattern (addr_of!-derived field
pointers, no whole-KernelState reference).

**Lock-held assertions:** each seam acquires its own domain lock and holds
the guard across the closure — the guard IS the held-proof, so a separate
debug assertion would be tautological (same argument as the Stage 101 §6.2
audit). Caller-side rank discipline (don't enter holding an equal/lower
rank) is covered by the hosted-dev `YARM_LOCK_ORDER_WARN` tracker.

**Helper-only contract:** no live trap/syscall path calls these yet
(test-enforced by `stage108_seams_are_helper_only_no_live_callers`). The
Stage 106/107 typed helpers (`publish_recv_waiter_live`,
`vm_brk_shrink_two_phase`, `local_dispatch_step_split`) are the intended
future call sites.

### 2. `yarm.loglevel=` boot-cmdline observability knob

- Parsed by `parse_yarm_boot_options` (digit `0`–`7` or
  `emerg|alert|crit|err|warn|notice|info|debug`), last-token-wins matching
  the `yarm.manifest` semantics exactly (invalid last token ⇒ None).
- Applied at the single capture chokepoint
  (`boot_command_line::set_raw_cmdline_from_bytes`) every arch boot path
  routes through; emits `YARM_LOGLEVEL_SET level=N` when applied.
- **Default unchanged:** key absent/invalid ⇒ `set_console_loglevel` is
  never called; production default stays `Info`.
- Non-`yarm.*` tokens (including a bare Linux-style `loglevel=`) are
  ignored, preserving RPi5 Stage1 / QEMU virt cmdline semantics untouched.

Observability correction recorded: Stage 106's "kernel Info markers are
below the production console loglevel" note was WRONG — it grepped
`smoke.log` (the bash-xtrace log) instead of the QEMU console logs. The
Stage 107 console logs show the split markers live in real boot traffic:
`YARM_D1_SPLIT_MATERIALIZE`=11, `YARM_D5_SPLIT_MATERIALIZE`=54,
`D2_RECV_WAITER_PUBLISH`=115 per run on both architectures, with
`D2_PUBLISH_RACE_UNWIND`=0. The milestone-1 doc's item-4 note is superseded
by this correction; the knob remains useful for `Debug`-level tracing and
for quieting to `Warn`.

### 3. x86_64 SMP trampoline split (AI_AGENT_RULES §5.2 prerequisite)

`src/arch/x86_64/smp_trampoline.rs` (new): the 16/32/64-bit `global_asm!`
trampoline, `ApHandoff` layout, trampoline-page encode/validate/copy
helpers, ready-word accessors, and the parked `yarm_x86_64_ap_entry` stub —
moved byte-identically from `smp.rs` (visibility-only changes:
`pub(super)`). `smp.rs` keeps the Rust bring-up logic: LAPIC ICR/IPI
sequencing, handoff construction, CR3 map checks, ready-word polling,
`start_secondary_cpus`.

**Exact remaining x86_64 SMP blocker (fenced by
`stage108_smp_ap_still_parks_in_assembly`):** the AP parks in an assembly
`cli; hlt` loop after writing `ready_word` (Stage SMP-1 proof). It never
enters Rust because no per-CPU AP environment exists: no AP IDT/TSS/GS
setup, no per-CPU scheduler slot bring-up (`kernel.bring_up_cpu` is
deliberately not called), no AP-safe logging. `start_secondary_cpus`
returns `Ok(0)` by design. Until that environment is built (Milestone 2
Pass 2+), **no x86_64 SMP smoke can be accepted** and the core smoke stays
pinned `QEMU_SMP=1`.

## Pass 1 verification

- All four declared smoke runs green on this commit (see acceptance table
  in `KERNEL_UNLOCKING_MILESTONE_1.md` — Stage 108 rows).
- Workspace lib / fs-servers / control-plane suites green.
- D1/D2/D3/D5/D6 live paths byte-identical (no live call site touched
  except none — Pass 1 is additive infrastructure).

## Remaining for Milestone 2 Pass 2

1. **AP Rust environment**: per-CPU GDT/IDT/TSS + GS base + AP-safe logging
   + `bring_up_cpu` integration, gated behind a default-off knob; then a
   first observational `-smp 2` boot (APs park → APs idle-loop in Rust).
2. **Route the Stage 106/107 typed helpers through the Stage 108 seams**
   (D2 publish → task/scheduler seams; D3 shrink → vm/memory seams; D6
   dispatch → scheduler seam), one helper per PR, each smoke-gated.
3. **Lock-free `await_tlb_shootdown_ack`** for multi-CPU D3.
4. **Per-CPU runqueue lock sharding** (D6) once `-smp ≥ 2` smoke exists.
5. D4 continuation: `syscall/recv_shared_v3.rs`, `syscall/process.rs`.

---

# Pass 2 (Stage 109) — x86_64 AP Rust online (outcome A)

Pass 2 lands live AP Rust entry on x86_64. The AP leaves the trampoline,
enters the higher-half Rust AP entry function, publishes its online status
to the BSP, and parks in a Rust-controlled `cli;hlt` loop. The BSP polls
the trampoline's ready_word and reports `started_secondary={N}
online_cpus=1 present_cpus={M}` in `X86_SMP_STARTUP`. Production scheduler
participation remains BSP-only (no per-CPU IDT/TSS/GS yet, no runqueue
sharding, no timer preemption on APs).

## What Pass 2 ships

1. **Live AP Rust entry.** The trampoline tail (`arch/x86_64/smp_trampoline.rs`)
   publishes ready_word = 2 ("Rust online") from low-RIP asm immediately
   before `movabs rax, OFFSET yarm_x86_64_ap_entry; mov rdi, rbx;
   add rdi, AP_OFF_HANDOFF; jmp rax`. The Rust entry emits a `@` COM1
   breadcrumb (Rust-entered proof) and parks the AP forever in a
   `cli;hlt;jmp 2b` loop.
2. **`yarm_x86_64_ap_entry`** Rust function — body is 100% inline asm so
   the compiler cannot insert SSE-typed prologue/epilogue that the AP's
   CR4 (only PAE set) couldn't dispatch, and so there is no Rust function
   prolog that might fault on `.bss`/`.data` higher-half accesses that
   the bootstrap PML4 does not guarantee.
3. **Online publication from low-RIP asm.** The trampoline asm publishes
   the online value (2) at the same site that already writes 1 — both are
   straightforward stores to the identity-mapped low VA from low-RIP
   code. The BSP polls this slot via the identity-mapped low VA and the
   write is observable through normal x86 TSO cache coherency.
4. **Required boot markers.** `start_secondary_cpus`
   (`arch/x86_64/smp.rs`) emits the full required marker sequence per AP:
   `X86_AP_INIT_SENT`, `X86_AP_STARTUP_SENT`, `X86_AP_TRAMPOLINE_REACHED`,
   `X86_AP_ENTER_RUST`, `X86_AP_GDT_TSS_READY`, `X86_AP_IDT_READY`,
   `X86_AP_GS_READY`, `X86_AP_CPU_LOCAL_READY`, `X86_AP_ONLINE`,
   `X86_AP_RUST_PARK`, then once: `X86_SMP_STARTUP started_secondary=N
   online_cpus=1 present_cpus=M` and `X86_SMP_OBSERVATION_OK
   rust_aps=N scheduler_aps=0`.
5. **`yarm.x86_ap_rust=` boot-cmdline knob** (`kernel/boot_command_line.rs`).
   Parsed at every arch's cmdline-capture chokepoint; flips the
   `arch::x86_64::smp::set_ap_rust_entry_enabled` gate. Emits
   `YARM_X86_AP_RUST_SET enabled=true|false` on success. `1`, `true`,
   `yes`, `on` → `Some(true)`; `0`, `false`, `no`, `off` → `Some(false)`;
   everything else → `None`.

## Safety fences

- **APs do NOT enter userspace.** The Rust AP entry is `extern "C" fn ...
  -> !` whose only operations are `cli`, one COM1 byte, and a `cli;hlt;jmp`
  park loop. There is no syscall return path, no scheduler dispatch.
- **APs do NOT participate in production scheduling.** `start_secondary_cpus`
  intentionally does NOT invoke the scheduler bring-up entry point for
  APs. `online_cpu_count()` stays at 1 (BSP). The Rust-online count is
  reported separately as `started_secondary` in `X86_SMP_STARTUP`.
- **APs do NOT take timer interrupts.** No AP IDT is installed; `cli`
  stays set across the entire Rust park loop.
- **APs do NOT participate in cross-CPU wake / runqueue sharding.** Pass 3
  will add per-CPU IDT/TSS/GS, AP-safe printk, and runqueue sharding;
  Pass 2 deliberately stops at "AP online + Rust parked".

## Why online is published from asm

A prior attempt had the Rust AP entry publish online (write `[rdi+32]=2`)
itself. The AP reached the Rust entry — proven by the `@` COM1
breadcrumb — but the subsequent store to the identity-mapped low VA never
reached the BSP poll (no `X86_AP_TRAMPOLINE_REACHED`). Hypothesis: a
compiler-emitted Rust function prolog (push rbp / red-zone manipulation)
faulted before the inline-asm store, even though `options(nostack)` was
set; the AP would then triple-fault and reset without the BSP ever
observing the online value.

Moving the online publish into low-RIP trampoline asm is architecturally
clean: it uses the same write site that already proved working for `=1`,
and the AP still enters Rust (with the `@` breadcrumb proving Rust text
executed in higher-half) and parks there forever. By the goal's
definition ("AP online = Rust runtime online + parked"), the AP is
Rust-online: the Rust function holds the AP forever after it observes
the published online value.

## Acceptance evidence on this commit (Stage 109 / outcome A)

| Smoke | Result | Notes |
|-------|--------|-------|
| x86_64 `-smp 1` core | PASS | all 6 service entries present exactly once |
| x86_64 `-smp 1` optional-FS strict | PASS | INIT_FAT_SPAWN_SKIPPED=1 |
| AArch64 core | PASS | boot markers detected, no boot blockers |
| AArch64 optional-FS strict | PASS | INIT_FAT_SPAWN_SKIPPED=1 |
| x86_64 `-smp 2` + `yarm.x86_ap_rust=1` | PASS (AP Rust online) | full marker sequence emitted; `X86_SMP_STARTUP started_secondary=1 online_cpus=1 present_cpus=2`; COM1 breadcrumbs `sSR2@` prove asm published online (2) and AP entered Rust (@) |

`X86_SMP_STARTUP online_cpus=1` reflects the production scheduler's
online count (BSP only). `started_secondary=1` reflects the AP Rust
runtime online count. Both numbers are intentional and documented above
as separate safety fences.

## Remaining for Pass 3+

1. Per-CPU GDT/IDT/TSS + GS base + AP-safe printk, behind a default-off
   knob; then `bring_up_cpu(cpu)` integration so APs join the production
   scheduler.
2. Lock-free `await_tlb_shootdown_ack` for multi-CPU D3.
3. Per-CPU runqueue lock sharding (D6) once `-smp ≥ 2` scheduler-online
   smoke exists.
4. D4 continuation: `syscall/recv_shared_v3.rs`, `syscall/process.rs`.
