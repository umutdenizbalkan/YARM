<!-- SPDX-License-Identifier: Apache-2.0 -->

# Kernel Status (Mechanism Layer Completion)

This snapshot reflects the current branch after the mechanism-hardening pass.

## Recent updates

- **PR-B migration/polish pass closed on this branch:** service-side decode and control-plane call/reply helpers were fully consolidated onto shared workspace utilities:
  - VFS reply decode ladders in `devfs` / `initramfs` / `ramfs` now use `yarm_srv_common::vfs_reply::VfsReply::{from_opcode_payload, as_u64}`.
  - POSIX service hooks now use consistent decode/error-mapping paths (`decode_message_u64`, `decode_vfs_u64`, `decode_vfs_fd_i32`) with shared `VfsReply`.
  - `services::control_plane::ipc_roundtrip::roundtrip_call_reply_with_budget` now centralizes reply-cap attach + timed call/reply choreography and is used by control-plane `vfs` and `process_manager`.
  - Remaining compatibility decode shims (`VfsReply::from_message(...)`) in `services::init`, `services::fs::ext4`, and `services::fs::fat` were retired in favor of `yarm-srv-common` decode helpers.
- **Stability tracker ST-1..ST-8 closed on this branch:** the previously-blocking bootstrap/runtime items are implemented and integrated:
  - ST-1 / CB-1: single-kernel boot flow (no dual `Bootstrap::init()` split between boot markers and init smoke path).
  - ST-2 / CB-3: x86_64 IDT assembly trap stub table with common GPR save/restore dispatch glue.
  - ST-3 / CB-2: canonical higher-half x86_64 split (`KERNEL_SPACE_BASE = 0xFFFF_8000_0000_0000`) with dependent constant/linker updates.
  - ST-4 / H-3: ELF64 parser now validates header/program-header structure and `PT_LOAD` bounds instead of magic+entry-only parsing.
  - ST-5 / CB-4: `syscall/sysret` fast-path wired (MSR programming + LSTAR entry), with `int 0x80` compatibility path retained.
  - ST-6 / MISS-1: demand paging path added for qualifying user page faults (heap/brk and bounded stack-growth windows).
  - ST-7 / CB-5: external IRQ decode range widened to platform-configured IRQ line count (64 lines currently).
  - ST-8 / CB-6: bootstrap now programs SMEP/SMAP/NXE correctly and safely (CPUID-gated to avoid unsupported-bit boot faults).
- **Boot-regression follow-up merged:** the post-hardening halt after serial marker `B` is addressed by CPUID-gating SMEP/SMAP/NXE writes before CR4/EFER updates in `pvh_start32`.
- **Unsupported ISA guardrails tightened:** architecture facade fallbacks now fail fast with `compile_error!` for unsupported `target_arch` values instead of silently selecting a RISC-V path.
- **PR-BND-3 pass A (IPC extraction stabilization) landed:** workspace now includes `yarm-kernel`; IPC core mechanism types (`ThreadId`, `TransferCapId`, `Message`, `SharedMemoryRegion`, `IpcError`) are owned by `crates/yarm-kernel/src/ipc.rs`, while `src/kernel/ipc.rs` retains endpoint queue mechanics and ISA register-lane pack/unpack wrappers via re-export bridge.
- **PR-BND-3 pass B (capability/scheduler type extraction) landed:** capability identity/rights/error core types and scheduler identity/priority/error types are now owned by `crates/yarm-kernel` (`capability.rs`, `scheduler.rs`), with in-tree `src/kernel/capabilities.rs` and `src/kernel/scheduler.rs` consuming them via re-export bridges plus parity tests.
- **PR-BND-3 pass C (boot telemetry/capacity bridge extraction) landed:** boot-facing IPC telemetry/capacity profile/config mechanism types now live in `crates/yarm-kernel/src/boot.rs`, while `src/kernel/boot/types.rs` re-exports them and retains boot-runtime glue types that still depend on in-tree kernel modules.
- **PR-BND-3 pass D (bridge cleanup/lock) landed:** extraction bridges are now consolidated under kernel-level parity/bridge tests (`src/kernel/extraction_bridge_tests.rs`) covering IPC/capability/scheduler/boot type families; stale `VfsReply::as_fd` compatibility shim was removed in favor of strict `expect_fd`.
- **PR-BND-4 first server-crate slice landed:** control-plane server entrypoints (`init_server`, `process_manager`, `vfs_server`) moved from root package `src/bin/*` into dedicated workspace package `crates/yarm-control-plane-servers`.
- **PR-BND-4 second server-crate slice landed:** filesystem server entrypoints (`devfs_srv`, `ramfs_srv`, `initramfs_srv`, `ext4_srv`, `fat_srv`) moved from root package `src/bin/*` into dedicated workspace package `crates/yarm-fs-servers`.
- **PR-BND-4 third server-crate slice landed:** network server entrypoints (`dhcp_srv`, `dns_srv`, `netmgr_srv`, `socket_srv`, `tcpip_srv`) moved from root package `src/bin/*` into dedicated workspace package `crates/yarm-network-servers`.
- **PR-BND-4 fourth server-crate slice landed:** driver/UI/compat server entrypoints were extracted from root package `src/bin/*` into dedicated workspace packages:
  - `crates/yarm-driver-servers` (`blkcache_srv`, `console_driver`, `input_srv`, `irqmux_srv`, `uart_srv`, `virtio_blk_srv`, `virtio_gpu_srv`, `virtio_net_srv`)
  - `crates/yarm-ui-servers` (`compositor_srv`, `display_srv`, `shell_srv`)
  - `crates/yarm-compat-servers` (`supervisor_srv`, `posix_compat_srv`)
