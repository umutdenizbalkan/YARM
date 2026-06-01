<!-- SPDX-License-Identifier: Apache-2.0 -->

# HAL Conformance Notes (RISC-V + x86 + AArch64 baseline)

This note freezes the minimum HAL portability checks expected by kernel-core code.

## Contract surface

Kernel core may depend on only these HAL primitives:

1. address-space switch
2. interrupt acknowledge/delivery handoff
3. external-interrupt completion (EOI/claim-complete)
4. timer programming
5. normalized trap event decode to `TrapEvent`

## Baseline ISA conformance targets

- RISC-V-like profile
- x86-like profile
- AArch64-like profile

All three profiles must satisfy identical kernel-facing semantics for:

- ASID switch observability
- IRQ acknowledge path observability
- external IRQ completion observability
- timer deadline programming observability
- trap decode into normalized events (`syscall` / `page_fault`)

## Test anchors

- `src/arch/hal.rs`
  - `hal_contract_is_isa_agnostic_for_riscv_like_impl`
  - `hal_contract_is_isa_agnostic_for_x86_like_impl`
  - `hal_contract_is_isa_agnostic_for_aarch64_like_impl`
- `src/arch/irq_guard.rs`
  - selected-ISA `external_irq_eoi(irq_line)` dispatch
- `src/kernel/boot/fault_state.rs`
  - external interrupt flow saves/restores IRQ state and calls arch EOI hook
- `src/arch/riscv64/trap.rs`
  - trap entry routes through normalized kernel trap handling
- `src/arch/x86_64/trap.rs`
  - trap entry routes through normalized kernel trap handling
- `src/arch/aarch64/trap.rs`
  - trap entry routes through normalized kernel trap handling
- `scripts/check-hal-conformance-targets.sh`
  - CI gate running baseline HAL + topology conformance tests across riscv64/x86_64/aarch64

## Current implementation note

- Architecture EOI hooks exist for `x86_64`, `riscv64`, and `aarch64`, and now issue register-level completion writes after boot-time platform-layout initialization.
- EOI backends are configuration-gated and no-op until initialized, avoiding accidental MMIO writes from implicit defaults.
- Hosted-dev boot supports description-driven controller initialization via `YARM_IRQ_CONTROLLER_DESCRIPTION` with automatic fallback to platform layout defaults.
- Hosted-dev boot additionally supports firmware-blob input via `YARM_IRQ_FIRMWARE_BLOB`, canonicalized by selected-ISA topology helpers before controller configuration.
- Boot entry now provides an explicit description-injection API (`run_kernel_boot_with_irq_description`) so firmware handoff can bypass env-var plumbing and configure controllers directly.
- Boot entry also provides `run_kernel_boot_with_firmware_blob(...)` for explicit firmware-blob canonicalization + configuration without env vars.
- Boot entry now also routes top-level kernel startup through selected-ISA boot hooks (`run_with_prepared_kernel`, `prepare_arch_boot`, `emit_panic`) to keep `src/bin/kernel_boot.rs` architecture-neutral.
- First user-task bootstrap/entry handoff is likewise routed through `arch::boot_entry` (`bootstrap_first_user_task`, `enter_dispatched_user_task_if_available`) with ISA-specific implementations in `src/arch/<isa>/boot.rs`.
- Boot entry additionally supports one-shot staged description handoff (`stage_irq_controller_description_for_boot`) for early-boot contexts where direct run-hook plumbing is inconvenient.
- Staged boot-description copy/read paths are now protected by a boot-entry lock guard to avoid concurrent staging races.
- Non-hosted early boot can now register a firmware-blob provider (`set_firmware_blob_provider_for_boot`) that run-boot consumes before env/platform fallbacks.
- A firmware-blob staging helper now stages canonical per-ISA IRQ controller descriptions derived by selected-ISA topology discovery (`discover_irq_controller_description`), reducing malformed handoff risk.
- Remaining integration work is implementing concrete platform ACPI/DT extraction producers that feed the registered provider in production boot environments.

## Platform layout profile status

`PROFILE_IS_PLACEHOLDER` on `src/arch/<isa>/platform_layout.rs` describes
whether that ISA's **platform-layout constants** are placeholders for the
currently supported QEMU smoke target. It does not claim generic hardware
discovery or production-board coverage. Current status:

| ISA | Current smoke target | `PROFILE_IS_PLACEHOLDER` | Hardcoded/static anchors | Remaining non-generic work |
|-----|----------------------|--------------------------|--------------------------|----------------------------|
| AArch64 | QEMU `virt`, `cortex-a72`, 1 GiB RAM default | `false` | RAM/kernel bootstrap anchors `0x4000_0000`/`0x4008_0000`, allocator floor `0x5000_0000`, GICv2 CPU interface fallback `0x0801_0000`, timer tick budget. | DTB is parsed for RAM/initrd/CPU bitmap/PSCI/GIC handoff, but non-QEMU boards still need full platform description handoff instead of relying on these fallback anchors. |
| x86_64 | QEMU `q35` PVH, `qemu64`, 512 MiB RAM default | `false` | Higher-half/direct-map bootstrap aliases, kernel link base, allocator floor `0x1000_0000`, PC-compatible LAPIC/IOAPIC physical MMIO addresses and aliases, timer tick budget. | PVH memmap supplies usable RAM, but ACPI/MP-table driven interrupt topology discovery remains future work for non-QEMU/non-PC-compatible targets. |
| RISC-V 64 | QEMU `virt`/OpenSBI, `rv64`, 512 MiB RAM default | `false` | Bootstrap VA/PA anchors, allocator floor `0x1000_0000`, PLIC base `0x0c00_0000`, S-mode context index `1`, timer tick budget. | Firmware-table/device-tree driven memory and interrupt topology discovery remains future work beyond the current QEMU `virt` profile. |

## Invariants

- syscall/trap arg register count must remain aligned across selected ISA ABI profiles for core syscall paths.
- IPC register-lane width must stay compatible with core syscall ABI assertions.
- per-ISA platform layout constants may differ, but kernel core must consume only `crate::arch::{platform_layout, syscall_abi, vm_layout}` re-exports.
