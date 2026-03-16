# QEMU BusyBox Boot Runbook (RISC-V64)

> Note: This runbook is currently RISC-V focused. The project has selected `x86_64-unknown-none` + musl sysdeps shim as the primary runtime direction for ongoing bring-up; see `X86_64_NONE_MUSL_PORT_TODO.md` for migration tasks.


## Prerequisites

- Rust toolchain + `rustup`
- target: `riscv64gc-unknown-linux-gnu`
- host tools: `qemu-system-riscv64`, `cpio`, `busybox` (or `busybox-static`)
- optional: `llvm-objcopy` or `rust-objcopy`

## One-command artifact staging

```bash
scripts/build-qemu-riscv64-artifacts.sh
```

Strict mode (fail if missing target/tools/artifacts):

```bash
ARTIFACTS_STRICT=1 scripts/build-qemu-riscv64-artifacts.sh
```

## One-command smoke boot

```bash
scripts/qemu-riscv64-busybox-smoke.sh
```

Strict mode:

```bash
QEMU_SMOKE_STRICT=1 scripts/qemu-riscv64-busybox-smoke.sh
```

## Success markers searched in serial log

- `YARM_BOOT_OK`
- `YARM_PROC_VFS_OK`
- `YARM_INIT_START`
- `YARM_INIT_DONE`
- `BusyBox` or `/ #`

## Override paths

```bash
KERNEL_IMAGE=build/yarm-riscv64.bin \
INITRAMFS_IMAGE=build/initramfs-busybox.cpio \
scripts/qemu-riscv64-busybox-smoke.sh
```

## Early x86_64-none bring-up commands

```bash
scripts/build-qemu-x86_64-artifacts.sh
```

```bash
scripts/qemu-x86_64-busybox-smoke.sh
```

> These x86_64 scripts are bootstrap scaffolds for the chosen B path and may require a finalized bootable kernel image format before strict smoke mode is enforced.

