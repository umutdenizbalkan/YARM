# YARM Step 5b: Architecture Boundary (RISC-V First)

This file documents the initial architecture split work for step 5b.

## Goals

- Keep `src/kernel/*` machine-neutral.
- Introduce architecture decoder layer under `src/arch/*`.
- Start with RISC-V trap decoding, add other ISAs later.
- Keep a thin HAL contract (`src/arch/hal.rs`) for only three kernel dependencies:
  - address-space switch
  - interrupt acknowledge/delivery
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
- HAL conformance unit coverage now includes two ISA-shaped mocks (RISC-V-like + x86-like) validating that kernel-facing expectations remain identical across architectures.

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
- Extend conformance coverage to ARM trap-context shape using the same HAL contract.


## VM layout boundary

- VM layout constants (page size, kernel split base, ASID width, static VM capacities) are now sourced from `src/arch/vm_layout.rs` and consumed by `kernel::vm` to keep architecture assumptions out of mechanism logic.
