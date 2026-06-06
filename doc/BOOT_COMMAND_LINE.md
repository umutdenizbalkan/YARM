<!-- SPDX-License-Identifier: Apache-2.0 -->

# YARM boot command line and future service-list manifest

Status: BOOTCMD-1 audit and design only. This document does not authorize or
implement a runtime service-policy change.

## Executive summary

The QEMU examples that pass `console=ttyS0 rdinit=/init` look like Linux boot
arguments, but YARM does not currently parse either option and does not pass a
raw command line to userspace.

- x86_64 receives a Xen PVH `start_info` pointer. YARM reads and logs the numeric
  `cmdline_paddr` field, but it never dereferences, copies, stores, or logs the
  main command-line text. Module-specific command lines are separately logged
  (up to 96 bytes), but those are not the QEMU `-append` kernel command line.
- AArch64 receives the QEMU FDT in `x0` and parses selected memory, CPU, PSCI,
  interrupt-controller, and `/chosen/linux,initrd-*` properties. It does not
  parse `/chosen/bootargs`.
- RISC-V QEMU/OpenSBI convention supplies the boot hart ID in `a0` and the FDT
  pointer in `a1`. YARM's `_start` currently calls `yarm_kernel_main` without
  moving `a1` to `a0`, so `prepare_arch_boot` is given the hart ID as its
  `start_info_ptr`. Even if that handoff were corrected, the current RISC-V
  function only validates an FDT-sized byte slice and does not parse
  `/chosen/bootargs` or initrd properties.
- The userspace startup context carries capability/task metadata and the mapped
  initrd pointer/length in slots 15 and 16. It has no command-line field or
  boot-options channel.
- `/init` is selected by hardcoded CPIO lookup in each architecture's bootstrap
  path, independently of `rdinit=`.

Consequently, changing `rdinit=` or `console=` does not change current YARM
behavior. The safest future design is for the kernel to capture a bounded raw
command line, expose it through a separately designed minimal handoff, and let
init select a service-list file using `yarm.manifest=`. The kernel must not make
service-policy decisions.

## Audit method and scope

The audit searched architecture boot code, kernel bootstrap state, userspace
startup slots, init/control-plane code, initramfs code, QEMU smoke scripts, and
the existing executable-manifest parser for:

- `cmdline`, `cmdline_paddr`, `bootargs`, `chosen`, and `PVH`;
- `console=`, `rdinit`, and generic `key=value` parsing;
- `startup_context`, startup slots, and initrd handoff;
- `/init` lookup and current service launch policy; and
- existing uses of the word "manifest".

No live parser scaffold is added by BOOTCMD-1.

### Source map

| Concern | Exact file and function/type | Current responsibility |
| --- | --- | --- |
| x86_64 PVH entry | `src/arch/x86_64/boot.rs`: `pvh_start32`, `long_mode_entry` | Preserves and passes the PVH `start_info` pointer |
| x86_64 PVH metadata | `src/arch/x86_64/boot.rs`: `PvhStartInfo`, `PvhModule`, `read_pvh_module_summary`, `log_pvh_boot_metadata`, `prepare_arch_boot` | Logs the main cmdline pointer, logs bounded module cmdline text, installs the first module as initrd |
| AArch64 FDT entry | `src/arch/aarch64/boot.rs`: `_start`, `dtb_slice_from_start_info`, `prepare_arch_boot` | Preserves the FDT pointer, maps boot metadata into bootstrap state |
| AArch64 FDT parser | `src/arch/aarch64/dtb.rs`: `ParsedDtb`, `parse_boot_dtb` | Parses selected memory/initrd/GIC/CPU/PSCI properties, not `bootargs` |
| RISC-V entry and FDT stub | `src/arch/riscv64/boot.rs`: `_start`, `prepare_arch_boot`, `dtb_slice_from_start_info` | Currently forwards `a0` unchanged and only validates/discards a putative FDT slice |
| Common kernel entry | `src/bin/kernel_boot.rs`: `yarm_kernel_main` | Passes its single argument to architecture `prepare_arch_boot` |
| Init selection | `src/arch/{x86_64,aarch64,riscv64}/boot.rs`: `load_init_elf_from_initramfs_vfs`, `bootstrap_first_user_task` | Hardcodes `/init`/`init` CPIO lookup |
| Kernel initrd state | `src/kernel/boot/bootstrap_state.rs`: `install_boot_initrd_bytes`, `boot_initrd_bytes` | Stores only the initrd byte window, not command-line metadata |
| Userspace startup context | `crates/yarm-user-rt/src/lib.rs`: `runtime::StartupContext`, `install_startup_arg_slots`, `startup_context` | Defines the 18 current startup slots; no command-line field |
| Live init control plane | `crates/yarm-control-plane-servers/src/control_plane/init/service.rs`: `run` | Reads PM caps and executes the current fixed spawn sequence |
| Existing executable manifest | `crates/yarm-fs-servers/src/fs/initramfs/manifest.rs`: `parse_core_service_manifest`, `build_core_service_elf_launch_plan` | Validates the binary core-image manifest; not a boot-selected service list |
| QEMU examples | `scripts/qemu-x86_64-core-smoke.sh`, `scripts/qemu-aarch64-core-smoke.sh`, `scripts/qemu-riscv64-core-smoke.sh` | Construct current `-append` arguments |

