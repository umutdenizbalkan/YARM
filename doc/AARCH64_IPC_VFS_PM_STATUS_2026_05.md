<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Umut Deniz Balkan -->

# YARM Context Seed — AArch64 IPC/VFS/PM Status (May 2026)

## Current high-level state

AArch64 userspace startup, PM orchestration, SpawnV5 flow, recv-v2 IPC semantics, blocked waiter completion, and reply-cap handling are now largely stabilized.

The system successfully spawns this service chain:

- `tid=10000` `initramfs_srv`
- `tid=10001` `devfs_srv`
- `tid=10002` `vfs_server`
- `tid=10003` `driver_manager`
- `tid=10004` `blkcache_srv`
- `tid=10005` `virtio_blk_srv`

Observed failures in this path are cleared:

- `InvalidCapability`
- `WrongObject`
- `StaleCapability`
- `ret0_nonzero_meta_unset`
- trap-handler fatal failures

## Finalized IPC architecture changes

1. `ipc_call` semantics
   - `ipc_call` is send/queue only.
   - No inline syscall reply consumption.
   - Callers consume replies via `ipc_recv_v2` or timeout variants.

2. recv-v2 ABI contract
   - recv-v2 metadata is out-meta only.
   - return registers carry syscall success/error only.
   - no reply metadata in return lanes.
   - no inline reply prefix stripping for plain replies.
   - reply payloads are unchanged.

3. Portable blocked recv-v2 completion
   - Generic `BlockedRecvState` is stored in task state.
   - Tracks recv cap, payload ptr/len, meta ptr/len, and recv ABI variant.
   - Delivery-time completion copies payload + 40-byte recv-v2 meta and resumes waiter with syscall success.
   - No ISA-specific logic in generic IPC/syscall code.

4. Syscall replay removal
   - Old blocked-recv replay model removed.
   - No stale `x0` leakage.
   - No userspace retry workaround.
   - Waiter wake/complete occurs at delivery time.

5. Reply-cap materialization stabilization
   - One-shot reply objects are consumed once.
   - No enqueue-after-blocked-completion duplication.
   - Reply objects materialized once, receiver-local only.
   - Raw reply handles are never exposed to userspace.

6. `ipc_reply` waiter-completion parity
   - `ipc_reply` now completes blocked waiters directly.
   - No duplicate enqueue on reply path.

7. PM SpawnV5 reply-path stabilization
   - Fixed reply-prefix stripping corruption.
   - Fixed nonblocking recv race.
   - Fixed shared reply-endpoint cross-contamination.
   - Fixed stale ret0 wake behavior.
   - Fixed duplicate reply-cap materialization.

## Final IPC contract snapshot

### `ipc_call`

- request send/queue only.

### `ipc_recv_v2`

- `ret0` = syscall success/error only.
- all metadata in `IpcRecvMetaV2`.

### blocked recv-v2

- portable blocked state.
- delivery-time payload/meta copy.
- one-shot message consumption.
- no syscall replay.
- no retry workaround.

### Capability materialization

- receiver-local cap ids only.
- reply/transfer caps materialized before meta write.
- no raw reply handles exposed.
- manually embedding raw cap-like values into message transfer fields is invalid; materialization requires legitimate call/send transfer flow.
- one delivered message materializes at most one receiver-local cap; replay/rematerialization from that same message is not possible.
- reply caps remain one-shot.

## Regression coverage (May 2026 hardening batch)

- `recv_v2_blocked_waiter_direct_delivery_consumes_exactly_once`
  - protects against blocked waiter replay/duplicate-enqueue regressions and validates delivery-time payload/meta copy to waiter user buffers.
- `ipc_reply_wakes_blocked_recv_v2_waiter_without_duplicate_enqueue`
  - protects reply-path parity so `ipc_reply` completes blocked recv-v2 waiters directly without queue duplication or stale-cap reuse behavior.
- `recv_v2_reports_metadata_only_via_out_meta_and_preserves_plain_reply_payload`
  - protects recv-v2 out-meta-only ABI (no metadata-in-register lanes) and plain-reply payload integrity (no prefix stripping/mutation).
