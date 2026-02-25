# YARM Step 5b: Architecture Boundary (RISC-V First)

This file documents the initial architecture split work for step 5b.

## Goals

- Keep `src/kernel/*` machine-neutral.
- Introduce architecture decoder layer under `src/arch/*`.
- Start with RISC-V trap decoding, add other ISAs later.

## Implemented

- `src/arch/mod.rs` with `riscv` module.
- `src/arch/riscv.rs` trap decoder:
  - maps RISC-V `scause`/`stval` into normalized kernel `TrapEvent`
  - converts user ecall -> `Trap::Syscall`
  - converts timer/external IRQ causes -> timer/external trap classes
  - converts load/store page faults -> `Trap::PageFault` with `FaultInfo`
- `KernelState::handle_trap_event` to consume normalized trap events from arch decoders.

## Next

- Connect real arch trap entry stubs to call `decode_trap`.
- Route per-CPU timer + IPI interrupts into cross-CPU work handling.
- Add architecture-specific context switch/trapframe save/restore.
