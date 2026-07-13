<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Umut Deniz Balkan -->

# Raspberry Pi 5 — Stage 1 / High-Half bring-up

> **Ownership rule.** All Raspberry Pi 5 hardware bring-up documentation
> lives here. Generic boot flow lives in `doc/BOOT.md`; AArch64 trap /
> syscall / userspace status lives in `doc/ARCH_AARCH64.md`. New RPi5
> fragment files are forbidden; update this doc instead. See
> `doc/DOCUMENTATION_MAP.md`.

This is a deliberately limited **UART-only bring-up scaffold**, not full
Raspberry Pi 5 support. It keeps the existing QEMU `virt` AArch64 kernel
path as the default and stops Raspberry Pi 5 before YARM's userspace /
service chain.

**Diagnostic-vs-production summary.** Stages 1A–2D and the high-half
HH-2/HH-3/HH-4/HH-5 paths are **diagnostic-only**. Production AArch64 boot
remains the QEMU `virt` path documented in `doc/ARCH_AARCH64.md`.

---

## 1. Boot directory generator

Stage an already-built raw Stage 1 image as `kernel_2712.img`:

```sh
scripts/create-rpi5-stage1-boot-dir.sh \
  --kernel-input path/to/kernel-image \
  --boot-dir build/rpi5-stage1-boot
```

Generated directory:

```text
config.txt
cmdline.txt
kernel_2712.img
README-RPI5-STAGE1.txt
```

Default `config.txt` / `cmdline.txt`:

```text
# config.txt
kernel=kernel_2712.img
arm_64bit=1
enable_uart=1
uart_2ndstage=1

# cmdline.txt
yarm.platform=auto yarm.boot_phase=uart yarm.max_cpus=1
```

Generator flags:

- `--phase entry|uart|dtb|mmu|kernel` — diagnostic stop point.
- `--cmdline-extra STRING` — append kernel arguments.
- `--os-check-off` — add `os_check=0` for firmware environments that
  require bypassing the OS check.
- `--enable-rp1-uart` — Pi 5-specific `enable_rp1_uart=1`; opt-in, not
  default.
- `--force` — overwrite generator output (it refuses by default).

The generated README records the selected phase, command line, the four
expected UART markers, and maps partial marker progress to the next
boundary to investigate during hardware smoke. The generator
intentionally does not download or redistribute firmware. Copy the
generated files onto a FAT boot partition alongside the Raspberry Pi 5
firmware files.

> **Load-address blocker.** The existing AArch64 image is linked for
> the QEMU `virt` bootstrap address. The scaffold does **not** claim
> that copying that image directly produces a firmware-loadable Pi 5
> kernel. A dedicated Pi 5 link/load layout and physical-memory / MMU
> plan is the next hardware-boot blocker.

---

## 2. Command-line policy

- `yarm.platform=auto|qemu-virt|rpi5` — `auto` classifies the DTB root
  `compatible` list. Raspberry Pi 5 is recognized from
  `raspberrypi,5-model-b` or `brcm,bcm2712`. QEMU `virt` is recognized
  from its existing compatible values.
- `yarm.boot_phase=entry|uart|dtb|mmu|kernel`:
  - default `kernel`, preserving existing QEMU behavior.
  - `entry` — DTB UART selected, prints `RPI5_BOOT_00_ENTRY`, halts.
  - `uart` — prints all four deterministic `RPI5_BOOT_00`–`RPI5_BOOT_03`
    markers and halts.
  - `dtb` — additionally reports memory, reserved-memory,
    interrupt-controller, and initrd state, then halts.
  - `mmu` — future-facing stop point; Stage 1 reports diagnostics and
    halts before changing the Pi 5 MMU path.
  - `kernel` — continues unchanged on QEMU. Raspberry Pi 5 refuses
    missing initrd and, even with one, **refuses entry into userspace
    because Stage 1 is UART-only**.
- `yarm.max_cpus=N` — default unset, preserving the existing QEMU CPU
  topology. Use `1` for Raspberry Pi 5 Stage 1. **No Raspberry Pi 5 SMP
  path is enabled.**

---

## 3. Stage 1 UART selection (proven base translation)

The kernel resolves `/chosen/stdout-path`, strips serial options such as
`:115200n8`, resolves aliases such as `serial10`, and accepts an enabled
PL011 node. For Raspberry Pi 5 it prefers
`/soc@107c000000/serial@7d001000` when that node is present and usable,
then translates its `reg` address through parent `ranges`.

