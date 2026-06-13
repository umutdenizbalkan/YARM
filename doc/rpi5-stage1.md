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
RPI5_RAW_ENTRY
RPI5_RAW_AFTER_MARKER
RPI5_DTB_X0 value=0x...
RPI5_BSS_CLEAR_BEGIN
RPI5_BSS_CLEAR_DONE
RPI5_STACK_READY
RPI5_BEFORE_EL1
RPI5_AFTER_EL1
RPI5_BEFORE_RUST
RPI5_RUST_ENTRY
RPI5_DTB_PARSE_BEGIN
RPI5_DTB_PARSE_DONE
RPI5_BOOT_OPTIONS_BEGIN
RPI5_BOOT_OPTIONS_DONE
RPI5_AFTER_BOOT_OPTIONS
RPI5_CONSOLE_SELECT_BEGIN
RPI5_SELECTED_UART_BASE value=0x000000107d001000
RPI5_CONSOLE_SELECT_DONE
RPI5_CONSOLE_WRITE_BEGIN
RPI5_BOOT_00_ENTRY
RPI5_TRY_WRITE_ENTER
RPI5_TRY_WRITE_BYTE_BEGIN
RPI5_PL011_FR value=0x...
RPI5_TRY_WRITE_TX_READY
RPI5_TRY_WRITE_BYTE_DONE
RPI5_TRY_WRITE_RETURN_OK
RPI5_CONSOLE_WRITE_DONE
RPI5_AFTER_CONSOLE_WRITE
RPI5_BEFORE_BOOT01
RPI5_BOOT_01_DTB_PTR
RPI5_BOOT_01_DTB_PTR value=...
RPI5_AFTER_BOOT01
RPI5_BEFORE_BOOT02
RPI5_BOOT_02_UART_SELECTED
RPI5_BOOT_02_UART_SELECTED base=...
RPI5_AFTER_BOOT02
RPI5_BEFORE_BOOT03
RPI5_BOOT_03_UART_OK
RPI5_AFTER_BOOT03
RPI5_DTB_DIAG_BEGIN
RPI5_DTB_MEMORY_RANGE index=... start=0x... size=0x...
RPI5_DTB_RESERVED_RANGE index=... start=0x... size=0x... no_map=...
RPI5_DTB_INITRD present=... start=0x... end=0x...
RPI5_DTB_BOOTARGS len=... truncated=...
RPI5_DTB_IRQC path=... base=0x... compatible=...
RPI5_DTB_IRQC_L2 path=... base=0x... compatible=...
RPI5_DTB_GIC_DIST base=0x...
RPI5_DTB_GIC_REDIST base=0x...
RPI5_DTB_PSCI conduit=...
RPI5_DTB_CPU_BITMAP value=0x... count=... max_cpus=1 effective=1
RPI5_DTB_PCIE_CONTROLLER path=... base=0x...
RPI5_DTB_RP1_PCIE present=...
RPI5_DTB_RP1_NODE path=...
RPI5_DTB_DIAG_DONE
```

`RPI5_RAW_ENTRY` is emitted directly from `_start`, before BSS clearing, Rust, DTB parsing, MMU
work, or console initialization. A temporary boot stack is established first so every register used
by the emergency writer can be saved and restored explicitly. The RPi5-only assembly path uses the
same translated physical PL011 base (`0x107d001000`) produced by the existing preferred-node DTB path for
`/soc@107c000000/serial@7d001000`. It retains firmware UART configuration, fences each MMIO write,
and abandons the marker after a bounded transmitter-ready poll rather than hanging entry forever.
The firmware handoff registers `x0` through `x3` are copied to `x20` through `x23` before the first
marker and restored before the first Rust call. `RPI5_DTB_X0` prints the saved DTB pointer as sixteen
hex digits using the same emergency writer. QEMU builds compile the assembly marker routines to
no-ops, compile the Rust marker helper to an empty function, and retain their `0x40080000` entry.

During the Stage 1 console transition, the selected DTB UART base is printed through the emergency
writer and must equal `0x107d001000`. A mismatch is reported and halted before any access through an
unproven MMIO base. `RPI5_BOOT_00_ENTRY` also remains on the emergency writer. The normal console is
then probed with a CRLF write bracketed by `RPI5_CONSOLE_WRITE_BEGIN` and
`RPI5_CONSOLE_WRITE_DONE`; RPi5 Stage 1 bounds its PL011 TX-ready poll and reports
`RPI5_CONSOLE_WRITE_TIMEOUT` instead of spinning forever.

The first empty-line normal-console probe bypasses the normal IRQ-safe log lock because RPi5 Stage 1
is explicitly single-CPU and interrupts are not yet part of this diagnostic boundary. Every emitted
byte still uses the bounded PL011 helper. The probe reports its first `FR` read, where offset `0x18`
and TX-full bit 5 match the emergency writer; data is written at offset `0x00`. Failure emits
`RPI5_TRY_WRITE_TIMEOUT`, then `RPI5_TRY_WRITE_RETURN_ERR`, and finally
`RPI5_CONSOLE_WRITE_TIMEOUT` before halting.

The Stage 1 BOOT_01/02/03 sequence stays entirely on the emergency writer. The generic
`yarm_log!` path formats into the printk ring and then enters the IRQ-safe `PRINTK_DRAIN_LOCK`; that
lock is not yet a proven-safe dependency at this boundary. Non-Stage1 builds retain the formatted
path and UART path-string report, while `rpi5-stage1` reports the DTB pointer and selected UART base
without printk formatting or drain locks.

An initial `PL011_FR` value of `0x38` has BUSY (bit 3), RXFE (bit 4), and TXFF (bit 5) set. It is
therefore not considered TX-ready at that instant. The bounded writer correctly continues polling;
the later `RPI5_TRY_WRITE_TX_READY` marker means a subsequent FR read had TXFF clear. No readiness
predicate change is required.

With `yarm.boot_phase=dtb`, Stage 1B performs one additional bounded DTB walk after
`RPI5_AFTER_BOOT03`. It records at most eight memory ranges and eight reserved-memory ranges, caps
bootargs inspection at 256 bytes, records paths in the existing 192-byte fixed path type, and uses a
384-byte stack line buffer for output through the already-proven bounded Stage 1 console. It reports
initrd state, the first interrupt-controller candidate and up to two translated GIC register ranges,
PSCI conduit, the firmware CPU bitmap constrained to the Stage 1 `max_cpus=1` policy, and RP1/PCIe
presence only. Missing GIC data emits `RPI5_DTB_GIC_MISSING`; parse or output failure emits an
explicit failure marker. `RPI5_DTB_DIAG_DONE` is followed directly by the safe Stage 1 halt, without
MMU, interrupt-controller, PCIe, SMP, or userspace initialization. Required Stage 1B output does not
use `yarm_log!`, printk, allocation, or an unbounded transmit loop.

Stage 1C classifies interrupt and PCIe resources by exact node role rather than by broad name
substrings. `brcm,bcm7271-l2-intc` is reported as `RPI5_DTB_IRQC_L2` and is never reused as a GIC
distributor. GIC distributor/redistributor records are emitted only for `arm,gic-v3`,
`arm,gic-400`, or `arm,cortex-a15-gic`; otherwise diagnostics report `RPI5_DTB_GIC_MISSING`.
Likewise, `RPI5_DTB_PCIE_CONTROLLER` describes a PCIe controller with a translated `reg` base, while
`RPI5_DTB_RP1_NODE` requires a node named exactly `rp1` whose direct parent is a PCIe controller.
This excludes reset controllers, GPIO hogs, firmware nodes, and RP1-related regulator names. These
records remain presence/classification diagnostics and do not initialize any interrupt controller,
PCIe controller, or RP1 device.

PCIe controller eligibility is structural: a node must be named `pcie@...`/`pci@...`, declare
`device_type = "pci"`, or carry a recognized host-controller compatible such as
`brcm,bcm2712-pcie`. Names containing `reset-controller` are explicitly excluded, so compatibles
such as `raspberrypi,rp1-pcie-reset` cannot steal the PCIe-controller slot. The complete DTB is
walked, including `/axi` outside `/soc`; an RP1 node elsewhere in the tree is ignored.

The selected UART `reg` address is a child-bus address. Translation walks each parent bus, uses that
bus node's `#address-cells` and `#size-cells` together with its parent's address-cell count, and scans
every `ranges` entry for a containing window. For the BCM2712 UART, child address `0x7d001000` falls
inside child window `0x7c000000..0x80000000`, whose CPU parent window begins at `0x107c000000`;
the translated physical address is therefore `0x107d001000`. Missing, malformed, or non-matching
`ranges` fails closed and emits `RPI5_UART_TRANSLATION_FAILED` rather than reusing the child address.

## Explicit non-goals

Stage 1 does **not** implement or claim:

- RP1 PCIe, GPIO, or PWM production support;
- Raspberry Pi 5 SMP;
- a Raspberry Pi 5 GIC driver or production interrupt path;
- initrd construction or full YARM userspace boot;
- changes to image IDs, CPIO packing, SpawnV5, PM, VFS, supervisor, or driver-manager policy;
- hardware proof from QEMU parser/policy tests.
