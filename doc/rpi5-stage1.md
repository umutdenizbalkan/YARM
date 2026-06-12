// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

# Raspberry Pi 5 Stage 1 boot scaffold

This is a deliberately limited **UART-only bring-up scaffold**, not full Raspberry Pi 5 support.
It keeps the existing QEMU `virt` AArch64 kernel path as the default and stops Raspberry Pi 5 before
YARM's userspace/service chain.

## Boot directory skeleton

Stage an already-built raw Stage 1 image as `kernel_2712.img`:

```sh
scripts/create-rpi5-stage1-boot-dir.sh \
  --kernel-input path/to/kernel-image \
  --boot-dir build/rpi5-stage1-boot
```

The generated directory contains:

```text
config.txt
cmdline.txt
kernel_2712.img
README-RPI5-STAGE1.txt
```

The default generated files include:

```text
# config.txt
kernel=kernel_2712.img
arm_64bit=1
enable_uart=1
uart_2ndstage=1

# cmdline.txt
yarm.platform=auto yarm.boot_phase=uart yarm.max_cpus=1
```

Use `--phase entry|uart|dtb|mmu|kernel` to choose the diagnostic stop point and
`--cmdline-extra STRING` to append additional kernel arguments. `--os-check-off` adds `os_check=0`
for firmware environments that require bypassing the OS check. `--enable-rp1-uart` adds the Pi
5-specific `enable_rp1_uart=1`; it is deliberately opt-in rather than a default. The generator
refuses to replace its four output files unless `--force` is supplied.

The generated README records the selected phase and command line, lists the four expected UART
markers, and maps partial marker progress to the next boundary to investigate during hardware smoke.
Copy the generated files alongside the Raspberry Pi 5 firmware files on a FAT boot partition. The
script intentionally does not download or redistribute firmware.

> **Load-address blocker:** the existing AArch64 image is linked for the QEMU `virt` bootstrap
> address. The scaffold does not claim that copying that image directly produces a firmware-loadable
> Raspberry Pi 5 kernel. A dedicated Pi 5 link/load layout and physical-memory/MMU plan is the next
> hardware-boot blocker.

## Command-line policy

- `yarm.platform=auto|qemu-virt|rpi5`
  - `auto` classifies the DTB root `compatible` list.
  - Raspberry Pi 5 is recognized from `raspberrypi,5-model-b` or `brcm,bcm2712`.
  - QEMU `virt` is recognized from its existing compatible values.
- `yarm.boot_phase=entry|uart|dtb|mmu|kernel`
  - The default is `kernel`, preserving existing QEMU behavior.
  - `entry` selects the DTB UART, prints `RPI5_BOOT_00_ENTRY`, and halts.
  - `uart` prints all four deterministic `RPI5_BOOT_00` through `RPI5_BOOT_03` markers and halts.
  - `dtb` additionally reports memory, reserved-memory, interrupt-controller, and initrd state, then halts.
  - `mmu` is a future-facing stop point; Stage 1 reports diagnostics and halts before changing the Pi 5 MMU path.
  - `kernel` continues unchanged on QEMU. Raspberry Pi 5 refuses missing initrd and, even with one,
    refuses entry into userspace because Stage 1 is UART-only.
- `yarm.max_cpus=N`
  - The default is unset, preserving the existing QEMU CPU topology.
  - Use `1` for Raspberry Pi 5 Stage 1. No Raspberry Pi 5 SMP path is enabled.

## UART selection

The kernel resolves `/chosen/stdout-path`, strips serial options such as `:115200n8`, resolves aliases
such as `serial10`, and accepts an enabled PL011 node. For Raspberry Pi 5 it prefers
`/soc@107c000000/serial@7d001000` when that node is present and usable, then translates its `reg`
address through parent `ranges`. The UART code uses register offsets relative to the selected DTB base;
the Raspberry Pi address is not the only supported base.

The expected UART markers are:

```text
RPI5_BOOT_00_ENTRY
RPI5_BOOT_01_DTB_PTR value=...
RPI5_BOOT_02_UART_SELECTED path=... base=...
RPI5_BOOT_03_UART_OK
```

## Explicit non-goals

Stage 1 does **not** implement or claim:

- RP1 PCIe, GPIO, or PWM production support;
- Raspberry Pi 5 SMP;
- a Raspberry Pi 5 GIC driver or production interrupt path;
- initrd construction or full YARM userspace boot;
- changes to image IDs, CPIO packing, SpawnV5, PM, VFS, supervisor, or driver-manager policy;
- hardware proof from QEMU parser/policy tests.
