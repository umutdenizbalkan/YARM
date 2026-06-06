// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

# TCP/IP Server Contract (NET-4)

## Scope

`tcpip_srv` is a userspace-only IPv4 planning skeleton. It sits conceptually between
`socket_srv` and `netmgr_srv`, but NET-4 does not establish live IPC between those services. The
service resolves route metadata, selects source and next-hop addresses, checks a conservative MTU
budget, and returns a deterministic transmit plan.

NET-4 creates no packet, packet buffer, descriptor, queue entry, Ethernet frame, IPv4 header, TCP
segment, or UDP datagram. The ABI is not a kernel syscall ABI and changes no runtime spawn policy.

## ABI v1

Each request and response is exactly 128 bytes. Strict decoding rejects incorrect lengths,
nonzero reserved bytes, zero request IDs, unsupported protocol values or flags, zero TTL values,
unspecified destinations, limited broadcast, and multicast source or destination addresses.

The planning ABI provides:

- `ROUTE_IPV4`, which resolves route metadata without selecting a source address;
- `PLAN_SEND_IPV4`, which produces a complete planning result;
- `GET_LOCAL_IPV4`, which asks the resolver for the first address on a device;
- `SET_DEFAULT_TTL`, which changes the TTL reported for route-only planning and status;
- `GET_STATUS`, which reports default TTL and successful/failed plan counts.

Protocol numbers are bounded placeholders for ICMP, TCP, UDP, and raw IPv4. Accepting a protocol
number does not implement that protocol.

## Route resolver abstraction

`TcpipRouteResolver` defines the internal boundary required by the planner:

- lookup an IPv4 route for a destination;
- return the first IPv4 address assigned to an output device;
- test whether an explicit source address belongs to an output device.

A resolved route contains route ID, output device ID, optional gateway, and device MTU. Lookup can
report `Unsupported`, `NoRoute`, or `LinkDown`.

The production NET-4 server uses `UnsupportedRouteResolver` because no live `netmgr_srv` endpoint
is wired into `tcpip_srv`. It therefore returns `Unsupported` for route-dependent operations
until a later userspace service-wiring task supplies a resolver. Tests use a fake resolver backed
by `NetmgrService`, so longest-prefix, metric, route-ID, and link-state semantics are exercised
without adding live service IPC.

## Transmit-plan behavior

For a valid planning request, the service:

1. resolves the destination through `TcpipRouteResolver`;
2. checks that payload length fits `MTU - 20`, using a conservative fixed IPv4-header allowance;
3. selects or validates a source address;
4. chooses the next hop;
5. returns route ID, output device, gateway, source, destination, next hop, MTU, payload length,
   protocol, and effective TTL.

For a direct route (`gateway == 0`), next hop is the destination. For a gateway route, next hop is
the gateway. The planner does not verify gateway reachability and does not perform ARP or NDP.

Payloads larger than `MTU - 20` return `MtuExceeded`. There is no fragmentation. Payload length is
planning metadata only; no payload bytes cross this ABI.

## Source-address selection

Source selection is deterministic:

- a nonzero explicit source is accepted only when the resolver reports that it is assigned to the
  selected output device;
- a zero source selects the resolver's first IPv4 address for that output device;
- an absent, invalid, or wrong-device source returns `NoSourceAddr`.

The fake netmgr resolver preserves address insertion order, so the first inserted address is used
in tests. NET-4 does not implement policy routing, source-prefix scoring, privacy addresses, or
address lifetime handling.

## Service process behavior

The hosted/freestanding binary installs the standard 256 KiB freestanding allocator, provides
`yarm_user_entry` and the runtime `_start` handoff, and emits:

- `TCPIP_BIN_ENTRY_START`;
- `TCPIP_SRV_ENTRY`;
- `TCPIP_READY`.

It remains in a receive loop, replies when a reply capability is provided, and yields indefinitely
when no receive endpoint exists. Malformed messages return `BadRequest`; unknown operations return
`Unsupported`.

## Explicitly deferred networking

NET-4 does not provide:

- packet creation, parsing, queues, buffers, or ownership transfer;
- IPv4 header construction or validation;
- IPv4 or transport checksums;
- IP fragmentation or reassembly;
- TCP connection or congestion state;
- UDP socket or checksum behavior;
- ICMP behavior;
- ARP or NDP;
- DHCP or DNS protocols;
- `virtio_net` or other NIC-driver I/O;
- live `tcpip_srv` to `netmgr_srv` IPC;
- socket notifications, blocking operations, poll, or select integration.

The intended future userspace layering remains:

```text
socket_srv -> tcpip_srv -> netmgr_srv -> virtio_net_srv / other NIC drivers
```

NET-3 can define a fake NIC packet service boundary without hardware networking. NET-5 can instead
clean up DHCP/DNS stubs with strict userspace ABIs and no real network I/O.

## Integration design

The design-only future IPC, capability, ownership, packet-lifecycle, loopback, and boot-placement
boundaries are documented in [`NETWORK_STACK_INTEGRATION.md`](NETWORK_STACK_INTEGRATION.md). That
document does not enable live service wiring or change this contract's current behavior.
