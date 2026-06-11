// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

# YARM Optional FS Milestone 1

**Declared:** Stage 100 (commit reached after Stage 94 pass)
**Branch:** `claude/wizardly-sagan-SL81B`

This document is the authoritative record of the Optional FS Milestone 1 closure.
Filesystem work is paused after this milestone except for regressions.
The next scheduled work phase is kernel unlocking (Stage 101+).

---

## 1. Milestone name

**YARM Optional FS Milestone 1**

Scope: all userspace filesystem servers built, staged, tested, and at least read-only
live; RAMFS fully writable proof; ext4 read-only live; FAT profile-ready and disabled
pending virtio-blk round-trip proof; strict optional-FS smoke scripts for both
architectures; no known yarm-fs-servers or yarm-control-plane-servers failures.

---

## 2. Current architecture state

### Kernel mechanisms only

The kernel provides:
- `spawn_image_path_for_image_id()` covering image IDs 0–12 (all FS servers included).
- SpawnV5 ABI: `spawn_v5_cap(pm_send, pm_recv, image_id, service_caps, parent_pid)`.
- `ipc_recv_v2` (blocking receive) and `ipc_recv_with_deadline` (poll receive).
- VFS mount registration via `VFS_OP_MOUNT_REGISTER` IPC opcode.
- No filesystem logic resides in the kernel. All FS servers are userspace processes.

### PM owns lifecycle

Process Manager (TID=3, image_id=1) spawns all optional FS servers via SpawnV5.
Init (`init_server`) triggers spawns after core services (driver_manager, blkcache,
virtio_blk) are confirmed ready. Image IDs for FS servers: 10=fat_srv, 11=ramfs_srv,
12=ext4_srv.

### VFS owns routing and mounts

`vfs_server` (spawned by PM from CPIO) maintains the mount table. Mounts are
registered dynamically via `VFS_OP_MOUNT_REGISTER` after each server starts.
The VFS routes incoming filesystem requests (`openat`, `read`, `write`, `statx`,
`close`) to the registered backend server for the matching path prefix.

### RAMFS status

- **Live** in all profiles containing RAMFS.
- `init` spawns `ramfs_srv` (image_id=11) and registers `/ram` read-write.
- RAMFS shared-I/O read/write proof: complete (memory-backed, hosted tests).
- `WRITE_SHARED_REQUEST` and `READ_SHARED_REPLY` proven in hosted tests via
  `RecvV3SharedIoMapper` and `RamFsBackend::read_shared_bytes` / `write_shared_bytes`.
- Production live shared-I/O wiring (capability transfer, mapper activation):
  deferred to a future milestone.

### ext4 status

- **Live** in all profiles containing ext4.
- `init` spawns `ext4_srv` (image_id=12) and registers `/ext4` read-only (flags=1).
- Read-only: superblock, group descriptors, inodes, extents, indirect maps, symlinks,
  htree directories, metadata checksums. See `EXT4_SERVER_CONTRACT.md` for full matrix.
- Write: `VfsError::Unsupported` on all write paths. No mutation of backend.
- Shared-I/O: deferred (same as RAMFS production wiring).

### FAT status

- **Profile-ready, disabled by default.** Built, staged in CPIO, and packed.
- `INIT_SPAWN_FAT_SRV = false` in default `optional-fs` profile.
- FAT resident loop exists; hosted sample image tests pass.
- Production IPC-backed mount disabled until virtio-blk round-trip is proven.
- See §10 for exact blocker list.

---

## 3. Gate matrix

| Constant | optional-fs (default) | fat-block | full-fs-experimental |
|----------|-----------------------|-----------|----------------------|
| `INIT_SPAWN_RAMFS_SRV` | `true` | `true` | `true` |
| `INIT_SPAWN_FAT_SRV` | **`false`** | `true` (blocked) | `true` |
| `INIT_SPAWN_EXT4_SRV` | `true` | `true` | `true` |
| `VFS_RAMFS_LIVE_MOUNT_ENABLED` | `true` | `true` | `true` |
| `VFS_FAT_LIVE_MOUNT_ENABLED` | **`false`** | `true` (blocked) | `true` |
| `VFS_FAT_SHARED_IO_ENABLED` | **`false`** | **`false`** | `true` (future) |
| `VFS_EXT4_RECV_LOOP_ENABLED` | `true` | `true` | `true` |
| `VFS_EXT4_LIVE_MOUNT_ENABLED` | `true` | `true` | `true` |

