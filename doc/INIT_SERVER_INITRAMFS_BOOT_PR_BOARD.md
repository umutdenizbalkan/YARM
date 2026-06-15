<!-- SPDX-License-Identifier: Apache-2.0 -->

# init_server from initramfs in separate user AS — PR execution board

This board breaks the implementation into small, reviewable PRs so we can move
from the current ring-3 bootstrap stub to loading `init_server` from initramfs
into a dedicated user address space.

## Current baseline

- Boot scheduler handoff currently calls `bootstrap_first_user_task(...)` and
  then `enter_dispatched_user_task_if_available(...)`.
- x86_64 `bootstrap_first_user_task(...)` currently maps a page and injects a
  tiny synthetic syscall/yield loop rather than loading an ELF image.
- Initramfs manifest helpers and launch-plan resolution already exist, but are
  not yet wired to perform full ELF segment mapping for the first boot task.

---

## PR-1 — Boot handoff model for initramfs image bytes

### Title
`boot: introduce initramfs image handoff model for first userspace launch`

### Scope
- Define a boot handoff structure carrying manifest bytes and image payload
  windows (or descriptors).
- Feed handoff from arch boot setup into kernel bootstrap orchestration.
- Keep `src/bin/kernel_boot.rs` ISA-agnostic; all ISA extraction remains under
  `src/arch/*`.

### File touch-list
- `src/kernel/boot/types.rs`
- `src/kernel/boot/mod.rs`
- `src/arch/boot_entry.rs`
- `src/arch/x86_64/boot.rs`
- `src/bin/kernel_boot.rs`

### Acceptance tests
- Unit test verifies handoff structure is populated when initramfs module is
  present.
- Boot emits deterministic marker/log that handoff is available.

### Risks
- Boot-time ownership/lifetime errors for payload memory windows.
- Accidentally leaking ISA-specific logic into non-arch layers.

---

## PR-2 — Kernel ELF PT_LOAD mapper for user AS

### Title
`boot: add ELF segment loader for user address spaces`

### Scope
- Implement reusable ELF64 PT_LOAD mapping routine for user tasks.
- Map/copy initialized bytes, zero bss, apply final page permissions.
- Return validated entrypoint + loaded memory layout summary.

### File touch-list
- `src/kernel/boot/exec_state.rs`
- `src/kernel/boot/memory_state.rs`
- `src/kernel/boot/user_memory_state.rs`
- `src/kernel/process.rs`
- `src/kernel/boot/tests.rs`

### Acceptance tests
- Unit tests for text/data/bss mapping, alignment, and permission finalization.
- Fault-path tests for malformed ELF and invalid segment layouts.

### Risks
- W^X/order-of-operations bugs (write then execute transition).
- Page alignment and bss clearing edge cases.

---

## PR-3 — Replace x86 synthetic stub with initramfs-backed `init_server`

### Title
`x86_64/boot: load init_server ELF from initramfs instead of injected stub`

### Scope
- Remove hardcoded synthetic code injection path for the first ring-3 task.
- Resolve `init_server` image via manifest + image payload handoff.
- Load ELF segments into a dedicated AS and use ELF entrypoint.

### File touch-list
- `src/arch/x86_64/boot.rs`
- `src/kernel/boot/exec_state.rs`
- `crates/yarm-fs-servers/src/fs/initramfs/manifest.rs` (if additional metadata is needed)

### Acceptance tests
- First dispatched user task entrypoint equals manifest/ELF entrypoint.
- No synthetic bootstrap code bytes remain in x86 first-task path.

### Risks
- Early-boot memory-map constraints around payload visibility.
- Regressions in ring-3 transition timing/stack setup.

---

## PR-4 — Startup capability contract for real init task

### Title
`boot/cspace: define and install init_server startup capability contract`

### Scope
- Define exact bootstrap capabilities/endpoints required by init.
- Install capability set into init task cspace during first launch.
- Document and test denied/missing-capability behavior.

### File touch-list
- `src/kernel/boot/capability_state.rs`
- `src/kernel/boot/cnode_state.rs`
- `src/kernel/boot/task_policy_state.rs`
- `crates/yarm-control-plane-servers/src/control_plane/init/core/mod.rs`
- `INIT_SERVER_BOOT_CONTRACT.md`

### Acceptance tests
- Init startup requests succeed with contract-complete cap set.
- Deterministic failure when required startup caps are missing.

### Risks
- Over-provisioned bootstrap capabilities.
- Drift between code and documented contract.

---

## PR-5 — Converge with manifest runtime path

### Title
`init-runtime: unify boot launch with InitCoreImageSource::Manifest path`

### Scope
- Reuse existing manifest image-resolution path for boot-time launch.
- Eliminate duplicate launch-plan resolution logic.
- Keep boot/runtime policy boundaries explicit.

### File touch-list
- `crates/yarm-control-plane-servers/src/control_plane/init/service.rs`
- `crates/yarm-control-plane-servers/src/control_plane/init/core/mod.rs`
- `src/arch/x86_64/boot.rs`
- `src/kernel/boot/types.rs`

### Acceptance tests
- Boot launch path uses manifest source mode for core image plan.
- Existing manifest-source tests remain green.

### Risks
- Dependency direction regressions between boot/runtime/service modules.
- Initialization-order coupling (mount/fault-handoff).

---

## PR-6 — End-to-end gate: init from initramfs in separate AS

### Title
`tests: add e2e gate for init_server initramfs-backed ring-3 launch`

### Scope
- Add integration tests validating first task provenance + AS isolation + ring-3 syscall.
- Add/adjust QEMU smoke script checks for new markers.

### File touch-list
- `tests/kernel_scenarios.rs`
- `src/kernel/boot/tests.rs`
- `scripts/qemu-x86_64-core-smoke.sh`
- `doc/BOOT.md` §4 (consolidated; replaces the former `BOOT_QEMU_RUNBOOK.md`)

### Acceptance tests
- E2E asserts:
  1. first task image is from initramfs manifest,
  2. task has dedicated user AS,
  3. entrypoint matches ELF,
  4. first syscall succeeds from ring-3.

### Risks
- Flaky timing in emulator-based tests.
- Insufficient boot breadcrumbs for failure triage.

---

## PR-7 — Cleanup/freeze

### Title
`cleanup/docs: remove transitional first-task stub path and freeze contracts`

### Scope
- Remove dead transitional fallback paths.
- Align and freeze docs/contracts/readiness references.
- Add CI checks preventing reintroduction of synthetic bootstrap path.

### File touch-list
- `src/arch/x86_64/boot.rs`
- `ARCH_SPLIT.md`
- `KERNEL_STATUS.md`
- `ROADMAP.md`
- `HAL_CONFORMANCE.md`
- `INIT_SERVER_BOOT_CONTRACT.md`

### Acceptance tests
- Contract docs match code path.
- CI gate rejects synthetic-injected first-task reintroduction.

### Risks
- Removing fallback too early can reduce bring-up recoverability.

---

## Recommended execution order

1. PR-1 (handoff model)
2. PR-2 (ELF mapper)
3. PR-3 (x86 real init_server load)
4. PR-4 (startup caps contract)
5. PR-5 (runtime-path convergence)
6. PR-6 (e2e gate)
7. PR-7 (cleanup/freeze)
