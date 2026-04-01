# ABI / Contract Freeze (Mechanism Layer)

This file marks the current in-kernel mechanism contracts as intentionally stable for the next implementation phase.

## Frozen contracts

1. **Linux-compat syscall dispatch table order and membership**
   - Source of truth: `LinuxCompatSyscall::DISPATCH_TABLE` in `src/linux_compat/mod.rs`.
   - Guarded by test: `linux_dispatch_table_is_frozen_contract`.

2. **Trap routing surface**
   - `TrapEvent` constructors and `route_trap(TrapEvent) -> TrapAction` in `src/arch/trap.rs` (`src/kernel/trap.rs` is a compatibility re-export shim).
   - Contract: one canonical entry event, explicit payload (`fault`/`irq`) and deterministic routing.

3. **Trap frame ABI encoding**
   - `#[repr(C)] TrapFrame` in `src/kernel/trapframe.rs`.
   - Contract: success iff `error == 0`; errors clear return registers.

4. **Timer preemption semantics**
   - `Timer::tick_and_check` and `Timer::should_preempt` in `src/kernel/timer.rs`.
   - Contract: at most one preempt decision per quantum boundary tick.

5. **Restart/fault contracts at bootstrap boundary**
   - `KernelState::restart_task`, `KernelState::exit_task`, `KernelState::handle_trap_event` in `src/kernel/boot/mod.rs`.
   - Contract: restart backoff/budget/token checks are enforced before making task runnable.

## Change process

Any intentional ABI/contract change must:

- Update this document and the corresponding module docs.
- Add/adjust tests that assert the new contract.
- Include migration notes if userspace-visible behavior changes.


## Compatibility gates in CI semantics

- Mechanism/core profile must pass: `cargo test`
- Linux personality profile must pass: `cargo test --features linux-compat`
- CI workflow source: `.github/workflows/compat-gates.yml`
- Typed codec golden vectors and truncation-rejection tests are required gates for wire-compatibility changes.
- No binary fixture blobs in-tree for compatibility gates (goldens should be source-level constants).
