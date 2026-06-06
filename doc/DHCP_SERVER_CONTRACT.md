// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

# DHCP Server Contract (NET-5)

## Scope

`dhcp_srv` is a userspace-only, deterministic DHCP **stub**. NET-5 defines a
strict service ABI and a bounded state machine, but it does not implement the
DHCP protocol or any network data path.

The service does not create or parse DHCP messages, open UDP sockets, send or
receive packets, resolve routes, configure `netmgr_srv`, or contact a netdev.
It adds no kernel syscall and changes no runtime spawn policy.

## ABI v1

`dhcp_abi` uses a fixed 128-byte request and reply payload. Decoding rejects
incorrect lengths, nonzero reserved bytes, unknown operations, invalid device
configuration, and malformed lease data. Requests carry a nonzero
`request_id`; error replies may use request ID zero when strict decoding could
not recover one safely.

The operations are:

- `DHCP_OP_GET_STATUS`
- `DHCP_OP_CONFIGURE_INTERFACE`
- `DHCP_OP_START`
- `DHCP_OP_STOP`
- `DHCP_OP_POLL`
- `DHCP_OP_GET_LEASE`
- `DHCP_OP_CLEAR_LEASE`

Interface configuration records a nonzero device ID, owner placeholder, and
generation placeholder. Those values are userspace registry metadata, not
kernel capabilities and not sufficient security authority.

The lease reply layout reserves bounded fields for an assigned IPv4 address,
prefix length, gateway, DNS server, lease duration, device ID, and generation.
NET-5 does not obtain such a lease from a network.

## State behavior

The service starts `Unconfigured` and has four states:

- `Unconfigured`
- `Configured`
- `Running`
- `Stopped`

Configuring an interface stores one configuration, clears any stored lease,
and enters `Configured`. Starting requires configuration and enters `Running`.
Starting again returns `AlreadyRunning`. Stopping a running service enters
`Stopped`; stopping a non-running configured service returns `NotRunning`.

`POLL` is deliberately conservative: while running it records one bounded,
saturating poll count and returns `NoLease` unless a unit-test-only lease was
injected. It never emits traffic. `GET_LEASE` likewise returns `NoLease` when
no lease exists. `CLEAR_LEASE` is idempotent and returns `Ok`.

Malformed wire requests return `BadRequest`, and unknown operations return
`Unsupported`.

## Service process

The existing canonical `dhcp_srv` wrapper remains unchanged. The service logs
`DHCP_SRV_ENTRY` and `DHCP_READY`, then receives requests on its own
startup-provided service endpoint. If no endpoint is available, it yields
indefinitely. The receive loop only decodes requests and sends fixed-size
replies; it performs no service-to-service wiring.

## Deferred DHCP work

A future DHCP client requires separately reviewed work for:

1. UDP support through `socket_srv` and `tcpip_srv`;
2. DHCP discover/offer/request/ack packet construction and validation;
3. retransmission, timeout, renewal, rebinding, and expiry policy;
4. server and transaction validation;
5. installing/removing addresses, routes, and DNS configuration in
   `netmgr_srv`;
6. capability authorization and lifecycle cleanup; and
7. runtime policy deciding whether and how `dhcp_srv` is spawned.

None of those behaviors are implied by the NET-5 stub.
