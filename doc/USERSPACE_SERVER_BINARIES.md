// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

# Userspace server binaries and image alignment

This document defines the wrapper and image-layout contract for ordinary YARM
userspace server binaries. It applies to the filesystem, driver, network,
compatibility, and UI server crates. Control-plane binaries are audit-only
references: `init_server`, `process_manager`, `supervisor`, and similar
programs may have startup synchronization or policy requirements and must not
be rewritten mechanically.

## Canonical hosted/freestanding wrapper

An ordinary server binary must:

1. select `no_std` and `no_main` when `hosted-dev` is disabled;
2. install a **256 KiB** (`256 * 1024`) freestanding allocator, unless a
   separately reviewed design documents a different bound;
3. expose a small `run()` wrapper and a hosted `main()` that calls it;
4. expose a freestanding `yarm_user_entry()`;
5. log `<SERVICE>_BIN_ENTRY_START` and `<SERVICE>_BEFORE_RUN` from
   `yarm_user_entry()`, before calling `run()`;
6. expose `_start(...)` and pass the startup task, process-manager
   capabilities, and startup-slot block to
   `runtime::enter_user_entrypoint(..., yarm_user_entry)`; and
7. provide a freestanding panic handler.

Do not casually use a `1024 * 1024` allocator. A larger heap is a resource and
image-layout decision, not boilerplate.

### Resident-loop ownership

The service implementation, not the binary wrapper, owns its IPC receive loop.
If `run_<service>()` is resident on every path, the wrapper ends with a single
`unreachable!` assertion after `run()`. It must not add another receive or
resident-yield loop. If the implementation is a bounded startup stub or may
return because no receive endpoint exists, the wrapper uses the standard
post-run `yield_now()` fallback. This keeps the task resident without creating
a second IPC consumer.

## RISC-V userspace ELF alignment

RISC-V userspace and server ELFs use `targets/riscv64-yarm-user-none.json`.
The target passes both of these linker constraints to `rust-lld`:

```text
-zmax-page-size=0x1000
-zcommon-page-size=0x1000
```

Together with the userspace linker script, every ELF `PT_LOAD` segment must
report `Align 0x1000` in `readelf -lW`. These flags are userspace-only; kernel
targets and kernel linker scripts are outside this contract.

## QEMU CPIO ELF data alignment

Every ELF file packed by the QEMU initramfs path must begin at a 4096-byte
archive data offset. This includes `/init`, every early and late `/sbin/*`
service, and any other ELF added to a profile. Alignment is based on ELF magic,
not on a hard-coded service list, so new services inherit the rule.

`scripts/pack-initramfs-aligned.py` inserts CPIO padding entries and emits one
proof line per ELF:

```text
ALIGN_PROOF path=/sbin/example data_offset=4096 alignment_mod=0 aligned=true
```

Packing fails rather than falling back to an unaligned CPIO writer when Python
or the mandatory packer is unavailable. A proof with `aligned=false` is a
build failure. All x86_64, AArch64, and RISC-V QEMU artifact profiles call the
same aligned packer.

## Checklist for future server work

- [ ] Do not edit special control-plane binaries by pattern replacement.
- [ ] Keep the allocator at `256 * 1024` unless a reviewed exception exists.
- [ ] Keep hosted `main`, freestanding `yarm_user_entry`, runtime `_start`, and
      the freestanding panic handler.
- [ ] Emit both required entry markers from `yarm_user_entry`.
- [ ] Determine whether the service implementation is resident before adding
      a post-run fallback; never duplicate its IPC receive loop.
- [ ] Build/check both hosted and `--no-default-features` forms.
- [ ] For RISC-V, inspect every `PT_LOAD` with `readelf -lW` and require
      `Align 0x1000`.
- [ ] For initramfs changes, retain an `ALIGN_PROOF ... aligned=true` line for
      every packed ELF, including `/init` and all `/sbin/*` entries.
- [ ] Do not change runtime spawn policy, service ABI semantics, or kernel
      linker behavior as part of wrapper hygiene.
