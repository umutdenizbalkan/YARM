# AArch64 initrd `/init` ELF + syscall bring-up notes

This document records the fixes required to reliably boot the AArch64 initrd `/init` ELF and sustain repeated syscall/yield loops.

## Key requirements and fixes

1. **Boot QEMU with `yarm-aarch64.bin` (raw), not `yarm-aarch64.elf`.**
   - The raw image path is required for correct DTB `x0` handoff during this boot flow.

2. **`/init` in initramfs is ELF, not shell script.**
   - Artifact staging copies the built server ELF directly to `/init` and marks it executable.

3. **AArch64 userspace must use a dedicated user linker script at `0x00400000`.**
   - Kernel and user binaries must not share the kernel link base on AArch64.

4. **ELF PT_LOAD permission contract is enforced (W^X).**
   - User page permissions are derived from ELF PT_LOAD `p_flags` (`PF_R`/`PF_W`/`PF_X`) on a per-page basis.
   - Overlapping PT_LOAD segments are combined per page before mapping (`p_flags` OR across overlapping load segments).
   - Expected mapping policy:
     - text/code: `RX` (`PF_R|PF_X`)
     - data/BSS: `RW` (`PF_R|PF_W`)
     - read-only: `RO` (`PF_R`)
   - Conservative compatibility policy for uncommon flags:
     - `PF_W` without `PF_R` maps as `RW` (no write-only user mode mapping)
     - `PF_X` without `PF_R` maps as `RX` (no execute-only user mode mapping)
   - `PF_W|PF_X` PT_LOAD pages are rejected with `KernelError::WrongObject` (no user W+X mappings).
   - Boot trace `ELF_MAP_PAGE_PERMS` reflects the final computed page flags.

5. **AArch64 EL1->EL0 handoff and syscall-return assembly must preserve critical state.**
   - Do not keep critical ELR/SP/SPSR values only in caller-saved registers across marker/logging calls.
   - Write critical system registers immediately after loading/preserving their values.

6. **`yield_current` handles same-task/no-peer yields safely.**
   - Cooperative yield with only one runnable user task must return to the same task cleanly and avoid poisoning scheduler/current-task state.

## Current validation state

- Repeated AArch64 `yield` syscalls work in QEMU with `/init` as a real ELF and return path ELR continuity preserved.
- ELF loader remains intentionally minimal; uncommon binaries requiring writable+executable PT_LOAD mappings are rejected by policy.
