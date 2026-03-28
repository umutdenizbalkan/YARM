# Kernel Status (Mechanism Layer Completion)

This snapshot reflects the current branch after the mechanism-hardening pass.

## Recent updates

- **Unsupported ISA guardrails tightened:** architecture facade fallbacks now fail fast with `compile_error!` for unsupported `target_arch` values instead of silently selecting a RISC-V path.
- **Trap decode correctness improved:** unknown architecture trap codes are normalized as `TrapEvent::Unknown { arch_code }` instead of being coerced into external IRQ semantics.
- **Per-CPU TLS restore observability:** architecture trap paths now expose CPU-indexed TLS-restore slots and include isolation tests to verify CPU-local behavior.
- **External IRQ completion plumbing added:** external IRQ trap handling now saves/restores interrupt state around routing and calls an ISA-selected `external_irq_eoi` hook for controller completion handoff.
- **IRQ completion integration advanced:** x86 APIC / aarch64 GIC / riscv64 PLIC EOI backends now perform register-level completion writes using selected-ISA platform-layout initialization at boot entry.
- **IRQ safety hardening added:** controller MMIO EOI writes are now explicitly configuration-gated, preventing accidental writes when controller state is not initialized.
- **Remaining hardware integration work tracked:** source controller MMIO/context values from ACPI/DT/platform firmware instead of static profile constants.

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