**Proven base translation.** The selected UART `reg` address is a
child-bus address. Translation walks each parent bus, uses that bus
node's `#address-cells` and `#size-cells` together with its parent's
address-cell count, and scans every `ranges` entry for a containing
window. For the BCM2712 UART, child address `0x7d001000` falls inside
child window `0x7c000000..0x80000000`, whose CPU parent window begins at
`0x107c000000`; the translated physical address is therefore
`0x107d001000`. Missing, malformed, or non-matching `ranges` fails
closed and emits `RPI5_UART_TRANSLATION_FAILED` rather than reusing the
child address.

### 3.1 Expected UART markers

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
RPI5_DTB_PCIE_CONTROLLER index=... path=... base=0x...
RPI5_DTB_RP1_PCIE present=... controller_index=...
RPI5_DTB_RP1_NODE path=...
RPI5_DTB_DIAG_DONE
```

### 3.2 Emergency writer + bounded TX

`RPI5_RAW_ENTRY` is emitted directly from `_start`, before BSS clearing,
Rust, DTB parsing, MMU work, or console initialization. A temporary
boot stack is established first so every register used by the emergency
writer can be saved and restored explicitly. The RPi5-only assembly
path uses the same translated physical PL011 base (`0x107d001000`)
produced by the existing preferred-node DTB path for
`/soc@107c000000/serial@7d001000`. It retains firmware UART
configuration, fences each MMIO write, and abandons the marker after a
bounded transmitter-ready poll rather than hanging entry forever. The
firmware handoff registers `x0`–`x3` are copied to `x20`–`x23` before
the first marker and restored before the first Rust call.
`RPI5_DTB_X0` prints the saved DTB pointer as sixteen hex digits using
the same emergency writer. QEMU builds compile the assembly marker
routines to no-ops, compile the Rust marker helper to an empty
function, and retain their `0x40080000` entry.

During the Stage 1 console transition, the selected DTB UART base is
printed through the emergency writer and must equal `0x107d001000`. A
mismatch is reported and halted before any access through an unproven
MMIO base. `RPI5_BOOT_00_ENTRY` also remains on the emergency writer.
The normal console is then probed with a CRLF write bracketed by
`RPI5_CONSOLE_WRITE_BEGIN` / `RPI5_CONSOLE_WRITE_DONE`; RPi5 Stage 1
bounds its PL011 TX-ready poll and reports `RPI5_CONSOLE_WRITE_TIMEOUT`
instead of spinning forever.

The first empty-line normal-console probe bypasses the normal IRQ-safe
log lock because RPi5 Stage 1 is explicitly single-CPU and interrupts
are not yet part of this diagnostic boundary. Every emitted byte still
uses the bounded PL011 helper. The probe reports its first `FR` read,
where offset `0x18` and TX-full bit 5 match the emergency writer; data
is written at offset `0x00`. Failure emits `RPI5_TRY_WRITE_TIMEOUT`,
then `RPI5_TRY_WRITE_RETURN_ERR`, and finally
`RPI5_CONSOLE_WRITE_TIMEOUT` before halting.

The Stage 1 BOOT_01/02/03 sequence stays entirely on the emergency
writer. The generic `yarm_log!` path formats into the printk ring and
then enters the IRQ-safe `PRINTK_DRAIN_LOCK`; that lock is not yet a
proven-safe dependency at this boundary. Non-Stage1 builds retain the
formatted path and UART path-string report, while `rpi5-stage1` reports
the DTB pointer and selected UART base without printk formatting or
drain locks.

An initial `PL011_FR` value of `0x38` has BUSY (bit 3), RXFE (bit 4),
and TXFF (bit 5) set. It is therefore not considered TX-ready at that
instant. The bounded writer correctly continues polling; the later
`RPI5_TRY_WRITE_TX_READY` marker means a subsequent FR read had TXFF
clear. No readiness predicate change is required.

---

## 4. Stage 1B — DTB diagnostics (with `yarm.boot_phase=dtb`)

Stage 1B performs one additional bounded DTB walk after
`RPI5_AFTER_BOOT03`. It records at most eight memory ranges and eight
reserved-memory ranges, caps bootargs inspection at 256 bytes, records
paths in the existing 192-byte fixed path type, and uses a 384-byte
stack line buffer for output through the already-proven bounded Stage 1
console.

It reports:

- initrd state,
- the first interrupt-controller candidate,
- up to two translated GIC register ranges,
- PSCI conduit,
- the firmware CPU bitmap constrained to Stage 1 `max_cpus=1` policy,
- RP1 / PCIe presence only.

Missing GIC data emits `RPI5_DTB_GIC_MISSING`; parse / output failure
emits an explicit failure marker. `RPI5_DTB_DIAG_DONE` is followed
directly by the safe Stage 1 halt, without MMU, interrupt-controller,
PCIe, SMP, or userspace initialization. Required Stage 1B output does
not use `yarm_log!`, printk, allocation, or an unbounded transmit loop.

### 4.1 Stage 1C — IRQ / PCIe classification

Stage 1C classifies interrupt and PCIe resources by **exact node role**
rather than broad name substrings. `brcm,bcm7271-l2-intc` is reported
as `RPI5_DTB_IRQC_L2` and is never reused as a GIC distributor. GIC
distributor / redistributor records are emitted only for `arm,gic-v3`,
`arm,gic-400`, or `arm,cortex-a15-gic`; otherwise diagnostics report
`RPI5_DTB_GIC_MISSING`.

`RPI5_DTB_PCIE_CONTROLLER` describes a PCIe controller with a
translated `reg` base; `RPI5_DTB_RP1_NODE` requires a node named exactly
`rp1` whose direct parent is a PCIe controller. This excludes reset
controllers, GPIO hogs, firmware nodes, and RP1-related regulator names.
These records remain presence/classification diagnostics and do not
initialize any interrupt controller, PCIe controller, or RP1 device.

PCIe controller eligibility is structural: a node must be named
`pcie@...`/`pci@...`, declare `device_type = "pci"`, or carry a
recognized host-controller compatible such as `brcm,bcm2712-pcie`.
Names containing `reset-controller` are explicitly excluded, so
compatibles such as `raspberrypi,rp1-pcie-reset` cannot steal the PCIe
slot. The complete DTB is walked, including `/axi` outside `/soc`; an
RP1 node elsewhere in the tree is ignored.

Up to eight PCIe controllers are retained in DTB traversal order and
printed with deterministic indices. RP1 lookup compares the exact
direct-parent path against every retained controller, so a later
controller containing `rp1` is selected even when an earlier eligible
PCIe controller has no RP1 child. If no matching child exists, all
controllers are still printed and `RPI5_DTB_RP1_PCIE present=0` is
emitted.

---

## 5. Stage 1D — Memory plan; Stage 1E — Identity-MMU smoke

### 5.1 Stage 1D memory plan

With `yarm.boot_phase=mmu` or `yarm.boot_phase=kernel`, Stage 1D
constructs a bounded kernel-core memory plan and halts before the
production boot path. Kernel image range comes from `__kernel_start` /
`__kernel_end`; DTB range comes from the firmware pointer and validated
FDT total size. The planner reserves `0..0x80000`, the kernel image,
the DTB, and every nonzero `/reserved-memory` range. Exact duplicate
reservations are collapsed; zero-sized firmware entries emit
`RPI5_KERNEL_RESERVED_ZERO_SKIPPED`.

The planner subtracts those ranges from at most eight firmware memory
ranges using a fixed 24-entry working array. It selects a 256 KiB
page-table pool followed by a 2 MiB early-heap range, both aligned to
64 KiB and checked against every reservation and each other. The
remaining fragments are printed as `RPI5_KERNEL_USABLE_RANGE`.
Capacity, overflow, malformed input, or inability to place either range
fails closed with `RPI5_KERNEL_PLAN_FAILED reason=...`. An initrd is
**not** required for this diagnostic path.

### 5.2 Stage 1E identity MMU + MMU enable

Stage 1E consumes the Stage1D page-table pool with a separate
identity-map builder rather than the production VM allocator. With a 4
KiB granule and a 39-bit TTBR0 address space, it maps every firmware
RAM range as normal WB/WA memory and maps only the translated PL011
page at `0x107d001000` as device-nGnRE. The hardware-proven kernel,
current boot stack, DTB, page-table pool, and early heap all lie in
those normal-memory ranges. The plan fails closed if any required range
is not covered or if a normal mapping overlaps the UART device page.

Tables are allocated sequentially and cleared only within the planned
256 KiB pool. For the hardware-proven two-range map the expected minimum
layout is one L1 root, one L2 table for `0..0x3fc00000`, and one L2
plus one L3 table for the high UART page; the aligned
`0x40000000..0x80000000` range uses an L1 block. Normal mappings use
AttrIdx 0 and privileged execution with EL0 execute-never; the UART
page uses AttrIdx 1, PXN, and UXN.

Stage 1E programs:

```text
MAIR_EL1 = 0x04ff               # Attr0 normal WB/WA, Attr1 device-nGnRE
TCR_EL1  = 0x0000000200803519   # T0SZ=25, 4 KiB TG0, inner-shareable WB/WA,
                                # 40-bit PA, TTBR1 disabled
