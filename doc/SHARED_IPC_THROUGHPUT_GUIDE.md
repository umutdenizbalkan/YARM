<!-- SPDX-License-Identifier: Apache-2.0 -->

# Shared IPC Throughput Guide (FS / Network / Display)

This guide captures recommended batching/ring usage patterns for the shared-memory IPC fast path.

## Common contract

- Prefer one long-lived data endpoint per producer/consumer pair.
- Use a ring descriptor in shared memory (`head`, `tail`, `entries[]`) and transfer only capabilities for reusable page-aligned regions.
- Receiver should call `IpcRecv` with auto-map target VA and then keep the mapping hot until ring pressure requires recycling.
- Recycle with `TransferRelease` fast path (`ptr=0`, `len=0`) when an active transfer mapping record exists.

## FS servers (large read/write)

- Batch adjacent file blocks into 64 KiB+ transfer windows when possible.
- Keep 2-4 in-flight transfer regions per client to overlap disk and user copy completion.
- Use ring watermarking:
  - low watermark: request refill,
  - high watermark: stop issuing new read windows.

## Network servers (RX/TX)

- Use fixed-size packet slot rings (e.g. MTU-sized or jumbo-sized classes).
- Reserve separate RX/TX rings to avoid cross-direction cache thrash.
- Return consumed RX slots in batches (every N packets or every poll tick).

## Display servers (framebuffer updates)

- Prefer tile/dirty-rect rings over full-frame transfers.
- Use stable backing mappings for frequently updated regions.
- Batch tile commit notifications so one control message can acknowledge multiple transfer ids.

## Operational notes

- Track `shared_mem_bytes_mapped`, `shared_mem_bytes_released`, and `transfer_release_calls` to validate reuse efficiency.
- If `shared_mem_bytes_mapped` grows much faster than released bytes under steady load, tune ring depth and release cadence.
