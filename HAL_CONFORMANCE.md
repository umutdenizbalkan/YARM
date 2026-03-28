# HAL Conformance Notes (RISC-V + x86 baseline)

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

Both profiles must satisfy identical kernel-facing semantics for:

- ASID switch observability
- IRQ acknowledge path observability
- external IRQ completion observability
- timer deadline programming observability
- trap decode into normalized events (`syscall` / `page_fault`)

## Test anchors

- `src/arch/hal.rs`
  - `hal_contract_is_isa_agnostic_for_riscv_like_impl`
  - `hal_contract_is_isa_agnostic_for_x86_like_impl`
- `src/arch/irq_guard.rs`
  - selected-ISA `external_irq_eoi(irq_line)` dispatch
- `src/kernel/boot/fault_state.rs`
  - external interrupt flow saves/restores IRQ state and calls arch EOI hook
- `src/arch/riscv64/trap.rs`
  - trap entry routes through normalized kernel trap handling
- `src/arch/x86_64/trap.rs`
  - trap entry routes through normalized kernel trap handling

## Current implementation note

- Architecture EOI hooks exist for `x86_64`, `riscv64`, and `aarch64`, and now issue register-level completion writes after boot-time platform-layout initialization.
- EOI backends are configuration-gated and no-op until initialized, avoiding accidental MMIO writes from implicit defaults.
- Hosted-dev boot supports description-driven controller initialization via `YARM_IRQ_CONTROLLER_DESCRIPTION` with automatic fallback to platform layout defaults.
- Boot entry now provides an explicit description-injection API (`run_kernel_boot_with_irq_description`) so firmware handoff can bypass env-var plumbing and configure controllers directly.
- Remaining integration work is feeding controller addresses/contexts from hardware discovery (ACPI/DT) rather than static platform profile constants.

## Invariants

- syscall/trap arg register count must remain aligned across selected ISA ABI profiles for core syscall paths.
- IPC register-lane width must stay compatible with core syscall ABI assertions.
- per-ISA platform layout constants may differ, but kernel core must consume only `crate::arch::{platform_layout, syscall_abi, vm_layout}` re-exports.
