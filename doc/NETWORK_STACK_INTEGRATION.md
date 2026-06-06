// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

# Userspace Network Stack Integration Design (NET-6)

## Status and scope

NET-6 defines a future userspace integration boundary among `socket_srv`,
`tcpip_srv`, `netmgr_srv`, and generic `netdev` providers such as
`virtio_net_srv`. It is a design document, not a live wiring change. The
existing services keep their current standalone behavior and ABI versions.

This design adds no packet creation or parsing, checksums, packet queues,
hardware access, service-to-service IPC calls, kernel socket syscalls, runtime
spawn policy, or driver-manager policy. Names for future requests and events in
this document are descriptive drafts, not assigned opcodes or ABI commitments.
A later task must define and review each wire format before implementation.

## Current layer responsibilities

### `socket_srv`

The socket service owns userspace socket handles and socket state. Its current
v1 profile is a bounded table of 64 AF_INET datagram sockets with one pending
64-byte datagram per socket. It implements only a local `127.0.0.1` table-to-
table shortcut; it does not serialize UDP/IP packets or contact another
service. Stream/TCP, local-domain sockets, listen, and accept remain
unsupported.

A socket handle is an index/generation-like service identifier interpreted by
`socket_srv`. It is not a kernel capability and grants no authority to invoke
another service.

### `tcpip_srv`

The TCP/IP service currently performs deterministic IPv4 transmit planning. It
accepts route and send-plan requests, applies an MTU-minus-20-byte allowance,
selects or validates a source address, and returns route, device, gateway,
next-hop, MTU, protocol, and TTL metadata. Its production resolver is
unsupported because no live netmgr endpoint is connected. Tests use an
in-process resolver backed by netmgr state.

It creates no IPv4, UDP, TCP, or ICMP bytes. It owns no packet queue and
receives no packets from a NIC service.

### `netmgr_srv`

The network manager owns bounded userspace registries for devices, IPv4
addresses, and IPv4 routes. Route selection uses longest prefix, then lowest
metric, then lowest route ID, while skipping missing and link-down devices. ABI
v1 is polling-only.

Device IDs, route IDs, owner IDs, and generations are registry metadata. They
are not kernel capabilities and are not sufficient authorization for IPC.

### `netdev` and `virtio_net_srv`

`netdev_abi` is the generic future NIC-service boundary. The current
`virtio_net_srv` implementation exposes deterministic fake device information,
eight-entry inline TX and RX queues, 96-byte maximum inline packet data, link
state, and counters. Its test-control operations inject RX data and drain TX
data. Packet bytes are opaque.

The fake service has no MMIO, PCI, DMA, IOMMU, virtqueue, IRQ, Ethernet/IP
parser, checksum engine, netmgr registration, or live tcpip connection.

## Intended service graph

The eventual logical graph is:

```text
Application / POSIX compatibility layer
                 |
                 v
             socket_srv
                 |
                 v
              tcpip_srv
              /       \
             v         v
        netmgr_srv   netdev service
             |         |
             |         +--> virtio_net_srv
             |         +--> another future NIC driver
             v
   device/address/route metadata
                         |
                         v
                    hardware later
```

`tcpip_srv` consults `netmgr_srv` for control-plane metadata and uses a selected
`netdev` service for packet movement. `netmgr_srv` is not on the packet data
path and must not forward packet bytes.

### Special paths

- **Current local datagrams:** `socket_srv` directly copies a bounded datagram
  between socket-table entries. This remains an isolated test profile.
- **Future IPv4 loopback:** traffic for `127.0.0.0/8` should traverse the
  tcpip planning/packet logic and resolve to `lo0`, then return through a
  virtual loopback ingress path without contacting a hardware NIC.
- **Shortcut policy:** a later design may retain a socket-local optimization,
  but observable addressing, readiness, error, ordering, and accounting
  semantics must match the full `tcpip_srv`/`lo0` path. The shortcut must not
  bypass security checks.
- **Broadcast and multicast:** transmission, reception, membership, and route
  policy are deferred. Existing validation restrictions remain in force.
- **DHCP:** a future DHCP client would use a datagram/control boundary and ask
  netmgr to install leased addresses and routes. It must not mutate registries
  merely by knowing device IDs.
- **DNS:** a future DNS service would be a client of socket/tcpip facilities,
  not a route registry or NIC driver. Resolver configuration and caching are
  separate policies.

## Future IPC boundaries

Each service should own one receive endpoint for its public protocol. Clients
receive only the send/reply capabilities necessary for their approved edge in
the graph. Endpoint distribution belongs to future process-manager and
driver-manager policy; NET-6 does not change that policy.

