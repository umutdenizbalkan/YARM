# Server Roadmap (YARM)

This roadmap tracks user-space server maturation from current scaffolds to a minimal production-capable server set.

## Ownership model

- **Kernel mechanisms owner**: kernel team (capability model, IPC transport, trap/IRQ routing, VM primitives)
- **Core control-plane servers owner**: core runtime team (`init.srv`, `process_manager.srv`, `vfs.srv`)
- **Driver servers owner**: device/runtime team (`*.drv.srv`, IRQ/DMA/IOMMU delegation)
- **Networking servers owner**: net team (`netmgr.srv`, `tcpip.srv`, `dns.srv`, `dhcp.srv`)
- **UI/display servers owner**: graphics/input team (`display.srv`, `compositor.srv`, `input.srv`)

## Milestones and test gates


## Service domain layout

- `src/services/init/*` for init service policy, launch ordering, and mount orchestration.
- `src/services/control_plane/process_manager/*` for process-manager control-plane logic.
- `src/services/control_plane/vfs/*` for the control-plane VFS request loop and policy glue.
- `src/services/fs/*` for filesystem and storage-facing services such as ramfs/initramfs/devfs/ext4/fat/blkcache.
- `src/services/drivers/*` for hardware/transport driver services.
- `src/services/network/*` for networking services.
- `src/services/ui/*` for display/input/session services.
- `src/services/compatibility/*` for personality/compatibility servers.


## Immediate next steps (review-driven)

1. Convert all thin `src/bin/*_srv.rs` stubs to call real service entrypoints with explicit typed request loops (one service family per PR).
2. Add deterministic fault-injection tests for each service family (`control_plane`, `drivers`, `fs`, `network`, `ui`) and wire them to `compat-gates`.
3. Gate phase advancement on runnable profile evidence (QEMU smoke marker + deterministic sim + codec compatibility vectors).

## Restart ownership matrix

| Service | Primary restart owner | Fallback / recovery path |
| --- | --- | --- |
| `process_manager.srv` | `supervisor.srv` | mark degraded and alert `init.srv` if restart budget is exhausted |
| `vfs.srv` | `supervisor.srv` | mark degraded and alert `init.srv` if restart budget is exhausted |
| driver servers | `supervisor.srv` | redelegate bundle automatically when recovery plan is known; otherwise alert `init.srv` for manual redelegation |
| `supervisor.srv` | `init.srv` | restart supervisor, replay core registrations, and rebind fault handoff |

## Phase 0 — Stabilize Core Control Plane ✅

### Scope

- `init.srv`: deterministic launch ordering and restart-policy table sanity checks.
- `process_manager.srv`: wait/reap lifecycle policy hardening (non-parent and unknown target rejection).
- `vfs.srv`: mount namespace policy gate + deterministic operation ordering counter.

### Implemented

- `InitService` now carries a baseline restart-policy table, validates policy sanity, and records deterministic launch order (`process_manager -> vfs -> supervisor`).
- `ProcessManager` wait-path now rejects unknown targets and enforces parent ownership more strictly.
- `VfsService` now supports mount namespace policy (allow/deny boot-path classes) and deterministic op-sequence accounting for successful requests.

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
- ✅ `irqmux.srv` deterministic routing/drop/mask behavior implemented (`services/drivers/irqmux/*` + thin `src/bin/irqmux_srv.rs`).
- ✅ `uart.srv` deterministic tx/rx accounting with queue backpressure implemented (`services/drivers/uart/*` + thin `src/bin/uart_srv.rs`).
- ✅ `virtio_net.srv` deterministic tx/rx packet accounting with queue backpressure implemented (`services/drivers/virtio_net/*` + thin `src/bin/virtio_net_srv.rs`).
- ✅ `virtio_gpu.srv` deterministic mode-set/frame-commit gating implemented (`services/drivers/virtio_gpu/*` + thin `src/bin/virtio_gpu_srv.rs`).
- ✅ `input.srv` deterministic accepted/dropped event accounting with queue limits implemented (`services/drivers/input/*` + thin `src/bin/input_srv.rs`).

