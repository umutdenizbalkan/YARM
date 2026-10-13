<!-- SPDX-License-Identifier: Apache-2.0 -->

# TLB Invalidation Policy (Hosted vs Production)

**Canonical: yes.** Owns the per-arch TLB invalidation instruction
contract and the `hosted-dev` no-op rationale. VM two-phase ordering
(`VmAnonMap` / `VmBrk` / `TransferRelease` /
`map_shared_region_into_receiver`) lives in
`doc/KERNEL_UNLOCKING.md` §4 (recent correctness fixes) and the
`vm.rs` source. D3 two-phase live-wiring status is tracked in
`doc/KERNEL_UNLOCKING.md` §6.3 and §7.

This note defines the expected behavior of the architecture page-table invalidation
hooks and clarifies why hosted runs differ from non-hosted targets.

## Hosted-dev behavior

- In `hosted-dev`, ISA page-table invalidation hooks are intentionally **no-op**.
- Reason: hosted runs execute in a process/simulation environment where there is
  no real privileged TLB instruction context to flush hardware translations.
- Scope: this applies to per-page and per-ASID invalidation hooks used by the
  architecture backends.

## Non-hosted (production/real-hardware) behavior

- Non-hosted builds must execute real ISA invalidation instructions.
- Current implementation:
  - `x86_64`: `invlpg` (page) and `invpcid` (ASID/PCID scope)
  - `aarch64`: `tlbi ...` + required barrier sequencing (`dsb/isb`)
  - `riscv64`: `sfence.vma` variants for page/asid scope

## Production expectation

- CI/QEMU lanes should exercise architecture boot/runtime flows for each ISA,
  but **hosted-dev pass/fail is not evidence of hardware TLB flush correctness**.
- Final sign-off for production requires non-hosted ISA execution paths and
  architecture-targeted smoke coverage.
## Completion and coverage contract (VM-INVALIDATION1)

- A VM mutation that owes an invalidation is complete only when the invalidation is acknowledged
  for every target. An unacknowledged one is a failure settlement, never a success, and the
  displaced backing stays held (permanent quarantine). Owners: VmMap (NR 3 / NR 13), COW (split
  and broad), fork write-protection, transfer revocation, reap.
- The requester is always the CPU executing the operation, passed explicitly
  (`complete_unmap_shootdown_from_split`, `unmap_range_two_phase_from_split`); no shootdown derives
  it from `sched.current_cpu`.
- The target set is the CPUs currently running the space. That is sufficient only because
  activation retires stale translations: x86_64 CR3 loads keep bit 63 clear, every RISC-V `satp`
  write is followed by `sfence.vma zero, zero`, and every AArch64 invalidation is an
  inner-shareable broadcast. `scripts/check-vm-invalidation-sequence.sh` checks those instructions
  in the built image.
- A page-table root is freed only when no CPU has it loaded (current-bound or last recorded
  activation); otherwise the address space is quarantined.
- Details: `doc/KERNEL_UNLOCKING.md` §VMI1.

## Verification status

- Non-hosted invalidation behavior is implemented for x86_64, aarch64, and riscv64 page/asid flows and validated by the project's current CI + test evidence.
- Hosted-dev remains intentionally no-op for invalidation hooks as documented above.
