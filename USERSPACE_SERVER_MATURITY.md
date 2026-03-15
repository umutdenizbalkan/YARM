# User-space Server Maturity Plan (no_std microkernel profile)

This plan tracks the transition from mechanism-complete kernel internals to production-quality user-space servers.

## Scope

- `procman.srv` maturity (parent/child semantics, wait/reap discipline, restart signaling)
- `vfs.srv` maturity (typed ABI conformance, mount routing, deterministic operation ordering)
- `driver *.srv` maturity (delegation from `init.srv`, revoke/restart lifecycle)

## Maturity gates

1. **Protocol gate**
   - All IPC request/reply payloads have versioned typed codecs.
   - Golden-vector tests prevent silent wire-format drift.

2. **Policy gate**
   - Delegation chain is explicit (`init.srv` -> service).
   - Service role policy is enforced for hardware delegation.

3. **Determinism gate**
   - Deterministic simulation scripts cover mixed subsystem interaction:
     - process-manager requests
     - VFS requests
     - notification/IRQ delivery

4. **Lifecycle gate**
   - Restart/revoke behavior is test-covered for driver-facing runtime caps.
   - Process-manager wait/reap permissions are test-covered.

## Constraints

- Keep kernel code `#![no_std]` and avoid `std` usage in mechanism paths.
- Keep architecture-specific logic under `arch/*`; kernel modules remain portable.