```

It writes the identity root to TTBR0, clears TTBR1, cleans the table
pool, performs explicit DSB / TLBI / ISB + I-cache maintenance, then
preserves the incoming SCTLR state while setting M, C, and I. A
pre-enabled MMU or failed `SCTLR.M` readback emits
`RPI5_MMU_ENABLE_FAILED`. Success emits `RPI5_MMU_ENABLE_DONE`, proves
the bounded UART path with `RPI5_UART_AFTER_MMU_OK`, emits
`RPI5_KERNEL_CORE_DONE`, and halts before production kernel or
userspace initialization.

---

## 6. Stage 1F — Allocator handoff

Stage 1F converts the same firmware memory and reservation plan into
sorted, page-aligned input for the kernel `PhysicalFrameAllocator`.
Reservations include low firmware memory, the kernel image, DTB,
Stage1E page-table pool, the complete early heap, and every nonzero
firmware reserved-memory range. Usable starts round up and ends round
down to 4 KiB boundaries; an overlap with any reservation fails closed.

The allocator object itself is placed at the beginning of the reserved
`0x5b90000..0x5d90000` early heap, so its fixed metadata cannot consume
a frame later advertised as free.

On the hardware-proven plan the installed usable extents are:

```text
0x5d90000..0x2efec000
0x2f000000..0x3fc00000
0x40000000..0x80000000
```

Stage 1F allocates the first 4 KiB frame, verifies alignment,
usable-range containment, and non-overlap with every reservation, then
releases it and verifies the free-page count is restored. No frame is
intentionally leaked.

All Stage 1F status / failure markers use the bounded Stage1
line/emergency writers. Success emits `RPI5_KERNEL_ALLOCATOR_READY` and
`RPI5_KERNEL_CORE_ALLOC_DONE`, then halts without installing a
scheduler, initializing interrupts or devices, or entering userspace.

---

## 7. Stage 1G / 1H / 1I — Read-only timer/IRQ diagnostics, bootstrap skeleton

### 7.1 Stage 1G — Timer + GIC read-only probe

Stage 1G follows the allocator smoke with read-only timer and
interrupt-controller diagnostics. It reads `CNTFRQ_EL0` and samples
`CNTPCT_EL0` around a bounded 4096-iteration delay, rejecting a zero or
backwards delta. It does **not** program `CNTP_CTL_EL0` or
`CNTP_TVAL_EL0`.

The identity map includes one device-nGnRE page for each
DTB-discovered GIC distributor and redistributor base. Stage 1G reads
only `GICD_TYPER` at offset `0x004`, `GICD_IIDR` at offset `0x008`, and
the 64-bit `GICR_TYPER` at offset `0x008`. It performs no GIC writes
and does not enable IRQs. The tree has no reviewed read-only register
definition for the `brcm,bcm7271-l2-intc`, so the L2 probe deliberately
emits `RPI5_L2_INTC_PROBE_DEFERRED reason=no_reviewed_read_only_offset`
instead of guessing an offset. Success ends with
`RPI5_IRQTIMER_DIAG_DONE` and `RPI5_KERNEL_IRQTIMER_READY`, then halts.

### 7.2 Stage 1H — GICR validation + bounded timer

Stage 1H performs a bounded validation pass over four possible GICv3
redistributor frames, starting at the DTB-discovered base with the
architectural 128 KiB frame stride. Only each candidate's 64-bit
`GICR_TYPER` is read. Zero and all-ones values are rejected. The
corresponding 4 KiB probe pages are included in the Stage1 identity
map as device-nGnRE.

No reviewed CPU-interface / redistributor write sequence is present
yet, so Stage 1H does **not** write GICD or GICR registers even if a
plausible frame is observed. A zero-only scan emits
`RPI5_GICR_VALIDATE_FAILED reason=no_valid_frame` and
`RPI5_IRQ_INIT_DEFERRED reason=gicr_unvalidated`; a plausible frame
emits validation success and defers with
`gic_init_sequence_not_reviewed`.

After that explicit GIC deferral, Stage 1H programs a diagnostic
physical-timer interval equal to one hundredth of `CNTFRQ_EL0`, clamped
to the positive 32-bit `CNTP_TVAL_EL0` range. It writes
`CNTP_CTL_EL0 = 3`, leaving the timer enabled but interrupt-masked,
verifies the enable/mask readback, emits
`RPI5_TIMER_INIT_DONE masked=1` and `RPI5_KERNEL_BOOT_PREP_DONE`, then
halts. It does **not** enable any IRQ, enter a scheduler, start another
CPU, or initialize an external device.

### 7.3 Stage 1I — Bootstrap skeleton record

Stage 1I converts the parsed Pi 5 platform diagnostics and the existing
Stage1F allocator handoff into a fixed-size kernel bootstrap record.
The record carries the translated UART base, firmware memory ranges,
complete allocator reservation ranges, total/free frame counts, CPU
bitmap with an effective CPU count of one, PSCI conduit, diagnostic GIC
bases, vector-table base, and explicit initrd presence. It does **not**
create another frame allocator.

The Stage1 path installs the normal AArch64 EL1 vector table while
setting all DAIF masks, verifies `VBAR_EL1`, and leaves the physical
timer interrupt masked. Because normal `KernelState` construction is
coupled to production bootstrap and userspace invariants, Stage1I uses
this diagnostic skeleton record instead of weakening those invariants.
It revalidates the live Stage1F allocator counts, reports GIC readiness
as deferred, emits `RPI5_KERNEL_BOOTSTRAP_NO_USERSPACE reason=no_initrd`
and `RPI5_KERNEL_BOOT_OK`, then halts without calling the scheduler,
userspace bootstrap, SMP, VFS, or production device initialization.

---

## 8. Stage 2A / 2B / 2C / 2D — Initrd planning and EL0 deferral

### 8.1 Stage 2A initrd planning

Stage 2A accepts firmware-provided `linux,initrd-start` and
`linux,initrd-end` values from `/chosen`, preserving the original byte
range while rounding a separate reservation down/up to 4 KiB pages.
The range must be nonempty, fully contained in one firmware RAM range,
and disjoint from every Stage1 allocator reservation (kernel, DTB,
page-table pool, early heap, firmware reserved-memory). Stage 2A
performs no frame allocations after this check.

For an accepted range, Stage 2A reads only the first bounded CPIO newc
header. It requires magic `070701`, parses the fixed-width hexadecimal
file-size and name-size fields, limits the first name to 96 bytes,
verifies its terminating NUL and entry bounds, and prints the first
entry name. It does **not** unpack the archive or start userspace.
Missing or malformed initrd state emits an explicit
`RPI5_STAGE2A_DEFERRED` reason and halts; success emits
`RPI5_INITRD_READY` and `RPI5_STAGE2A_DONE`.

The Pi boot-directory generator stages an optional archive as
`initramfs-stage2a.cpio` and uses the Raspberry Pi firmware directive
`initramfs initramfs-stage2a.cpio followkernel`. This directive is
space-separated, without `=`, and asks firmware to place the archive
after the kernel and publish the `/chosen` initrd properties. Omitting
`--initrd-input` preserves the prior no-initrd config.

### 8.2 Stage 2B CPIO walk + ELF validation

Stage 2B continues only after Stage 2A has validated and reserved the
initrd. It walks the `newc` archive in place with a 4096-entry bound,
validates every header, name, file range, and four-byte alignment, and
accepts either `init` or `/init`. It does **not** unpack the archive.

The selected file must be a little-endian AArch64 ELF64 executable.
Stage 2B bounds the program-header table and up to eight `PT_LOAD`
segments, rejects file sizes larger than memory sizes, out-of-file
segments, invalid alignment, writable-plus-executable segments, and
entry points outside executable load segments. The resulting fixed-size
load plan is diagnostic only.

### 8.3 Stage 2C task construction

Stage 2C consumes that load plan with the existing Stage1F
physical-frame allocator. It allocates a standalone three-level 4 KiB
granule user page table, zeroed data pages, and a four-page stack
ending at `0x3fe00000`. RX pages are EL0-readable / executable and
never writable; RW/BSS and stack pages are EL0-readable / writable with
both privileged and user execute disabled. File bytes are copied
directly from the reserved initrd, while every destination page is
zeroed before copying, which also clears the complete BSS and
partial-page gaps.

This produces a diagnostic init task record with TID 1, page-table
root, entry, stack pointer, and a `TrapFrame` carrying the EL0 PC/SP.
It intentionally does **not** register the task with the production
scheduler, capability, PM, supervisor, or VFS state. The frame
allocator is the already initialized Stage1F allocator; Stage 2C never
creates a second allocator.

### 8.4 Stage 2D — TTBR-split blocker (real-entry preflight)

Stage 2D performs a real-entry preflight against the live architectural
state. The Stage1E identity map is installed in `TTBR0_EL1`, `TTBR1_EL1`
is zero, `TCR_EL1.EPD1` disables TTBR1 walks, and the kernel is
executing from a low identity-mapped address. **Replacing TTBR0 with
the Stage 2C user root would therefore remove the translations for the
executing kernel, vector table, stack, and UART before ERET could
complete.**

The bridge also requires a production current-task registration,
matching task/address-space root, and exact trap-frame entry / SP.
Stage 2C deliberately has only a diagnostic task record, so those
normal scheduler invariants are not fabricated. Stage 2D emits
`RPI5_STAGE2D_REAL_DEFERRED reason=ttbr_split_not_ready` and
`RPI5_STAGE2E_DEFERRED reason=el0_not_entered`, then halts. It does
**not** write TTBR0, execute ERET, dispatch a syscall, enable
interrupts, or claim userspace / service-chain progress.

---

## 9. RPi5 high-half address contract

The selected long-term TTBR split keeps EL0 mappings in `TTBR0_EL1`
and moves the kernel to `TTBR1_EL1`. The RPi5 contract preserves the
physical firmware load base at `0x80000`, uses
`0xffffff8000000000` as the kernel virtual offset, and therefore maps
the first kernel load address to `0xffffff8000080000`. Kernel virtual
and physical addresses convert by adding or subtracting that fixed
offset; checked helpers reject arithmetic overflow and addresses below
the kernel high half.

`targets/aarch64-rpi5-stage2-highhalf-none.ld` is a **non-default**
scaffold for that transition. It defines the low boot range, physical
and virtual kernel ranges, and the VA offset while assigning high-half
VMAs and low physical LMAs. It is deliberately not referenced by the
current target JSON. The hardware-proven `aarch64-rpi5-stage1-none`
profile still links and executes at `0x80000`.

<!-- The two phrases below are pinned verbatim by tests/rpi5_stage1_scope.rs::rpi5_high_half_scaffold_is_explicit_and_non_default — do not reflow. -->
This scaffold does not install TTBR1, alter TCR, branch to a high virtual address, replace TTBR0, or attempt ERET.
A later transition must first map the executing kernel, vectors, stack, page tables, and required MMIO in TTBR1, branch into the high half, and only then install a user root in TTBR0.

---

## 10. HH-2 — Explicit transition diagnostic

The non-default `rpi5-highhalf` feature and
`targets/aarch64-rpi5-stage2-highhalf-none.json` implement that first
transition as a self-contained low assembly diagnostic. The current
Stage1 target does not enable the feature or select the linker script.
HH-2 reserves a 16-page low physical page-table pool, a low boot stack,
and a 2 MiB diagnostic heap in the high-half linker layout.

The trampoline builds distinct roots:

- **TTBR0** maps physical `0..2 GiB` identically and remains installed
  only to keep the low trampoline executable.
- **TTBR1** maps the same RAM at `PA + 0xffffff8000000000`; this
  contains the linked kernel, low boot stack and vectors, page-table
  pool, diagnostic heap, and the firmware DTB after a bounded range
  check.
- A separate three-level TTBR1 path maps physical UART page
  `0x107d001000` at `0xffffff907d001000` with MAIR AttrIdx 1,
  device-nGnRE.

Conflicting table entries and exhausted/reserved table storage fail
closed.

HH-2 programs:

```text
MAIR_EL1 = 0x04ff
TCR_EL1  = 0x00000002b5193519   # T0SZ=T1SZ=25 (39-bit), 4 KiB TG0/TG1,
                                # inner-shareable WB/WA, EPD1 clear, 40-bit PA
