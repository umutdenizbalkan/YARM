<!-- SPDX-License-Identifier: Apache-2.0 -->

# Kernel Status (Mechanism Layer Completion)

This snapshot reflects the current branch after the mechanism-hardening pass.

## Recent updates

- **Stability tracker ST-1..ST-8 closed on this branch:** the previously-blocking bootstrap/runtime items are implemented and integrated:
  - ST-1 / CB-1: single-kernel boot flow (no dual `Bootstrap::init()` split between boot markers and init smoke path).
  - ST-2 / CB-3: x86_64 IDT assembly trap stub table with common GPR save/restore dispatch glue.
  - ST-3 / CB-2: canonical higher-half x86_64 split (`KERNEL_SPACE_BASE = 0xFFFF_8000_0000_0000`) with dependent constant/linker updates.
  - ST-4 / H-3: ELF64 parser now validates header/program-header structure and `PT_LOAD` bounds instead of magic+entry-only parsing.
  - ST-5 / CB-4: `syscall/sysret` fast-path wired (MSR programming + LSTAR entry), with `int 0x80` compatibility path retained.
  - ST-6 / MISS-1: demand paging path added for qualifying user page faults (heap/brk and bounded stack-growth windows).
  - ST-7 / CB-5: external IRQ decode range widened to platform-configured IRQ line count (64 lines currently).
  - ST-8 / CB-6: bootstrap now programs SMEP/SMAP/NXE correctly and safely (CPUID-gated to avoid unsupported-bit boot faults).
- **Boot-regression follow-up merged:** the post-hardening halt after serial marker `B` is addressed by CPUID-gating SMEP/SMAP/NXE writes before CR4/EFER updates in `pvh_start32`.
- **Unsupported ISA guardrails tightened:** architecture facade fallbacks now fail fast with `compile_error!` for unsupported `target_arch` values instead of silently selecting a RISC-V path.
- **Trap decode correctness improved:** unknown architecture trap codes are normalized as `TrapEvent::Unknown { arch_code }` instead of being coerced into external IRQ semantics.
- **Per-CPU TLS restore observability:** architecture trap paths now expose CPU-indexed TLS-restore slots and include isolation tests to verify CPU-local behavior.
- **External IRQ completion plumbing added:** external IRQ trap handling now saves/restores interrupt state around routing and calls an ISA-selected `external_irq_eoi` hook for controller completion handoff.
- **IRQ completion integration advanced:** x86 APIC / aarch64 GIC / riscv64 PLIC EOI backends perform register-level completion writes with selected-ISA dispatch.
- **IRQ safety hardening added:** controller MMIO EOI writes are configuration-gated, preventing accidental writes when controller state is not initialized.
- **Firmware-driven boot wiring added:** boot now accepts staged descriptions, hosted env (`YARM_IRQ_CONTROLLER_DESCRIPTION`, `YARM_IRQ_FIRMWARE_BLOB`), explicit firmware-blob API calls, and a non-hosted firmware-blob provider hook for early boot handoff.
- **x86_64 SMP AP startup wired in boot path:** a dedicated `arch::x86_64::smp` module now prepares a trampoline handoff page (`0x7000`), emits LAPIC INIT-SIPI-SIPI for present secondary CPUs, and then finalizes scheduler/topology online accounting through `KernelState::bring_up_cpu`; boot now emits `YARM_SMP_STARTUP` before `YARM_BOOT_OK`.
- **Remaining hardware integration work tracked:** implement concrete board/bootloader ACPI/DT extractors that feed the registered firmware-blob provider in production boot flows.

## In-kernel mechanism status

The kernel mechanism layer is now considered **complete for the current milestone**:

- **Type consistency in integration paths:** key boot internals now use typed identities (`ThreadId`) for driver records, endpoint waiters, and delegation routing.
- **Kernel-state decomposition:** `KernelState` is no longer a flat god-struct; mechanism data is split into subsystem state blocks (`IpcSubsystem`, `MemorySubsystem`, `DriverSubsystem`, `FaultSubsystem`, `RestartSubsystem`).
- **Trap/IPC/scheduler invariants:** targeted invariant tests cover preemption rotation, trap fault routing, restart backoff, and cross-CPU deferred-work behavior.
- **Mechanism-policy separation:** service-specific Linux process/VFS manager wiring is outside `KernelState`; kernel mechanisms remain service-agnostic.
- **Boot/runtime separation:** boot orchestration now lives under `kernel::boot`, while init policy lives in `services::init` and executable startup helpers live outside the kernel core.
- **ABI/contract freeze:** mechanism contracts are explicitly frozen and test-guarded across `process_abi`, `vfs_abi`, and related kernel interfaces.

## Completion criteria check

- **Mechanism API stability:** met for the current milestone scope.
- **Invariants encoded and tested:** met for core trap/IPC/scheduler/restart paths.
- **Policy separation:** met at kernel state boundary.
- **Test confidence:** broad module suite plus deterministic integration coverage in `tests/kernel_scenarios.rs` exercises init/process_manager/VFS/IRQ flows alongside invariants and adversarial boundary tests.
- **No known must-fix blockers in core mechanism paths:** none currently open in this branch.

## Next phase

With in-kernel mechanisms complete for this milestone, primary effort can now move to user-space components:

1. continue maturing `InitService` launch/mount orchestration and recovery policy,
2. harden the process-manager and VFS service surfaces around the frozen ABI modules,
3. expand driver server runtime and hardware adapters,
4. grow Linux personality coverage and compatibility conformance.