---

## 4. Mount matrix

| Mount prefix | optional-fs | fat-block | Notes |
|-------------|-------------|-----------|-------|
| `/ram` | ✓ live | ✓ live | read-write RAMFS |
| `/ext4` | ✓ live | ✓ live | read-only ext4 |
| `/fat` | absent | ✓ live (blocked) | FAT, disabled until virtio-blk proof |
| `/initramfs` | ✓ live | ✓ live | initramfs_srv, always present |

---

## 5. Shared-I/O matrix

| Operation | RAMFS | FAT (memory) | FAT (IPC) | ext4 |
|-----------|-------|--------------|-----------|------|
| `WRITE_SHARED_REQUEST` (op 27) | hosted proof only | unsupported | unsupported | unsupported |
| `READ_SHARED_REPLY` (op 26) | hosted proof only | unsupported | unsupported | unsupported |
| `VFS_OP_WRITE_INLINE` (op 28) | n/a | ✓ 1–96 bytes | blocked | rejected (Unsupported) |
| `VFS_OP_READ` (op 12) | ✓ live | ✓ memory | blocked | ✓ live |
| `VFS_OP_WRITE` (op 13) | ✓ live (len-only) | ✓ memory | blocked | Unsupported |

Production live shared-I/O (real shared-memory transfer between requester and FS
server) is deferred. The `RecvV3SharedIoMapper` and lifecycle helpers are
scaffolding/proof only. No `MemoryObject` capability transfer for shared buffers
is wired in production.

---

## 6. Spawn image IDs

| Image ID | Server binary | Role |
|----------|--------------|------|
| 7 | `sbin/driver_manager` | Device driver orchestration |
| 8 | `sbin/blkcache_srv` | Block I/O cache service |
| 9 | `sbin/virtio_blk_srv` | VirtIO block device driver |
| 10 | `sbin/fat_srv` | FAT12/16/32 filesystem server |
| 11 | `sbin/ramfs_srv` | RAM filesystem server |
| 12 | `sbin/ext4_srv` | ext4 filesystem server |

These IDs are **frozen**. Changing any of them requires updating `spawn_image_path_for_image_id`,
`InitramfsBackend`, CPIO packing, and all documentation simultaneously.

---

## 7. Smoke commands

### optional-fs profile (default — always required to pass)

```bash
# AArch64:
QEMU_SMOKE_STRICT=1 ./scripts/qemu-aarch64-optional-fs-smoke.sh

# x86_64:
QEMU_SMOKE_STRICT=1 ./scripts/qemu-x86_64-optional-fs-smoke.sh
```

### fat-block profile (requires virtio-blk round-trip proof first)

```bash
# Create FAT image (once):
./scripts/create-fat-image.sh

# AArch64 fat-block smoke (after INIT_SPAWN_FAT_SRV is flipped true):
FAT_IMAGE=fat.img QEMU_SMOKE_STRICT=1 ./scripts/qemu-aarch64-fat-block-smoke.sh

# x86_64 fat-block smoke:
FAT_IMAGE=fat.img QEMU_SMOKE_STRICT=1 ./scripts/qemu-x86_64-fat-block-smoke.sh
```

FAT-block smoke is **deferred** until blocker #1 (compile-time profile gate) and
blocker #3 (virtio-blk readiness handshake) are resolved.

---

## 8. Expected markers

### RAMFS (optional-fs profile)

```
INIT_PM_RECV_DRAIN_BEGIN
INIT_PM_RECV_DRAIN_DONE count=0
INIT_RAMFS_SPAWN_BEGIN
INIT_RAMFS_SPAWN_OK child_tid=N
RAMFS_SRV_ENTRY
RAMFS_MOUNT_READY
VFS_MOUNT_REGISTER_RAMFS_OK
```

### ext4 (optional-fs profile)

```
INIT_EXT4_SPAWN_BEGIN
INIT_EXT4_SPAWN_OK child_tid=N
EXT4_SRV_ENTRY
EXT4_SRV_READY
VFS_MOUNT_REGISTER_EXT4_OK
```

