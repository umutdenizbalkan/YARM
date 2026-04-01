<!-- SPDX-License-Identifier: Apache-2.0 -->

# User-space Server Maturity Plan (no_std microkernel profile)

This plan tracks the transition from mechanism-complete kernel internals to production-quality user-space servers.

## Scope

- `process_manager.srv` maturity (parent/child semantics, integrated exit/wait discipline, restart signaling breadth)
- `vfs.srv` maturity (typed ABI conformance, mount routing, deterministic operation ordering)
- `driver *.srv` maturity (delegation from `init.srv`, revoke/restart lifecycle)

## Maturity gates

1. **Protocol gate**
   - All IPC request/reply payloads have versioned typed codecs.
   - Golden-vector tests prevent silent wire-format drift.

2. **Policy gate**
   - Delegation chain is explicit (`init.srv` -> service).
   - Service role policy is enforced for hardware delegation.
   - Delegation graph remains mechanism-auditable (allowed role edges are explicit and test-covered).

3. **Determinism gate**
   - Deterministic simulation scripts cover mixed subsystem interaction:
     - process-manager requests
     - VFS requests
     - notification/IRQ delivery

4. **Lifecycle gate**
   - Restart/revoke behavior is test-covered for driver-facing runtime caps.
   - Process-manager wait/reap permissions are test-covered.
   - `spawn_v2` drives an image-backed launch path rather than pid allocation only.
   - `exit` mutates process lifecycle through the service path, and waiting on a running child yields a typed non-ready condition.

5. **Scenario harness gate**
   - Reusable deterministic scenario catalog is present.
   - All catalog scenarios must replay to fixed expected summaries (proc/vfs/irq + request counts).

## Constraints

- Keep kernel code `#![no_std]` and avoid `std` usage in mechanism paths.
- Keep architecture-specific logic under `arch/*`; kernel modules remain portable.