## Per-architecture command-line status

The classification letters used below are:

- **A**: raw command line captured and logged;
- **B**: raw command line captured but unused;
- **C**: parsed in the kernel;
- **D**: passed to userspace;
- **E**: completely ignored; and
- **F**: unclear or a boot-handoff bug.

### x86_64 Xen PVH

QEMU `-append` is represented by the Xen PVH `start_info.cmdline_paddr` field.
The 32-bit PVH entry preserves the `start_info` pointer in `esi`, transitions to
long mode, and passes it to `yarm_kernel_main` in `rdi`.

`PvhStartInfo` includes `cmdline_paddr`. `read_pvh_module_summary` logs the
numeric value as part of `YARM_BOOT_PVH_START_INFO`, but the field is not used
after that log. There is no bounded read of the main command-line text, no
persistent raw-command-line buffer, and no option parser.

`PvhModule` also has a `cmdline_paddr`. YARM reads at most 96 bytes from each
module-specific command line and logs valid UTF-8 as
`YARM_BOOT_PVH_MODULE_CMDLINE`. This describes a boot module and must not be
confused with `start_info.cmdline_paddr` supplied by QEMU `-append`.

The first valid PVH module is independently treated as the initramfs and stored
in kernel bootstrap state. This module handling does not consult either command
line.

**Classification:** effective **E** for the QEMU `-append` text, with a partial
metadata observation: YARM logs its pointer value but does not capture the raw
string. It is not A, B, C, or D. Module command-line logging is unrelated to
kernel option handling.

### AArch64 QEMU `virt`

QEMU supplies the FDT pointer in `x0`. The entry code preserves `x0` across BSS
initialization and EL setup, then passes it to `yarm_kernel_main`.

`parse_boot_dtb` recognizes selected root, memory, CPU, PSCI, interrupt
controller, and `/chosen/linux,initrd-start` and `linux,initrd-end` properties.
It has no `bootargs` field in `ParsedDtb` and no property branch for
`/chosen/bootargs`. Therefore a QEMU `-append` string may be present in the FDT,
but YARM neither captures nor logs it.

The AArch64 smoke script leaves `KERNEL_CMDLINE` empty by default and only adds
`-append` when explicitly overridden; its comment already notes that AArch64
command-line parsing is not validated.

**Classification:** **E**. The FDT is captured and partially parsed, but the
command-line property itself is ignored. It is not C or D.

### RISC-V QEMU `virt` with OpenSBI

The standard boot register convention is `a0 = boot hart ID` and `a1 = FDT
pointer`. YARM's primary `_start` establishes a stack and directly calls
`yarm_kernel_main`; it does not preserve the two values or move `a1` into the
first C argument register. Thus `yarm_kernel_main(start_info_ptr)` receives the
hart ID from `a0`, not the FDT pointer.

