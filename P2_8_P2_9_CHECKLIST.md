# P2.8 / P2.9 Implementation Checklist

Ordered by **risk first**, then **effort**.

## 1) [High risk, medium effort] TLB invalidation fidelity on non-hosted ISAs

- [x] Implement non-hosted invalidation instructions in AArch64 backend:
  - page invalidation hook
  - ASID invalidation hook
- [x] Implement non-hosted invalidation instructions in RISC-V backend:
  - page invalidation hook
  - ASID invalidation hook
- [x] Add architecture-targeted smoke validation for invalidation paths in CI (QEMU lanes).
- [x] Add explicit doc note for hosted-dev no-op behavior and production expectations (`TLB_INVALIDATION_POLICY.md`).

## 2) [High risk, higher effort] Physical frame allocator scaling strategy

- [x] Replace single fixed bitmap capacity (`MAX_TRACKED_FRAMES`) with scalable storage.
- [ ] Add fast-path data structure for contiguous allocations (run list / buddy metadata).
- [ ] Preserve O(1)-ish single-page allocation hinting under fragmentation pressure.
- [ ] Add long-run fragmentation/throughput tests.
- [ ] Add profile-aware sizing knobs for hosted-dev vs non-hosted deployments.

## 3) [Medium risk, medium effort] Cross-CPU shootdown hardening

- [ ] Ensure ASID retirement ack path cannot stall indefinitely (timeouts/telemetry/escalation).
- [ ] Add stress tests for repeated destroy/recreate cycles with pending shootdowns.
- [ ] Validate arch-specific invalidate sequencing against shootdown completion semantics.

## 4) [Medium risk, low effort] Mapping attribute completeness

- [ ] Extend `PageFlags` with cache policy for DMA/device mappings.
- [ ] Thread cache policy bits through each ISA page-table encoder.
- [ ] Add tests for cache-policy encoding and validation.