### Application or POSIX layer to `socket_srv`

The application-facing side creates, binds, connects, sends through, receives
from, shuts down, and closes userspace sockets. `socket_srv` owns socket-table
entries and validates that requests refer to live handles belonging to the
calling context once caller identity is designed.

A future blocking API must not hold a synchronous IPC call indefinitely by
default. Initial integration should preserve explicit `WouldBlock` replies and
polling. Readiness notification, cancellation, deadlines, and task-exit cleanup
require a separate reviewed protocol.

### `socket_srv` to `tcpip_srv`

For a future datagram send, `socket_srv` supplies transport intent: local and
remote endpoint metadata, protocol, payload length, and eventually payload
ownership. `tcpip_srv` validates network-layer feasibility, resolves a route
and source, constructs transport/network headers in later stages, and reports
success, backpressure, or a stable error.

For receive, `tcpip_srv` eventually reports a validated and demultiplexed
transport payload to `socket_srv`; `socket_srv` decides which socket queue owns
it and exposes readiness to the application.

Future stream support would add connection identity, listen/accept queues,
sequence and retransmission state, shutdown, and flow control. None of those
semantics should be inferred from the current datagram ABI, and no stream
opcode is assigned by this document.

Ownership rule: socket state and application payload queues belong to
`socket_srv`; IP-layer plans and later IP/transport processing belong to
`tcpip_srv`. A request/reply transfer must say whether bytes were copied,
borrowed for the call, or transferred. Silence must never imply ownership.

### `tcpip_srv` to `netmgr_srv`

The first live boundary should use existing netmgr v1 request/reply semantics
for:

- destination route lookup;
- output-device metadata and link state;
- local IPv4 address lookup or bounded device-address enumeration;
- registry status when diagnostic counts are needed.

NET-7 should adapt `TcpipRouteResolver` to an IPC-backed implementation without
changing route ranking. `tcpip_srv` must treat route replies as snapshots:
link, address, route, owner, or generation state can change immediately after a
reply. Before an eventual transmit, stale metadata may require one bounded
retry or a deterministic error; unbounded retry is forbidden.

Netmgr v1 remains polling-only. Future subscriptions may reduce polling but
must not become a prerequisite until their lifecycle and backpressure rules are
specified.

### `tcpip_srv` to `netdev` / `virtio_net_srv`

The future TX request carries a complete link-layer packet, selected device
identity/generation, packet metadata, and a completion token. The netdev
service validates the selected device instance, MTU, supported flags, and link
state before accepting ownership or copying bytes. Acceptance means only that
the bounded driver queue owns the request; hardware transmission and completion
are later states.

The future RX direction may initially be polled: `tcpip_srv` asks the selected
netdev service for one packet and receives `Empty` when none is available. A
later event can indicate RX availability, but packet retrieval and ownership
must remain explicit.

The present 96-byte inline fake packet is useful for tests but cannot represent
normal MTU-sized frames. A production-capable ABI will need either:

1. a bounded inline form for small packets plus an explicit size limit; or
2. a separately designed shared-memory object/capability transfer for full
   frames.

Shared memory requires validation of capability type, length, offset,
direction, mutability, lifetime, revocation, and cleanup. A numeric packet ID
alone cannot authorize memory access. Scatter/gather, offload, and multi-queue
selection are deferred.

Backpressure is reported at queue admission. A full queue returns a retryable
status without ambiguous ownership. The caller retains the packet when
admission fails; the netdev owns it only after an explicit accepted result.

### `netmgr_srv` to netdev and driver-manager policy

A future NIC driver registers descriptor metadata with netmgr only after
`driver_manager` authenticates the driver instance and grants the appropriate
endpoint capabilities. Driver manager should allocate or approve the device
ID, owner token, generation, and allowed operations. Netmgr then enforces the
owner/generation tuple against stale or cross-driver mutation.

Live link-state updates flow from the authorized NIC driver to netmgr. Address
and route installation may instead be performed by an authorized network
configuration service. Unregistering a device invalidates its generation and
cascades associated addresses and routes as defined by the current contract.

NET-6 does not define a live netmgr-to-driver callback. Registration and status
updates remain request/reply operations initiated by an authorized driver until
a future event protocol is approved.

## Capability and authority model

### Endpoint ownership

A least-authority deployment should eventually provide:

| Principal | Receives authority to | Must not automatically receive |
|---|---|---|
| Application | Call its assigned `socket_srv` endpoint | netmgr or NIC control endpoints |
| POSIX compatibility service | Translate approved calls to `socket_srv` | direct driver or route mutation |
| `socket_srv` | Call the application-facing tcpip endpoint | netdev test-control operations |
| `tcpip_srv` | Query netmgr and use approved netdev data endpoints | device registration or driver-manager policy |
| Network configuration service | Mutate approved netmgr address/route state | packet-buffer or hardware capabilities |
| NIC driver | Update its device metadata and serve its netdev endpoint | other devices' generations or routes |
| `driver_manager` | Provision/authorize NIC service relationships | ownership of application socket state |

