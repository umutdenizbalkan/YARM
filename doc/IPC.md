<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Umut Deniz Balkan -->

# YARM IPC

> **Ownership rule.** All IPC documentation — message framing, fragmentation
> policy, shared-memory fastpath, throughput patterns, migration / phase
> history — lives here. New IPC fragment files are forbidden; update this doc
> instead. The per-syscall public ABI is in `doc/SYSCALL_ABI.md` (canonical);
> the typed wire codec versions are in `doc/VFS.md` §6. See
> `doc/DOCUMENTATION_MAP.md`.

For the finalized recv-v2 / reply-cap contract see `doc/ARCH_AARCH64.md` §4
(the portable AArch64 reference). For the kernel-side directive split
status (D1 / D2 / D5 routers and fallbacks) see `doc/KERNEL_UNLOCKING.md`
§2 and §6.

---

## 1. Frozen payload + framing policy

- **Inline payload capacity:** `Message::MAX_PAYLOAD = 128` bytes, frozen.
- Medium payloads (`129..=1024` bytes) use the fragmentation protocol (§2).
- Large payloads (`>1024` bytes) use the shared-memory descriptor path
  (`OPCODE_SHARED_MEM`) with auto-map on receive (§3).

Phase 1 benchmark snapshot (historical, see `doc/PROJECT_HISTORY.md`):
`inline64 = 94.96 ns/op`, `inline128 = 96.80 ns/op`,
`shared_desc = 80.93 ns/op`, `simulated_2x128 = 193.61 ns/op`.

---

## 2. Medium-payload fragmentation protocol

Each fragment is a normal `Message` payload with this fixed 12-byte prefix:

| Field | Type | Size |
|-------|------|------|
| `message_id` | `u32` | 4 |
| `fragment_index` | `u16` | 2 |
| `fragment_count` | `u16` | 2 |
| `fragment_len` | `u16` | 2 |
| `reserved` | `u16` | 2 |
| (data) | `[u8; fragment_len]` | up to 116 |

Usable fragment data per message: `MAX_PAYLOAD (128) − prefix (12) = 116`
bytes.

### Sender rules

1. Generate a non-zero `message_id` unique per sender endpoint stream.
2. Compute `fragment_count = ceil(total_len / 116)`.
3. Emit fragments in index order (`0..fragment_count-1`).
4. Use the consistent opcode for all fragments of the same logical message.

### Receiver rules

1. Group by `(sender_tid, opcode, message_id)`.
2. Reject duplicate fragment indexes.
3. Require all fragments to arrive before exposing the reassembled payload.
4. Drop partial assemblies on timeout / sender death / endpoint teardown.

---

## 3. Shared-memory fastpath

Receiver auto-map shared-memory delivery with explicit lifecycle and
revocation. The full phased plan is closed; current live behavior:

### Transfer object model

Dedicated kernel record: `(transfer_id, source_tid, receiver_tid,
endpoint_binding, memory_object_id / dma_region_id, byte_range, rights_mask,
generation)`. Explicit states: `Created → MappedReceiver → MappedBoth →
(Released | Revoked)`. Telemetry counters track creation /
materialization / revocation / failures and map/release parity.

### Receiver auto-map plumbing

On `IpcRecv` of `OPCODE_SHARED_MEM`:

1. Recv-side map request contract = `(target_VA, map_flags,
   optional fixed/anywhere policy)`.
2. Receiver pages are mapped automatically from the transferred capability
   according to policy.
3. Result metadata (`mapped_VA`, `mapped_length`, `transfer_id`) is returned
   through syscall return lanes.

Partial-map mid-range mapping faults trigger rollback (no half-mapped
state). The legacy descriptor return path remains as a compatibility
fallback behind an ABI gate.

### Sender / receiver dual-map + pinning lifecycle

- Pin/unpin rules pin shared transfer frames while either side holds active
  mappings.
- Map refcounts are updated for both sides and survive task scheduling /
  restart boundaries.
- The unmap / release syscall path drops active mapping references and
  transitions transfer state.

### Telemetry contract

Track `shared_mem_bytes_mapped`, `shared_mem_bytes_released`, and
`transfer_release_calls`. If `_mapped` grows much faster than `_released`
under steady load, tune ring depth and release cadence.

Reclamation tests must prove no early free while mappings remain.

---

## 4. Throughput patterns (FS / network / display)

### Common contract

- Prefer **one long-lived data endpoint per producer/consumer pair**.
- Use a **ring descriptor in shared memory** (`head`, `tail`, `entries[]`)
  and transfer only capabilities for reusable page-aligned regions.
- Receiver calls `IpcRecv` with an auto-map target VA and keeps the mapping
  hot until ring pressure requires recycling.
- Recycle with `TransferRelease` fast path (`ptr=0`, `len=0`) when an
  active transfer mapping record exists.

### FS servers (large read / write)

