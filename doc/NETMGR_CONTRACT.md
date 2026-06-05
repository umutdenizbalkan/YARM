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

The registry contains at most 16 devices and uses no heap allocation. A device record contains a
nonzero device ID, nonzero owner ID, nonzero generation, unicast MAC address, MTU from 576 through
9000, bounded capability flags, and link state.

Registering an existing device ID returns `AlreadyExists`, even if the descriptor is identical.
Unregistering requires the current owner and generation. Successful unregister cascades through
all IPv4 addresses and routes associated with that device. `ListDevices` uses a table-slot cursor;
the response returns the next populated slot or `u32::MAX` when iteration is complete.

No registration request starts or configures a driver. A future driver-manager integration may
provision owner/generation values and arrange service capabilities, but that policy is deferred.

## IPv4 address registry

The registry contains at most 32 address records. Each record names a registered device, an IPv4
address, prefix length, owner, and generation. Prefixes from 0 through 32 are accepted; address
zero is not accepted as an interface address. Add and remove operations require the current
device owner and generation. Duplicate device/address/prefix tuples return `AlreadyExists`.

Address records are metadata only. NET-2 performs no duplicate-address detection, ARP, NDP,
DHCP, source-address selection, interface configuration, or packet emission.

## IPv4 route table

The route table contains at most 32 entries. Each route has a nonzero route ID, normalized IPv4
destination prefix, optional gateway (`0` means direct), output device, metric, owner, and
generation. Routes may be installed while their device link is down.

Lookup applies these deterministic rules:

1. consider routes whose prefixes contain the requested destination;
2. ignore routes whose output device is missing;
3. skip routes whose output device is link-down;
4. choose the longest prefix;
5. for equal prefixes, choose the lowest metric;
6. for a remaining tie, choose the lowest route ID.

If prefixes match but every matching route uses a link-down device, lookup returns `LinkDown`.
Otherwise, no usable match returns `NotFound`. A `0.0.0.0/0` entry is the default route. Route
lookup returns metadata only and never forwards a packet.

The status response returns the device count in `value`; `auxiliary` packs the IPv4-address count
in its upper 16 bits and route count in its lower 16 bits.

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
- socket notifications or blocking wakeups;
- live driver-manager registration policy.

The intended future userspace layering is:

```text
socket_srv
    -> tcpip_srv
        -> netmgr_srv
            -> virtio_net_srv / other NIC drivers
```

NET-3 may define a fake `virtio_net` packet service boundary without hardware networking. NET-4
may define a `tcpip_srv` skeleton that consumes route lookup without sending or receiving real
packets.
