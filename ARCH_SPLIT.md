# YARM Step 5b: Architecture Boundary (RISC-V First)

This file documents the initial architecture split work for step 5b.

## Goals

- Keep `src/kernel/*` machine-neutral.
- Introduce architecture decoder layer under `src/arch/*`.
- Start with RISC-V trap decoding, add other ISAs later.
- Keep a thin HAL contract (`src/arch/hal.rs`) for only kernel dependencies:
  - address-space switch
  - interrupt acknowledge/delivery
  - external-interrupt completion (EOI/claim-complete)
  - timer programming

## Implemented

- `src/arch/mod.rs` with `hal` + `riscv` + `vm_layout` modules.
- `src/arch/hal.rs` trait boundary for minimal machine adaptation.
- `src/arch/riscv.rs` trap decoder:
  - maps RISC-V `scause`/`stval` into normalized kernel `TrapEvent`
  - converts user ecall -> `Trap::Syscall`
  - converts timer/external IRQ causes -> timer/external trap classes
  - converts load/store page faults -> `Trap::PageFault` with `FaultInfo`
- `KernelState::handle_trap_event` to consume normalized trap events from arch decoders.
- `arch::riscv::handle_trap_entry` to set current CPU, drain per-CPU deferred work, then route normalized trap events.
- HAL conformance unit coverage now includes three ISA-shaped mocks (RISC-V-like + x86-like + AArch64-like) validating that kernel-facing expectations remain identical across architectures.

## HAL conformance checklist (RISC-V + x86 baseline)

- [x] `switch_address_space` exercised with architecture-specific context wrapper.
- [x] `acknowledge_interrupt` called with `(CpuId, irq_line)` pair.
- [x] `program_timer_deadline` called with `(CpuId, ticks_from_now)`.
- [x] `decode_trap_event` normalizes ISA-specific trap context into shared `TrapEvent`.
- [x] Non-syscall trap path carries fault metadata (`FaultInfo`) through normalized event.

## Next

- Connect real arch trap entry stubs to call `decode_trap`.
- Route per-CPU timer + IPI interrupts into cross-CPU work handling.
- Add architecture-specific context switch/trapframe save/restore.
- [x] Extend conformance coverage to ARM trap-context shape using the same HAL contract.


## VM layout boundary

- VM layout constants (page size, kernel split base, ASID width, static VM capacities) are now sourced from `src/arch/vm_layout.rs` and consumed by `kernel::vm` to keep architecture assumptions out of mechanism logic.


## Per-ISA layout module rule (implemented)

Architecture/address-space constants are now selected through per-ISA modules instead of scattered `#[cfg]` constants:

- `src/arch/riscv64/{vm_layout,platform_layout,syscall_abi}.rs`
- `src/arch/x86_64/{vm_layout,platform_layout,syscall_abi}.rs`
- `src/arch/aarch64/{vm_layout,platform_layout,syscall_abi}.rs`
- `src/arch/mod.rs` selects one ISA module and re-exports selected views via:
  - `src/arch/vm_layout.rs`
  - `src/arch/platform_layout.rs`
  - `src/arch/syscall_abi.rs`

Kernel code consumes only the selected re-export modules (`crate::arch::{vm_layout,platform_layout,syscall_abi}`), which keeps mechanism code architecture-agnostic.


## Boundary enforcement in CI

- `scripts/check-kernel-arch-boundary.sh` rejects direct architecture-shape constants in `src/kernel/*` for selected migrated fields (CPU/IRQ sizing, trap arg width, IPC register lanes, bootstrap VA/PA seed constants).


## Trap-entry plumbing status

- Added trap decode + trap entry handlers for all three ISA bring-up paths:
  - `src/arch/riscv64/trap.rs`
  - `src/arch/x86_64/trap.rs`
  - `src/arch/aarch64/trap.rs`
- Added selected-ISA trap dispatch facade `src/arch/trap_entry.rs` so kernel integration can call one arch-selected entrypoint shape.
- Unknown ISA trap/exception codes now decode to `TrapEvent::Unknown { arch_code }` for explicit fault-policy handling instead of being folded into synthetic IRQ values.
- Trap restore diagnostics now track last-restored TLS base per CPU (`MAX_CPUS`-indexed slots), with per-ISA tests that assert CPU-local slot isolation.
- External interrupt handling now routes through `KernelState::handle_trap_event(...)` with explicit IRQ save/restore and selected-ISA EOI hook (`arch::irq_guard::external_irq_eoi`).
- x86 trap entry includes an explicit note that IDT + assembly entry stubs are still required for real hardware delivery into `handle_trap_entry`.

## External IRQ completion status

- Selected-ISA EOI plumbing is present:
  - `src/arch/irq_guard.rs` facade
  - `src/arch/{x86_64,riscv64,aarch64}/irq.rs` `external_irq_eoi(...)` hooks
- Current ISA hooks now perform register-level completion writes and are initialized from selected-ISA `platform_layout` constants during boot entry.
- IRQ backends now gate EOI writes on explicit controller configuration, so unconfigured paths no longer write using synthetic implicit defaults.
- Hosted-dev boot now attempts `YARM_IRQ_CONTROLLER_DESCRIPTION` parsing first, then falls back to selected-ISA `platform_layout` constants if absent/invalid.
- `arch::boot_entry` now also exposes `run_kernel_boot_with_irq_description(...)`, allowing non-hosted callers to pass an explicit firmware-derived controller description blob.
- `arch::boot_entry::stage_irq_controller_description_for_boot(...)` is now available for early-boot firmware handoff before `run_kernel_boot(...)`; staged descriptions are consumed once at boot.
- `arch::selected_isa::topology::discover_irq_controller_description(...)` now derives canonical IRQ-controller description strings from firmware blobs (including common alias keys), and `stage_irq_controller_description_from_firmware_blob(...)` stages that canonical output for boot.
- Remaining work is platform discovery/DT/ACPI handoff so runtime controller addresses/contexts come from hardware description instead of profile constants.


## Runtime entry wiring

- `KernelState::handle_selected_arch_trap_entry(...)` now forwards trap handling through `crate::arch::trap_entry::handle_trap_entry(...)`, so runtime integration can use the selected-ISA facade from kernel state.


## Unsupported-architecture policy

- The architecture facade now fails loudly on unsupported targets:
  - `src/arch/mod.rs` emits `compile_error!` when no supported ISA cfg matches.
  - `src/arch/irq_guard.rs` emits `compile_error!` for unsupported `target_arch`.
  - `src/arch/trap_entry.rs` emits `compile_error!` for unsupported `target_arch`.
- This replaces the prior silent fallback behavior that could otherwise produce
  incorrect binaries by compiling unsupported targets against RISC-V paths.


## Boot entry migration status

- `src/bin/kernel_boot.rs` is kept ISA-agnostic and delegates boot wiring via `src/arch/boot_entry.rs`.
- ISA-specific boot assembly/symbols live under `src/arch/<isa>/boot.rs` (`x86_64`, `riscv64`, `aarch64`).
- `scripts/check-kernel-arch-boundary.sh` also enforces a bin-layer leakage rule for `src/bin/kernel_boot.rs` (no `global_asm!`, no ISA cfg tags, no direct x86 kernel-entry symbol).