`prepare_arch_boot` calls `dtb_slice_from_start_info(start_info_ptr)`. For the
usual boot hart 0 this immediately sees a null pointer and returns. For any
nonzero hart ID it would attempt to interpret that small integer as an FDT
address. This is a boot-handoff bug independent of command-line policy.

Even with the register handoff corrected, `prepare_arch_boot` currently only
validates the FDT magic and total size, then discards the slice. There is no
RISC-V FDT property parser for `bootargs`, `linux,initrd-start`, or
`linux,initrd-end` in this path.

The RISC-V smoke script passes `console=ttyS0 rdinit=/init`, but neither token is
consumed by YARM.

**Classification:** **F** for the current DTB-register handoff and **E** for
command-line content. It is not A, B, C, or D.

## Current status matrix

| Architecture | Where QEMU `-append` lands | Raw text captured? | Logged? | Parsed? | Userspace handoff? | Classification |
| --- | --- | --- | --- | --- | --- | --- |
| x86_64 PVH | `PvhStartInfo.cmdline_paddr` | No; only the pointer field is read | Pointer value only; module command lines are separately logged | No | No | E, with pointer metadata observed |
| AArch64 | FDT `/chosen/bootargs` | No | No | No | No | E |
| RISC-V | FDT `/chosen/bootargs`, with FDT pointer supplied in `a1` | No | No | No | No | F for handoff, E for content |

There is no `BootConfig`, `KernelBoot`, or equivalent persistent boot-options
object in the audited paths.

## Current meaning of `rdinit=/init` and `console=ttyS0`

### `rdinit=/init`

`rdinit=/init` currently does nothing.

Each non-hosted architecture has a `load_init_elf_from_initramfs_vfs` helper
that searches the boot CPIO for `/init` and then `init`. The architecture's
`bootstrap_first_user_task` calls that helper and uses an architecture-local
fallback image when it cannot load the file. No helper accepts a command-line
value, and no `rdinit` parser exists.

Therefore:

- `/init` is selected because it is hardcoded in x86_64, AArch64, and RISC-V
  bootstrap code;
- changing `rdinit=/init` to another value does not select another executable;
- removing `rdinit=` does not stop the hardcoded `/init` lookup; and
- supporting an alternate init path would be a separate policy change, not an
  incidental consequence of implementing `yarm.manifest=`.

### `console=ttyS0`

`console=ttyS0` currently does nothing.

Serial output is selected and initialized by architecture/platform code and by
QEMU's serial configuration. No `console` parser exists. Removing or changing
the token does not alter YARM's console selection. `console=` should remain
documented as ignored unless a later, explicit console-selection design is
approved.

## Existing userspace handoff

The runtime startup ABI contains 18 fixed slots. They carry task IDs,
capabilities, supervisor metadata, two service-extra capabilities, the boot
initrd pointer/length, and the PM request endpoint. No slot contains a command
line, command-line pointer, boot-options object, or manifest path.

The init service reads `startup_context()` and immediately uses its PM request
and reply capabilities to request the existing hardcoded service sequence. It
does not receive or query boot options. The CPIO bytes made available through
initrd slots 15 and 16 are archive content only; they are not a channel for
QEMU `-append` metadata.

**Conclusion:** no command-line handoff to userspace currently exists. BOOTCMD-1
must not repurpose a startup slot, extend SpawnV5, add a syscall, synthesize a
CPIO file, or change VFS behavior. The handoff requires a separately reviewed
BOOTCMD-2 design.

## Existing executable manifest is not the proposed service list

`crates/yarm-fs-servers/src/fs/initramfs/manifest.rs` already parses a bounded
binary **executable manifest**. It identifies required core ELF images and
validates image lengths, entry addresses, and load segments. It is not a text
list selected by a boot option, and it must not silently acquire runtime service
ordering or profile semantics.

The future text **service-list manifest** described below is a separate
init-owned policy input. Naming, documentation, and implementation should keep
these two concepts distinct.

