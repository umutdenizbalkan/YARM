<!-- SPDX-License-Identifier: Apache-2.0 -->

# Phase 1 Payload Capacity and Framing Policy

## Decision

- **Freeze inline payload capacity at `Message::MAX_PAYLOAD = 128`**.
- Keep large payload path on shared-memory descriptor transfer (`OPCODE_SHARED_MEM`).
- For medium payloads that exceed 128B and do not justify shared-memory setup, use fragmentation protocol (see `IPC_FRAGMENTATION_POLICY.md`).

## Benchmark snapshot (this branch)

Command:

```bash
cargo test -q --test phase1_payload_bench -- --nocapture
```

Observed output:

- `inline64 = 94.96 ns/op`
- `inline128 = 96.80 ns/op`
- `shared_desc = 80.93 ns/op`
- `simulated_2x128 = 193.61 ns/op`

## Interpretation

- 128B inline remains close to 64B inline in message construction cost (~2% slower in this snapshot).
- Two-fragment 256B simulation is roughly 2x the 128B path, validating need for explicit fragmentation policy and/or shared memory for larger payloads.
- Shared-memory descriptor envelope creation is inexpensive, so descriptor+map path is preferred for large payloads where mapping overhead is acceptable.

## Policy thresholds

- `0..=128` bytes: inline `Message` payload.
- `129..=1024` bytes: fragmentation protocol (single logical message across inline fragments).
- `>1024` bytes: shared-memory transfer path by default.

## Notes

- This benchmark measures envelope construction only; end-to-end transport cost depends on endpoint queueing, scheduling, and map/release lifecycle.
- Thresholds can be revisited in a later perf phase if measured production traces disagree.
