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

## Single-crate boundary risk and enforcement

- YARM currently keeps kernel + servers in one crate for iteration speed, which means `crate::kernel::*` is technically visible to service code.
- To preserve microkernel user-space boundaries until a multi-crate split lands, CI rejects kernel-path imports from `src/services/**` and `src/bin/*_srv.rs` via `scripts/check-service-arch-boundary.sh`.
- Workspace split is active with dedicated `yarm-ipc-abi` and `yarm-srv-common` crates; service-side VFS decode and control-plane call/reply helpers are now centralized there.
- Remaining step is a full `yarm-kernel` crate extraction (and server crate wiring) so the type system fully enforces service/kernel separation instead of grep-based policy.

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
- Remaining milestone focus shifts to broader server crate extraction/rewiring completion (PR-BND-4), CI promotion to structural/type gates (PR-BND-5), and final stale-path cleanup/freeze (PR-BND-6).
- Latest PR-BND-4 pass also moved remaining hosted service bins out of root package ownership:
  - `console_driver` -> `crates/yarm-driver-servers`
  - `driver_manager` -> `crates/yarm-control-plane-servers`
- Root package bin ownership is now kernel bootstrap only (`kernel_boot`).
- PR-BND-4 pass E rewired extracted server crates to call `yarm-server-runtime` wrappers instead of root `yarm` paths directly at bin-entry level.

## Definition of done for the boundary milestone

- No server crate can access kernel internals except through explicitly exported mechanism interfaces.
- Boundary enforcement is compile-time structural (crate graph + visibility), not primarily grep-policy based.
- Shared helper usage is uniform and covered by negative/compat tests across service boundaries.
