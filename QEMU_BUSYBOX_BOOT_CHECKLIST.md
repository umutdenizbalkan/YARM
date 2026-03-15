# QEMU + BusyBox Bring-up Checklist (Current Execution Batch)

This file tracks the 7 immediate tasks and their implementation status.

## Immediate next 7 tasks (concrete)

- [x] 1. Add `init.srv` capability intake table + validation.
  - Implemented in `src/kernel/init_server.rs` as `StartupCap`, `StartupCapSet`, and `validate_startup_caps()`.

- [x] 2. Implement user-task spawn-from-image path in kernel bootstrap.
  - Implemented in `src/kernel/bootstrap.rs` as `UserImageSpec`, `SpawnedUserTask`, and `spawn_user_task_from_image()`.

- [x] 3. Implement minimal ELF loader path in `procman.srv`.
  - Implemented in `src/kernel/process_manager.rs` as `ElfImageInfo::parse()` (ELF magic + entry extraction) with tests.

- [x] 4. Implement read-only initramfs VFS backend.
  - Implemented in `src/kernel/vfs_lite.rs` as `ReadOnlyInitramfsBackend` and `INITRAMFS_BUSYBOX_PATH_PTR`.

- [x] 5. Implement serial-console driver service path via VFS (`/dev/console`).
  - Implemented in `src/kernel/vfs_lite.rs` as `ConsoleBackend` and `DEV_CONSOLE_PATH_PTR` with tests.

- [x] 6. Add QEMU boot script for one ISA.
  - Added `scripts/qemu-riscv64-busybox-smoke.sh`.

- [x] 7. Add non-blocking CI smoke for "BusyBox appears on serial".
  - Added `.github/workflows/busybox-qemu-smoke.yml` (`continue-on-error: true`) invoking the script.

## Notes

- The QEMU smoke script intentionally exits success when artifacts or QEMU are unavailable to keep this job non-blocking during incremental bring-up.
- Once kernel and initramfs artifacts are generated in CI, this can be promoted from non-blocking smoke to required gate.
