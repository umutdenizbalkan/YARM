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
- The boot test guard scans `src/kernel/**/*.rs` and `crates/yarm-*-servers/src/**/*.rs` and
  fails if direct `self.cspace.{get,revoke,has_right}` appears outside the
  canonical helper definitions.

## CNode Storage/Resize Invariants (internal note)

This subsystem now uses allocator-backed CNode slot storage and runtime resize.

### Enforced runtime bounds

- Requested CNode slot capacity must be non-zero.
- Requested capacity must be within runtime per-CNode policy (`max_capability_slots`).
- Requested capacity must be representable by `CapId` index encoding.
- Global reserved slot accounting must remain within runtime pool
  (`max_total_cnode_slots`).
- Profile constants like `MAX_CAPABILITIES_PER_CSPACE` are runtime defaults/policy
  inputs, not fixed representation ceilings.

### Resize and accounting invariants

- Grow/shrink are fallible operations.
- Shrink is rejected when any truncated slot is live.
- Grow allocates new backing and commits only on success (replace-on-success).
- Failed resize must leave both slot contents and slot accounting unchanged.
- Successful resize must update CNode slot accounting by exactly the delta.
- Process CNode cleanup must release its reserved slot accounting.

### Revoke scratch behavior

- Revoke traversal scratch/worklists are allocator-backed (`Vec`) and sized from
  current runtime CNode capacity.
- Scratch may be cached and reused between revokes when capacity is sufficient.
- Scratch cache is dropped/rebuilt when capacity growth requires larger buffers.