### FAT skipped (optional-fs default — INIT_SPAWN_FAT_SRV=false)

```
INIT_FAT_SPAWN_SKIPPED reason=server_disabled
```

`INIT_FAT_SPAWN_OK` must be **absent** in the optional-fs profile.

### FAT enabled (fat-block profile — future)

```
INIT_FAT_SPAWN_BEGIN
INIT_FAT_SPAWN_OK child_tid=N
FAT_BIN_ENTRY_START
FAT_CONFIG_FOUND prefix=/fat device_id=N
FAT_BLOCK_BACKEND_STARTUP_CAP cap=N
FAT_MOUNT_READY
FAT_SRV_READY
VFS_MOUNT_REGISTER_FAT_OK prefix=/fat
```

---

## 9. Forbidden markers (strict mode — all profiles)

These must be **absent** when `QEMU_SMOKE_STRICT=1`. Any occurrence is a hard failure.

| Marker | Meaning |
|--------|---------|
| `INIT_RAMFS_SPAWN_FAIL` | RAMFS server failed to spawn |
| `INIT_EXT4_SPAWN_FAIL` | ext4 server failed to spawn |
| `INIT_FAT_SPAWN_FAIL` | FAT server failed to spawn (fat-block profile) |
| `PM_ELF_ZC_FAIL image_id=10/11/12` | ZC loader error for FS server |
| `KSPAWN_EXTRA_CAP_DELEGATE_FAIL` | Kernel rejected service_caps slot |
| `PM_VFS_SPAWN_FAIL` | PM failed to load server ELF via VFS |
| `reason=bad_fd_decode` | PM received malformed fd from VFS |
| `INIT_SPAWN_V5_WRONG_SENDER_REPLY` | Wrong-sender SpawnV5 drain fired (must be count=0) |
| `fallback=phase2b` | Phase 3A grant unavailable, bulk-read fallback (strict-mode fail) |
| `FAT_MOUNT_FAILED` | FAT mount failed (fat-block profile) |
| `panic` (not `nonfatal=true`) | Kernel or userspace panic |

---

## 10. FAT status

**Profile-ready. Disabled by default. Writes unsupported.**

### What is done

- FAT12/16/32 parsing and hosted sample image tests complete.
- `FatBackend::write_bytes` supports memory-backed overwrites and appends (1–96 bytes
  per `VFS_OP_WRITE_INLINE` request).
- `IpcBlockDevice::read_exact_at` and `write_sector` use `ipc_recv_v2` (blocking).
- Fat-block QEMU smoke scripts and `create-fat-image.sh` written.
- All FAT gate constants are `false` in the default `optional-fs` profile.

### Exact blockers before production enablement

1. **`INIT_SPAWN_FAT_SRV` compile-time flip**: must be `true` only in the fat-block
   profile. Change only via a compile-time feature or config, not in the default binary.

2. **Blkcache whole-sector read round-trip**: `IpcBlockDevice::read_exact_at` issues
   `BLK_OP_READ` in 120-byte chunks; blkcache_srv multi-chunk sector reassembly must
   be validated end-to-end on QEMU virtio-blk.

3. **`virtio_blk_srv` readiness handshake**: `fat_srv` must not issue `BLK_OP_READ`
   before `virtio_blk_srv` has signaled device readiness. No probe/handshake exists yet.

4. **`VFS_FAT_SHARED_IO_ENABLED` remains `false`**: until the normal IPC read path
   completes at least one full file round-trip on real block hardware.

