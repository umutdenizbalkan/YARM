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
  procman.srv
  vfs.srv
  ext4.srv
  tcp.srv
  usb.srv
  posix.srv
```

Kernel responsibilities are limited to capability validation and IPC transport.
There is no privileged driver class in the kernel object model.
Hardware access is modeled as capabilities held by normal servers.
