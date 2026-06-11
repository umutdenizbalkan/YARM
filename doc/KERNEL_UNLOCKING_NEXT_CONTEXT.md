// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

# YARM Kernel Unlocking — Next Context Handoff Seed

**Written at:** Stage 94/100 — Optional FS Milestone 1 closure
**Branch:** `claude/wizardly-sagan-SL81B`

This document is the handoff seed for resuming kernel-unlocking work after
Optional FS Milestone 1. Filesystem work is paused (regressions only).
The recommended next stage is Stage 101 — Kernel unlocking restart.

---

## 1. Filesystem work is paused

After Optional FS Milestone 1, filesystem work is **paused** except:
- Regressions in existing RAMFS/ext4 behavior.
- Regressions in optional-FS smoke scripts.
- Emergency fixes to the FAT profile gate (no code changes).

Do NOT start new FS feature work (FAT production enablement, ext4 writes,
shared-I/O production wiring, POSIX API) until a dedicated future FS milestone
is explicitly opened.

---

## 2. Current clean FS baseline

| Filesystem | Status | Mount | Writes |
|-----------|--------|-------|--------|
| RAMFS | ✓ live | `/ram` | ✓ read-write |
| ext4 | ✓ live | `/ext4` | `Unsupported` |
| FAT | profile-ready, disabled | `/fat` (blocked) | `Unsupported` |
| initramfs | ✓ live | `/initramfs` | n/a |

Strict optional-FS smoke passes on both x86_64 and AArch64 with:
- `INIT_SPAWN_V5_WRONG_SENDER_REPLY` count=0
- `KSPAWN_EXTRA_CAP_DELEGATE_FAIL` count=0
- `INIT_RAMFS_SPAWN_FAIL` absent
- `INIT_EXT4_SPAWN_FAIL` absent

Test counts: 572 yarm-fs-servers + 130 yarm-control-plane-servers = 702, 0 failures.
Workspace total: 1282 tests, 0 failures.

---

## 3. Invariants that must not be broken during kernel unlocking

The following invariants are required for the FS layer to continue working correctly
after any kernel-unlocking change. Violating any of them without a corresponding FS
update will silently break RAMFS, ext4, or FAT production paths.

### SpawnV5 ABI

`spawn_v5_cap(pm_send, pm_recv, image_id, service_caps, parent_pid)` returns
`Option<(pid, service_send_cap)>` encoded in a 16-byte reply (`SpawnV5CapResult::ENCODED_LEN = 16`).

- Do not change the argument layout.
- Do not change the 16-byte reply encoding.
- Do not change `service_caps` slot semantics: slots are kernel cap-transfer slots only,
  never payload integers.

### Image IDs (frozen)

```
7  = driver_manager
8  = blkcache_srv
9  = virtio_blk_srv
10 = fat_srv
11 = ramfs_srv
12 = ext4_srv
```

Do not change any image ID without updating `spawn_image_path_for_image_id`,
`InitramfsBackend`, CPIO packing, and all documentation.

### Startup slot count

`STARTUP_SLOT_COUNT = 18` — do not increase or decrease.
Slots 0–17 are documented in `doc/INIT_SERVER_BOOT_CONTRACT.md`.
Slot 12 is PM-private for PM↔VFS subcalls.

### Syscall count

`SYSCALL_COUNT = 31` — do not add or remove syscalls without a new ABI stage.

### recv_shared_v3 ABI offsets

`RecvSharedV3Delivery` field offsets are frozen. Do not change struct layout.

### Optional-FS smoke markers

The following markers are checked by `qemu-*-optional-fs-smoke.sh`. Do not rename
or remove them from source code without updating both smoke scripts:

- `INIT_PM_RECV_DRAIN_DONE count=N`
- `INIT_RAMFS_SPAWN_OK`, `RAMFS_SRV_ENTRY`, `RAMFS_MOUNT_READY`, `VFS_MOUNT_REGISTER_RAMFS_OK`
- `INIT_EXT4_SPAWN_OK`, `EXT4_SRV_ENTRY`, `EXT4_SRV_READY`, `VFS_MOUNT_REGISTER_EXT4_OK`
- `INIT_FAT_SPAWN_SKIPPED reason=server_disabled`

### PM/private reply endpoint isolation

`pm_recv` (the shared PM receive endpoint) must be drained before each new protocol phase.
The drain pattern (`INIT_PM_RECV_DRAIN_BEGIN` / `INIT_PM_RECV_DRAIN_DONE`) must remain
in `init/service.rs` before any SpawnV5 call. Do not remove the drain loop.

### No deadline-0 required replies

`ipc_recv_with_deadline(ep, 0)` is a non-blocking poll. It must **never** be used
as a required-reply receive for VFS IPC helpers (`vfs_statx`, `vfs_openat`, `vfs_read`,
`vfs_close`) or for `IpcBlockDevice::read_exact_at` / `write_sector`.
All four VFS helpers and both IpcBlockDevice methods use `ipc_recv_v2` (blocking).

### Initramfs path table completeness

`spawn_image_path_for_image_id()` must cover all image IDs 0–12. Adding a new sbin
server requires: bump `MAX_INITRAMFS_INODES`, add inode entry, add `from_cpio_newc`
match arm, and add a path test.

### VM mapping and drain invariants

`vm.rs` `Result`/`DrainedMapping` semantics must not change. Two-phase unmap
(phase 1 = PTE removal, phase 2 = TLB shootdown + reclaim) must remain ordered.
`VmBrk` shrink, `VmAnonMap` rollback, and `TransferRelease` all rely on this order.

