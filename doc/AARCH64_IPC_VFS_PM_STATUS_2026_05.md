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
5. Add real initramfs-backed exec loading through VFS path.
6. Enable `VFS_READ_SHARED_REPLY_ENABLED` only after VFS stabilization.
7. Continue PM lifecycle work (supervised task table, restart-token wiring, service lifecycle bookkeeping, restart policy).
8. Longer-term storage/platform goals (blkcache maturity, virtio-blk real I/O, driver-manager orchestration, PCI/DTB enumeration, DMA/MMIO grant flow, ext4/fat/ramfs expansion).

## Architectural status

IPC is no longer in unstable bring-up mode.
The recv-v2 + reply-cap + blocked-waiter model is coherent and portable.
Focus can move toward VFS, PM lifecycle, and storage-stack progress.
