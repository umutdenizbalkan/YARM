# P2.10 Checklist: Page-Table + Frame-Allocator Production Hardening

**Status:** ✅ Completed (all checklist items implemented and validated)

Ordered by **risk first**, then **effort**.

## 1) [High risk, medium effort] Make strict ISA smoke validation merge-blocking

- [x] Run strict QEMU smoke jobs for `x86_64`, `aarch64`, and `riscv64` on every pull request affecting `src/arch/**`, `src/kernel/vm.rs`, `src/kernel/frame_allocator.rs`, or boot/memory initialization paths.
- [x] Keep non-strict smoke jobs as fast preflight, but require strict jobs to pass before merge.
- [x] Publish strict smoke logs as workflow artifacts for every PR run.

**Acceptance criteria**
- PRs touching scoped paths trigger strict lanes automatically (not only `workflow_dispatch`).
- Branch protection requires strict lanes: all must pass before merge.
- Each strict run archives logs and includes marker assertions (`YARM_INIT_DONE`, UI boot marker, and ISA-specific boot success marker if available).

## 2) [High risk, medium effort] Non-hosted invalidation correctness sign-off

- [x] Add architecture-specific invalidation correctness tests that exercise page and ASID invalidation effects in non-hosted paths.
- [x] Ensure each ISA test verifies required ordering/barrier semantics around invalidation.
- [x] Record a per-ISA sign-off artifact (test log or report) in CI outputs.

**Acceptance criteria**
- Each ISA has at least one test proving stale translations are not observed after invalidate+sync sequence.
- Tests execute in non-hosted configuration (QEMU or hardware-like boot path), not hosted-dev no-op mode.
- CI publishes per-ISA pass/fail evidence linked from the workflow summary.

## 3) [Medium risk, low effort] Define and enforce production sign-off policy

- [x] Add `P2_10_SIGNOFF_POLICY.md` describing required checks for page-table and allocator changes.
- [x] Include explicit requirements for strict ISA smoke, invalidation correctness evidence, and allocator stress coverage.
- [x] Add a PR template checklist section that references the sign-off policy.

**Acceptance criteria**
- Policy doc exists and is referenced from README/CONTRIBUTING or equivalent developer entrypoint.
- PR template includes required sign-off checkboxes and links to CI evidence.
- Maintainers can reject PRs missing required evidence using objective policy items.

## 4) [Medium risk, medium effort] Long-run allocator + mapping stress in CI

- [x] Add long-duration stress jobs focused on fragmentation, reserve/free churn, and map/unmap churn under ASID turnover.
- [x] Define quantitative health metrics (e.g., allocation success ratio, max extent fragmentation, mean allocation latency proxy).
- [x] Fail CI when metrics regress beyond agreed thresholds.

**Acceptance criteria**
- Stress job runs in CI at least nightly, with an optional PR-triggered shortened variant.
- Metrics are emitted in machine-readable form and retained as artifacts.
- Regression thresholds are codified and enforced automatically.

## 5) [Low risk, low effort] Observability and operational diagnostics

- [x] Standardize telemetry counters/events for allocator pressure, ASID retire backlog, and invalidate operations.
- [x] Add a concise troubleshooting runbook for common failure signatures.

**Acceptance criteria**
- Telemetry fields are documented with names, units, and expected ranges.
- Runbook includes at least: symptom, likely cause, and first diagnostic command/log to inspect.