## Proposed YARM boot-option namespace

Use:

```text
yarm.manifest=/boot/services-core.txt
```

`yarm.manifest` is preferred over `yarm.loadserverlist` because it is shorter,
namespaced, and does not encode the implementation detail that PM performs the
spawn request. Documentation should consistently call its value the
"service-list manifest path" to distinguish it from the existing executable
manifest.

Linux-compatible-looking names must not implicitly control YARM policy:

- unrecognized keys without `yarm.` are ignored;
- `console=` remains ignored until deliberately implemented;
- `rdinit=` remains ignored until deliberately implemented; and
- YARM-owned options use the `yarm.` prefix.

### Proposed command-line grammar and bounds

The first implementation should use the following deliberately small grammar:

1. The input is a byte string of at most **4096 bytes**, excluding a terminating
   NUL supplied by firmware or a bootloader.
2. ASCII space, tab, carriage return, and line feed separate tokens.
3. A recognized option is exactly `key=value`; the first `=` separates key and
   value.
4. Keys are 1 to **64 bytes**. Values are 1 to **1024 bytes**.
5. Keys use ASCII letters, digits, `.`, `_`, and `-`. A recognized YARM key must
   begin with `yarm.`.
6. Values are opaque non-whitespace bytes at the generic parser layer. For
   `yarm.manifest`, the value must be valid UTF-8, at most **255 bytes**, and an
   absolute path beginning with `/`.
7. Quoting, escaping, environment expansion, embedded whitespace, and NUL bytes
   are not supported.
8. A duplicate recognized key is invalid. For `yarm.manifest`, duplication must
   reject the command-line selection and invoke the configured fallback rather
   than using "first wins" or "last wins".
9. Unknown non-`yarm.` keys are ignored. Unknown `yarm.` keys are ignored with a
   diagnostic so newer boot media can still run on an older kernel/init pair.
10. Malformed or oversized input must never be partially interpreted as a valid
    `yarm.manifest` selection. Record a diagnostic and use the profile's
    fallback policy.

These are design bounds, not current behavior.

## Proposed service-list manifest

### Safer first format

For the first implementation, use **one absolute service path per line**:

```text
# Minimal development profile
/sbin/initramfs_srv
/sbin/devfs_srv
/sbin/vfs_srv
/sbin/driver_manager
/sbin/blkcache_srv
/sbin/virtio_blk_srv
/sbin/fat_srv
/sbin/ext4_srv
```

Rules:

- UTF-8 text, maximum **64 KiB**;
- LF or CRLF line endings;
- blank lines ignored;
- `#` starts a full-line comment after optional leading ASCII whitespace;
- one absolute path per non-comment line;
- no inline comments, quoting, escaping, fields, or whitespace inside a path;
- path length 1 to **255 bytes**;
- reject NUL, `.` and `..` path components, repeated entries, trailing fields,
  non-absolute paths, and more than **128 entries**; and
- preserve file order as the requested order only when a later staged runtime
  policy explicitly permits that order. Parsing the file must not by itself
  change today's spawn order.

This format is safer than an initial
`path class start-policy` table. Classes and start policies would immediately
create policy vocabulary, compatibility rules, and ordering semantics before
the control-plane behavior is staged. Optional metadata can be introduced in a
versioned second format after the path-only parser and policy boundary are
proven.

### Selection and fallback

Future intended flow:

1. The kernel captures a bounded raw command line but makes no service-policy
   decision.
2. Init receives that command line or a minimal boot-options view through an
   approved handoff.
3. If `yarm.manifest=/path` is present, init reads that logical absolute path
   from the CPIO/initramfs through existing userspace facilities.
4. Init validates the service-list file completely before requesting any
   manifest-selected service.
5. Init requests PM to spawn the selected services only after the existing
   bootstrap prerequisites are satisfied.
6. PM remains the spawning mechanism; the supervisor remains the fault and
   restart authority.

Recommended fallback policy:

- **Development QEMU profile:** if the option is absent, the file is missing,
  or validation fails, emit a clear diagnostic and use a built-in minimal core
  service list.
- **Future hardened/production profile:** permit an explicit fail-closed mode,
  but do not make it the default until recovery and diagnostics are defined.
- Never execute a partially parsed manifest.
- A valid empty manifest should be rejected rather than interpreted as "spawn
  nothing."

The fallback list must be owned by init and versioned with init. It must not be
hidden in the kernel or inferred by PM.

## Kernel versus userspace responsibility

### Kernel

The kernel should eventually:

- obtain the architecture-provided command-line bytes;
- copy them into bounded kernel-owned storage before boot memory can be reused;
- retain whether the source was absent, malformed, or truncated; and
- expose the raw bytes or a minimal neutral boot-options view to init through a
  separately approved channel.

The kernel should not:

- choose a service-list path;
- open or parse the service-list file;
- assign service classes or start policies;
- reorder services;
- request PM spawns; or
- interpret Linux `rdinit=` as YARM service policy.

### Init, PM, and supervisor

- **Init** owns option interpretation, service-list selection, full-file
  validation, fallback selection, and staged spawn requests.
- **PM** performs spawn using existing contracts and semantics.
- **Supervisor** remains the fault/restart authority.
- **VFS/initramfs services** retain their existing filesystem and CPIO behavior;
  BOOTCMD work must consume existing userspace access rather than changing VFS
  routing or parser behavior.

## Implementation blockers and required follow-up

A live implementation is blocked on all of the following:

1. **x86_64 capture:** safely translate and bounded-read
   `start_info.cmdline_paddr`, distinguish absent/unterminated/oversized data,
   and copy it before boot memory ownership changes.
2. **AArch64 capture:** extend the FDT result with bounded
   `/chosen/bootargs` extraction without disturbing existing memory/initrd/PSCI
   parsing.
3. **RISC-V handoff:** correct the primary entry contract so the FDT pointer in
   `a1` reaches architecture boot preparation, then add bounded FDT bootargs
   and initrd parsing as separately reviewed work.
4. **Architecture-neutral storage:** define bounded kernel-owned raw-command-line
   state and source/error status.
5. **Userspace handoff:** no current startup slot or other channel carries boot
   options. A minimal safe channel must be designed without changing the
   syscall ABI, SpawnV5 ABI, VFS, Phase2B/Phase3B, or startup-slot semantics.
6. **Init parser:** add isolated command-line parsing and service-list parsing
   with focused tests before live wiring.
7. **Userspace file access point:** identify the exact existing init/initramfs
   API by which init reads the logical CPIO path, without changing filesystem
   parser or VFS behavior.
8. **Profile policy:** define how development fallback versus future fail-closed
   operation is selected and diagnosed.
9. **Staging:** integrate service requests only in a later task that explicitly
   approves runtime spawn order and policy changes.

## Frozen boundaries for BOOTCMD and manifest work

Until a later task explicitly changes scope, this design must not change:

- syscall ABI or `SYSCALL_COUNT`;
- SpawnV5 ABI;
- VFS logic or filesystem parser behavior;
- Phase2B or Phase3B logic;
- PM zero-copy service-loading semantics;
- current runtime spawn order or policy;
- service startup slots;
- IPC internals;
- VM/capability internals;
- scheduler, trap, timer, or interrupt code; or
- driver-manager live behavior.

## Recommended next task

Proceed with **BOOTCMD-2**, not live manifest spawning.

BOOTCMD-2 should design and, only if its boundary review approves, implement:

- correct architecture-specific raw command-line capture, including the RISC-V
  register handoff fix;
- bounded architecture-neutral storage and diagnostics; and
- a minimal userspace handoff to init that does not alter SpawnV5, VFS,
  Phase2B/Phase3B, or existing startup-slot meanings.

After init can reliably receive the command line, **MANIFEST-1** can add
helper-only parsers and focused tests for `yarm.manifest=` and the path-per-line
service list. Live spawn-policy integration remains a later, explicitly staged
task.
