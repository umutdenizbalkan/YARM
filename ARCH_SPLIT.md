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

- `src/arch/mod.rs` with `hal` + `riscv` modules.
- `src/arch/hal.rs` trait boundary for minimal machine adaptation.
- `src/arch/riscv.rs` trap decoder:
  - maps RISC-V `scause`/`stval` into normalized kernel `TrapEvent`
  - converts user ecall -> `Trap::Syscall`
  - converts timer/external IRQ causes -> timer/external trap classes
  - converts load/store page faults -> `Trap::PageFault` with `FaultInfo`
- `KernelState::handle_trap_event` to consume normalized trap events from arch decoders.
- `arch::riscv::handle_trap_entry` to set current CPU, drain per-CPU deferred work, then route normalized trap events.

## Next

- Connect real arch trap entry stubs to call `decode_trap`.
- Route per-CPU timer + IPI interrupts into cross-CPU work handling.
- Add architecture-specific context switch/trapframe save/restore.
- Add second-ISA conformance for HAL trait (ARM or x86).
