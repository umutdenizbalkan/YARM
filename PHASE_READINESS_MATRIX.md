# Phase Readiness Matrix (YARM)

This matrix maps roadmap phase completion to required contracts, CI jobs, and deterministic tests.

## Phase 2 — Device Driver Servers

- Contract docs:
  - `PHASE2_DRIVER_CONTRACT.md`
- Required CI jobs:
  - `phase2-driver-gates`
- Required deterministic tests:
  - `kernel::boot::tests::delegate_driver_bundle_checked_enforces_service_role_edges`
  - `kernel::boot::tests::restart_denial_escalates_to_supervisor_every_threshold`
  - `kernel::boot::tests::driver_restart_revokes_runtime_caps`
  - `services::drivers::irqmux::service::tests::irqmux_routes_masks_and_drops_deterministically`
  - `services::drivers::uart::service::tests::uart_backpressure_is_deterministic`
  - `services::drivers::virtio_net::service::tests::virtio_net_queue_backpressure_is_deterministic`
  - `services::drivers::virtio_gpu::service::tests::virtio_gpu_rejects_commit_before_modeset`
  - `services::drivers::input::service::tests::input_queue_overflow_and_drain_is_deterministic`

## Phase 3 — Networking Servers

- Contract docs:
  - `PHASE3_NETWORK_CONTRACT.md`
- Required CI jobs:
  - `phase3-network-gates`
- Required deterministic tests:
  - `services::network::netmgr::service::tests::netmgr_tracks_link_state_events`
  - `services::network::tcpip::service::tests::tcpip_deterministic_packet_path`
  - `services::network::dns::service::tests::dns_timeout_retry_is_reproducible`
  - `services::network::dhcp::service::tests::dhcp_lease_accounting_is_deterministic`
  - `services::network::socket::service::tests::socket_adapter_roundtrip_is_accounted`
  - `services::network::sim::tests::deterministic_network_bootstrap_flow_is_stable`
  - `services::network::sim::tests::link_flap_dhcp_rebind_and_socket_recovery_is_deterministic`

## Phase 4 — Display + UI input servers

- Contract docs:
  - `PHASE4_UI_CONTRACT.md`
- Required CI jobs:
  - `phase4-ui-gates`
  - `phase4-ui-smoke-marker`
  - `riscv64-busybox-smoke-strict` (workflow_dispatch strict runtime path)
- Required deterministic tests/checks:
  - `services::ui::display::service::tests::boot_marker_is_stable`
  - `services::ui::display::service::tests::display_tracks_modeset_and_present`
  - `services::ui::compositor::service::tests::compositor_replay_is_deterministic`
  - `services::ui::shell::service::tests::shell_session_counter_increments`
  - strict QEMU log marker grep for `\[ui\] boot-to-shell marker`
