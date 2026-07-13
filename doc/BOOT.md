<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Umut Deniz Balkan -->

# YARM Boot Reference

> **Ownership rule.** All generic boot documentation lives here. Architecture-
> specific boot/trap/syscall/userspace status lives in `doc/ARCH_AARCH64.md`,
> `doc/ARCH_X86_64.md`, `doc/ARCH_RISCV64.md`. Raspberry Pi 5 hardware bring-up
> lives in `doc/RPI5_BRINGUP.md`. New boot fragment files are forbidden — update
> the canonical doc instead. See `doc/DOCUMENTATION_MAP.md`.

This document covers the generic boot flow common to every YARM target:
artifact layout, command-line capture, initrd handoff, memory reservation,
and the QEMU run commands. Per-architecture status (markers, fences,
remaining work) is in `doc/ARCH_*.md`.

---

## 1. Common boot artifacts

YARM builds one kernel image and one initramfs CPIO per target:

| Artifact | x86_64 | AArch64 | RISC-V64 |
|----------|--------|---------|----------|
| Kernel image | `build-x86_64/kernel_boot.elf` (PVH) | `build-aarch64/yarm-aarch64.bin` (raw) and `.elf` | `build-riscv64/yarm-riscv64.bin` |
| Initramfs | `build-x86_64/initramfs-core.cpio` | `build-aarch64/initramfs-core.cpio` | `build-riscv64/initramfs-core.cpio` |

Build helpers:

```sh
scripts/build-qemu-x86_64-artifacts.sh
scripts/build-qemu-aarch64-artifacts.sh
scripts/build-qemu-riscv64-artifacts.sh
```

`ARTIFACTS_STRICT=1` makes any missing target/tool/artifact fatal.

The CPIO packer enforces a mandatory 4096-byte alignment for every entry
(`ALIGN_PROOF` lines in the build log). Adding a new sbin server requires
bumping `MAX_INITRAMFS_INODES`, adding an inode entry, adding the
`from_cpio_newc` match arm, and adding a path test — see
`doc/KERNEL_UNLOCKING.md` §3 (Initramfs path table completeness).

---

## 2. Boot command-line capture

### 2.1 Storage and capture chokepoint

A single fixed-size, heap-free `BootCommandLine` lives in
`src/kernel/boot_command_line.rs`:

- Backing store: `[u8; 2048]` + length + status (Absent / Captured /
  Truncated).
- Bytes are stored losslessly; UTF-8 validation belongs to consumers that
  interpret a particular option.
- Input is copied through the first NUL byte if any.
- Empty input or an immediate NUL produces `Absent`.
- Up to 2048 bytes produces `Captured`; over 2048 stores the first 2048 and
  marks `Truncated`.
- The global instance is `SpinLock`-protected and copied out by value; it
  is never exposed to userspace.

Every architecture's boot path routes through one capture chokepoint:
`boot_command_line::set_raw_cmdline_from_bytes` (or the monotonic variant
used by RISC-V — see `doc/ARCH_RISCV64.md`). Knobs are applied at this
chokepoint, so new boot-cmdline knobs require zero arch-specific code.

### 2.2 Per-architecture source

| Architecture | Source | Marker |
|--------------|--------|--------|
| x86_64 PVH | `PvhStartInfo.cmdline_paddr` (translated via `KERNEL_BOOTSTRAP_VIRT_BASE + phys`) | `YARM_BOOT_CMDLINE_CAPTURE arch=x86_64 len=N truncated=0|1` |
| AArch64 | FDT `/chosen/bootargs` (extracted by `crate::arch::fdt::chosen_bootargs`) | `YARM_BOOT_CMDLINE_CAPTURE arch=aarch64 len=N truncated=0|1` |
| RISC-V64 | FDT `/chosen/bootargs` with the FDT pointer from OpenSBI `a1` | `YARM_BOOT_CMDLINE_CAPTURE arch=riscv64 len=N truncated=0|1` |

x86_64 reads exactly 2049 bytes from the bootloader; the extra byte
distinguishes an exact 2048-byte value from an overlong/unterminated
source.

The PVH `start_info` pointer is preserved by the entry path and passed to
`yarm_kernel_main`. `capture_pvh_command_line`:

1. Validates the PVH magic.
2. Rejects zero or a range outside `KERNEL_PHYS_DIRECT_MAP_BYTES`.
3. Translates the physical address through the bootstrap direct map.
4. Reads exactly 2049 bytes.
5. Copies through NUL into kernel-owned fixed storage.

QEMU virt (AArch64 / RISC-V) supplies the FDT pointer in `x0` / `a1` and
the existing entry preserves it. `dtb_slice_from_start_info` validates
the FDT magic and total size; `chosen_bootargs` performs bounded
structural/string-table checks and returns the raw bytes only when they
belong to the root `/chosen` node.

### 2.3 Recognized `yarm.*` knobs

YARM-owned tokens are parsed by `parse_yarm_boot_options`. Grammar:

