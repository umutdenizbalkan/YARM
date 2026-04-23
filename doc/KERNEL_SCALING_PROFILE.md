<!-- SPDX-License-Identifier: Apache-2.0 -->

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

Runtime profile config also carries:

- `default_cnode_slot_capacity`
- `driver_cnode_slot_capacity`
- `max_total_cnode_slots` (global budget across all process cnode spaces)

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
- IPC queue policy is endpoint-class-aware (`ControlPlane` vs `DataPlane`) with
  class-specific queue depth defaults.
- Runtime capacity config phase 1 is available via boot-time profile selection
  (`HostedDefault`, `Constrained`, `Throughput`).
- Driver records can track multiple IRQ and DMA caps.
- DMA region minting checks against parent memory object length.
- Capacity values are profile-aware and documented.
- Per-process cnode spaces can be created/resized with runtime-selected slot capacities.
- Global cnode slot consumption is bounded via `max_total_cnode_slots` budget checks.

## Dynamic CNode sizing status

The current implementation is **dynamically sizable**, but not yet "fully dynamic/unbounded":

- ✅ Slot capacity is dynamic per cnode (`ensure_cnode_space_with_slots`, `resize_cnode_slots`).
- ✅ Per-class defaults and request-time overrides are supported (policy-gated).
- ✅ Capacity accounting is global-budget-aware (`max_total_cnode_slots`) and exposed in telemetry.
- ✅ Revoke traversal scratch/worklists are allocator-backed (`Vec`) and sized to each cspace's runtime `slot_capacity` (no hidden fallback to `MAX_CAPABILITIES_PER_CSPACE` for traversal buffers).
- ⚠️ Slot capacity is still hard-capped by `MAX_CAPABILITIES_PER_CSPACE_HARD`.
- ⚠️ CNode registry count remains bounded by kernel capability table sizing (not allocator-unbounded).

## Related architecture hardening updates

Although not directly a capacity knob, the same hardening pass also tightened
architecture boundary behavior:

- unsupported `target_arch` selections now fail at compile time in
  `arch::mod`, `arch::irq_guard`, and `arch::trap_entry` (no silent fallback);
- trap decoding for unknown ISA-specific trap/exception codes now preserves
  unknown-ness via `TrapEvent::Unknown { arch_code }`;
- per-CPU TLS restore tracking includes explicit CPU-slot isolation tests in all
  three ISA trap modules (`aarch64`, `riscv64`, `x86_64`).

## Remaining work

The current approach still uses fixed-size arrays. To fully scale from minimal
hardware to large systems, we should migrate toward a configurable allocator-backed
model.

Recommended follow-ups:

1. **Allocator-backed registries**
   - Replace fixed arrays for endpoints/notifications/drivers/tasks/**cnode registries** with slabs or
     arena-indexed vectors.

2. **Memory object sizing model**
   - Support explicit multi-page memory-object creation in public APIs so DMA sizing
     can be expressed directly rather than inferred.

3. **Capacity telemetry + pressure signals**
   - Export "near-full" metrics to supervisor/init for proactive remediation.
