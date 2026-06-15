<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Umut Deniz Balkan -->

# YARM Networking

> **Ownership rule.** All networking documentation (netmgr, DHCP, DNS,
> TCP/IP, socket adapter, virtio-net netdev) lives here. New networking
> fragment files are forbidden; update this doc instead. See
> `doc/DOCUMENTATION_MAP.md`.

Networking is **not part of the core boot smoke**. Services exist in
`crates/yarm-network-servers` with strict standalone ABIs and stub
behaviors; integration into a working userspace IP stack is staged work
(NET-6 design, not live wiring).

---

## 1. Phase 3 contract — minimal invariants

These invariants gate `phase3-network-gates`:

### Packet path

- **Link bring-up (`netmgr`) precedes packet routing (`tcpip`).**
- Routed and dropped packet counters are deterministic for a given event
  sequence.

### Name / lease

- DNS cache-hit vs upstream-query accounting is deterministic.
- DHCP lease-grant vs renewal accounting is deterministic.

### Socket adapter

- Open / close accounting must remain balanced for deterministic round-
  trips.
- Adapter behavior remains transport-agnostic and does not depend on FS /
  UI internals.

### IPC transfer-cap ABI prerequisite

- Kernel IPC syscall ABI is frozen at v3.
- Transfer-cap send requires a known waiting receiver (`WouldBlock`
  otherwise).
- Transfer metadata is an envelope handle, **not** a raw source capability
  id.
- Reference: `doc/LIBC_ABI_X86_64_NONE.md`.

### CI gate-mapped tests

- `kernel::syscall::tests::transfer_send_without_waiter_returns_would_block`
- `yarm_network_servers::network::netmgr::service::tests::netmgr_tracks_link_state_events`
- `yarm_network_servers::network::tcpip::service::tests::tcpip_deterministic_packet_path`
- `yarm_network_servers::network::dns::service::tests::dns_timeout_retry_is_reproducible`
- `yarm_network_servers::network::dhcp::service::tests::dhcp_lease_accounting_is_deterministic`
- `yarm_network_servers::network::socket::service::tests::socket_adapter_roundtrip_is_accounted`
- `yarm_network_servers::network::sim::tests::deterministic_network_bootstrap_flow_is_stable`
- `yarm_network_servers::network::sim::tests::link_flap_dhcp_rebind_and_socket_recovery_is_deterministic`

---

## 2. netmgr_srv (NET-2)

`netmgr_srv` is a **userspace-only metadata registry**. It does **not**
transmit, receive, serialize, parse, or queue packets. The ABI is not a
kernel syscall ABI.

### ABI v1

Every request and response occupies exactly **128 inline bytes**. Strict
decoding rejects incorrect lengths, unknown enum/boolean values, invalid
prefixes, invalid device/route records, and any nonzero reserved byte.

Operations:

- Register / unregister / read / list devices.
- Change link state.
- Add / remove IPv4 addresses.
- Add / remove / look up IPv4 routes.
- Read first IPv4 address assigned to a device.
- Check whether an IPv4 address is assigned to a device.
- Read registry counts.

Device-scoped mutations carry an owner identifier and generation. These
are **opaque userspace registry tokens, not kernel capabilities**. Owner
mismatch → `OwnerMismatch`; old / incorrect generation → `StaleGeneration`.

### Device registry

- Maximum **16 devices**; no heap allocation.
- One slot permanently occupied by the system-owned virtual loopback
  device `lo0` (reserved device ID `1`, system owner/generation values,
  `Virtual` and `Loopback` flags).
- Device records: nonzero device ID, nonzero owner ID, nonzero
  generation, unicast MAC, MTU `576..=9000`, bounded capability flags,
  link state.

---

## 3. virtio_net_srv (NET-3 — fake driver)

`virtio_net_srv` implements a **fake, testable userspace NIC-driver
boundary**. It exposes generic netdev metadata and bounded inline
TX / RX queues, but it does **not** access virtio hardware and **does not
claim to transmit or receive a real network packet**.

