// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

# Socket Server Contract (NET-1)

## Scope

`socket_srv` is a userspace-only socket service foundation. Its version 1 ABI is a fixed-size,
strictly decoded IPC protocol implemented by `yarm-ipc-abi`; it is **not** a kernel socket ABI.
NET-1 adds no socket syscalls, syscall table entries, traps, scheduler behavior, kernel IPC
behavior, or runtime spawn policy.

The service is deliberately an in-memory test profile. It does not claim TCP/IP, UDP wire
compatibility, or access to a network device.

## ABI v1

Each request and response occupies exactly 128 inline bytes. Request operation codes are carried
in the IPC message opcode and cover:

- create and close;
- bind;
- listen and accept placeholders;
- connect;
- send and receive;
- shutdown;
- status queries.

The ABI names `AF_INET` and local-domain placeholders, stream and datagram types, and default,
TCP, and UDP protocol selectors. These identifiers reserve service-facing vocabulary only. The
NET-1 service accepts only the `AF_INET` datagram profile with the default or UDP selector.
Stream, TCP, and local-domain requests return `Unsupported`.

All unused and reserved bytes must be zero. Decode rejects unknown operations, unknown enum
values, incompatible type/protocol combinations, invalid loopback endpoints, incorrect message
lengths, oversized data, and nonzero reserved bytes. Data is inline and bounded to 64 bytes.

## In-memory datagram loopback profile

The implemented profile supports:

1. creating a datagram socket;
2. binding it to `127.0.0.1` and a nonzero local port;
3. connecting another datagram socket to that bound endpoint;
4. sending one inline datagram to the receiver's bounded queue;
5. receiving and removing that datagram;
6. shutting down read and/or write directions;
7. closing and later reusing table storage.

There is no packet serialization. A send copies bytes directly between entries in the same
`SocketService` table. An empty receive and a send to an occupied one-datagram queue return
`WouldBlock`. Closing a bound destination removes the endpoint, so a later send returns
`NotFound`. The sender address is reported only when the sender was explicitly bound.

## States and limits

The table has 64 entries and uses no heap allocation. Entries move among:

- `Empty`;
- `Created`;
- `Bound`;
- `Connected`;
- `Closed`.

`Listening` is represented in the ABI for future stream work but is not entered by NET-1. Closed
slots can be reused with a new nonzero handle. Each socket has at most one pending datagram, and
each datagram carries at most 64 bytes.

## Service process behavior

The freestanding binary follows the established userspace server shape: allocator installation,
`yarm_user_entry`, runtime `_start` handoff, a panic handler, entry/readiness markers, and a
resident receive loop. When no receive capability is supplied it remains resident by yielding.
Unknown IPC operations receive an `Unsupported` response when a reply capability is available.
No runtime startup or spawn configuration is changed by NET-1.

## Explicitly deferred networking

The following remain outside this contract:

- `netmgr` device and route registry integration;
- the `tcpip_srv` implementation;
- `virtio_net` packet input/output and hardware transport;
- real DHCP and DNS protocol behavior;
- IPv4/IPv6 routing;
- ARP and NDP;
- UDP, TCP, IP, or device checksums;
- TCP connection establishment, congestion control, retransmission, and windowing;
- stream listen/connect/accept behavior;
- blocking wakeups and poll/select integration;
- kernel socket syscalls.

The intended future userspace layering is:

```text
socket_srv -> tcpip_srv -> netmgr -> virtio_net
```

NET-2 can add the bounded `netmgr` device/route registry without real packets. NET-3 can instead
clean up a fake `virtio_net` packet queue/service boundary without real hardware networking.
