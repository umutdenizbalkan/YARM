<!-- SPDX-License-Identifier: Apache-2.0 -->

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

- Define an explicit delegation path from `init.srv` to service graph (`process_manager.srv`, `vfs.srv`, `usb.srv`, etc.).
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
- ✅ Added codec freeze artifact and exact-length decode enforcement for `ProcV2Args`/`VfsV1Args`: `PROC_VFS_CODEC_FREEZE.md`.

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
- Added x86_64 artifact and smoke scaffolds: `scripts/build-qemu-x86_64-artifacts.sh`, `scripts/qemu-x86_64-core-smoke.sh`.
- For x86 QEMU bring-up, the first blocker is producing a direct-bootable kernel artifact (PVH-enabled ELF or `bzImage`); immediate validation target is serial success markers (`YARM_BOOT_OK`, `YARM_PROC_VFS_OK`, `YARM_INIT_START`, `YARM_INIT_DONE`) once kernel entry is working.
- Added freestanding build bootstrap helper script: `scripts/build-x86_64-none-bootstrap.sh` (checks nightly + rust-src before running `-Z build-std`).
- Added network/mirror bootstrap wrapper: `scripts/bootstrap-nightly-mirror.sh` (installs nightly + rust-src from configured Rust dist/update endpoints, then runs the freestanding bootstrap build).
- Added Linux-compat sysdeps bootstrap module: `src/services/compatibility/linux_compat/sysdeps.rs` (startup + memory contract + clock stub).
- Expanded sysdeps shim scaffolding with startup/memory/clock/thread/futex hooks and focused tests (bootstrap-grade semantics).
- Expanded deterministic end-to-end server coverage with `tests/kernel_scenarios.rs`, including the init/process_manager/VFS/IRQ flow exercised by `run_init_core_bootstrap_scenario()`.

## 9) Shared-memory IPC fast-path implementation track

- Stepwise execution plan: `IPC_SHARED_MEMORY_FASTPATH_PLAN.md`.
- Scope: receiver auto-map path, lifecycle/refcount semantics, revocation behavior, and throughput tuning for FS/network/display data-plane traffic.
- Post-fastpath hardening gate: `scripts/phase7-shared-ipc-gates.sh` + migration/runtime guides (`SHARED_IPC_MIGRATION_GUIDE.md`, `SHARED_IPC_THROUGHPUT_GUIDE.md`).

## Immediate next 5 implementable steps

1. Wire synchronous IPC fast-path switching into measured scheduler path.
2. Add delegation-bundle helper APIs for hardware servers with stale-cap regression tests.
3. Freeze and document typed process/VFS server codecs with versioned structs.
4. Add minimal HAL trait conformance docs/tests for RISC-V and one additional ISA target.
   - ✅ Added HAL conformance note for RISC-V + x86 baseline: `HAL_CONFORMANCE.md`.
5. Expand deterministic end-to-end server flow tests (process_manager + VFS + notification routing).
   - ✅ Added deterministic end-to-end scenario replay coverage in `tests/kernel_scenarios.rs`, including process-manager/VFS opcode flow checks and repeated IRQ notification routing checks.

Progress notes:
- ✅ Added IPC fastpath-vs-queued-vs-blocked telemetry tests under contention in `kernel::boot` tests.
- ✅ Added restart/redelegation stale-cap regression for checked driver delegation bundles.
- ✅ Wired synchronous IPC fast-path switching into measured scheduler telemetry (`scheduler_*` counters) with regression assertions in `kernel::boot` fast-path tests.
- ✅ Added delegation-bundle helper APIs (`delegate_driver_bundle_checked`, `redelegate_driver_bundle`) and stale-cap regression coverage for helper-driven redelegation paths.
- ✅ Extended typed process/VFS codec freeze with stable golden vectors + CI enforcement script (`scripts/check-proc-vfs-codec-freeze.sh`).
- ✅ Added extra HAL conformance targets (AArch64 baseline included) with CI gate script `scripts/check-hal-conformance-targets.sh`.
- ✅ Expanded deterministic end-to-end server flow test coverage for process-manager + VFS + notification routing with replay-stability assertions.

## Review follow-up next steps (after oversized placeholder PR)

1. **Split architecture work into reviewable PR slices**
   - PR A: arch module split + HAL adapter shims only.
     - ✅ Added `src/arch/hal_adapters.rs` and routed `SelectedIsaHal` through adapter shims to keep HAL call sites ISA-facade-only.
   - PR B: syscall/trap normalization only.
     - ✅ Moved normalized trap types/routing into `src/arch/trap.rs` and switched ISA trap decoders to consume arch-layer trap normalization directly.
   - PR C: platform layout constants + docs only.
     - ✅ Added `src/arch/platform_constants.rs` and moved kernel call sites to the platform-constants facade (`crate::arch::platform_constants`) instead of direct `platform_layout` constant use.
2. **Tighten CI to block placeholder/overscoped submissions**
   - add commit/PR lint that rejects placeholder PR bodies and change sets spanning unrelated domains.
   - require `compat-gates` and at least one architecture smoke job on every architecture-touching PR.
   - ✅ Added `scripts/check-pr-scope-and-message.sh` to reject placeholder/WIP commit markers and fail overscoped arch+kernel+services diffs by default.
   - ✅ Added `scripts/check-ci-workflow-enforcement.sh` and wired it into `compat-gates.yml` to enforce `compat-gates` profile jobs and core QEMU smoke workflow presence on pull requests.
3. **Promote contract docs from draft to enforced gates**
   - pin `ABI_CONTRACT_FREEZE.md`, `SYSCALL_ABI.md`, and `PROC_VFS_CODEC_FREEZE.md` as CI-checked reference artifacts.
   - ✅ Added `scripts/check-contract-doc-enforcement.sh` to enforce required freeze-doc markers and run targeted frozen-contract tests (`trap_router_maps_syscall`, `proc_v2_golden_vector_is_stable`, `vfs_v1_golden_vector_is_stable`).
4. **Reduce bootstrap script drift**
   - consolidate x86/riscv qemu build/smoke scripts behind a shared helper to avoid duplicated maintenance.
   - ✅ Added shared smoke helper `scripts/qemu-smoke-common.sh` and refactored riscv64/aarch64 smoke scripts to consume it for common file/QEMU checks, timeout logging, and marker validation.
5. **Define a minimum runnable server profile**
   - ship a single "core profile" path (`init/process_manager/vfs/supervisor + devfs + one FS`) and keep it green before expanding feature breadth.
   - ✅ The hosted smoke/runtime path now exercises `init/process_manager/vfs/supervisor + devfs + initramfs` through `run_minimum_profile_with_kernel(...)` and `src/bin/core_profile_smoke.rs`.


## init.srv scaffold status

- Initial boot-contract scaffold added: `INIT_SERVER_BOOT_CONTRACT.md`
- Initial implementation added: `src/services/init/mod.rs` + demo `src/bin/init_server.rs`
