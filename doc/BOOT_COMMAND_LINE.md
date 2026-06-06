<!-- SPDX-License-Identifier: Apache-2.0 -->

# YARM boot command line and future service-list manifest

Status: BOOTCMD-2 capture foundation. The kernel now captures bounded raw boot
command-line bytes on x86_64, AArch64, and RISC-V and provides an isolated
`yarm.manifest` parser helper. No command line is passed to userspace and no
runtime service policy is changed.

## Implemented scope

BOOTCMD-2 adds:

- fixed-size, heap-free kernel storage for up to 2048 raw command-line bytes;
- explicit absent, captured, and truncated status;
- lossless storage of non-UTF-8 bytes;
- bounded x86_64 Xen PVH `cmdline_paddr` capture;
- bounded AArch64 and RISC-V FDT `/chosen/bootargs` extraction;
- the narrow RISC-V OpenSBI register handoff correction needed to reach the FDT;
- architecture capture markers; and
- a helper-only parser for `yarm.manifest=/absolute/path`.

BOOTCMD-2 does **not** add a userspace handoff, read a manifest from CPIO,
change `/init` selection, or alter any spawn order or policy.

## Source map

| Concern | File and function/type |
| --- | --- |
| Fixed storage and parser | `src/kernel/boot_command_line.rs`: `BootCommandLine`, `BootCommandLineStatus`, `set_raw_cmdline_from_bytes`, `boot_command_line`, `parse_yarm_boot_options` |
| Shared FDT bootargs extraction | `src/arch/fdt.rs`: `chosen_bootargs` |
| x86_64 capture | `src/arch/x86_64/boot.rs`: `PvhStartInfo`, `capture_pvh_command_line`, `prepare_arch_boot` |
| AArch64 capture | `src/arch/aarch64/boot.rs`: `dtb_slice_from_start_info`, `prepare_arch_boot` |
| Existing AArch64 platform parser | `src/arch/aarch64/dtb.rs`: `parse_boot_dtb` |
| RISC-V FDT handoff/capture | `src/arch/riscv64/boot.rs`: `_start`, `prepare_arch_boot`, `dtb_slice_from_start_info` |
| Common kernel entry | `src/bin/kernel_boot.rs`: `yarm_kernel_main` |
| Hardcoded init selection | `src/arch/{x86_64,aarch64,riscv64}/boot.rs`: `load_init_elf_from_initramfs_vfs` |
| Existing userspace slots | `crates/yarm-user-rt/src/lib.rs`: `runtime::StartupContext` and startup-slot constants |
| Existing executable manifest | `crates/yarm-fs-servers/src/fs/initramfs/manifest.rs` |

## Policy-neutral storage

`BootCommandLine` owns a `[u8; 2048]`, a length, and a status. It uses no heap.
The global instance is protected by the existing kernel spin lock and is copied
out as a value; it is not exposed to userspace.

### Input and truncation rules

- Input is copied through the first NUL byte, if any.
- Empty input or an immediate NUL produces `Absent`.
- Up to 2048 bytes produces `Captured`.
- More than 2048 bytes without an earlier NUL stores the first 2048 bytes and
  produces `Truncated`.
- Bytes are retained losslessly. Kernel storage does not require UTF-8 because
  architecture boot protocols provide bytes and policy interpretation belongs
  above the capture layer.
- The x86_64 NUL-string reader provides 2049 bytes to storage. The extra byte
  distinguishes an exact 2048-byte value from an overlong or unterminated
  source. FDT capture passes the structurally bounded property slice directly.

The kernel does not parse `console=`, `rdinit=`, or service policy while
capturing the bytes.

## Per-architecture status after BOOTCMD-2

| Architecture | Source | Capture | Marker | Userspace |
| --- | --- | --- | --- | --- |
| x86_64 PVH | `PvhStartInfo.cmdline_paddr` | Implemented, bounded to 2049 source bytes and copied into 2048-byte kernel storage | `YARM_BOOT_CMDLINE_CAPTURE arch=x86_64 len=N truncated=0|1` | Deferred |
| AArch64 | FDT `/chosen/bootargs` | Implemented through shared structural FDT extraction | `YARM_BOOT_CMDLINE_CAPTURE arch=aarch64 len=N truncated=0|1` | Deferred |
| RISC-V/OpenSBI | FDT `/chosen/bootargs`, FDT pointer in `a1` | Handoff corrected and capture implemented | `YARM_BOOT_CMDLINE_CAPTURE arch=riscv64 len=N truncated=0|1` | Deferred |