- ASCII-whitespace-separated tokens.
- Only tokens containing `=` are considered.
- First `=` separates key and value.
- Keys longer than 64 bytes / values longer than 1024 bytes are ignored.
- Keys outside `yarm.` are ignored.
- Unknown `yarm.*` keys are ignored.
- No quoting or escaping.
- Duplicate recognized keys use **last-wins** semantics; an invalid last
  token clears back to None.

Current recognized keys:

| Key | Meaning | Default if absent |
|-----|---------|-------------------|
| `yarm.manifest=/absolute/path` | Future service-list selector (parser is helper-only today) | none |
| `yarm.platform=auto\|qemu-virt\|rpi5` | DTB-based platform classification | `auto` |
| `yarm.boot_phase=entry\|uart\|dtb\|mmu\|kernel` | RPi5 Stage1 diagnostic stop point | `kernel` |
| `yarm.max_cpus=N` | Cap present-CPU count | unset (firmware topology) |
| `yarm.loglevel=0..7` or `emerg\|alert\|crit\|err\|warn\|notice\|info\|debug` | Console loglevel | `Info` (unchanged) |
| `yarm.x86_ap_rust=1\|true\|yes\|on` / `0\|false\|no\|off` | x86_64 AP Rust-entry gate (see `doc/ARCH_X86_64.md`) | unset |
| `yarm.d6_switch_proof=1\|true\|yes\|on` / `0\|false\|no\|off` | Stage 120 x86_64-only, single-CPU, one-shot `switch_frames` proof harness gate | unset (disabled) |

Linux-style options are **captured but not policy**:

- `rdinit=/init` is ignored. `/init` is selected because the arch-neutral
  `crate::kernel::boot::load_required_init_elf_bytes()` searches the boot CPIO
  for `/init` and then `init`.
- `console=ttyS0` is ignored. Serial selection remains architecture /
  platform / QEMU decisions.

### 2.4 Mandatory init loading (fail-fast, no synthetic fallback)

