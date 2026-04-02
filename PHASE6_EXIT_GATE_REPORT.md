<!-- SPDX-License-Identifier: Apache-2.0 -->

# Phase 6 Exit-Gate Report (Draft)

This report tracks the closure criteria for **Phase 6 — Service migration and deprecation**.

## Gate checklist

- ✅ Control-plane legacy blocking receive guardrail (`kernel.ipc_recv`) is active.
- ✅ Control-plane exit-gate migration bundle canary is active.
- ✅ Process Manager kernel-IPC roundtrip uses reply-cap call/reply helper path.
- ✅ VFS kernel-IPC roundtrip uses reply-cap call/reply helper path.
- 🟡 Supervisor remains on budgeted receive loop migration; status-query path has reply-cap compatibility support, while broader call/reply choreography evaluation/waiver finalization is still pending.
- 🟡 Init orchestration path requires explicit non-kernel-loop waiver closure in final migration guide bundle.

## Dated deprecation checkpoints

- **Soft sunset checkpoint:** June 30, 2026
  - expected state: all core control-plane services have timed-receive migration + source guardrails.
- **Hard sunset target:** September 30, 2026
  - expected state: no legacy ad-hoc two-endpoint request/reply choreography in core control-plane services unless explicitly documented as dated waiver with owner and closure plan.

## Current dated waivers (draft)

1. **Supervisor call/reply choreography waiver (temporary)**
   - Scope: `src/services/control_plane/supervisor/service.rs`
   - Reason: service currently prioritizes budgeted event-loop receives; explicit reply-cap choreography migration is pending deeper flow segmentation.
   - Owner: control-plane
   - Target closure: September 30, 2026

2. **Init orchestration-path waiver (temporary)**
   - Scope: `src/services/control_plane/init/service.rs`
   - Reason: module orchestrates service startup and does not currently operate as a dedicated kernel-IPC request loop; final migration guide must document non-applicability vs required call/reply migration paths.
   - Owner: init/runtime
   - Target closure: September 30, 2026

## Remaining closure work before Phase 6 completion

1. Convert supervisor flows that are semantically request/reply into reply-cap call/reply paths **or** lock final waiver rationale with explicit compatibility boundary.
2. Publish the final migration guide section that maps each core service to:
   - migrated primitive(s),
   - waiver status (if any),
   - deprecation/sunset closure evidence.
3. Mark matrix rows as `✅ migrated` or `✅ waived (dated)` and flip Phase 6 status to complete only when all exit checks are green.
