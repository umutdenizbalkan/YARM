// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

# Network Manager Contract (NET-2)

## Scope

`netmgr_srv` is a userspace-only metadata registry. NET-2 establishes a bounded boundary for
future communication among `socket_srv`, `tcpip_srv`, network management, and NIC drivers. It
does not transmit, receive, serialize, parse, or queue packets.

The ABI is not a kernel syscall ABI. NET-2 changes no syscall table, runtime spawn policy, kernel
IPC behavior, driver-manager behavior, or NIC hardware transport.

## ABI v1

Every request and response occupies exactly 128 inline bytes. Strict decoding rejects incorrect
lengths, unknown enum or boolean values, invalid prefixes, invalid device/route records, and any
nonzero reserved byte. Operations cover:

- registering, unregistering, reading, and listing devices;
- changing link state;
- adding and removing IPv4 addresses;
- adding, removing, and looking up IPv4 routes;
- reading registry counts.

Device-scoped mutations carry an owner identifier and generation. These are opaque userspace
registry tokens, not kernel capabilities. An owner mismatch returns `OwnerMismatch`; an old or
incorrect generation returns `StaleGeneration`.

## Device registry

The registry contains at most 16 devices and uses no heap allocation. One slot is permanently
occupied by the system-owned virtual loopback device `lo0`, leaving 15 slots for registered
devices. A device record contains a nonzero device ID, nonzero owner ID, nonzero generation,
unicast MAC address, MTU from 576 through 9000, bounded capability flags, and link state.

`lo0` has reserved device ID `1`, system owner/generation values, `Virtual` and `Loopback` flags,
a synthetic locally administered MAC required by the v1 descriptor format, MTU 9000, and link-up
state. It is initialized with `127.0.0.1/8` and a direct `127.0.0.0/8` route. It is metadata-only:
it is not hardware-backed, does not depend on `virtio_net`, and never emits packets. Normal
register, unregister, link-state, address, and route mutation paths cannot replace or mutate it.

Registering an existing device ID returns `AlreadyExists`, even if the descriptor is identical.
Unregistering requires the current owner and generation. Successful unregister cascades through
all IPv4 addresses and routes associated with that device. `ListDevices` uses a table-slot cursor;
the response returns the next populated slot or `u32::MAX` when iteration is complete.

No registration request starts or configures a driver. A future driver-manager integration may
provision owner/generation values and arrange service capabilities, but that policy is deferred.

## IPv4 address registry

The registry contains at most 32 address records. The `127.0.0.1/8` loopback record permanently
occupies one slot, leaving 31 slots for normal registrations. Each record names a registered
device, an IPv4 address, prefix length, owner, and generation. Prefixes from 0 through 32 are
accepted; address zero is not accepted as an interface address. Add and remove operations require the current
device owner and generation. Duplicate device/address/prefix tuples return `AlreadyExists`.

Address records are metadata only. NET-2 performs no duplicate-address detection, ARP, NDP,
DHCP, source-address selection, interface configuration, or packet emission.

## IPv4 route table

The route table contains at most 32 entries. The direct `127.0.0.0/8` loopback route permanently
occupies one slot, leaving 31 slots for normal routes. Each route has a nonzero route ID,
normalized IPv4 destination prefix, optional gateway (`0` means direct), output device, metric,
owner, and generation. Routes may be installed while their device link is down.

Lookup applies these deterministic rules:

1. consider routes whose prefixes contain the requested destination;
2. ignore routes whose output device is missing;
3. skip routes whose output device is link-down;
4. choose the longest prefix;
5. for equal prefixes, choose the lowest metric;
6. for a remaining tie, choose the lowest route ID.

If prefixes match but every matching route uses a link-down device, lookup returns `LinkDown`.
Otherwise, no usable match returns `NotFound`. A `0.0.0.0/0` entry is accepted as the default
route; prefix length zero is handled without a shift-by-32 operation. More-specific usable routes
beat the default. If a more-specific route is link-down but an up default route also matches, the
lookup skips the down route and returns the default. Equal-prefix routes still use metric and then
route ID as tie-breakers. Route lookup returns metadata only and never forwards a packet.

The status response returns the device count in `value`; `auxiliary` packs the IPv4-address count
in its upper 16 bits and route count in its lower 16 bits.

## Event subscription policy

NET-2B keeps netmgr ABI v1 polling-only. No notification endpoint, subscription record, or event
IPC is added. For now, `tcpip_srv`, `socket_srv`, and other userspace clients may poll
`GET_STATUS`, `GET_DEVICE`, and `LOOKUP_ROUTE` when they need current registry state.

A future ABI v2 may add subscriptions for link up/down, device register/unregister, IPv4 address
add/remove, and route add/remove events. That design must specify endpoint and capability
validation, bounded backpressure, replay versus drop policy, event ordering, and cleanup when a
subscriber exits before notification delivery can be enabled.

## Service process behavior

The hosted/freestanding binary installs the standard 256 KiB freestanding allocator and exposes
`yarm_user_entry` plus the runtime `_start` handoff. It emits `NETMGR_BIN_ENTRY_START`,
`NETMGR_SRV_ENTRY`, and `NETMGR_READY`, then remains in its IPC receive loop. Without a receive
endpoint it yields indefinitely. Malformed messages return `BadRequest`; unknown operations
return `Unsupported` when a reply capability is supplied.

## Deferred packet path and layering

NET-2 intentionally provides none of the following:

- real TCP/IP or UDP behavior;
- packet queues, packet buffers, or packet ownership transfer;
- `virtio_net` input/output or hardware transport;
- routing-based packet forwarding;
- Ethernet framing;
- IPv4/IPv6 parsing or checksums;
- ARP or NDP;
- DHCP or DNS protocols;
- socket notifications, netmgr event subscriptions, or blocking wakeups;
- live driver-manager registration policy.

The intended future userspace layering is:

```text
socket_srv
    -> tcpip_srv
        -> netmgr_srv
            -> virtio_net_srv / other NIC drivers
```

NET-4 now supplies the planning-only `tcpip_srv` boundary. The recommended next userspace task is
NET-3: define a fake `virtio_net` packet service boundary without real hardware networking.