### Scheduler membership invariants

Scheduler slot/runqueue mutual exclusion, tombstone reuse, and idle re-enqueue
after `dispatch_next_task` must remain intact. See `KERNEL_TEST_RULES.md` Rules 1–2.

### VFS_SUPERVISOR_TASK_EXIT_NOTIFICATION_ENABLED

Remains `false`. Do not change.

---

## 4. Recent kernel correctness fixes to preserve

These fixes were landed in Stages 81–93 and address real hardware/scheduler bugs.
Do not revert or accidentally re-break them during kernel unlocking.

### Scheduler membership / runqueue mutual exclusion

Fixed in Stage 8x: scheduler membership slots and runqueue operations are now
mutually exclusive. Tombstone reuse (after task exit) is safe. Tests in
`KERNEL_TEST_RULES.md` Rules 1–2 and `stage9x_tests` suites must continue to pass.

### vm.rs map/unmap/drain/page_align/BBM fixes

Fixed in Stage 5x–8x: correct ordering of PTE write, TLB shootdown, and physical
frame reclaim. `VmAnonMap`, `VmBrk`, `TransferRelease`, and `map_shared_region_into_receiver`
all use two-phase unmap. Stage 5C–8 test suites must continue to pass.

### Stage 81A syscall error parity

`handle_trap`'s `Trap::Syscall` arm encodes errors into the trapframe instead of
propagating them to the kernel fatal path. This fix allows `spawn_image_path_for_image_id`
returning `InvalidArgs` to be handled gracefully by PM (not kernel-halt on AArch64).
Do not revert to the `?` propagation pattern.

### Stage 92 vfs_client blocking-receive fix

All four `vfs_client.rs` IPC helpers use `ipc_recv_v2` (blocking). The Stage 91
wrong-sender drain loop remains as defense-in-depth but fires 0 times. Do not
introduce any new `ipc_recv_with_deadline(_, 0)` in required-reply paths.

### Stage 93 IpcBlockDevice blocking-receive fix

`IpcBlockDevice::read_exact_at` and `write_sector` use `ipc_recv_v2` (blocking).
Same root cause as Stage 92. Latent bug; would cause `FatError::Io` on slow schedulers.

### BT2 LAPIC timer fix (x86_64)

BSP LAPIC timer is armed exactly once via `start_bsp_periodic_timer(kernel)` in
`run_scheduler_loop()`, after `signal_bootstrap_scheduler_ready()`. The early arming
in `init_lapic_mmio_base()` was removed. Do not re-introduce early timer arming.

---

## 5. Recommended next kernel-unlocking target

### Continue global KernelState decomposition

The active workstream at Stage 5B–8 is decomposing `KernelState` into domain
sub-locks to eliminate the coarse global `SpinLock<KernelState>` from trap/syscall
paths. The following is the recommended sequence:

1. **Audit trap/syscall paths for coarse borrows first.** Use the lock-rank audit
   from `KERNEL_LOCKING.md` to identify which paths still hold the global lock.
   Produce a ranked list before starting any conversion.

2. **Convert low-risk read-only helpers first.** Helpers that only read a single
   domain sub-lock (e.g., fault domain, telemetry domain) and return a `Copy`
   snapshot are safe starting points. Apply `Rule N+3` (equivalence test) before
   each conversion.

3. **Do not convert trap-boundary-sensitive TID reads.** `entering_tid` and
   `exiting_tid` in `yarm_x86_dispatch_trap_from_stub` are Class F (global lock
   required). Stage 4T+6 attempted this and was smoke-broken. See `Rule N+4`.

4. **Avoid x86_64 SMP** until the trampoline/assembly split in `src/arch/x86_64/smp.rs`
   is complete. Keep x86_64 smoke at `-smp 1`. See `AI_AGENT_RULES.md` Rule 5.1.

5. **Each conversion requires x86_64 `-smp 1` smoke** after unit tests pass.
   Unit-test value-equivalence is necessary but not sufficient (Stage 4T+6 proof).

6. **No VmAnonMap live conversion** without resolving the three blockers in
   `KERNEL_LOCKING.md §18.2` and obtaining x86_64 smoke approval.

### Secondary target: blkcache sector-reassembly proof

If FAT production is needed before a full kernel-unlocking pass:
- Prove `IpcBlockDevice::read_exact_at` end-to-end on QEMU virtio-blk.
- This is the single highest-value unblocking action for FAT production.
- See `OPTIONAL_FS_MILESTONE_1.md §10` for the full blocker list.

---

## 6. Suggested next stage name and description

**Stage 101 — Kernel unlocking restart / trap-syscall borrow audit**

Goals:
- Produce a complete lock-rank audit of all trap and syscall handlers.
- Identify the top 3–5 lowest-risk global-lock acquisition sites.
- Convert the first 1–2 read-only domain helpers with passing equivalence tests
  and x86_64 smoke.
- Update `KERNEL_LOCKING.md` §17 with the current decomposition state.

Constraints (all from §3 above):
- SYSCALL_COUNT = 31 unchanged.
- STARTUP_SLOT_COUNT = 18 unchanged.
- SpawnV5 ABI unchanged.
- Optional-FS smoke must continue to pass.
- All 702 FS userspace server tests must continue to pass.

Not acceptable:
- Converting Class F (trap-boundary TID) reads without smoke approval.
- Enabling x86_64 SMP.
- Reverting any correctness fix from §4.
- Touching filesystem server code (regressions only).