- `recv_v2_materializes_reply_cap_once_per_message`
  - protects one-shot receiver-local cap materialization semantics: exactly one materialized cap per delivered message and no second receive/rematerialization.

## Hosted-dev test harness note (non-ABI)

- hosted-dev sparse user-memory backing guarantees readability only for bytes actually written by the kernel/user-memory path in these tests.
- recv-v2 tests should read back the actual payload length written, not full receive-buffer capacity.
- syscall/blocked-recv tests must pass mapped user virtual addresses as user pointers; host stack pointers are invalid for user copy paths.

## Portability boundary

Generic portable code:

- blocked recv state
- delivery-time completion
- payload/meta copy
- capability materialization
- waiter lifecycle
- abstract syscall completion

Architecture-specific code:

- register mapping only (`x0`/`rax`/`a0`)
- ISA-specific resume semantics only

No ISA-specific assumptions are introduced in generic IPC or syscall logic.

## Debug/noise cleanup status

Removed temporary high-noise traces:

- SpawnV5 byte dumps
- recv-v2 retry spam
- InvalidCapability argument spam
- deep reply wrong-object spam
- cap materialization success spam

Useful lifecycle/debug markers were retained.

## Warning status

Fixed:

- unused `mut` in aarch64 user-rt
- duplicate `#[inline]`
- unused imports
- visibility warnings
- hosted-dev cfg warning

Intentionally left:

- `made_progress` unused-assignment warning
  - likely tied to supervisor-loop semantics
  - to be audited separately

## Near-term goals

1. Add targeted IPC tests
   - blocked reply waiter completion
   - one-shot reply consumption
   - duplicate materialization prevention
   - recv-v2 out-meta correctness
   - timeout cleanup paths

2. Reduce remaining verbose IPC tracing further.
3. Document finalized IPC model (`SYSCALL_ABI.md`, recv-v2 semantics, blocked completion lifecycle, portability boundaries, reply-cap semantics).
4. Continue VFS bring-up (routing, path normalization, mount-registration cleanup, per-client fd lifecycle hardening).
5. Enable `VFS_READ_SHARED_REPLY_ENABLED` only after VFS stabilization.
6. Continue PM lifecycle work (supervised task table, restart-token wiring, service lifecycle bookkeeping, restart policy).
7. Longer-term storage/platform goals (blkcache maturity, virtio-blk real I/O, driver-manager orchestration, PCI/DTB enumeration, DMA/MMIO grant flow, ext4/fat/ramfs expansion).

## PM exec-load source policy (current)

- For service/image IDs `1..=3` (bootstrap-critical), PM keeps the direct
  kernel spawn path because these services must be reachable before VFS is
  guaranteed.
- For service/image IDs `4..=6` (`initramfs_srv`, `devfs_srv`, `vfs_server`),
  PM keeps direct-initrd/bootstrap spawn so those services can come up before
  VFS-backed executable loading is available.
- After the bootstrap VFS chain is live, service/image IDs `7..=9`
  (`driver_manager`, `blkcache_srv`, `virtio_blk_srv`) use canonical
  initramfs paths and VFS `STATX -> OPENAT -> READ* -> CLOSE`, then spawn via
  `spawn_process_from_user_buf`.
- PM performs VFS-backed `7..=9` loads only when it is explicitly given a
  `vfs_server` request SEND cap (currently passed from init in SpawnV5 service
  caps slot 0). Missing cap is reported as a truthful spawn failure.
- PM nested/outbound VFS calls use a **dedicated PM-owned reply RECEIVE cap**
  in startup slot 2 (`process_manager_reply_recv_cap`). This cap is created as
  a separate endpoint during boot wiring and must be distinct from PM's main
  request receive endpoint (startup slot 17 / `pm_request_recv_cap`).
- VFS errors are surfaced as spawn failures; PM does not silently mask failures
  by pretending VFS-based loads succeeded.
- Spawn-source logging remains explicit to avoid duplicate-path ambiguity.
- `VFS_READ_SHARED_REPLY_ENABLED` remains disabled in this phase.

## Architectural status

IPC is no longer in unstable bring-up mode.
The recv-v2 + reply-cap + blocked-waiter model is coherent and portable.
Focus can move toward VFS, PM lifecycle, and storage-stack progress.
