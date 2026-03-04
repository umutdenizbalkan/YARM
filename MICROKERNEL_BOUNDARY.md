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
- device-driver logic and protocol policy
- POSIX personality/syscall policy translation

## Driver model

Drivers are user-space tasks registered with system servers.
Kernel responsibilities are limited to capability validation and message routing
(IRQ notifications, DMA-capability based mappings, IPC transport).
