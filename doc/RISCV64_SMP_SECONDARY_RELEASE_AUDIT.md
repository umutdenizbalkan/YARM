<!-- SPDX-License-Identifier: Apache-2.0 -->

# RISC-V SMP Secondary Release Audit

## Scope

This note tracks the staged QEMU `virt`/OpenSBI path for
`src/arch/riscv64/boot.rs::release_secondary_cpus_after_bootstrap()`. The current
implementation starts secondary harts only into a dedicated park path; it does not claim
full RISC-V SMP, does not mark secondaries scheduler-online, and does not change syscall
ABI, IPC, VFS, MemoryObject, x86_64 SMP, or AArch64 behavior.

## Current status

The original claim was true before this staged work: the RISC-V release hook was empty and
RV64 was effectively single-hart. The hook now attempts a conservative QEMU `virt` OpenSBI
HSM release, but secondaries are only started, acknowledged, and parked.

Key properties:

- The normal `_start` path remains the bootstrap-hart path: it installs the bootstrap stack
  and calls `yarm_kernel_main`.
- SBI HSM `hart_start` targets `yarm_riscv64_secondary_entry`, not `_start` and not
  `yarm_kernel_main`.
- The secondary entry consumes a per-hart handoff pointer from SBI `opaque`, switches to the
  handoff stack, installs a local park trap vector, disables supervisor interrupts, calls a
  tiny Rust park routine, records an ack marker, and then stays in `wfi`.
- The boot hart waits for the ack marker after a successful `hart_start` call and logs
  whether the secondary reached the parked path.
- No secondary CPU is marked scheduler-online in this stage.

## QEMU `virt` hart-ID assumption

The current release loop is intentionally limited to the QEMU `virt`/OpenSBI profile and
uses the conventional hart-ID range `0..8`, skipping `BOOTSTRAP_CPU_ID` (`0`). This is not a
real DTB CPU map. Failed `hart_start` calls (for example on single-hart `-smp 1`) are logged
and non-fatal, so single-hart QEMU boot remains preserved.

`prepare_arch_boot()` can locate a DTB blob but still does not stage parsed RISC-V CPU IDs
for `Bootstrap::init()`. The RISC-V topology helper still parses only text fixture shapes
such as `/cpus { cpu@1 { }; }`, not binary FDT CPU nodes.

## SBI HSM status

A minimal local RISC-V SBI wrapper now exists for:

- base `probe_extension`;
- HSM `hart_start(hartid, start_addr, opaque)`;
- HSM `hart_get_status(hartid)` as a small helper for future bring-up;
- standard SBI error-code decoding for clean logs.

The release hook probes HSM first. If HSM is missing or probing fails, it logs the result and
returns without changing boot behavior.

## Comparison with other architecture release paths

- AArch64 has a PSCI-based secondary-start path: it records a PSCI conduit from the DTB,
  calls `CPU_ON`, has a dedicated secondary entry, waits for an explicit BSP release flag,
  and then marks secondaries ready/online.
- x86_64 has an AP trampoline implementation in `src/arch/x86_64/smp.rs`, but x86_64 SMP
  behavior is outside this audit and was intentionally not changed.
- RISC-V is now at an earlier staged point than AArch64: secondary harts can reach a safe
  park path, but they are not scheduler participants.

## Remaining blockers before real RISC-V SMP

1. Real FDT `/cpus` parsing that records firmware hart IDs separately from scheduler
   `CpuId` indices and stages the discovered topology before `Bootstrap::init()`.
2. A secondary handoff that includes root page-table/SATP details and a shared-kernel pointer
   once secondaries are ready to run kernel work instead of parking.
3. Secondary-local initialization for `satp`, `sfence.vma`, trap vectors, interrupt/timer
   state, and per-CPU scheduler identity.
4. A scheduler/topology handshake where the BSP marks a CPU online only after the secondary
   has acknowledged complete local initialization.
5. QEMU `virt` gating based on parsed platform identity rather than a compile-time profile
   assumption.

## VisionFive 2 / hardware-board decision

VisionFive 2 and other hardware boards remain explicitly deferred. Board-specific hart
availability and boot-hart identity must come from real DTB/firmware parsing or an explicit
board profile. In particular, designs where hart `0` is not a normal S-mode application hart
cannot use the QEMU `virt` `0..N-1` assumption.
