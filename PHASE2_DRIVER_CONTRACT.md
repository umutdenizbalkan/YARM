<!-- SPDX-License-Identifier: Apache-2.0 -->

# Phase 2 Driver Contract (YARM)

This contract defines the minimum invariants for Phase-2 driver services (`irqmux`, `uart`, `virtio_blk`, `virtio_net`, `virtio_gpu`, `input`).

## Delegation contract

- Driver runtime capability bundles are delegated only through validated service-role edges.
- Bundles are composed of IRQ and DMA primitives, with optional IOVA-space attachment.
- Delegation must be deterministic for the same input plan and service graph.

## Fault / restart contract

- Driver restart requires a valid restart token.
- Driver runtime caps are revoked on restart and faulted teardown.
- Restart denial escalation is reported at class-configured thresholds.

## DMA / IOVA contract

- IOVA windows must be page-aligned and non-zero sized.
- Validation fails when no IOVA space is attached or window constraints are violated.
- Detaching an IOVA space immediately invalidates DMA window validation.

## Service-level deterministic counters

- `irqmux.srv`: routed vs dropped IRQ accounting.
- `uart.srv`: tx/rx byte counters.
- `virtio_net.srv`: tx/rx packet counters.
- `virtio_gpu.srv`: mode-set and frame commit counters.
- `input.srv`: accepted vs dropped input events.

## CI gate mapping

- Transfer-cap ABI prerequisite:
  - Kernel IPC syscall ABI is frozen at v3.
  - Transfer-cap send requires a known waiting receiver (`WouldBlock` otherwise).
  - Transfer metadata is an envelope handle (not a raw source capability id).
  - Reference: `LIBC_ABI_X86_64_NONE.md`.
- `kernel::boot::tests::delegate_driver_bundle_checked_enforces_service_role_edges`
- `kernel::boot::tests::restart_denial_escalates_to_supervisor_every_threshold`
- `kernel::boot::tests::driver_restart_revokes_runtime_caps`
- `kernel::syscall::tests::transfer_send_without_waiter_returns_would_block`

The above are required to pass in `.github/workflows/compat-gates.yml` under `phase2-driver-gates`
and are executed via `scripts/phase2-driver-gates.sh`.

## Backpressure and queueing contract

- `uart.srv` enforces TX queue limits with deterministic drop accounting.
- `virtio_net.srv` enforces TX queue limits with deterministic drop accounting and completion-based recovery.
- `input.srv` enforces queue limits and deterministic overflow handling.
- `virtio_gpu.srv` rejects frame commit before mode-set and reports deterministic rejection counters.
- `irqmux.srv` supports per-line masking and deterministic masked/drop routing semantics.
