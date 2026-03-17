# QEMU BusyBox Boot Runbook (x86_64 target-first, multi-ISA support)

This runbook is **x86_64-first** for booting a kernel to an initramfs BusyBox prompt, while keeping secondary ISA scaffolding available.

## Prerequisites

- Rust toolchain + `rustup`
- host tools (x86_64 target path): `qemu-system-x86_64`, `cpio`, `busybox` (or `busybox-static`)
- optional secondary ISA path: `qemu-system-riscv64`
- optional: `llvm-objcopy` or `rust-objcopy`

## x86_64 target path (primary)

### One-command artifact staging

```bash
scripts/build-qemu-x86_64-artifacts.sh
```

Strict mode (fail if missing target/tools/artifacts):

```bash
ARTIFACTS_STRICT=1 scripts/build-qemu-x86_64-artifacts.sh
```

### One-command smoke boot

```bash
scripts/qemu-x86_64-busybox-smoke.sh
```

Strict mode:

```bash
QEMU_SMOKE_STRICT=1 scripts/qemu-x86_64-busybox-smoke.sh
```

## Success markers searched in serial log

- `YARM_BOOT_OK`
- `YARM_PROC_VFS_OK`
- `YARM_INIT_START`
- `YARM_INIT_DONE`
- `BusyBox` or `/ #`

## Override paths (x86_64)

```bash
KERNEL_IMAGE=build-x86_64/yarm-x86_64.elf \
INITRAMFS_IMAGE=build-x86_64/initramfs-busybox.cpio \
scripts/qemu-x86_64-busybox-smoke.sh
```

## Secondary ISA path (RISC-V scaffolding)

```bash
scripts/build-qemu-riscv64-artifacts.sh
scripts/qemu-riscv64-busybox-smoke.sh
```

> musl sysdeps portability work is ISA-agnostic; boot scripts differ only in machine image/runner details.
