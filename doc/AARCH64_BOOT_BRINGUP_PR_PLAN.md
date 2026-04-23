# AArch64 Boot Bring-up PR Plan

This plan breaks AArch64 bring-up into small, reviewable PRs so we can start implementation immediately and keep regressions localized.

## Goal
Boot `kernel_boot` on `qemu-system-aarch64` (`virt`) to stable kernel markers, then incrementally enable IRQ/timer, user handoff, and initramfs-backed `init_server` launch.

---

## PR 1 — Early AArch64 serial + deterministic boot markers
**Scope**
- Implement PL011 early console write path for AArch64.
- Emit deterministic markers in `_start`, `prepare_arch_boot`, and `run_with_prepared_kernel`.
- Wire `emit_panic` on AArch64 to serial output.

**Acceptance**
- `scripts/qemu-aarch64-core-smoke.sh` captures early marker sequence.
- Panic path emits marker + message to serial.

---

## PR 2 — Exception vectors + EL transition baseline
**Scope**
- Add AArch64 exception vector table and `VBAR_EL1` setup.
- Establish EL2→EL1 transition path when booting from EL2.
- Define minimal trap entry/return ABI for synchronous exceptions and IRQs.
- Configure `CPACR_EL1.FPEN` before entering Rust code paths that may use FP/NEON instructions.

**Acceptance**
- Deliberate exception test reaches trap handler and returns/halts predictably.
- No silent timeout on trap.

---

## PR 3 — DTB parsing for memory + interrupt bases
**Scope**
- Parse DTB from boot register convention (QEMU `virt`).
- Extract RAM layout, initramfs module bounds, GIC base/config, timer properties.
- Feed parsed values into `prepare_arch_boot` and IRQ setup.

**Acceptance**
- Boot log shows parsed memory/IRQ base values.
- Parsed values are used instead of hardcoded placeholders.

---

## PR 4 — AArch64 MMU bootstrap mapping
**Scope**
- Build initial EL1 page tables for kernel text/data/bss/stack + direct-map window.
- Program MAIR/TCR/TTBR and enable MMU/cache in SCTLR.
- Include MMU-transition maintenance sequence (`TLBI VMALLE1`, `IC IALLU`, `DSB ISH`, `ISB`) around enable.
- Remove placeholder assumptions in aarch64 page-table bootstrap path.

**Acceptance**
- Kernel executes with MMU enabled and reaches `YARM_BOOT_OK` marker.
- No early translation faults during `Bootstrap::init()`.

---

## PR 5 — GIC IRQ ack/eoi and timer tick delivery
**Scope**
- Implement real GIC init and IRQ acknowledge/EOI for runtime path.
- Implement timer deadline programming for bootstrap CPU.
- Hook timer IRQ into scheduler tick path.

**Acceptance**
- Timer markers (IRQ/EOI/tick) appear in AArch64 smoke logs.
- Scheduler tick progression verified.

---

## PR 6 — Context switch + syscall/trapframe correctness on AArch64
**Scope**
- Finalize AArch64 trapframe save/restore contract.
- Validate syscall argument/return ABI and TLS restore behavior.
- Ensure context switch updates per-thread kernel/user state correctly.

**Acceptance**
- Trap/syscall unit tests pass with non-stub behavior.
- Thread switch and TLS restore tests pass consistently.

---

## PR 7 — First user task handoff (non-stub)
**Scope**
- Implement `bootstrap_first_user_task` for AArch64 (real task/image setup).
- Implement `enter_dispatched_user_task_if_available` with EL0 handoff (`eret`).
- Ensure initial capability space/bootstrap namespace is populated before EL0 handoff.
- Remove AArch64 no-op first-task boot stubs.

**Acceptance**
- Boot reaches user-mode handoff path without panic/hang.
- Expected first-task telemetry markers present.

---

## PR 8 — Initramfs-backed `init_server` launch on AArch64
**Scope**
- Reuse initramfs manifest/loader to launch `init_server` in its own user AS.
- Connect service bootstrap ordering to control-plane expectations.
- Align AArch64 smoke marker checks with real init flow.

**Acceptance**
- `YARM_INIT_START` / `YARM_INIT_DONE` observed via AArch64 smoke.
- `init_server` launch path active and stable.

---

## PR 9 — Gate hardening and de-flaking
**Scope**
- Tighten AArch64 smoke scripts from marker-only to stricter progression checks.
- Convert temporary skips/lenient paths back to strict where stability is proven.
- Add targeted regression tests for previously failing bring-up points.

**Status update**
- In-progress: AArch64 smoke now checks ordered early boot progression markers
  and requires timer progression markers (`YARM_TIMER_IRQ_DELIVERED`,
  `YARM_TIMER_EOI_DONE`, `YARM_SCHED_TICK`) when strict mode is enabled.

**Acceptance**
- AArch64 gate scripts run green in strict mode.
- No known flaky test/script exemptions remain for this path.

---

## TODOs captured from review notes
- [ ] Keep EL2→EL1 drop as an explicit, review-visible implementation item inside PR2 (do not bury in vector-only changes).  
- [ ] Add explicit `.bss` zeroing in AArch64 boot assembly before entering Rust.  
- [ ] Preserve DTB pointer from boot register (`x0`) across EL2→EL1 transition and into early boot parsing.  
- [ ] Keep CPACR/FPEN setup explicitly called out in PR2 acceptance/testing notes.  
- [ ] Keep MMU enable maintenance sequence (TLBI/ICACHE barriers) explicit in PR4 implementation checklist.  
- [ ] Add SMP/PSCI placeholder follow-up as **PR10** (single-core bringup first, SMP later).  
- [ ] Call out initial capability-space bootstrap as a first-class item in PR7 description/checklist.  

### PR10 placeholder — SMP/PSCI bring-up (post-boot baseline)
**Scope**
- Add PSCI CPU onlining scaffold and topology integration for secondary cores.
- Define bootstrap-core-only fallback behavior when PSCI/firmware paths are unavailable.

**Acceptance**
- Secondary CPU online path is detectable/telemetry-visible.
- Single-core fallback remains stable when SMP is disabled/unavailable.

---

## Implementation order (start now)
1. PR 1 (serial + markers)
2. PR 2 (vectors + EL transition)
3. PR 3 (DTB parsing)
4. PR 4 (MMU bootstrap)
5. PR 5 (GIC + timer)
6. PR 6 (trap/syscall correctness)
7. PR 7 (first user handoff)
8. PR 8 (initramfs init_server)
9. PR 9 (gate hardening)

We should begin immediately with **PR 1** in the next commit.
