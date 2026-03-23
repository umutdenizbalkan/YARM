# QEMU + BusyBox Bring-up Checklist (Current Execution Batch)

This file tracks the 7 immediate tasks and their implementation status.

## Immediate next 7 tasks (concrete)

- [x] 1. Add `init.srv` capability intake table + validation.
  - Implemented in `src/services/init/mod.rs` as `StartupCap`, `StartupCapSet`, and `validate_startup_caps()`.

- [x] 2. Implement user-task spawn-from-image path in kernel bootstrap.
  - Implemented in `src/kernel/boot/mod.rs` as `UserImageSpec`, `SpawnedUserTask`, and `spawn_user_task_from_image()`.

- [x] 3. Implement minimal ELF loader path in `process_manager.srv`.
  - Implemented in `src/kernel/process.rs` as `ElfImageInfo::parse()` (ELF magic + entry extraction) with tests.

- [x] 4. Implement read-only initramfs VFS backend.
  - Implemented in `src/services/fs/initramfs/archive.rs` as `InitramfsBackend` and `INITRAMFS_BUSYBOX_PATH_PTR`, then exercised through `src/services/fs/initramfs/service.rs` and `tests/kernel_scenarios.rs`.

- [x] 5. Implement serial-console driver service path via VFS (`/dev/console`).
  - Implemented in `src/services/fs/devfs/nodes.rs` as `DevFsBackend` and `DEV_CONSOLE_PATH_PTR`, then exercised through `src/services/fs/devfs/service.rs`.

- [x] 6. Add QEMU boot script for one ISA.
  - Added `scripts/qemu-riscv64-busybox-smoke.sh`.

- [x] 7. Add CI smoke for "BusyBox appears on serial" with strict-mode toggle.
  - Added `.github/workflows/busybox-qemu-smoke.yml` invoking the script; strict failure is controlled by `QEMU_SMOKE_STRICT`.

## Notes

- The QEMU smoke script intentionally exits success when artifacts or QEMU are unavailable to keep this job non-blocking during incremental bring-up.
- Once kernel and initramfs artifacts are generated in CI, this can be promoted from non-blocking smoke to required gate.


## CI execution notes

- Artifact staging script: `scripts/build-qemu-riscv64-artifacts.sh`
- QEMU smoke gate now runs as a normal workflow job; strict failure mode can be enabled by setting `QEMU_SMOKE_STRICT=1`.


## Real-artifact script path

- `scripts/build-qemu-riscv64-artifacts.sh` now performs concrete staging steps: target cross-build attempt, busybox-based initramfs assembly, and optional ELF->binary conversion if objcopy is available.
- `scripts/qemu-riscv64-busybox-smoke.sh` now runs a concrete parameterized QEMU command (`machine/cpu/mem/smp/bios/cmdline`) and checks boot shell markers with strict-mode support.


## Golden path runbook

- See `BOOT_QEMU_RUNBOOK.md` for exact local command sequence and required success markers.