- Batch adjacent file blocks into **64 KiB+ transfer windows** when
  possible.
- Keep **2–4 in-flight transfer regions per client** to overlap disk and
  user-copy completion.
- Ring watermarking: low watermark → request refill; high watermark → stop
  issuing new read windows.

### Network servers (RX / TX)

- Use fixed-size packet slot rings (MTU-sized or jumbo-sized classes).
- Reserve separate RX / TX rings to avoid cross-direction cache thrash.
- Return consumed RX slots in batches (every `N` packets or every poll
  tick).

### Display servers (framebuffer updates)

- Prefer tile / dirty-rect rings over full-frame transfers.
- Use stable backing mappings for frequently updated regions.
- Batch tile commit notifications so one control message can acknowledge
  multiple transfer ids.

---

## 5. Shared-IPC migration ownership

- **ABI opcode/payload ownership:** `crates/yarm-ipc-abi`.
- **Shared service-side helper / runtime glue:** `crates/yarm-srv-common`.
- **Service implementation ownership:** extracted server crates
  (`yarm-*-servers`).

### Migration rule

When migrating an IPC surface:

1. Define / freeze the request+reply codec in `yarm-ipc-abi`.
2. Use shared decode / reply helpers from `yarm-srv-common` where
   applicable.
3. Keep policy / orchestration in service crates, **not** kernel.
4. Add deterministic tests in the owning service crate.

### Shared-memory flow expectations

For transfer-cap / shared-memory flows:

1. Receive / map through the current IPC contract.
2. Consume in a bounded region.
3. Release transfer mapping (`TransferRelease`) to avoid leaks / drift.

### Gate expectations

- `scripts/phase7-shared-ipc-gates.sh` is the shared-IPC migration check.
- Map / release parity must remain green in canary tests
  (`transfer_records_created == transfer_records_revoked`;
  `shared_mem_bytes_mapped == shared_mem_bytes_released`).

---

## 6. Finalized IPC ABI summary

(Full contract: `doc/ARCH_AARCH64.md` §4; per-syscall ABI: `doc/SYSCALL_ABI.md`.)

- **`ipc_call`** is send / queue only. No inline syscall reply consumption.
- **`ipc_recv_v2`** ABI: `ret0` carries syscall success / error only; all
  metadata in `IpcRecvMetaV2` (out-meta only); no inline reply prefix
  stripping for plain replies.
- **Portable blocked recv-v2 completion** with delivery-time payload + 40-byte
  meta copy; one-shot message consumption; no syscall replay; no retry
  workaround.
- **`ipc_reply`** completes blocked recv-v2 waiters directly; no duplicate
  enqueue on the reply path.
- **Reply-cap materialization:** receiver-local CapIDs only; reply caps are
  one-shot; raw reply handles are never exposed to userspace.
- **`recv_shared_v3`** ABI offsets are frozen (see `doc/KERNEL_UNLOCKING.md`
  §3).

### Regression coverage (regression-set)

- `recv_v2_blocked_waiter_direct_delivery_consumes_exactly_once`
- `ipc_reply_wakes_blocked_recv_v2_waiter_without_duplicate_enqueue`
- `recv_v2_reports_metadata_only_via_out_meta_and_preserves_plain_reply_payload`
- `recv_v2_materializes_reply_cap_once_per_message`

---

## 7. Phase 5 — shared-memory transfer hardening artifacts

These invariants remain live:

- `IpcSend` large-payload transfer path requires transfer-cap rights
  `READ | MAP` before descriptor send.
- Repeated rejection due to missing transfer rights leaves
  `transfer_records_created` unchanged (`0`).
- Shared-memory recv validation / map failures revoke materialized
  transfer caps (no leaked receiver-local cap on failure).
- Receiver mapping-intent validation: `read` required; unknown bits
  rejected; write-intent rejected unless materialized transfer cap
  includes `WRITE`; read-only intent attenuates receiver-local cap to
  `READ | MAP` (drops `WRITE`).
- Repeated recv map-intent / write-intent failures keep
  `shared_mem_bytes_mapped` and `_released` at `0` (no accounting drift).
- Process-cleanup purge of active shared-memory transfer mappings records
  `shared_mem_bytes_released`.
- Direct transfer-cap revoke force-unmap records released-byte telemetry.
- Mixed cleanup / revoke keeps both invariants stable:
  `transfer_records_revoked >= transfer_records_created` (no stale
  records); `shared_mem_bytes_mapped == shared_mem_bytes_released`.

---

## 8. Authoring rule

Future IPC changes must update this doc and the relevant per-syscall ABI
in `doc/SYSCALL_ABI.md` (or the typed codec in `doc/VFS.md` §6).
Do **not** create new `IPC_*` / `SHARED_IPC_*` fragment files. Closed
phase / milestone outcomes belong in `doc/PROJECT_HISTORY.md`.
