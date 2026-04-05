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

## Definition of done for the boundary milestone

- No server crate can access kernel internals except through explicitly exported mechanism interfaces.
- Boundary enforcement is compile-time structural (crate graph + visibility), not primarily grep-policy based.
- Shared helper usage is uniform and covered by negative/compat tests across service boundaries.
