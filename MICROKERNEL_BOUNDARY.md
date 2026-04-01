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
- Planned longer-term shape: split into boundary-enforcing crates (for example `yarm-kernel`, `yarm-ipc`, `yarm-srv-common`) so the type system enforces separation instead of grep-based policy.