The boundary is named `netdev_abi` so later NIC drivers can implement the
same userspace contract. NET-3's implementation remains explicitly fake.

### ABI v1

- Every request / response: exactly **128 inline bytes**.
- Packet-bearing messages: at most **96 data bytes** + packet ID + length
  + flags + optional ethertype metadata.
- Ethertype is **never** parsed or validated as a protocol header.

Operations: `GET_INFO`, `GET_STATUS`, test-only `SET_LINK_STATE`,
`TX_INLINE`, `RX_INLINE`, test-only `INJECT_RX_TEST`, test-only
`DRAIN_TX_TEST`, `CLEAR_STATS`.

Strict decoding rejects incorrect lengths, nonzero reserved bytes,
unknown operations, invalid booleans, invalid device metadata, zero
packet IDs, empty or oversized packets, unknown packet flags, and nonzero
bytes beyond the declared packet length.

Checksum-request flag is reserved only so unsupported offload requests can
be rejected explicitly with `ChecksumUnsupported`. The service computes
and verifies no checksum.

---

## 4. tcpip_srv

The TCP/IP service currently performs **deterministic IPv4 transmit
planning**. It accepts route + send-plan requests, applies
MTU-minus-20-byte allowance, selects / validates a source address, and
returns route, device, gateway, next-hop, MTU, protocol, and TTL
metadata.

Production resolver is **unsupported** because no live netmgr endpoint is
connected. Tests use an in-process resolver backed by netmgr state.

It creates no IPv4 / UDP / TCP / ICMP bytes. It owns no packet queue and
receives no packets from a NIC service.

---

## 5. socket_srv

Userspace socket handles and socket state. Current v1 profile:

- Bounded table of **64 AF_INET datagram sockets**.
- One pending **64-byte datagram per socket**.
- Implements only a local `127.0.0.1` table-to-table shortcut.
- **Does not** serialize UDP/IP packets or contact another service.
- Stream / TCP, local-domain sockets, `listen`, and `accept` remain
  **unsupported**.

A socket handle is an index/generation-like service identifier
interpreted by `socket_srv`. It is **not** a kernel capability and grants
no authority to invoke another service.

---

## 6. dhcp_srv / dns_srv

NET-5 defines **strict, standalone no-network stub contracts** for both
services. The stubs do not alter the service graph or add live
integration:

- **DHCP:** lease-grant / renewal accounting is deterministic;
  v1 strict stub; no live wire activity.
- **DNS:** cache-hit / upstream-query accounting is deterministic;
  timeout-retry is reproducible; no live wire activity.

Authoritative implementations: `crates/yarm-network-servers`.

---

## 7. NET-6 integration design (not live wiring)

NET-6 defines a future userspace integration boundary among `socket_srv`,
`tcpip_srv`, `netmgr_srv`, and generic `netdev` providers (such as
`virtio_net_srv`). It is a **design document**, not a live wiring
change. The existing services keep their current standalone behavior and
ABI versions.

This design adds **no** packet creation or parsing, **no** checksums, **no**
packet queues, **no** hardware access, **no** service-to-service IPC
calls, **no** kernel socket syscalls, **no** runtime spawn policy, and
**no** driver-manager policy. Names for future requests and events are
descriptive drafts, not assigned opcodes or ABI commitments. A later
task must define and review each wire format before implementation.

### Current layer responsibilities (summary)

1. **`socket_srv`** owns userspace socket handles + state.
2. **`tcpip_srv`** owns IPv4 transmit planning (no packet bytes).
3. **`netmgr_srv`** owns the userspace metadata registry.
4. **`virtio_net_srv` (fake netdev)** owns the test-time netdev boundary.
5. **`dhcp_srv` / `dns_srv`** are strict standalone stubs.

---

## 8. Authoring rule

Future networking changes update **this file**. The per-service ABI
constants live in `crates/yarm-ipc-abi/` and `crates/yarm-network-servers`.
Do **not** create new `NETMGR_*` / `DHCP_*` / `DNS_*` / `TCPIP_*` /
`SOCKET_*` / `VIRTIO_NET_*` / `NETWORK_*` fragment files.