```

After installing the distinct roots and enabling the MMU, the
trampoline branches to its high alias, moves SP to its high alias,
installs the high alias of a diagnostic vector table in `VBAR_EL1`,
switches the bounded writer to the high UART alias, and reaches the
high continuation boundary.

The trampoline does **not** install the Stage 2C user root, enter EL0,
dispatch syscalls, or start the scheduler / service chain. The only
`eret` in the trampoline is the existing architectural EL2→EL1 descent
when firmware did not already enter at EL1.

---

## 11. HH-3 — High-linked Rust continuation

HH-3 keeps the HH-2 mappings and replaces the final assembly halt with
a direct branch to the high-linked `yarm_rpi5_hh_rust_continue`
function. The continuation uses only the bounded high UART alias and
stack-local fixed buffers. It prints and validates the current PC, SP,
`VBAR_EL1`, `TTBR0_EL1`, `TTBR1_EL1`, and `TCR_EL1`.

Validation:

- PC, SP, VBAR must be high; VBAR must retain 2 KiB alignment.
- The roots must be nonzero, page-aligned, distinct, and equal the
  linker-reserved roots.
- `EPD1` must be clear; `T1SZ` must remain 25.
- `TCR` must equal `0x00000002b5193519`.

Success ends with `RPI5_HH_REGISTERS_OK`, `RPI5_HH_RUST_UART_OK`, and
`RPI5_HH3_DONE`, then enters HH-4. The four Rust success markers live
in a dedicated retained `.rodata.rpi5_hh_markers` section. The
high-half linker uses `KEEP` for that section so optimized code
generation cannot turn the only copies into non-contiguous immediate
stores or discard them.

`scripts/build-rpi5-highhalf-artifact.sh` is the explicit build entry
point. It selects the HH target and feature and writes
`build-rpi5/kernel_2712_hh.img`; it refuses to replace the default
`build-rpi5/kernel_2712.img`. After objcopy, the script scans the raw
image and fails unless every required low-transition, HH-3, HH-4, and
HH-5 audit marker is present. The boot-directory generator stages that
image as firmware-visible `kernel_2712.img` only when invoked with
`--highhalf`. The generated README labels the image as a high-half
diagnostic. An initrd remains optional and is not consumed by HH-3.

HH-3 still does **not** install a user root in TTBR0, enter EL0, enable
interrupts, remove the low identity map, or start any scheduler or
service-chain component.

---

## 12. HH-4 and BOOT-5 ladder

After HH-3 validates the high PC, stack, VBAR, TTBR roots, TCR, and
UART, HH-4 uses a distinct linker-reserved, page-aligned empty TTBR0
root. Before replacement it verifies that every continuation entry
point and required output marker is a high VA, zeros and cleans the
empty root, then performs `DSB ISHST`, writes `TTBR0_EL1`, executes
`ISB`, invalidates EL1 translations with `TLBI VMALLE1`, and finishes
with `DSB ISH` plus `ISB`. The post-replacement path reads back TTBR0
and proves PC, SP, VBAR, and the bounded UART remain usable solely
through TTBR1 high aliases.

The accepted RPi5 boot ladder is now:

| Stage | Scope | Status |
|-------|-------|--------|
| BOOT-1 | raw entry/UART | live diagnostic |
| BOOT-2 | DTB/platform/initrd discovery | live diagnostic |
| BOOT-3 | high-half execution and TTBR0 removal | live diagnostic |
| BOOT-4 | high physmap, frame allocator, global allocator | live diagnostic |
| BOOT-5A | CPU0/scheduler/KernelState audit and HH5 bridge | in progress; CPU0/scheduler probe now runs, KernelState construction deferred at constructor stack/copy invariant |
| BOOT-5B | real `/init` and first EL0 | future |
| BOOT-5C | complete userspace server chain | future |
| future | GIC/timer, RP1/PCIe, physical drivers, SMP | not part of BOOT-5 |

Current BOOT-5A source audit records the normal bootstrap prerequisites with
`RPI5_BOOT5_KERNELSTATE_AUDIT_*` markers after
`RPI5_KERNEL_GLOBAL_ALLOCATOR_HIGHMAP_OK`. The audit found that canonical storage
is `BOOTSTRAP_KERNEL_STATE` plus `BOOTSTRAP_SHARED_KERNEL` in
`src/kernel/boot/bootstrap_state.rs`; `init_shared_static_with_boot_memory_map`
initializes `KernelState` in static `MaybeUninit`, then performs a `ptr::read`
into `SharedKernel::new`. That means BOOT-5 must remove the remaining large
move/constructor risk before constructing it on hardware. The fields requiring
allocation include address-space managers, IPC, task tables, capability state,
memory, driver, telemetry, boot-config, fault, and restart subsystems. CPU0,
per-CPU state, the scheduler run queue, idle/bootstrap representation, current
state before ERET, ASID/user address-space state, and boot initrd publication are
normal-bootstrap prerequisites. No timer IRQ, GIC, SMP, RP1, PCIe, DMA, or
physical driver initialization is permitted in BOOT-5.

The HH5 handoff bridge now validates the DTB high alias, mandatory initrd high
alias, usable memory window alignment, reserved-range exclusion for kernel/DTB
/initrd, high-half heap alias, and RPi5-gated physmap offset, then emits
`RPI5_BOOT5_HANDOFF_BRIDGE_OK`. BOOT-5A1 additionally calls the generic
`Bootstrap::boot5a1_validate_single_cpu_scheduler` helper, which stages a CPU0-only
present bitmap and exercises the current `SmpScheduler` path with the canonical
TID-0 idle/bootstrap current, empty run queue, online=1, dispatching=1, and
wake_only=0. KernelState construction remains deferred at the precise current
source blocker: `Bootstrap::init_static_with_boot_memory_map` still constructs a
large by-value frame allocator and `init_shared_static_with_boot_memory_map` still
moves a by-value `KernelState` into `SharedKernel::new`. The hardware path now
reports `RPI5_HH5_DEFERRED reason=kernel_state_constructor_large_stack`; it does
not emit `RPI5_ENTER_USER_ERET`, install a user TTBR0, create a synthetic init,
use NR27, or start a byte-copy ELF fallback.

BOOT-5 policy updates:

- NR27 no longer exists; do not reintroduce `InitramfsReadChunk` or any NR27
  loader/probe marker.
- The synthetic init fallback no longer exists.
- A real CPIO and real `/init` are mandatory; missing or malformed boot resources
  must use canonical `BOOT_FATAL_*` markers.
- Zero-copy ELF loading is mandatory; ZC failure is fatal and must not retry with
  a byte-copy loader.
- Hardware boot failures after BOOT-4 must emit precise failure/deferred/fatal
  markers rather than silently hanging.
- BOOT-5 is single-core and interrupt-masked: CPU0 only, no timer IRQ, no GIC,
  no SMP/PSCI CPU_ON.
- Full userspace startup precedes real RPi5 driver bring-up; driver services must
  start inertly and wait for explicit resource assignment.

## 13. Current RPi5 HH state and next target

| Stage | Status |
|-------|--------|
| BOOT-1 raw UART | LIVE diagnostic |
| BOOT-2 DTB/platform/initrd discovery | LIVE diagnostic |
| BOOT-3 high-half execution + TTBR0 removal | LIVE diagnostic |
| BOOT-4 high physmap + allocators | LIVE diagnostic |
| BOOT-5A KernelState/CPU0/scheduler | CPU0/scheduler probe live; deferred at KernelState constructor stack/copy invariant |
| BOOT-5B `/init` + first EL0 | not reached |
| BOOT-5C full userspace chain | not reached |

**Next target:** remove the remaining large by-value constructor/move from the
canonical `KernelState`/`SharedKernel` bootstrap so the validated CPU0/scheduler
probe can proceed to real in-place KernelState construction without enabling
interrupts, GIC, timer, SMP, RP1/PCIe, physical drivers, NR27, or synthetic-init
fallbacks.

### 13.1 Hardware artifact build

```sh
# Hardware-proven Stage 1 image:
scripts/create-rpi5-stage1-boot-dir.sh --kernel-input <kernel-image> --boot-dir build/rpi5-stage1-boot

# High-half diagnostic artifact (non-default):
scripts/build-rpi5-highhalf-artifact.sh
scripts/create-rpi5-stage1-boot-dir.sh --kernel-input build-rpi5/kernel_2712_hh.img --boot-dir build/rpi5-hh-boot --highhalf
```

---

## 14. Explicit non-goals

Stage 1 / HH does **not** implement or claim:

- RP1 PCIe, GPIO, or PWM production support.
- Raspberry Pi 5 SMP.
- A Raspberry Pi 5 GIC driver or production interrupt path.
- Initrd construction or full YARM userspace boot.
- Changes to image IDs, CPIO packing, SpawnV5, PM, VFS, supervisor, or
  driver-manager policy.
- Hardware proof from QEMU parser/policy tests.

---

## 15. Authoring rule

Future Raspberry Pi 5 docs update **this file**. AArch64 production
boot / IPC / VFS / userspace updates `doc/ARCH_AARCH64.md`. Generic
boot flow updates `doc/BOOT.md`.