Stage 197A removed the synthetic/placeholder init ELF fallback. The `/init` ELF
is **mandatory**: every fatal load condition halts boot with an explicit
`BOOT_FATAL_*` diagnostic (via each architecture's existing fatal-halt path),
never a fake init or a silent limp-on:

| Condition | Marker |
|---|---|
| No boot initramfs / CPIO | `BOOT_FATAL_INITRAMFS_MISSING` + `BOOT_FATAL_NO_CPIO` |
| Initramfs is not a valid CPIO | `BOOT_FATAL_CPIO_INVALID` |
| CPIO has no `/init` (or `init`) | `BOOT_FATAL_INIT_NOT_FOUND path=/init` |
| `/init` is malformed / oversized ELF | `BOOT_FATAL_INIT_ELF_INVALID` |
| Required init ELF segment load fails | `BOOT_FATAL_INIT_ZC_LOAD_FAILED` |

A default-off fault-injection knob `yarm.force_init_zc_load_fail=1` forces the
required init load to fail so the fatal halt path is exercisable under QEMU
(`scripts/qemu-x86_64-init-fail-fast-negative.sh` covers all four conditions).

### 2.5 Responsibility boundary

- **Kernel:** capture bounded raw bytes, retain status, apply knobs at the
  capture chokepoint.
- **Init:** future — interpret `yarm.*` and select/validate a manifest.
- **PM:** remain the spawn authority.
- **Supervisor:** remain fault/restart authority.
- **VFS/initramfs:** retain existing filesystem and CPIO semantics.

The kernel does not select services, open a manifest, assign classes,
reorder services, or treat `rdinit=` as YARM policy. A future `BOOTCMD-3`
will design a minimal immutable command-line handoff to init without
changing SpawnV5, VFS, or existing startup-slot meanings.

---

## 3. Initrd handoff and memory reservation

### 3.1 Initrd source by ISA

- **x86_64:** initrd bytes come from the PVH module list
  (`start_info` module window) discovered during early boot.
- **AArch64:** initrd bytes come from DTB `/chosen` properties
  `linux,initrd-start` and `linux,initrd-end`.
- **RISC-V64:** initrd bytes come from the same DTB `/chosen` properties
  via the shared `crate::arch::fdt::chosen_initrd`.

`install_boot_initrd_bytes()` stores a pointer/length pair for that
boot-memory window; `boot_initrd_bytes()` later exposes it as
`&'static [u8]`.

### 3.2 CRITICAL: reserve initrd before allocator reuse

The initrd physical region **MUST** be reserved before:

1. frame-allocator initialization, and
2. any physical-memory reuse by allocators.

This invariant is required because `boot_initrd_bytes()` returns a
borrowed slice into boot memory, not copied data. The pointer must never
refer to temporary buffers or memory that may be reclaimed.

#### x86_64 PVH initrd (past bug, current fix)

- **Past bug:** the PVH module handoff path installed initrd bytes
  without reserving the module window, allowing allocator-reuse overlap.
- **Current fix:** x86_64 PVH handoff explicitly reserves the
  page-aligned initrd window through
  `Bootstrap::install_boot_reserved_range(...)` **before**
  `install_boot_initrd_bytes(...)`.

#### Failure mode if invariant is violated

Allocator reuse can overwrite initrd bytes, corrupting CPIO/ELF parsing
and producing non-deterministic early-boot failures.

### 3.3 RISC-V64 RAM staging

RISC-V64 also stages the real RAM window from the DTB `/memory` node and
reserves firmware / DTB / initramfs before frame-allocator init. Without
this, the common fallback memory map seeds allocators with MMIO addresses
(QEMU virt UART region) and the first frame write store-faults. See
`doc/ARCH_RISCV64.md` for the full chain.

---

## 4. QEMU run commands

### 4.1 x86_64 (primary path; -smp 1 accepted baseline)

```sh
# One-command artifact staging
scripts/build-qemu-x86_64-artifacts.sh
# Strict (fail if missing tools/artifacts)
ARTIFACTS_STRICT=1 scripts/build-qemu-x86_64-artifacts.sh

# Core smoke
scripts/qemu-x86_64-core-smoke.sh
# Strict mode
QEMU_SMOKE_STRICT=1 scripts/qemu-x86_64-core-smoke.sh

# Optional-FS strict smoke (RAMFS+ext4 expected; FAT skipped by default)
QEMU_SMOKE_STRICT=1 scripts/qemu-x86_64-optional-fs-smoke.sh

# Override artifact paths
KERNEL_IMAGE=build-x86_64/kernel_boot.elf \
  INITRAMFS_IMAGE=build-x86_64/initramfs-core.cpio \
  scripts/qemu-x86_64-core-smoke.sh
```

Core smoke success markers searched in the serial log:

- `YARM_BOOT_OK`
- `YARM_PROC_VFS_OK`
- `YARM_INIT_START`
- `YARM_INIT_DONE`

Optional-FS strict smoke marker contract is documented in
`doc/KERNEL_UNLOCKING.md` §3 (do not rename or remove these markers).

### 4.2 AArch64 (gate-hardened)

```sh
scripts/build-qemu-aarch64-artifacts.sh
QEMU_SMOKE_STRICT=1 scripts/qemu-aarch64-core-smoke.sh
QEMU_SMOKE_STRICT=1 scripts/qemu-aarch64-optional-fs-smoke.sh
```

By default the AArch64 runner launches QEMU **without** `-append` so
early bring-up debug can rely on serial breadcrumbs even when cmdline
parsing is not implemented yet. Override:

```sh
KERNEL_CMDLINE="console=ttyAMA0 rdinit=/init" scripts/qemu-aarch64-core-smoke.sh
```

Hardened progression markers required by the AArch64 strict gate (not
marker-only presence — ordered progression):

```text
YARM_AARCH64_BOOT_MARKER stage=_start
YARM_AARCH64_BOOT_MARKER stage=prepare_arch_boot
YARM_AARCH64_BOOT_MARKER stage=vbar_el1_ready
YARM_AARCH64_BOOT_MARKER stage=mmu_enabled
YARM_AARCH64_BOOT_MARKER stage=run_with_prepared_kernel
YARM_BOOT_OK
YARM_INIT_START
YARM_INIT_DONE
```

Plus timer/runtime progression:

```text
YARM_TIMER_IRQ_DELIVERED
YARM_TIMER_EOI_DONE
YARM_SCHED_TICK
```

### 4.3 RISC-V64

```sh
scripts/build-qemu-riscv64-artifacts.sh
# Direct command (see doc/ARCH_RISCV64.md for the full marker sequence)
qemu-system-riscv64 -machine virt -m 512M -smp 1 \
  -nographic -monitor none -serial stdio -bios default \
  -kernel build-riscv64/yarm-riscv64.bin \
  -initrd build-riscv64/initramfs-core.cpio \
  -append "console=ttyS0 rdinit=/init"
```

Per-arch smoke contracts and current state live in `doc/ARCH_*.md`.

---

## 5. Frozen boundaries

Boot capture/runbook changes do **not** change:

- syscall ABI or `SYSCALL_COUNT` (= 31).
- SpawnV5 ABI.
- VFS logic or filesystem parser behavior.
- PM zero-copy service-loading semantics.
- Runtime spawn order or policy.
- Service startup-slot meanings (`STARTUP_SLOT_COUNT` = 18; slot 12 is
  PM-private).
- IPC internals.
- VM/capability internals.
- Scheduler, trap, or timer behavior.
- Driver-manager live behavior.

See `doc/KERNEL_UNLOCKING.md` §3 for the full invariant list.

---

## 6. Authoring rule

Future generic boot docs update **`doc/BOOT.md`**. Per-arch docs update
**`doc/ARCH_AARCH64.md`**, **`doc/ARCH_X86_64.md`**,
**`doc/ARCH_RISCV64.md`**. RPi5 hardware docs update
**`doc/RPI5_BRINGUP.md`**. Do not create new milestone / status / context
/ audit fragment files.