- **PR-BND-4 fifth server-crate slice landed:** remaining hosted service bins were removed from root package ownership:
  - `console_driver` moved into `crates/yarm-driver-servers`
  - `driver_manager` moved into `crates/yarm-control-plane-servers`
  - root package ownership reduced to non-server boot/smoke bins.
- **PR-BND-4 sixth extraction slice landed:** `core_profile_smoke` moved into a dedicated hosted runtime tooling crate `crates/yarm-runtime-tools`, leaving root package bin ownership kernel-only (`kernel_boot`).
- **PR-BND-4 pass E (server dependency rewiring bridge) landed:** extracted server bins now depend on `crates/yarm-server-runtime` wrappers instead of depending on root `yarm` directly, reducing direct monolith visibility from server entrypoints while keeping runtime behavior unchanged.
- **PR-BND-4 pass F (rewiring closeout gate + doc alignment) landed:** added `scripts/check-server-crate-deps.sh` to enforce extracted server crates depend on `yarm-server-runtime` (not root `yarm`), and recorded PR-BND-4 as complete with remaining milestone focus moved to PR-BND-5/6.
- **PR-BND-5 pass A started (crate-graph type gate):** added `scripts/check-crate-graph-boundary.py` using `cargo metadata` to enforce structural dependency edges (`server crates -> yarm-server-runtime`, no direct `server crates -> yarm`/`yarm-kernel`, and no root `yarm -> server crate` back-edge).
- **PR-BND-5 pass B landed (CI integration):** added `scripts/phase5-boundary-gates.sh` and wired `phase5-boundary-gates` job in `.github/workflows/compat-gates.yml` to run crate-graph + dependency + boundary scripts and extracted server compile checks.
- **PR-BND-5 pass C landed (required-gate promotion):** promoted `phase5-boundary-gates` to required readiness/workflow enforcement by wiring the token into `scripts/check-ci-workflow-enforcement.sh`, `scripts/check-roadmap-readiness.sh`, and `PHASE_READINESS_MATRIX.md`.
- **PR-BND-6 pass A started (cleanup of transitional boundary checks):** retired redundant `scripts/check-server-crate-deps.sh`; `scripts/check-crate-graph-boundary.py` is now the single structural dependency-edge gate used by `scripts/phase5-boundary-gates.sh`.
- **Trap decode correctness improved:** unknown architecture trap codes are normalized as `TrapEvent::Unknown { arch_code }` instead of being coerced into external IRQ semantics.
- **Per-CPU TLS restore observability:** architecture trap paths now expose CPU-indexed TLS-restore slots and include isolation tests to verify CPU-local behavior.
- **External IRQ completion plumbing added:** external IRQ trap handling now saves/restores interrupt state around routing and calls an ISA-selected `external_irq_eoi` hook for controller completion handoff.
- **IRQ completion integration advanced:** x86 APIC / aarch64 GIC / riscv64 PLIC EOI backends perform register-level completion writes with selected-ISA dispatch.
- **IRQ safety hardening added:** controller MMIO EOI writes are configuration-gated, preventing accidental writes when controller state is not initialized.
- **Firmware-driven boot wiring added:** boot now accepts staged descriptions, hosted env (`YARM_IRQ_CONTROLLER_DESCRIPTION`, `YARM_IRQ_FIRMWARE_BLOB`), explicit firmware-blob API calls, and a non-hosted firmware-blob provider hook for early boot handoff.
- **x86_64 SMP AP startup wired in boot path:** a dedicated `arch::x86_64::smp` module now prepares a trampoline handoff page (`0x7000`), emits LAPIC INIT-SIPI-SIPI for present secondary CPUs, and then finalizes scheduler/topology online accounting through `KernelState::bring_up_cpu`; boot now emits `YARM_SMP_STARTUP` before `YARM_BOOT_OK`.
- **Boot flow refactor scaffolded:** `kernel_boot` no longer runs the in-kernel init/process/VFS smoke bootstrap path directly; after `run_boot_markers` it now enters a scheduler-loop handoff scaffold (`YARM_SCHED_LOOP_START`) as a stepping stone toward loading `init_server` from initramfs in a separate user address space.
- **Remaining hardware integration work tracked:** implement concrete board/bootloader ACPI/DT extractors that feed the registered firmware-blob provider in production boot flows.

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
2. harden the process-manager and VFS service surfaces around the frozen ABI modules and shared roundtrip/decode helpers,
3. expand driver server runtime and hardware adapters,
4. grow Linux personality coverage and compatibility conformance,
5. complete crate split to a strict `yarm-kernel` + service crates boundary (remove residual single-crate visibility risk).