Reply capabilities are single-exchange authority and should not be retained as
subscriptions. Long-lived event endpoints require separate validation and
cleanup rules.

### Identifiers are not capabilities

- A **socket handle** identifies state inside `socket_srv`; it is not a kernel
  capability and must be scoped to an authorized client/session.
- A **device ID** or **route ID** identifies netmgr metadata; possession of the
  number grants no mutation or endpoint authority.
- An **owner ID and generation** reject accidental/stale registry mutation, but
  are userspace tokens and cannot replace authenticated IPC provenance.
- A **netdev packet ID** correlates queue operations or completion; it grants no
  right to a packet buffer and is not globally authoritative.
- A **kernel endpoint or memory capability** is actual authority enforced by
  the kernel. It must be distributed according to policy and validated at the
  receiving service boundary.

Registry tokens are intentionally useful for deterministic state-machine
checks, not security by themselves. Security requires both an authorized IPC
capability and request fields consistent with the caller's assigned identity.

## Future packet lifecycle

### Transmit path

1. An application send reaches `socket_srv` for a live socket handle.
2. `socket_srv` validates socket state and asks `tcpip_srv` to prepare or send
   the datagram/stream bytes.
3. `tcpip_srv` queries `netmgr_srv` for route, source-address, device, link, and
   MTU snapshots.
4. A later packet builder constructs transport and IPv4 headers, applies
   required checksums, and resolves link-layer addressing through a separately
   designed neighbor mechanism.
5. `tcpip_srv` submits the complete packet to the selected netdev service.
6. The netdev service admits the packet to a bounded software/hardware queue.
7. Admission failure, later completion, or drop information propagates through
   tcpip and socket accounting according to the chosen completion policy.

Steps 4 through 7 are not implemented. In particular, an accepted send must
later distinguish "copied to a service queue" from "transmitted by hardware";
these are not the same completion point.

### Receive path

1. A future netdev implementation receives a frame or a fake harness injects
   one into a bounded RX queue.
2. `tcpip_srv` polls or is notified, then explicitly retrieves ownership of one
   frame.
3. Later tcpip logic validates Ethernet/IP/transport lengths, addresses,
   fragmentation, protocol, and checksums before trusting metadata.
4. Valid traffic is demultiplexed to `socket_srv` using a future socket-delivery
   boundary.
5. `socket_srv` enqueues bounded payload data and exposes `WouldBlock`/readiness
   behavior to the application.

No current service performs these steps as a live chain.

### Backpressure and drop points

Every queue must have a fixed or configured bound. Expected pressure points are
application-to-socket send queues, socket-to-tcpip work, tcpip-to-netdev TX,
netdev RX, reassembly, and per-socket receive queues. Each boundary must define:

- the exact admission/completion point;
- whether failure is retryable;
- who owns bytes after each result;
- queue ordering;
- per-client fairness or quotas;
- drop reason and counter;
- whether an event is edge- or level-triggered.

A reasonable initial policy is tail-drop on full bounded queues, with the
caller retaining TX ownership on rejected admission and the receiver counting
RX drops. Silent overwrite is not allowed. Counter wrap/saturation and reset
authority must be specified by the ABI.

### Cleanup on exit

On service or client exit, the supervisor/process manager will eventually need
to revoke endpoints and notify surviving services through a separately designed
lifecycle path. Each service then cleans only the state it owns:

- `socket_srv`: client socket handles, queued payloads, and waiters;
- `tcpip_srv`: pending plans, packet work, reassembly, and completion tokens;
- `netmgr_srv`: device/address/route records owned by the exited authorized
  principal, subject to protected-system records such as `lo0`;
- netdev service: queued buffers, completion tokens, and subscriber endpoints.

Transferred shared-memory capabilities must be revoked or returned exactly
once. Cleanup must be idempotent and generation-safe.

## Polling and future events

### Current model

All current integration assumptions are polling/request-reply:

- netmgr clients use `GET_STATUS`, `GET_DEVICE`, and `LOOKUP_ROUTE`;
- tcpip route planning returns a synchronous metadata result;
- fake netdev RX returns `Empty` when no packet is queued;
- socket receive returns `WouldBlock` when no datagram is queued.

Polling is less efficient but keeps v1 lifecycle and backpressure explicit.

### Possible v2 subscriptions

