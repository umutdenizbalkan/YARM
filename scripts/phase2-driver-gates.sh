#!/usr/bin/env bash
set -euo pipefail

cargo test -q kernel::boot::tests::delegate_driver_bundle_checked_enforces_service_role_edges
cargo test -q kernel::boot::tests::restart_denial_escalates_to_supervisor_every_threshold
cargo test -q kernel::boot::tests::driver_restart_revokes_runtime_caps
cargo test -q services::drivers::irqmux::service::tests::irqmux_routes_masks_and_drops_deterministically
cargo test -q services::drivers::uart::service::tests::uart_backpressure_is_deterministic
cargo test -q services::drivers::virtio_net::service::tests::virtio_net_queue_backpressure_is_deterministic
cargo test -q services::drivers::virtio_gpu::service::tests::virtio_gpu_rejects_commit_before_modeset
cargo test -q services::drivers::input::service::tests::input_queue_overflow_and_drain_is_deterministic