### x86_64 Xen PVH

The PVH entry already preserves the `start_info` pointer and passes it to
`yarm_kernel_main`. `PvhStartInfo.cmdline_paddr` is a physical address.
`capture_pvh_command_line` now:

1. validates the PVH magic;
2. rejects zero or a range outside `KERNEL_PHYS_DIRECT_MAP_BYTES`;
3. translates the physical address with `KERNEL_BOOTSTRAP_VIRT_BASE` while the
   bootstrap direct map is active;
4. reads exactly 2049 bytes; and
5. copies through NUL into kernel-owned fixed storage.

PVH boot data is trusted bootloader input, but the direct-map range check avoids
constructing a slice outside the mapped bootstrap physical window. The copy is
performed during `prepare_arch_boot`, before ordinary memory allocation can
reuse boot data. The separate `PvhModule.cmdline_paddr` logging remains module
metadata and is not used as the kernel command line.

### AArch64 QEMU `virt`

QEMU supplies the FDT pointer in `x0`; the existing entry path preserves it and
passes it to `yarm_kernel_main`. The existing `dtb_slice_from_start_info`
validates the FDT header and size. The shared `chosen_bootargs` helper performs
bounded structural/string-table checks and returns the raw `bootargs` property
only when it belongs to the root `/chosen` node. Storage handles the optional
trailing NUL and truncation.

The existing AArch64 `parse_boot_dtb` remains responsible for memory, initrd,
CPU, GIC, and PSCI metadata. Its behavior is unchanged.

### RISC-V QEMU `virt` with OpenSBI

OpenSBI enters the primary hart with:

- `a0 = hart ID`; and
- `a1 = FDT pointer`.

Previously `_start` directly called the one-argument `yarm_kernel_main`, so the
hart ID was misinterpreted as the FDT pointer. BOOTCMD-2 performs only
`mv a0, a1` immediately before that call. The primary hart ID is not consumed by
this boot path; CPU identity continues to use existing architecture mechanisms.
No SMP, secondary-hart, interrupt, memory-map, or platform policy is changed.

`prepare_arch_boot` can now validate the actual FDT and capture
`/chosen/bootargs`. It still does not broaden RISC-V platform parsing or add
initrd parsing.

## Current meaning of Linux-style options

### `rdinit=/init`

`rdinit=/init` remains ignored. `/init` is selected because each architecture's
`load_init_elf_from_initramfs_vfs` searches the CPIO for `/init` and then
`init`. Changing or removing `rdinit=` does not affect that lookup.

### `console=ttyS0`

`console=ttyS0` remains ignored. Serial selection and initialization remain
architecture/platform and QEMU configuration decisions. Capturing a token does
not make it active policy.

## Helper-only YARM option parser

`parse_yarm_boot_options` is isolated from architecture capture, CPIO, init, PM,
and spawning. It accepts raw bytes and returns a borrowed `YarmBootOptions`.
It is not called by a live boot path.

Grammar and limits:

- ASCII-whitespace-separated tokens;
- only tokens containing `=` are considered;
- the first `=` separates key and value;
- keys longer than 64 bytes are ignored;
- values longer than 1024 bytes are ignored;
- keys outside `yarm.` are ignored;
- unknown `yarm.*` keys are ignored;
- no quoting or escaping;
- duplicate recognized keys use **last-wins** semantics; and
- no Linux-compatible key implicitly controls YARM policy.

### `yarm.manifest=` behavior

The helper recognizes:

```text
yarm.manifest=/boot/services-core.txt
```

A manifest path must:

- be nonempty;
- be absolute (`/` prefix);
- be at most 255 bytes; and
- contain no ASCII whitespace or control byte.

Invalid or later duplicate values set the helper result to no manifest path.
The helper does not normalize paths, read CPIO, validate service entries, or
spawn anything.

## Service-list manifest design remains unchanged

The recommended first service-list format remains one absolute service path per
line, with blank lines and full-line `#` comments. It should be implemented as a
separate userspace/helper parser before any live integration. An initial
`path class start-policy` table is not recommended because it prematurely
creates ordering and policy semantics.

For development QEMU, init should eventually fall back to a built-in minimal
core list when the option is absent or the selected file is missing or invalid.
A future hardened profile may explicitly fail closed. A partial or empty
manifest must not be executed.

