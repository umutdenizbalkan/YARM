// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

# DNS Server Contract (NET-5)

## Scope

`dns_srv` is a userspace-only, deterministic DNS resolver **stub**. NET-5
provides a strict ABI, server configuration state, bounded query names, and
stable no-answer behavior. It does not implement a DNS resolver or network
transport.

The service does not create or parse DNS packets, open UDP or TCP sockets,
perform retries, contact `tcpip_srv`, consult `netmgr_srv`, or transmit through
a netdev. It adds no kernel syscall and changes no runtime spawn policy.

## ABI v1

`dns_abi` uses fixed 128-byte request and reply payloads. Query names are
bounded to 88 bytes so every request remains inline and allocation-free.
Strict decoding rejects incorrect lengths, nonzero reserved bytes, unknown
operations, invalid server addresses, oversized names, and malformed names.
Requests carry a nonzero `request_id`; malformed-request replies may use zero.

The operations are:

- `DNS_OP_GET_STATUS`
- `DNS_OP_CONFIGURE_SERVER`
- `DNS_OP_CLEAR_SERVER`
- `DNS_OP_QUERY_A`
- `DNS_OP_QUERY_AAAA`
- `DNS_OP_QUERY_PTR`
- `DNS_OP_CLEAR_CACHE`

The reply reserves fields for an IPv4 answer, an IPv6 answer, TTL, cached flag,
and query count. NET-5 returns no answer data.

## Name and server validation

Names must be nonempty ASCII labels separated by dots. Each label is at most
63 bytes and may contain letters, digits, and interior hyphens. Empty labels,
leading or trailing hyphens, trailing dots, non-ASCII bytes, and names over 88
bytes are rejected.

A configured IPv4 server must be nonzero and unicast according to the bounded
v1 check: unspecified, limited broadcast, class-D multicast, and higher ranges
are rejected. This validation does not perform route or interface discovery.

## Stub behavior

The service stores at most one configured IPv4 DNS server and a saturating
query count. Querying without a server returns `NotConfigured`. A valid A,
AAAA, or PTR query with a configured server increments the count and returns
`NoAnswer`; it never emits a packet.

`CLEAR_SERVER` is idempotent. NET-5 has no cache, so `CLEAR_CACHE` deterministically
returns `CacheEmpty`. Malformed wire requests return `BadRequest`, and unknown
operations return `Unsupported`.

## Service process

The existing canonical `dns_srv` wrapper remains unchanged. The service logs
`DNS_SRV_ENTRY` and `DNS_READY`, then receives requests on its own
startup-provided service endpoint. If no endpoint exists, it yields
indefinitely. The resident loop only handles its local strict ABI.

## Deferred DNS work

A future resolver requires separately reviewed work for:

1. UDP and TCP transport through the userspace network stack;
2. DNS header, question, answer, name-compression, and bounds validation;
3. transaction IDs, timeout, retry, and server failover policy;
4. bounded positive and negative caches with TTL and eviction policy;
5. search domains, CNAME traversal, and response-size handling;
6. capability authorization, request cancellation, and client cleanup; and
7. runtime policy deciding whether and how `dns_srv` is spawned.

None of those behaviors are implemented or promised by NET-5.
