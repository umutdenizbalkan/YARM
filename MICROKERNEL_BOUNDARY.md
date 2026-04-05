<!-- SPDX-License-Identifier: Apache-2.0 -->

# Microkernel Boundary Contract

This contract locks the kernel to mechanisms and pushes policies to user space.

## In-kernel mechanisms only

- thread scheduling and context-switch plumbing
- virtual memory/address-space management
- IPC and notifications
- capabilities and rights checks
- interrupt/trap normalization and routing

## Must remain in user space

- process-management policies
- filesystems and VFS policy
- networking stack
- device logic and protocol policy
- POSIX personality/syscall policy translation

## Server model (uniform vocabulary)

All user-space components are **servers**:

```
/srv/
  init.srv
  process_manager.srv
  vfs.srv
  ext4.srv
  tcp.srv
  usb.srv
  posix.srv
```

Kernel responsibilities are limited to capability validation and IPC transport.
There is no privileged driver class in the kernel object model.
Hardware access is modeled as capabilities held by normal servers.


## Release profiles

- Core-only systems: follow `CORE_PROFILE.md` (no Linux personality feature required).
- Linux personality systems: enable feature `linux-compat` and include the linux compatibility server profile.


## Per-ISA arch layout boundary

- Arch/address-space constants and syscall shape constants are selected via `crate::arch::{vm_layout, platform_layout, syscall_abi}` and should not be newly introduced directly under `src/kernel/`.

## Current boundary enforcement model

- Kernel mechanism types are owned by `yarm-kernel`; extracted server bins are owned by dedicated server crates and call `yarm-server-runtime` wrapper entrypoints.
- Structural boundary enforcement is now crate-graph driven (`scripts/check-crate-graph-boundary.py`) and compiled in CI via `phase5-boundary-gates`.
- `scripts/check-service-arch-boundary.sh` remains as a complementary source-shape guard (thin entry wrappers + denylisted imports), not the primary boundary mechanism.
- Root package binary ownership is kernel bootstrap only (`kernel_boot`).

## Remaining milestone PR list

This is the concrete PR sequence for closing the boundary milestone.

1. **PR-BND-1: Harden shared service helper contracts**
   - tighten `yarm-srv-common` helper invariants (decode strictness, timeout/cap-attach semantics, typed errors),
   - add negative-path tests for malformed ABI payloads and unsupported opcode handling.
2. **PR-BND-2: Complete shared-helper migration**
   - migrate remaining service call sites to hardened helpers,
   - remove now-redundant local helper shims.
3. **PR-BND-3: Extract `yarm-kernel` crate**
   - move mechanism-only modules into `yarm-kernel`,
   - expose only minimal stable interfaces required by boot/runtime consumers.
4. **PR-BND-4: Extract/rewire server crates**
   - split server code into workspace crates and wire bins to them,
   - ensure services consume only `yarm-ipc-abi` + `yarm-srv-common` + approved runtime crates.
5. **PR-BND-5: Promote type-system boundary enforcement in CI**
   - make crate-graph and Rust visibility the primary boundary gate,
   - keep grep guard as transitional defense, then retire it once no longer needed.
6. **PR-BND-6: Cleanup and contract freeze update**
   - remove stale paths/compat layers after extraction,
   - update boundary docs and declare strict separation complete.

## Boundary milestone status

- ✅ **COMPLETE**: PR-BND-1 through PR-BND-6 are now landed on this branch.
- Primary enforcement is structural/type-driven (crate graph + Rust visibility via extracted crates and CI boundary gates).
- Source-shape guards remain as companion checks, not primary policy.

### Current extraction progress snapshot

- PR-BND-1 and PR-BND-2 are complete (shared helper hardening + adoption).
- PR-BND-3 is now complete through passes A-D (IPC core, capability/scheduler core, boot telemetry/capacity core, and bridge cleanup/lock tests).
- PR-BND-4 has started with extracted server packages:
  - `crates/yarm-driver-servers` (`blkcache_srv`, `console_driver`, `input_srv`, `irqmux_srv`, `uart_srv`, `virtio_blk_srv`, `virtio_gpu_srv`, `virtio_net_srv`)
  - `crates/yarm-ui-servers` (`compositor_srv`, `display_srv`, `shell_srv`)
  - `crates/yarm-compat-servers` (`supervisor_srv`, `posix_compat_srv`)
  - `crates/yarm-control-plane-servers` (`init_server`, `process_manager`, `driver_manager`, `vfs_server`)
  - `crates/yarm-fs-servers` (`devfs_srv`, `ramfs_srv`, `initramfs_srv`, `ext4_srv`, `fat_srv`)
  - `crates/yarm-network-servers` (`dhcp_srv`, `dns_srv`, `netmgr_srv`, `socket_srv`, `tcpip_srv`)
  - `crates/yarm-runtime-tools` (`core_profile_smoke`)
  - `crates/yarm-server-runtime` (server entry wrapper surface used by extracted server-bin crates)
- PR-BND-4 is complete through passes A-F (server bin extraction, root ownership cleanup, `yarm-server-runtime` rewiring bridge, and dependency-wiring closeout checks).
- Remaining milestone focus shifts to CI promotion to structural/type gates (PR-BND-5) and final stale-path cleanup/freeze (PR-BND-6).
- Latest PR-BND-4 pass also moved remaining hosted service bins out of root package ownership:
  - `console_driver` -> `crates/yarm-driver-servers`
  - `driver_manager` -> `crates/yarm-control-plane-servers`
- Root package bin ownership is now kernel bootstrap only (`kernel_boot`).
- PR-BND-4 pass E rewired extracted server crates to call `yarm-server-runtime` wrappers instead of root `yarm` paths directly at bin-entry level.
- PR-BND-4 pass F added `scripts/check-server-crate-deps.sh` to fail if extracted server crates depend on root `yarm` directly instead of `yarm-server-runtime`.
- PR-BND-5 pass A started with a crate-graph gate (`scripts/check-crate-graph-boundary.py`) that validates dependency edges via `cargo metadata` instead of source-path greps.
- PR-BND-5 pass B landed with CI wiring (`scripts/phase5-boundary-gates.sh` + `phase5-boundary-gates` workflow job) to run structural boundary scripts and extracted server compile checks.
- PR-BND-5 pass C promoted `phase5-boundary-gates` to required readiness/workflow token checks (`check-ci-workflow-enforcement`, `check-roadmap-readiness`, and `PHASE_READINESS_MATRIX`).
- PR-BND-6 pass A started by removing redundant transitional dependency script (`check-server-crate-deps.sh`) now that crate-graph enforcement is active (`check-crate-graph-boundary.py`).
- PR-BND-6 pass B landed by rewriting stale single-crate transition text to reflect the current enforcement model (crate-graph primary + source-shape companion guard).
- PR-BND-6 pass C landed by locking a completion marker + freeze checks (`scripts/check-boundary-milestone-freeze.sh`) into the boundary gate path.

## Definition of done for the boundary milestone

- No server crate can access kernel internals except through explicitly exported mechanism interfaces.
- Boundary enforcement is compile-time structural (crate graph + visibility), not primarily grep-policy based.
- Shared helper usage is uniform and covered by negative/compat tests across service boundaries.
