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

- `kernel::bootstrap::tests::delegate_driver_bundle_checked_enforces_service_role_edges`
- `kernel::bootstrap::tests::restart_denial_escalates_to_supervisor_every_threshold`
- `kernel::bootstrap::tests::driver_restart_revokes_runtime_caps`

The above are required to pass in `.github/workflows/compat-gates.yml` under `phase2-driver-gates`.
