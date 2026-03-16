# Server Roadmap (YARM)

This roadmap tracks user-space server maturation from current scaffolds to a minimal production-capable server set.

## Ownership model

- **Kernel mechanisms owner**: kernel team (capability model, IPC transport, trap/IRQ routing, VM primitives)
- **Core control-plane servers owner**: core runtime team (`init.srv`, `procman.srv`, `vfs.srv`)
- **Driver servers owner**: device/runtime team (`*.drv.srv`, IRQ/DMA/IOMMU delegation)
- **Networking servers owner**: net team (`netmgr.srv`, `tcpip.srv`, `dns.srv`, `dhcp.srv`)
- **UI/display servers owner**: graphics/input team (`display.srv`, `compositor.srv`, `input.srv`)

## Milestones and test gates


## Service domain layout

- `src/services/control_plane/*` for init/procman/vfs/supervisor service implementations.
- `src/services/fs/*` for filesystem and storage-facing services.
- `src/services/drivers/*` for hardware/transport driver services.
- `src/services/network/*` for networking services.
- `src/services/ui/*` for display/input/session services.
- `src/services/compatibility/*` for personality/compatibility servers.

## Phase 0 — Stabilize Core Control Plane ✅

### Scope

- `init.srv`: deterministic launch ordering and restart-policy table sanity checks.
- `procman.srv`: wait/reap lifecycle policy hardening (non-parent and unknown target rejection).
- `vfs.srv`: mount namespace policy gate + deterministic operation ordering counter.

### Implemented

- `InitServerLite` now carries a baseline restart-policy table, validates policy sanity, and records deterministic launch order (`procman -> vfs -> supervisor`).
- `ProcessManagerLite` wait-path now rejects unknown targets and enforces parent ownership more strictly.
- `VfsLiteService` now supports mount namespace policy (allow/deny boot-path classes) and deterministic op-sequence accounting for successful requests.

### Test gates (must pass)

- init gates:
  - launch order deterministic
  - invalid restart policy rejected
  - begin-running requires installed fault handoff
- process gates:
  - waitpid non-parent denied
  - waitpid unknown-target denied
  - exited child is reaped exactly once
- vfs gates:
  - mount policy denial enforced on `/dev/console` path
  - op sequence increments monotonically for successful operations

## Phase 1 — File System Servers (basic set)

### Target servers

1. `ramfs.srv`
2. `initramfs.srv`
3. `devfs.srv`
4. `ext4.srv` (or `fat.srv` as first persistent FS)
5. optional `blkcache.srv`

### Test gates

- protocol gate: typed versioned codecs + golden vectors
- mount gate: namespace + mount route tests
- lifecycle gate: mount/unmount + failure/recovery tests

### Current implementation status

- ✅ `ramfs.srv` scaffold implemented (`services/fs/ramfs/*` + thin `src/bin/ramfs_srv.rs` entrypoint).
- ✅ `initramfs.srv` scaffold implemented (`services/fs/initramfs/*` + thin `src/bin/initramfs_srv.rs` entrypoint).
- ✅ `devfs.srv` scaffold implemented (`services/fs/devfs/*` + thin `src/bin/devfs_srv.rs` entrypoint (console/null nodes)).
- ✅ `ext4.srv` scaffold implemented (`services/fs/ext4/*` + thin `src/bin/ext4_srv.rs` entrypoint).

- 🚧 `fat.srv` scaffold started (`services/fs/fat/*` + thin `src/bin/fat_srv.rs` entrypoint).
- 🚧 `blkcache.srv` scaffold started (`services/fs/blkcache/*` + thin `src/bin/blkcache_srv.rs` entrypoint).
- 🚧 `virtio_blk.srv` scaffold started (`services/drivers/virtio_blk/*` + thin `src/bin/virtio_blk_srv.rs` entrypoint).

## Phase 2 — Device Driver Servers

### Target servers

- `irqmux.srv`
- `uart.srv`
- `virtio_blk.srv`
- `virtio_net.srv`
- `virtio_gpu.srv`
- `input.srv`

### Current implementation status

- ✅ `virtio_blk.srv` scaffold implemented (`services/drivers/virtio_blk/*` + thin `src/bin/virtio_blk_srv.rs`).
- 🚧 `uart.srv` scaffold started (`services/drivers/uart/*` + thin `src/bin/uart_srv.rs`).
- 🚧 `virtio_net.srv` scaffold started (`services/drivers/virtio_net/*` + thin `src/bin/virtio_net_srv.rs`).
- 🚧 `virtio_gpu.srv` scaffold started (`services/drivers/virtio_gpu/*` + thin `src/bin/virtio_gpu_srv.rs`).
- 🚧 `input.srv` scaffold started (`services/drivers/input/*` + thin `src/bin/input_srv.rs`).

### Test gates

- delegation gate: init->driver role edges and cap bundle validation
- fault gate: revoke/restart behavior deterministic and test-covered

## Phase 3 — Networking Servers

### Target servers

- `netmgr.srv`
- `tcpip.srv`
- `dns.srv`
- `dhcp.srv`
- `socket.srv` adapter

### Test gates

- deterministic packet path simulation
- timeout/retry policy reproducibility
- compatibility adapter vector tests

## Phase 4 — Display + UI input servers

### Target servers

- `display.srv`
- `compositor.srv`
- `input.srv` (if not complete in phase 2)
- `shell.srv` / session manager

### Test gates

- boot-to-shell marker in QEMU log
- input event routing deterministic replay
- display mode-set and frame-present checks

## Architecture follow-up status (completed)

- ✅ Next move 1: `kernel::vfs` promoted as primary API (with `vfs_lite` compatibility shim and migrated imports).
- ✅ Next move 2: typed VFS request/response wrappers added in `kernel::vfs` and adopted by service entry/service tests.
- ✅ Next move 3: FAT scaffold now models directory entries + cluster growth and typed VFS messaging path.
- ✅ Next move 4: blkcache now has policy knobs + writeback scheduling and is integrated by FAT/EXT4 backends.
- ✅ Next move 5: init launch flow now records mount execution status, with deterministic recovery/fallback simulation telemetry.
- ✅ Next move 6: CI/service boundary gate added (`scripts/check-service-arch-boundary.sh`) and wired into compat gates workflow.
- ✅ Storage service contract published (`STORAGE_SERVICE_CONTRACT.md`) for blkcache/fat/ext4/virtio_blk protocol stability.

## Readiness criteria

Phase N is considered complete only when:

- implementation exists,
- test gates for that phase pass in CI,
- contract docs are updated,
- deterministic simulations cover the new server interactions.
