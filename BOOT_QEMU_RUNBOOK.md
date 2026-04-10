<!-- SPDX-License-Identifier: Apache-2.0 -->

# QEMU Core-Marker Boot Runbook (x86_64 target-first, multi-ISA support)

This runbook is **x86_64-first** for booting the kernel and validating serial success markers, while keeping secondary ISA scaffolding available.

## Prerequisites

- Rust toolchain + `rustup`
- host tools (x86_64 target path): `qemu-system-x86_64`, `cpio`
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
scripts/qemu-x86_64-core-smoke.sh
```

Strict mode:

```bash
QEMU_SMOKE_STRICT=1 scripts/qemu-x86_64-core-smoke.sh
```

## Success markers searched in serial log

- `YARM_BOOT_OK`
- `YARM_PROC_VFS_OK`
- `YARM_INIT_START`
- `YARM_INIT_DONE`

## Override paths (x86_64)

```bash
KERNEL_IMAGE=build-x86_64/yarm-x86_64.elf \
INITRAMFS_IMAGE=build-x86_64/initramfs-core.cpio \
scripts/qemu-x86_64-core-smoke.sh
```

## Secondary ISA path (RISC-V scaffolding)

```bash
scripts/build-qemu-riscv64-artifacts.sh
scripts/qemu-riscv64-core-smoke.sh
```

> musl sysdeps portability work is ISA-agnostic; boot scripts differ only in machine image/runner details.

## AArch64 strict smoke progression (gate-hardened)

Build artifacts:

```bash
scripts/build-qemu-aarch64-artifacts.sh
```

Run strict smoke:

```bash
QEMU_SMOKE_STRICT=1 scripts/qemu-aarch64-core-smoke.sh
```

The hardened AArch64 smoke gate now requires ordered progression markers (not marker-only presence):

- `YARM_AARCH64_BOOT_MARKER stage=_start`
- `YARM_AARCH64_BOOT_MARKER stage=prepare_arch_boot`
- `YARM_AARCH64_BOOT_MARKER stage=vbar_el1_ready`
- `YARM_AARCH64_BOOT_MARKER stage=mmu_enabled`
- `YARM_AARCH64_BOOT_MARKER stage=run_with_prepared_kernel`
- `YARM_BOOT_OK`
- `YARM_INIT_START`
- `YARM_INIT_DONE`

And timer/runtime progression markers:

- `YARM_TIMER_IRQ_DELIVERED`
- `YARM_TIMER_EOI_DONE`
- `YARM_SCHED_TICK`
