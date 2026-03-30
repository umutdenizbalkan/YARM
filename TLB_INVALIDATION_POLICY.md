# TLB Invalidation Policy (Hosted vs Production)

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
## Verification status

- Non-hosted invalidation behavior is implemented for x86_64, aarch64, and riscv64 page/asid flows and validated by the project's current CI + test evidence.
- Hosted-dev remains intentionally no-op for invalidation hooks as documented above.
