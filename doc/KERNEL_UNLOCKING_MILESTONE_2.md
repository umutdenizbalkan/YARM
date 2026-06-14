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

# Pass 2 (Stage 109) — AP Rust-entry scaffolding (outcome B)

Pass 2 lands the cmdline + arch-SMP plumbing that Pass 3's live AP Rust-entry
work will use, without modifying the trampoline assembly. This is outcome
**B** from the goal: "AP Rust entry partially works but is default-off with
exact blocker; -smp 1 and FS smokes pass."

## What Pass 2 ships

1. **`yarm.x86_ap_rust=` boot-cmdline knob** (`kernel/boot_command_line.rs`).
   Parsed at every arch's cmdline-capture chokepoint; flips the gate in
   `arch::x86_64::smp::set_ap_rust_entry_enabled`. Emits
   `YARM_X86_AP_RUST_SET enabled=true|false` on success.
   *Note: `1`, `true`, `yes`, `on` map to `Some(true)`; `0`, `false`, `no`,
   `off` map to `Some(false)`; everything else is `None`.*
2. **`AP_RUST_ENTRY_ENABLE` gate** (`arch/x86_64/smp.rs`,
   `ap_rust_entry_enabled` / `set_ap_rust_entry_enabled`). Default `false`.
3. **`AP_RUST_ONLINE` per-CPU AtomicBool array**
   (`arch/x86_64/smp_trampoline.rs`). All slots `false` today; Pass 3 will
   have the AP Rust entry set them on park.
4. **`yarm_x86_64_ap_entry`** future Rust entry function (unchanged from
   Stage 108 — still a `cli;hlt` loop that the trampoline does not yet
   `jmp rax` into).

## What Pass 2 does NOT touch

- **Trampoline assembly is byte-identical to Stage 108.** The trampoline
  still writes `ready_word = 1` and parks the AP in the assembly cli/hlt
  loop. No `jmp rax` into Rust.
- **`start_secondary_cpus` still returns `Ok(0)`** on -smp N > 1: the AP
  reaches the trampoline assembly park, but no Rust runtime / scheduler
  participation. BSP service chain owns all execution.

## Why the trampoline asm wasn't modified

A prototype Pass 2 added an `ap_entry_addr: u64` field to `ApHandoff` (slot
+8 bytes) and changed the trampoline tail to
`mov rax, [rbx + AP_OFF_HANDOFF + 40]; test rax,rax; jnz jmp_rax; F_park`.
The Rust entry was reached (trampoline UART breadcrumb `J` observed under
`-cpu qemu64,+pdpe1gb,+x2apic -smp 2 -append "...yarm.x86_ap_rust=1"`), but
the BSP service-chain task (tid=2) subsequently faulted on wild addresses
(`addr=0x89416581`, `0x8000000000`, `0xE8C0DAAC` — varied per run) with
`#GP` and `#UD` traps escalating to `PANIC strict unknown trap policy`.

The regression appeared even with the gate disabled (ap_entry_addr=0,
trampoline took the `F`allback assembly park) and even on the original
`-cpu qemu64` model. Reverting just the trampoline asm restored the clean
behavior — so the trigger sits in the trampoline asm changes, not in the
gate / cmdline-knob plumbing.

## Exact remaining blockers (Pass 3 work list)

1. **Root-cause the trampoline-tail / BSP regression.** Hypotheses to test:
   - The `.zero 48` (vs `.zero 40`) reservation shifts a downstream symbol
     used by the BSP boot path.
   - The handoff field expansion creates an alignment hazard that the BSP
     copy-out hits.
   - Modifying the trampoline tail invalidates an implicit AP cache state
     that, when AP later #UDs on the Rust entry, IPIs back to BSP through
     LAPIC interrupts that the BSP can't yet route.
   - The AP triple-fault path (when Rust entry isn't safely usable) resets
     the AP, which then runs through BIOS and corrupts low memory shared
     with bootstrap data.
2. **Minimum AP per-CPU env.** Once root-cause is in hand: AP IDT/TSS/GS,
   FX state init, AP-safe `printk` path, before flipping the trampoline
   `jmp rax`.
3. **Wire `prepare_trampoline_for_cpu` to read the gate.** Then re-attempt
   the trampoline asm change behind the gate.

## Acceptance evidence on this commit (Stage 109)

| Smoke | Result | Notes |
|-------|--------|-------|
| x86_64 `-smp 1` core | PASS | unchanged from Stage 108 |
| x86_64 `-smp 1` optional-FS strict | PASS | unchanged |
| AArch64 core | PASS | unchanged |
| AArch64 optional-FS strict | PASS | unchanged |
| x86_64 `-smp 2` observational | PASS (idle reached) | BSP service chain owns boot; no AP scheduler participation; AP parks in assembly cli/hlt per Stage 108 |
| x86_64 `-smp 2` + `yarm.x86_ap_rust=true` | PASS (gate set, no asm change) | `YARM_X86_AP_RUST_SET enabled=true` observed; behavior identical to default |

`yarm_x86_64_ap_entry` is still not called from anywhere — Pass 3 will
change the trampoline tail to `jmp rax` once the per-CPU env exists.