The existing binary executable manifest in
`crates/yarm-fs-servers/src/fs/initramfs/manifest.rs` validates core ELF image
metadata and is not this future service-list policy file.

## Userspace handoff remains deferred

No current startup slot, syscall, CPIO file, or IPC message carries the captured
command line to init. Startup slots 15 and 16 continue to mean only initrd
pointer and length. BOOTCMD-2 deliberately does not:

- repurpose or extend startup slots;
- place a pointer in a capability field;
- change SpawnV5;
- synthesize command-line CPIO content; or
- change VFS.

A future **BOOTCMD-3** must design a minimal compatible handoff. Adding slots
would require reviewing slot-block length compatibility, every freestanding
entry shim, startup memory ownership/lifetime, pointer validation, and how init
obtains immutable bytes. That review must precede implementation.

## Responsibility boundary

- **Kernel:** capture bounded raw bytes, retain status, and eventually expose a
  neutral immutable view through an approved handoff.
- **Init:** interpret `yarm.*`, select and validate a service-list file, choose
  development fallback, and make staged requests.
- **PM:** remain the spawn authority using existing contracts.
- **Supervisor:** remain fault and restart authority.
- **VFS/initramfs:** retain existing filesystem and CPIO semantics.

The kernel must not select services, open the manifest, assign classes, reorder
services, or interpret `rdinit=` as YARM service policy.

## Manual QEMU validation

QEMU smoke is optional for BOOTCMD-2. If run, grep the serial log for exactly one
architecture-appropriate marker:

```text
YARM_BOOT_CMDLINE_CAPTURE arch=x86_64 len=N truncated=0
YARM_BOOT_CMDLINE_CAPTURE arch=aarch64 len=N truncated=0
YARM_BOOT_CMDLINE_CAPTURE arch=riscv64 len=N truncated=0
```

With `-append "console=ttyS0 rdinit=/init"`, `N` should be nonzero. An input over
2048 bytes should report `len=2048 truncated=1`. Existing boot-success markers
must still appear, but neither `console=` nor `rdinit=` should alter behavior.

## Frozen boundaries

BOOTCMD-2 does not change:

- syscall ABI or `SYSCALL_COUNT`;
- SpawnV5 ABI;
- VFS logic or filesystem parser behavior;
- Phase2B or Phase3B logic;
- PM zero-copy service-loading semantics;
- runtime spawn order or policy;
- service startup-slot meanings;
- IPC internals;
- VM/capability internals;
- scheduler, trap, or timer behavior; or
- driver-manager live behavior.

## Recommended next tasks

1. **BOOTCMD-3:** design and review a minimal immutable command-line handoff to
   init without changing SpawnV5, VFS, or existing startup-slot meanings.
2. **MANIFEST-1:** add a userspace/helper-only path-per-line service-list parser
   and focused tests. It may proceed independently as long as it is not wired to
   live spawning.
3. A later explicitly staged task may connect init selection to PM requests
   without moving policy into the kernel.

## MANIFEST-1 status

MANIFEST-1 adds the helper-only text service-list parser described in
`doc/SERVICE_MANIFEST.md`. The parser accepts a bounded UTF-8, one-absolute-path-
per-line format and rejects the whole file on any invalid line. It is not called
by boot capture, init, PM, VFS, or the initramfs service request loop.

The `yarm.manifest=/boot/services-core.txt` option therefore remains a future
selection mechanism: BOOTCMD-3 must first provide an immutable compatible
handoff to init. MANIFEST-2 subsequently added helper-only CPIO existence and
ELF-ident validation. No live service ordering or fallback policy changed.

## MANIFEST-2 status

MANIFEST-2 adds a helper-only validator that checks a parsed service manifest
against raw initramfs CPIO bytes. It verifies complete archive parsing, absolute
path lookup, regular-file type, minimum ELF-ident size, and ELF magic. It does
not call VFS, init policy, PM, or any spawning path, and it does not replace the
CPIO packer's mandatory 4096-byte `ALIGN_PROOF` checks.

BOOTCMD-3 remains required before init can receive `yarm.manifest=`. The future
flow is command-line handoff, manifest text read, MANIFEST-1 syntax validation,
MANIFEST-2 archive/ELF validation, init-owned fallback selection, and only then
PM-owned spawning.
