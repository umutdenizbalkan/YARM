<!-- SPDX-License-Identifier: Apache-2.0 -->

# IPC Medium-Payload Fragmentation Policy

This document defines the Phase 1 design for medium payloads (`129..=1024` bytes).

## Goals

- Avoid mandatory shared-memory setup for moderate payload sizes.
- Keep endpoint semantics and capability checks unchanged.
- Preserve receiver-side reassembly determinism.

## Fragment wire format

Each fragment is a normal `Message` payload with this fixed prefix:

- `u32 message_id`
- `u16 fragment_index`
- `u16 fragment_count`
- `u16 fragment_len`
- `u16 reserved`
- followed by `fragment_len` bytes

Total prefix size: 12 bytes.

With `MAX_PAYLOAD=128`, usable fragment data per message is `116` bytes.

## Sender rules

1. Generate a non-zero `message_id` unique per sender endpoint stream.
2. Compute `fragment_count = ceil(total_len / 116)`.
3. Emit fragments in index order (`0..fragment_count-1`).
4. Use consistent opcode for all fragments of the same logical message.

## Receiver rules

1. Group by `(sender_tid, opcode, message_id)`.
2. Reject duplicate fragment indexes.
3. Require all fragments to arrive before exposing reassembled payload.
4. Drop partial assemblies on timeout / sender death / endpoint teardown.

## Error handling

- Missing fragment timeout => discard partial assembly and return `WouldBlock`/timeout.
- Invalid headers (count=0, index>=count, len overflow) => `InvalidArgs`.
- Reassembly buffer exhaustion => `QueueFull`.

## Capacity policy

- Max in-flight fragmented logical messages per endpoint: implementation-defined (recommended: 8).
- Max reassembly bytes per endpoint: implementation-defined (recommended: 8 KiB).

## Compatibility

- Fragmentation is additive and does not change existing inline or shared-memory contracts.
- Services may opt in per opcode.
