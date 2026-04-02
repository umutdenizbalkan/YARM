<!-- SPDX-License-Identifier: Apache-2.0 -->

# Phase 6 Exit-Gate Report

This report tracks the closure criteria for **Phase 6 — Service migration and deprecation**.

## Gate checklist

- ✅ Control-plane legacy blocking receive guardrail (`kernel.ipc_recv`) is active.
- ✅ Control-plane exit-gate migration bundle canary is active.
- ✅ Process Manager kernel-IPC roundtrip uses reply-cap call/reply helper path.
- ✅ VFS kernel-IPC roundtrip uses reply-cap call/reply helper path.
- ✅ Supervisor non-RPC/event-driven flows are covered by dated non-applicability waiver; request/reply-like status-query path supports reply-cap compatibility + helper entrypoint.
- ✅ Init orchestration path is covered by dated non-applicability waiver (not a dedicated kernel request/reply loop service).

## Dated deprecation checkpoints

- **Soft sunset checkpoint:** June 30, 2026
  - expected state: all core control-plane services have timed-receive migration + source guardrails.
- **Hard sunset target:** September 30, 2026
  - expected state: no legacy ad-hoc two-endpoint request/reply choreography in core control-plane services unless explicitly documented as dated waiver with owner and closure plan.

## Current dated waivers

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

## Closure summary

- Matrix rows are now marked as `✅ migrated` or `✅ waived (dated)` for all core control-plane services.
- Exit-gate canaries are active and passing.
- Phase 6 sign-off recommendation: **complete**, with dated waivers tracked through the September 30, 2026 hard sunset checkpoint.
