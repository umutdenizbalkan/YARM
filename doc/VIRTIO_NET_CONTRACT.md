// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

# Fake Virtio-Net Service Contract (NET-3)

## Scope

`virtio_net_srv` implements a fake, testable userspace NIC-driver boundary. It exposes generic
netdev metadata and bounded inline TX/RX queues, but it does not access virtio hardware and does
not claim to transmit or receive a real network packet.

The boundary is named `netdev_abi` because later NIC drivers can implement the same userspace
contract. NET-3's implementation remains explicitly fake and is hosted by `virtio_net_srv`.

## ABI v1

Every request and response occupies exactly 128 inline bytes. Packet-bearing messages contain at
most 96 data bytes plus packet ID, length, flags, and an optional ethertype metadata value.
Ethertype is never parsed or validated as a protocol header.

Operations are:

- `GET_INFO`;
- `GET_STATUS`;
- test-only `SET_LINK_STATE`;
- `TX_INLINE`;
- `RX_INLINE`;
- test-only `INJECT_RX_TEST`;
- test-only `DRAIN_TX_TEST`;
- `CLEAR_STATS`.

Strict decoding rejects incorrect lengths, nonzero reserved bytes, unknown operations, invalid
booleans, invalid device metadata, zero packet IDs, empty or oversized packets, unknown packet
flags, and nonzero bytes beyond the declared packet length.

The ABI reserves a checksum-request flag only so unsupported offload requests can be rejected
explicitly with `ChecksumUnsupported`. The service computes and verifies no checksum.

## Fake device information

The fake device reports:

- device ID 2 and generation 1;
- synthetic locally administered MAC `02:00:00:00:00:02`;
- MTU 1500;
- fake TX, fake RX, and test-control feature flags;
- link state;
- TX and RX queue capacities of eight packets each.

These values are deterministic test metadata. The device is not registered with `netmgr_srv` and
has no MMIO, PCI, DMA, IOMMU, virtqueue, interrupt, or hardware ownership.

## Fake TX queue

`TX_INLINE` copies a valid inline packet into an eight-entry FIFO. Link-down TX returns
`LinkDown` without enqueueing or incrementing the drop counter. A full queue returns `TableFull`
and increments `tx_dropped`. Successful enqueue increments `tx_packets`.

`DRAIN_TX_TEST` removes and returns the oldest packet exactly as submitted. It returns `Empty`
when the queue has no packet. Draining is a fake-harness operation and does not represent hardware
completion.

## Fake RX queue

`INJECT_RX_TEST` copies an inline packet into an eight-entry FIFO. It is allowed while link-down,
so a deterministic harness may stage receive data independently of link state. A full queue
returns `TableFull` and increments `rx_dropped`.

`RX_INLINE` removes and returns the oldest injected packet. It returns `Empty` when no packet is
available and increments `rx_packets` only when a packet is delivered to the client.

Neither queue parses Ethernet, IPv4, IPv6, ARP, TCP, UDP, or ICMP. Packet bytes are opaque.

## Status and counters

`GET_STATUS` directly includes current device information and link state, queue depths, and:

- successful TX enqueues;
- successful RX deliveries;
- TX drops caused by queue capacity;
- RX drops caused by queue capacity;
- malformed or unsupported wire requests.

`CLEAR_STATS` resets counters but does not change link state or clear queued packets. Packet
responses prioritize the inline packet payload; clients use `GET_STATUS` for full counters.

## Service process behavior

The hosted/freestanding binary installs the standard 256 KiB allocator, provides
`yarm_user_entry` and runtime `_start`, and emits:

- `VIRTIO_NET_BIN_ENTRY_START`;
- `VIRTIO_NET_SRV_ENTRY`;
- `VIRTIO_NET_READY`.

It remains in an IPC receive loop and yields indefinitely if no receive endpoint exists. Malformed
requests return `BadRequest`; unknown operations return `Unsupported` when a reply capability is
available.

## Explicitly deferred work

NET-3 adds none of the following:

- virtio MMIO, PCI transport, feature negotiation, or device reset;
- DMA, IOMMU mappings, descriptor rings, or real virtqueues;
- IRQ registration, delivery, acknowledgement, or polling;
- packet buffers outside the fixed inline ABI;
- Ethernet or IP parsing;
- checksums or checksum offload;
- segmentation, fragmentation, or reassembly;
- live registration with `netmgr_srv`;
- live packet exchange with `tcpip_srv` or `socket_srv`;
- DHCP, DNS, ARP, NDP, TCP, UDP, or ICMP behavior.

A future integration may register device metadata with `netmgr_srv`, let `tcpip_srv` consume
netmgr route lookup, and connect a separately designed packet-transfer boundary between
`tcpip_srv` and NIC drivers. Real hardware work remains a distinct later task.

## Integration design

The design-only future IPC, capability, ownership, packet-lifecycle, loopback, and boot-placement
boundaries are documented in [`NETWORK_STACK_INTEGRATION.md`](NETWORK_STACK_INTEGRATION.md). That
document does not enable live service wiring or change this contract's current behavior.
