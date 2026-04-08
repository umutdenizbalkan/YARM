# TID allocation policy cleanup PR plan

## Goal
Define and enforce a clear split between static/bootstrap TIDs and dynamically allocated thread TIDs, then evolve allocator behavior in small reviewable PRs.

## Proposed PR sequence

1. **Phase 1 — policy floor + gap enforcement (completed)**
   - Codify the dynamic allocation floor (`INITIAL_DYNAMIC_TID`) as a hard lower bound.
   - Normalize allocator cursor state if it falls below the dynamic range.
   - Add regression tests for floor enforcement and wrap behavior.

2. **Phase 2 — explicit allocation model + cursor abstraction (completed)**
   - Introduce a small `TidAllocationPolicy`/cursor helper in `kernel::boot`.
   - Make allocation semantics explicit (reserved range, dynamic range, wrap contract).
   - Remove ad-hoc cursor arithmetic from state methods.

3. **Phase 3 — gap accounting + diagnostics (completed)**
   - Add policy-aware telemetry (`dynamic_tid_allocations`, `dynamic_tid_wraps`, `gap_floor_repairs`).
   - Emit structured boot-time diagnostics for configured policy boundaries.

4. **Phase 4 — static/dynamic boundary validation in CI**
   - Add tests/check scripts that fail if dynamic allocation returns a TID below floor.
   - Include a targeted kernel test suite command in CI for TID policy invariants.

5. **Phase 5 — follow-on integration cleanup**
   - Audit callers assuming monotonic non-wrapping TIDs and update contracts/docs.
   - Update roadmap/status docs to reflect stable allocation policy.

## Scope notes
- This plan keeps behavior changes incremental to reduce kernel risk.
- Phases 2–5 can be split further if reviewers prefer smaller diffs.