## Proposed PR sequence (remaining boundary work)

The following PR plan breaks down the remaining `shared-helper` hardening and `yarm-kernel` extraction work into mergeable slices.

### PR-1: Shared-helper hardening baseline

- Expand `yarm-srv-common` usage inventory and remove any remaining service-local decode/roundtrip helper duplication.
- Add stricter helper contracts (typed error categories, canonical timeout/retry behavior, and reply payload invariants).
- Add unit tests in helper crates for malformed payloads, invalid opcodes, and capability-attach edge cases.

### PR-2: Shared-helper adoption completion across services

- Migrate all services to the hardened helper APIs (including init/process-manager/VFS/filesystem adapters).
- Remove legacy compatibility helper shims from service modules once all call sites are migrated.
- Add integration coverage to ensure cross-service call/reply behavior stays ABI-consistent.

### PR-3: Create `yarm-kernel` crate skeleton and move kernel-only modules

- Introduce a new workspace crate `yarm-kernel` with kernel-owned modules (scheduler, trap routing, memory mechanisms, capability checks, IPC transport internals).
- Re-export only stable kernel-facing mechanism interfaces required by existing binaries/tests.
- Keep behavior identical (no semantic policy changes) while relocating code.

### PR-4: Server crate extraction and dependency rewiring

- Split service/runtime code into server-oriented crates (`init`, `process_manager`, `vfs`, fs servers, posix personality) or grouped workspace crates as appropriate.
- Make services depend on `yarm-ipc-abi` and `yarm-srv-common`; disallow direct dependency on kernel-internal modules.
- Move bin entrypoints to depend on the extracted crates.

### PR-5: Type-enforced boundary gate in CI

- Replace (or complement) grep-based boundary checks with compile-time enforcement:
  - service crates cannot import private `yarm-kernel` internals by construction,
  - visibility boundaries are validated by crate graph + Rust privacy.
- Keep `scripts/check-service-arch-boundary.sh` as transitional defense until crate extraction is complete, then simplify/remove.

### PR-6: Final cleanup + documentation lock

- Delete dead compatibility code and stale module paths left from the extraction.
- Refresh architecture docs and developer onboarding for the new crate layout.
- Update status/contract docs and mark boundary enforcement milestone complete.
