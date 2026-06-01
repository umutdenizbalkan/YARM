<!-- SPDX-License-Identifier: Apache-2.0 -->

# RISC-V SMP Secondary Release Audit

## Scope

This note audits whether `src/arch/riscv64/boot.rs::release_secondary_cpus_after_bootstrap()`
can safely release secondary harts today. It is documentation-only: no syscall ABI, IPC,
VFS, MemoryObject, x86_64 SMP, or other architecture behavior is changed.

## Current status

The claim is true: the RISC-V release hook is empty, so YARM does not release or online
RISC-V secondary harts after bootstrapping the first user tasks. The current RISC-V boot
path is effectively single-hart for the supported QEMU `virt` smoke profile.

Key findings:

- The RISC-V `_start` path installs a single bootstrap stack, calls `yarm_kernel_main`, and
  then parks only after that function returns. It does not read `mhartid`, branch secondary
  harts away from the BSP path, select per-hart stacks, or provide a secondary entry point.
- `release_secondary_cpus_after_bootstrap()` is currently a no-op.
- `run_with_prepared_kernel()` initializes a normal `KernelState` and logs topology, but it
  does not install a shared/static trap owner or start secondary harts.
- `prepare_arch_boot()` can locate a DTB blob but currently discards it; it does not stage
  the RISC-V CPU bitmap for `Bootstrap::init()`.
- The RISC-V topology helper parses a text fixture shape such as `/cpus { cpu@1 { }; }`; it
  is not a real flattened-device-tree CPU parser for QEMU `virt` or hardware DTBs.
- There is no RISC-V SBI HSM wrapper in the tree. The only direct SBI use in the RISC-V arch
  code is the legacy console `console_putchar` ecall path.

## Comparison with other architecture release paths

- AArch64 has a PSCI-based secondary-start path: it records a PSCI conduit from the DTB,
  calls `CPU_ON`, has a dedicated secondary entry, waits for an explicit BSP release flag,
  and then marks secondaries ready/online.
- x86_64 has an AP trampoline implementation in `src/arch/x86_64/smp.rs`, but its
  `release_secondary_cpus_after_bootstrap()` hook is empty; x86_64 SMP behavior is outside
  this audit and was intentionally not changed.
- The generic hook is therefore an architecture handoff point after first-user bootstrap,
  but RISC-V lacks the prerequisites that AArch64 already has for safe secondary execution.

## SBI HSM and hart discovery audit

For QEMU `virt` under OpenSBI, secondary harts are expected to remain stopped/parked until
S-mode requests `hart_start` through the SBI HSM extension. However, YARM cannot safely add
that call yet because the repository currently lacks all of the following RISC-V pieces:

1. An SBI HSM wrapper (`hart_start`) and extension availability/probing/error handling.
2. A known-safe RISC-V secondary entry symbol with the correct physical entry address for
   OpenSBI.
3. Per-hart bootstrap stacks and handoff state populated before release.
4. Secondary code that sets `stvec`, `satp`, stack/TLS/per-CPU state, interrupt state, and
   scheduler CPU identity before entering any shared kernel path.
5. Real DTB CPU/hart-id discovery. The existing RISC-V `prepare_arch_boot()` does not stage
   parsed CPU IDs, and the topology helper does not parse binary FDT CPU nodes.
6. A scheduler-online handshake that distinguishes "hart was started by firmware" from
   "hart initialized enough to run scheduler work". Calling `bring_up_cpu()` immediately
   after `hart_start()` would fake SMP readiness before the secondary has acknowledged local
   initialization.

## QEMU `virt` decision

Do not implement QEMU `virt` secondary release yet. Even with the common QEMU convention of
hart IDs `0..N-1`, starting a hart without a dedicated secondary entry and per-hart stack
would risk running the normal `_start`/`yarm_kernel_main` path on multiple harts sharing the
same bootstrap stack and global initialization path. That is not a safe incremental change
and could break single-hart boot by introducing partial SMP state.

The safe near-term behavior is to leave RISC-V secondary release disabled and keep
single-hart boot unchanged.

## VisionFive 2 / hardware-board decision

VisionFive 2 and other hardware boards remain explicitly deferred. Board-specific hart
availability and boot-hart identity must come from a real DTB/firmware parser or an explicit
board profile. In particular, designs where hart `0` is not a normal S-mode application hart
cannot use the QEMU `virt` `0..N-1` assumption.

## Recommended design before enabling RISC-V secondary release

1. Add a minimal RISC-V SBI module with HSM extension probing and `hart_start` wrapper.
2. Add a real FDT parser path for `/cpus` that records firmware hart IDs separately from
   scheduler `CpuId` indices, and stage the discovered topology before `Bootstrap::init()`.
3. Add per-hart boot stacks and a small secondary handoff record containing scheduler CPU ID,
   hart ID, stack top, root page-table/SATP information, and shared-kernel pointer.
4. Add a dedicated RISC-V secondary entry that does not run `_start`/global bootstrap again;
   it should initialize local trap state (`stvec`), page-table state (`satp`/`sfence.vma`),
   interrupt state, and then acknowledge readiness.
5. Extend the scheduler/topology path so the BSP marks a CPU online only after the secondary
   has acknowledged local initialization.
6. Gate any QEMU `virt` convenience assumptions behind the existing QEMU `virt` platform
   profile, and keep VisionFive 2/hardware board profiles deferred until their hart IDs are
   parsed or explicitly described.

