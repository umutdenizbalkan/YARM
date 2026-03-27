# Kernel Scaling Profile

This document records the current scaling strategy for kernel fixed-size tables,
why hosted and non-hosted profiles differ, and where we still need follow-up work.

## Why profiles exist

The kernel still relies on fixed-size arrays for several subsystems (IPC tables,
thread/task tables, capability spaces, etc.).

A single value for every target is a poor fit:

- **hosted-dev** needs lower stack pressure for tests and local simulation.
- **non-hosted** (real hardware / deployment builds) needs larger ceilings.

To balance both, capacities are now profile-gated via `#[cfg(feature = "hosted-dev")]`
and `#[cfg(not(feature = "hosted-dev"))]`.

## Current profile values

### Kernel boot subsystem (`src/kernel/boot/mod.rs`)

| Constant | hosted-dev | non-hosted |
|---|---:|---:|
| `MAX_ENDPOINTS` | 64 | 32 |
| `MAX_ENDPOINT_SENDER_WAITERS` | 8 | 4 |
| `MAX_TASKS` | 64 | 128 |
| `MAX_MEMORY_OBJECTS` | 1024 | 256 |
| `MAX_NOTIFICATIONS` | 64 | 32 |
| `MAX_DRIVERS` | 64 | 32 |
| `MAX_DRIVER_IRQ_CAPS` | 16 | 8 |
| `MAX_DRIVER_DMA_CAPS` | 16 | 8 |
| `MAX_TRANSFER_ENVELOPES` | 256 | 64 |

### Capability space (`src/kernel/capabilities.rs`)

| Constant | hosted-dev | non-hosted |
|---|---:|---:|
| `MAX_CAPABILITIES_PER_CSPACE` | 1024 | 512 |

### VM layout (`src/arch/*/vm_layout.rs`)

Across `aarch64`, `x86_64`, and `riscv64`:

| Constant | hosted-dev | non-hosted |
|---|---:|---:|
| `MAX_MAPPINGS` | 128 | 128 |
| `MAX_ADDRESS_SPACES` | 16 | 32 |

## Stack-safety changes introduced with scaling

Larger capacities made stack pressure more visible in hosted tests.
To keep `cargo test` stable while increasing selected ceilings, the following
changes were made:

1. **Endpoint storage indirection in `IpcSubsystem`**
   - `endpoints` now stores `Option<KernelStorage<Endpoint>>`
   - hosted-dev uses boxed endpoint objects instead of embedding every endpoint inline.

2. **KernelState large-table indirection**
   - `user_spaces`, `tcbs`, `tls_restore_pending`, and `robust_futex` are stored through `KernelStorage`.

3. **Trap tests create boxed kernel state**
   - architecture trap tests now allocate `KernelState` through `Box` in test setup paths.

## Scope of what is solved

- Multiple blocked sender waiters per endpoint are supported.
- Driver records can track multiple IRQ and DMA caps.
- DMA region minting checks against parent memory object length.
- Capacity values are profile-aware and documented.

## Remaining work

The current approach still uses fixed-size arrays. To fully scale from minimal
hardware to large systems, we should migrate toward a configurable allocator-backed
model.

Recommended follow-ups:

1. **Runtime capacity config**
   - Move from compile-time constants to a boot-time capacity descriptor.

2. **Allocator-backed registries**
   - Replace fixed arrays for endpoints/notifications/drivers/tasks with slabs or
     arena-indexed vectors.

3. **IPC queue model upgrades**
   - Introduce queue depth policy per endpoint class (control-plane vs data-plane).

4. **Memory object sizing model**
   - Support explicit multi-page memory-object creation in public APIs so DMA sizing
     can be expressed directly rather than inferred.

5. **Capacity telemetry + pressure signals**
   - Export "near-full" metrics to supervisor/init for proactive remediation.

