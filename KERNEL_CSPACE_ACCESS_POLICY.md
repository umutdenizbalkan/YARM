<!-- SPDX-License-Identifier: Apache-2.0 -->

# Kernel CSpace/CNode Access Policy

This document defines where kernel code is allowed to access capabilities from the
global kernel capability space versus task-local CNode spaces.

## Policy

1. **Task execution paths MUST use task-local capability lookup.**
   - Any path acting on behalf of the currently running task (syscall IPC send/recv,
     map/unmap/protect, task fault handling flows, etc.) must resolve capabilities
     from the current task CNode.

2. **Global kernel capability access is allowed only for kernel-internal orchestration.**
   - Examples: delegation records, driver runtime-cap revocation, transfer-envelope
     staging helpers that intentionally operate on globally minted capabilities.

3. **All global access must use explicit helper APIs (never direct `self.cspace.*`).**
   - `kernel_global_capability(...)`
   - `kernel_global_capability_has_right(...)`
   - `revoke_kernel_global_capability(...)`

This naming is intentional: reviewers can quickly spot global-kernel access and
decide if it is justified.

## Guardrail

When adding or modifying kernel code:

- Prefer task-local helpers first.
- If global access is required, use the explicit `kernel_global_*` methods and add
  a short comment in code explaining why task-local semantics are not applicable.
- The boot test guard scans `src/kernel/**/*.rs` and `src/services/**/*.rs` and
  fails if direct `self.cspace.{get,revoke,has_right}` appears outside the
  canonical helper definitions.
