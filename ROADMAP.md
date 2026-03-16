# YARM Microkernel Next-Step Checklist (Portable POSIX Direction)

This checklist focuses on turning the current in-memory kernel model into a portable, architecture-neutral microkernel core with a clear machine-adaptation boundary.

## 0) Ground Rules (keep these invariant)

- Keep kernel core logic free of external libraries/crates unless explicitly approved.
- Keep kernel policy and mechanisms architecture-neutral.
- Isolate machine-specific code behind a strict HAL/arch boundary (`arch/*` or equivalent), not mixed into core scheduler/IPC/capability/VM logic.
- Preserve static/bounded data structures where possible for determinism.
- Treat all user-space components uniformly as **servers** (`*.srv`); do not encode monolithic-kernel concepts in kernel object model.

## 1) IPC Fast Path + Scheduler Co-design

- Add synchronous IPC fast path that can directly switch sender->receiver when rendezvous preconditions hold.
- Keep IPC latency accounting in scheduler (`context switch + enqueue + wake` cycles) and track regressions.
- Add deterministic tests for fast-path vs queued-path behavior under contention.
- Ensure API semantics are explicit: endpoint primitive is bounded queue, rendezvous behavior is kernel scheduling policy.

## 2) Capability Delegation Chain (Init -> Server -> Server)

- Define an explicit delegation path from `init.srv` to service graph (`procman.srv`, `vfs.srv`, `usb.srv`, etc.).
- Keep kernel-side APIs mechanism-only: mint, transfer, revoke; policy remains in user-space supervisors.
- Standardize delegation bundles for hardware servers (IRQ + MMIO + IOVA window).
- Add tests for stale-cap rejection and delegation revocation behavior.

## 3) Thin HAL Portability Contract

- Kernel core should only depend on HAL primitives for:
  - address-space switch
  - interrupt acknowledge/delivery
  - timer programming
- Keep trap decoding in `arch/<isa>` and feed normalized `TrapEvent` to core.
- Add bring-up checklist for RISC-V, ARM, and x86 behind same HAL contract.

## 4) Process Manager + VFS Server Contracts

- Freeze typed request/reply payload codecs for process and VFS calls.
- Add deterministic mixed-flow tests (`getpid/openat/exit`) across server boundaries.
- Add mount routing and path-based dispatch abstractions in VFS server model.

## 5) Driver-as-Server Model Completion

- Keep kernel vocabulary object/capability-centric; no privileged "driver object" type.
- Represent hardware access as capabilities held by normal servers.
- Maintain docs/examples under `/srv` naming to keep mental model uniform.

## 6) Validation Strategy

- Keep exhaustive unit tests for state machines.
- Add property-style tests for capability and scheduler invariants.
- Add deterministic simulations (multi-task IPC + faults + interrupts + server IPC mix).
- Keep architecture contract tests that verify normalized trap events expected by core.


## 7) Chosen Runtime Target Direction (x86_64)

- Decision: adopt **`x86_64-unknown-none` + custom musl sysdeps shim** as the primary path.
- Rationale: better host/QEMU iteration on x86_64 while preserving microkernel-faithful runtime semantics (no Linux-hosted ABI dependency).
- Tracking checklist: `X86_64_NONE_MUSL_PORT_TODO.md`.

## 8) B-path bootstrap execution (started)

- Added target spec: `targets/x86_64-yarm-none.json`.
- Added cargo aliases for x86_64-none bring-up in `.cargo/config.toml`.
- Added x86-none build profile knobs in `Cargo.toml` and wired them into x86 artifact staging.
- Added x86_64 artifact and smoke scaffolds: `scripts/build-qemu-x86_64-artifacts.sh`, `scripts/qemu-x86_64-busybox-smoke.sh`.

## Immediate next 5 implementable steps

1. Wire synchronous IPC fast-path switching into measured scheduler path.
2. Add delegation-bundle helper APIs for hardware servers with stale-cap regression tests.
3. Freeze and document typed process/VFS server codecs with versioned structs.
4. Add minimal HAL trait conformance docs/tests for RISC-V and one additional ISA target.
5. Expand deterministic end-to-end server flow tests (procman + VFS + notification routing).


## init.srv scaffold status

- Initial boot-contract scaffold added: `INIT_SERVER_BOOT_CONTRACT.md`
- Initial implementation added: `src/kernel/init_server.rs` + demo `src/bin/init_server.rs`