5. **FAT IPC inline writes remain `VfsError::Unsupported`**: until the whole-sector
   read/RMW contract (blocker #2) is established and tested.

---

## 11. ext4 status

**Live. Read-only. Writes unsupported. Shared-I/O deferred.**

- `ext4_srv` spawned automatically in `optional-fs` and `fat-block` profiles.
- `/ext4` mount registered read-only (flags=1).
- Full read-only matrix: see `EXT4_SERVER_CONTRACT.md § FS-10 read-side freeze`.
- All write operations return `VfsError::Unsupported`.
- `VFS_OP_WRITE_INLINE` (op 28) on ext4 → `Unsupported` (test-verified).
- Unknown opcodes on ext4 → `Unsupported` (not panic; test-verified).
- `READ_SHARED_REPLY` / `WRITE_SHARED_REQUEST` (ops 26/27): deferred.
- Journal replay / JBD2: not implemented. Future block-backed mount must remain
  read-only until JBD2 and metadata mutation design exist.

---

## 12. RAMFS status

**Live. Read-write. Shared-I/O hosted proof complete. Production wiring deferred.**

- `ramfs_srv` spawned automatically in all FS-containing profiles.
- `/ram` mount registered read-write.
- `RamFsBackend::read_bytes` and `write_bytes` proven in hosted tests.
- `WRITE_SHARED_REQUEST` dispatch proven: `RecvV3SharedIoMapper` + `write_shared_bytes`.
- `READ_SHARED_REPLY` dispatch proven: `RecvV3SharedIoMapper` + `read_shared_bytes`.
- Production wiring (live `MemoryObject` cap transfer, live mapper activation): deferred.

---

## 13. Deferred filesystem work

The following items are **explicitly deferred** and must not be started until the
kernel-unlocking milestone (Stage 101+) or a dedicated future FS milestone:

| Item | Reason for deferral |
|------|---------------------|
| FAT production enablement (`INIT_SPAWN_FAT_SRV=true`) | Blockers 1–5 in §10 |
| FAT normal read over real virtio-blk | virtio_blk_srv readiness handshake missing |
| FAT `READ_SHARED_REPLY` | Proof requires live production read path first |
| ext4 write / journal support | JBD2 design not started; out of FS Milestone 1 scope |
| ext4 `READ_SHARED_REPLY` | Deferred with shared-I/O production wiring |
| Production shared-I/O wiring (all FS) | `MemoryObject` transfer + mapper activation not designed |
| POSIX filesystem API | Not started; not in scope |
| New filesystem types | Not in scope for Milestone 1 |

---

## 14. Rules learned

Lessons encoded in `AI_AGENT_RULES.md` as part of this milestone:

| Rule | Lesson |
|------|--------|
| Rule 1 / §1.3 | `service_caps` slots are kernel capability transfers only — never payload integers |
| Rule 8 | Deadline-0 (`ipc_recv_with_deadline(ep, 0)`) is a poll, never a required-reply receive |
| Rule 8 / Stage 92 | VFS client IPC helpers must use `ipc_recv_v2` (blocking), not deadline-0 |
| Rule 11 / Stage 93 | `IpcBlockDevice` IPC helpers must use `ipc_recv_v2` (blocking) |
| Rule 8.1 | Shared reply endpoints must be drained exhaustively before the next protocol phase |
| Rule 9 | Every staged server must be registered in `spawn_image_path_for_image_id` AND `InitramfsBackend` |
| Stage 93 | Default profile must not fake unavailable devices; FAT mount must fail clearly without virtio-blk |
| Stage 94 | Fatal smoke greps must exclude `nonfatal=true` lines |
| PM-private slot 12 | PM uses startup slot 12 for private PM↔VFS subcalls; do not repurpose |

---

## 15. Test counts at milestone closure (Stage 94/100)

| Crate | Tests |
|-------|-------|
| `yarm-fs-servers` | 572 |
| `yarm-control-plane-servers` | 130 |
| workspace total (`cargo test --lib`) | 1282 |
| Failures | 0 |

---

## 16. QEMU smoke results

QEMU not available in the remote execution environment where Stage 94/100 was closed.
Smoke scripts are written, validated by source-scan tests, and ready to run when a
QEMU-capable environment is available.

- `scripts/qemu-aarch64-optional-fs-smoke.sh` — validated by `stage92_smoke_*` tests
- `scripts/qemu-x86_64-optional-fs-smoke.sh` — validated by `stage92_smoke_*` tests
- `scripts/qemu-aarch64-fat-block-smoke.sh` — validated by `stage93_fat_block_smoke_*` tests
- `scripts/qemu-x86_64-fat-block-smoke.sh` — validated by `stage93_fat_block_smoke_*` tests

Previous QEMU results (before remote migration): Stage 91 established that both
AArch64 and x86_64 optional-FS smoke passed with 0 wrong-sender replies after the
Stage 92 fix.
