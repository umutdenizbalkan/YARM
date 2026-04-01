# Shared-memory IPC Migration Guide (Phase 6)

This guide describes how to migrate servers and clients to the frozen shared-memory IPC contract.

## What changed in Phase 6

- User-mode `IpcRecv` for `OPCODE_SHARED_MEM` now requires auto-map inputs (`ptr != 0`, sufficient `len`).
- Descriptor-only user receive fallback is removed.
- Transfer lifecycle must use:
  1. `IpcRecv` auto-map
  2. consume mapped region
  3. `TransferRelease` (explicit bounds or fast-path `ptr=0,len=0`)

## Sender-side requirements

1. For payloads larger than inline size, send a transfer-cap-enabled message.
2. Ensure descriptor offset/length remain page-aligned and bounded by the delegated memory object.
3. Reuse transfer windows to reduce allocation churn under steady throughput.

## Receiver-side requirements

1. Call `IpcRecv(recv_cap, target_va, budget_len, ...)` with page-aligned `target_va`.
2. Read mapped bytes from `ret` metadata and process in-place.
3. Call `TransferRelease(transfer_cap, 0, 0, ...)` for active-mapping fast-path recycle, or pass explicit bounds when needed.

## Compatibility checklist

- [ ] no user client relies on descriptor-only shared-memory receive
- [ ] every shared-memory receive path has a `TransferRelease` path
- [ ] server/client rings apply bounded in-flight transfer depth
- [ ] telemetry (`shared_mem_bytes_mapped`, `shared_mem_bytes_released`, `transfer_release_calls`) is monitored

## Rollout notes

- Roll out by service class: filesystem, network, display.
- Keep per-service canaries that verify map/release parity under load.
- Treat any sustained map>release byte drift as a leak/regression signal.
