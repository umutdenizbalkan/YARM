# Phase 3 Network Contract (YARM)

This contract defines minimal invariants for networking services (`netmgr`, `tcpip`, `dns`, `dhcp`, `socket`).

## Packet path contract

- Link bring-up (`netmgr`) precedes packet routing (`tcpip`).
- Routed and dropped packet counters are deterministic for a given event sequence.

## Name/lease contract

- DNS cache-hit vs upstream-query accounting is deterministic.
- DHCP lease-grant vs renewal accounting is deterministic.

## Socket adapter contract

- Socket open/close accounting must remain balanced for deterministic roundtrips.
- Adapter behavior must remain transport-agnostic and not depend on FS/UI internals.

## IPC transfer-cap ABI prerequisite

- Kernel IPC syscall ABI is frozen at v3.
- Transfer-cap send requires a known waiting receiver (`WouldBlock` otherwise).
- Transfer metadata is an envelope handle (not a raw source capability id).
- Reference: `LIBC_ABI_X86_64_NONE.md`.

## CI gate mapping

- `kernel::syscall::tests::transfer_send_without_waiter_returns_would_block`
- `services::network::netmgr::service::tests::netmgr_tracks_link_state_events`
- `services::network::tcpip::service::tests::tcpip_deterministic_packet_path`
- `services::network::dns::service::tests::dns_timeout_retry_is_reproducible`
- `services::network::dhcp::service::tests::dhcp_lease_accounting_is_deterministic`
- `services::network::socket::service::tests::socket_adapter_roundtrip_is_accounted`
- `services::network::sim::tests::deterministic_network_bootstrap_flow_is_stable`
- `services::network::sim::tests::link_flap_dhcp_rebind_and_socket_recovery_is_deterministic`