### Test gates

- delegation gate: init->driver role edges and cap bundle validation (wired to compat-gates workflow).
- fault gate: revoke/restart behavior deterministic and test-covered (wired to compat-gates workflow).

## Phase 3 — Networking Servers 🚧

### Target servers

- `netmgr.srv`
- `tcpip.srv`
- `dns.srv`
- `dhcp.srv`
- `socket.srv` adapter

### Current implementation status

- ✅ `netmgr.srv` now tracks deterministic link-state events (`services/network/netmgr/*` + thin `src/bin/netmgr_srv.rs`).
- ✅ `tcpip.srv` scaffold implemented with deterministic route/drop counters (`services/network/tcpip/*` + thin `src/bin/tcpip_srv.rs`).
- ✅ `dns.srv` scaffold implemented with deterministic cache/upstream accounting (`services/network/dns/*` + thin `src/bin/dns_srv.rs`).
- ✅ `dhcp.srv` scaffold implemented with deterministic lease accounting (`services/network/dhcp/*` + thin `src/bin/dhcp_srv.rs`).
- ✅ `socket.srv` adapter scaffold implemented (`services/network/socket/*` + thin `src/bin/socket_srv.rs`).

### Test gates

- deterministic packet path simulation (wired to compat-gates workflow).
- timeout/retry policy reproducibility (wired to compat-gates workflow).
- compatibility adapter vector tests (socket adapter coverage wired to compat-gates workflow).

## Phase 4 — Display + UI input servers 🚧

### Target servers

- `display.srv`
- `compositor.srv`
- `input.srv` (if not complete in phase 2)
- `shell.srv` / session manager

### Current implementation status

- ✅ `display.srv` scaffold now emits a stable boot marker and tracks mode-set/frame-present counters (`services/ui/display/*` + thin `src/bin/display_srv.rs`).
- ✅ `compositor.srv` scaffold implemented with deterministic frame composition replay (`services/ui/compositor/*` + thin `src/bin/compositor_srv.rs`).
- ✅ `shell.srv` session-manager scaffold implemented (`services/ui/shell/*` + thin `src/bin/shell_srv.rs`).

### Test gates

- boot-to-shell marker in QEMU log (marker stabilized + gate wired for marker string stability).
- input event routing deterministic replay (covered by deterministic compositor/shell path gates and existing input driver scaffolds).
- display mode-set and frame-present checks (wired to compat-gates workflow).
- phase contract gate: `PHASE4_UI_CONTRACT.md` published and enforced by readiness checks.

## Architecture follow-up status (frozen)

- ✅ Next move 1: `kernel::vfs` promoted as primary API (with `vfs_server` binary naming and migrated imports).
- ✅ Next move 2: typed VFS request/response wrappers added in `kernel::vfs` and adopted by service entry/service tests.
- ✅ Next move 3: FAT scaffold now models directory entries + cluster growth and typed VFS messaging path.
- ✅ Next move 4: blkcache now has policy knobs + writeback scheduling and is integrated by FAT/EXT4 backends.
- ✅ Next move 5: init launch flow now records mount execution status, with deterministic recovery/fallback simulation telemetry.
- ✅ Next move 6: CI/service boundary gate added (`scripts/check-service-arch-boundary.sh`) and wired into compat gates workflow.
- ✅ Storage service contract published (`STORAGE_SERVICE_CONTRACT.md`) for blkcache/fat/ext4/virtio_blk protocol stability.

## Architecture follow-up addenda

- 2026-03-16: Added phase-readiness automation, phase3 network recovery simulation, and strict UI marker validation in QEMU smoke/CI without mutating frozen architecture checklist.

## Readiness criteria

- Readiness evidence mapping is maintained in `PHASE_READINESS_MATRIX.md` and enforced by `scripts/check-roadmap-readiness.sh`.

Phase N is considered complete only when:

- implementation exists,
- test gates for that phase pass in CI,
- contract docs are updated,
- deterministic simulations cover the new server interactions.