Future versioned protocols may add events for:

- link up/down;
- device register/unregister;
- IPv4 address add/remove;
- route add/remove;
- netdev RX availability and TX completion;
- socket readable/writable/error/hangup readiness.

Before any event ABI is enabled, its design must define endpoint and capability
validation, subscriber identity, bounded event queues, level versus edge
triggering, ordering, coalescing, replay versus drop behavior, overflow
recovery, acknowledgement, cancellation, and cleanup on subscriber exit.
Events are hints about state transitions; clients must be able to re-query
authoritative state after overflow or reconnect.

## Loopback and default-route policy

Netmgr reserves device ID `1` for `lo0`. It is virtual, system-owned, link-up,
and uses the stable system generation. It owns `127.0.0.1/8` and the direct
`127.0.0.0/8` route. Normal owner operations cannot unregister or mutate it.
It is not a hardware device and must never submit traffic to `virtio_net_srv` or
another physical NIC service.

A `0.0.0.0/0` route is a valid fallback. Longest-prefix selection means the
loopback `/8` and any other usable specific route beat a default route. If the
specific route's link is down, current netmgr lookup may select an up default
route; however, loopback's protected link remains up, so `127.0.0.0/8` must not
escape through the default route in normal state. A future tcpip integration
should additionally classify loopback destinations and reject hardware egress
if registry corruption or stale metadata would select a physical device.

Today, socket-local datagram loopback bypasses this route. The eventual full
path should resolve `127.0.0.1` through tcpip/netmgr and reinject it through a
virtual `lo0` ingress boundary. Whether the shortcut remains as an optimization
is a later compatibility decision.

## Boot and CPIO placement

The QEMU initramfs may eventually contain these binaries as an executable
depot:

- `socket_srv`;
- `tcpip_srv`;
- `netmgr_srv`;
- `virtio_net_srv` or another netdev provider;
- `dhcp_srv`;
- `dns_srv`.

Presence in CPIO does not imply startup, authority, or ordering. A future
runtime policy may let init/driver-manager decide what should exist and let the
process manager perform the actual spawn and capability distribution. Network
services remain optional until that policy is designed and reviewed. NET-6
changes neither staging lists nor spawn behavior.

Every ELF packed into a QEMU CPIO archive, including `/init` and every
`/sbin/*` service, must retain BIN-HYGIENE-1's 4096-byte file-data alignment and
an `ALIGN_PROOF ... alignment_mod=0 aligned=true` record. This storage-layout
requirement does not grant execute or service capabilities.

## Suggested incremental implementation plan

These stages are suggestions, not commitments. Each needs its own scope review,
ABI tests, capability policy, and failure-mode documentation.

### NET-7 — live tcpip-to-netmgr route lookup, no packets

Implement an IPC-backed `TcpipRouteResolver` using existing netmgr v1 route,
device, and address queries. Preserve deterministic route/source selection and
keep tcpip packet output unsupported.

### NET-8 — tested IPv4/UDP packet builder

Add bounded pure functions and golden tests for IPv4 and UDP headers. Do not add
live service queues, NIC calls, DHCP/DNS traffic, or hardware access.

### NET-9 — tcpip-to-fake-netdev TX

Define a reviewed packet-transfer ABI and connect tcpip output to the fake
netdev TX queue. Keep real virtio hardware, DMA, IRQ, and MMIO out of scope.

### NET-10 — fake RX through tcpip to socket

Define bounded fake RX retrieval, validation/demultiplexing, and socket receive
queue behavior. Add explicit ownership, drop, and `WouldBlock` tests.

### NET-11 — NIC registration authorization

Design driver-manager provisioning of netmgr device IDs, owner/generation
tokens, endpoint capabilities, link updates, unregister, and driver-exit
cleanup. Do not treat registry tokens as security authority.

### NET-12 — DHCP/DNS boundary work

Either give DHCP/DNS strict userspace stub ABIs with no network I/O, or produce
a separately reviewed real-client design that uses socket/tcpip and authorized
netmgr configuration boundaries.

## Non-goals retained after NET-6

NET-6 does not implement or authorize:

- live IPC between any network services;
- real packet buffers, packet parsing, packet construction, or checksums;
- TCP, UDP, ICMP, DHCP, DNS, ARP, NDP, broadcast, or multicast behavior;
- fragmentation, reassembly, neighbor discovery, congestion, or retransmission;
- blocking socket wakeups, poll, select, or event subscriptions;
- real netdev hardware, MMIO, PCI, DMA, IOMMU, virtqueues, or IRQ delivery;
- driver-manager NIC policy or runtime spawn ordering;
- kernel socket syscalls or any syscall ABI change.
