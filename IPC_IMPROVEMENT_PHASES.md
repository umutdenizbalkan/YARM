<!-- SPDX-License-Identifier: Apache-2.0 -->

# IPC Improvement Phases

This plan breaks the IPC hardening work into incremental, reviewable phases.

## Implementation status (current branch)

- ✅ **Phase 0 — Baseline and rollback guardrails** (implemented in this pass).
- ✅ **Phase 1 — Payload capacity and framing policy** (completed in this pass).
- ✅ **Phase 2 — Real IPC timeout semantics** (completed in this pass).
- ✅ **Phase 3 — Lightweight notification primitive** (completed in this pass).
- 🟡 **Phase 4 — Call/Reply capability model** (Slices 1–3 syscall wiring complete: `IpcCall` + `IpcReply` available; lifecycle hardening in progress: caller exit/reap/restart revocation and responder-task binding added).
- ✅ **Phase 5 — Shared-memory transfer hardening** (passes 1–3 complete: recv rights attenuation + failure rollback + fault/cancel accounting + repeated teardown canaries).
- 🟡 **Phase 6 — Service migration and deprecation** (passes 1–16 in progress: policy + VFS timed-recv migration + supervisor receive-loop budgeted migration + cross-service guardrails + service migration matrix freeze + control-plane-wide nonblocking regression expansion + process-manager kernel-IPC timed-recv migration + process-manager source guardrails + process-manager reply-cap call/reply helper migration + VFS reply-cap call/reply helper migration + exit-gate bundle bootstrap + draft exit-gate report/dated waivers; full core-service cutover/deprecation sunset pending).

## Phase 0 — Baseline and rollback guardrails

- Re-validate current IPC behavior (send/recv, shared-memory transfer, notification routing).
- Add conformance tests that lock existing semantics before refactors.
- Define explicit non-goals for each phase to prevent ABI drift.

**Exit criteria**
- Baseline tests pass and are tagged as migration guards.

## Phase 1 — Payload capacity and framing policy

- Keep inline payload fixed-size for fastpath.
- Evaluate final inline capacity target (`64`, `128`, or `256`) with benchmark data.
- Add documented policy for when payloads must use shared-memory transfer.
- Add fragmentation design doc for medium payloads that should not allocate transfer objects.

**Exit criteria**
- Measured latency/throughput tradeoff table is checked in.
- One chosen inline size is frozen in ABI docs.

## Phase 2 — Real IPC timeout semantics

- Introduce true timeout semantics tied to kernel time source (not retry loops).
- Add timed wait state for endpoint receive/send paths.
- Wake blocked tasks on timeout expiry with deterministic error code.
- Ensure timeout interacts correctly with scheduler preemption and task teardown.

**Exit criteria**
- Timeout is driven by timer ticks/deadlines.
- New tests cover: immediate timeout, finite timeout, infinite wait, timeout race with message arrival.

## Phase 3 — Lightweight notification primitive

- Replace endpoint-backed notification internals with dedicated lightweight object.
- Support signal coalescing semantics and bounded wake latency.
- Keep capability rights model (`SIGNAL`/`RECEIVE`) stable for migration ease.
- Provide compatibility shim if existing users assume message-like notification payload.

**Exit criteria**
- Notification fastpath no longer allocates/queues `Message` envelopes.
- IRQ-to-notification route tests remain green.

## Phase 4 — Call/Reply capability model

- Add `IpcCall` primitive that creates/attaches ephemeral reply capability.
- Bind reply capability to caller + invocation context + single-use lifecycle.
- Add reply syscall/path that consumes reply cap atomically.
- Enforce revocation on caller death/timeout to avoid stale authority.

**Exit criteria**
- Confused-deputy regression tests demonstrate authority confinement.
- Explicit two-endpoint request/reply choreography no longer required for standard RPC.

## Phase 5 — Shared-memory transfer hardening

- Enforce map/release accounting invariants in all fault/cancel paths.
- Add stricter rights attenuation for transferred memory capabilities.
- Validate receiver mapping intent against requested access rights.
- Add anti-leak canaries for transfer-envelope lifecycle under failure injection.

**Exit criteria**
- Map/release parity canaries pass under repeated load and forced faults.
- Transfer revoke paths are deterministic and auditable.

## Phase 5 artifacts

- Shared-memory transfer-cap preflight validation:
  - `IpcSend` large-payload transfer path now requires transfer cap rights `READ|MAP` before descriptor send.
- Rights-rejection leak canary:
  - repeated shared-memory send rejection due to missing transfer rights leaves `transfer_records_created` unchanged (`0`).
- Recv-path rollback hardening:
  - shared-memory recv validation/map failures now revoke materialized transfer caps (no leaked receiver-local transfer cap on failure).
- Receiver mapping-intent validation:
  - shared-memory recv now validates optional map-intent flags (read required; unknown bits rejected) before mapping.
  - write-intent mapping is rejected unless the materialized transfer cap includes `WRITE`.
  - read-only intent attenuates receiver-local transferred capability to `READ|MAP` (drops `WRITE`).
- Map/release telemetry canary on failure path:
  - repeated recv map-intent failures keep `shared_mem_bytes_mapped` and `shared_mem_bytes_released` at `0` (no accounting drift).
  - repeated recv write-intent (`WRITE`) failures against non-writable transfer caps also keep map/release counters stable.
- Fault/cancel-path accounting closure:
  - process-cleanup purge of active shared-memory transfer mappings now records `shared_mem_bytes_released`.
  - direct transfer-cap revoke path that force-unmaps active shared-memory mappings now also records released-byte telemetry.
- Anti-leak + accounting canaries under repeated teardown:
  - repeated process-cleanup transfer-envelope purge keeps `transfer_records_created == transfer_records_revoked`.
  - repeated direct transfer-cap revoke force-unmap cycles keep `shared_mem_bytes_mapped == shared_mem_bytes_released`.
- Exit-criteria verification canary:
  - mixed transfer-envelope cleanup + force-unmap revoke path keeps both invariants stable:
    - `transfer_records_revoked >= transfer_records_created` (no stale transfer records)
    - `shared_mem_bytes_mapped == shared_mem_bytes_released`

## Phase 6 — Service migration and deprecation

- Migrate core services to timed call/reply and lightweight notifications.
- Deprecate legacy patterns (ad-hoc reply endpoints where replaceable).
- Publish migration guide with ABI/version matrix and compatibility windows.

**Exit criteria**
- Core control-plane services run on new primitives.
- Deprecated paths are either removed or formally sunset with dates.

## Phase 6 artifacts (pass 1)

- Migration matrix + compatibility window:
  - `SYSCALL_ABI.md` now documents Phase 6 migration policy and compatibility targets for:
    - timed recv (`IpcRecvTimeout`) adoption in control-plane waits,
    - call/reply (`IpcCall`/`IpcReply`) adoption over ad-hoc two-endpoint request/reply choreography,
    - shared-memory `TransferRelease` lifecycle requirement after auto-map receives.
- Deprecation policy checkpoint:
  - legacy two-endpoint request/reply choreography is marked as **deprecated for new/updated core services** during ABI v9 migration window.
  - full removal is explicitly deferred until all core control-plane services are migrated.

## Phase 6 artifacts (pass 2)

- First core-service migration cut:
  - `src/services/control_plane/vfs/service.rs` now uses timed receive (`ipc_recv_with_deadline`) in its kernel-IPC request/response roundtrip path for both server-side request receive and client-side reply receive.
- Migration guard:
  - added VFS control-plane canary test for timed-receive empty-queue behavior under deadline receive path.

## Phase 6 artifacts (pass 3)

- Timed-recv migration hardening for first service cut (VFS):
  - `src/services/control_plane/vfs/service.rs` now routes roundtrip receive operations through a budgeted helper (`roundtrip_ipc_with_budget`) so timeout policy is explicit and testable.
- Migration guard coverage:
  - added VFS canary validating zero-tick budget behavior on queued request/reply flow.

## Phase 6 artifacts (pass 4)

- Deprecation guardrail for migrated service:
  - `src/services/control_plane/vfs/service.rs` now includes a source-level canary test that rejects regressions back to legacy blocking `ipc_recv` in the VFS control-plane roundtrip flow.
- Compatibility validation checkpoint:
  - pass-2 and pass-3 VFS timed-recv path tests remain green alongside the new deprecation guardrail.

## Phase 6 artifacts (pass 5)

- Compatibility + deprecation validation expansion for first migrated cut (VFS):
  - pass-2 (timed recv path), pass-3 (explicit budget helper), and pass-4 (source guardrail) are now validated together as the pass-5 compatibility bundle.
- Guardrail stabilization:
  - VFS source-level regression guardrail now checks for legacy blocking `ipc_recv` usage via a non-self-referential pattern, preventing false positives in the guard test itself.

## Phase 6 artifacts (pass 6)

- Supervisor receive-loop migration:
  - `src/services/control_plane/supervisor/service.rs` now drains control/fault queues via a budget-aware helper (`recv_with_budget`) that probes nonblocking first and then uses timed receive where capability context allows.
- Supervisor migration guardrail:
  - added source-level canary requiring supervisor loop code to keep try/budgeted receive paths and reject regression to legacy blocking `ipc_recv`.
- Exit-gate re-evaluation:
  - Phase 6 remains in-progress: VFS + supervisor receive-loop migration slices are landed, but full core control-plane migration/deprecation sunset is still pending.

## Phase 6 artifacts (pass 7)

- Cross-service migration guardrail:
  - `src/services/control_plane/mod.rs` now includes a control-plane-wide canary that rejects legacy blocking `kernel.ipc_recv` calls in migrated VFS and supervisor service sources.
- Gate reinforcement:
  - pass-7 guardrail complements per-service guard tests by asserting migration invariants at the control-plane module boundary.

## Phase 6 remaining work (open items)

- Core-service migration completion:
  - migrate remaining control-plane services (beyond VFS + supervisor receive-loop) to timed receive and/or call/reply primitives where applicable.
- Legacy choreography deprecation sunset:
  - replace remaining ad-hoc two-endpoint request/reply patterns in core services, then set and publish a concrete removal target release/date for legacy flow.
- Migration-guide completion:
  - publish an operator/developer-facing migration guide with per-service status, required syscall primitive, and compatibility window closure criteria.
- Exit-criteria closure bundle:
  - add/refresh an explicit Phase 6 exit-gate test bundle that demonstrates:
    - all core control-plane services use migrated receive/call-reply paths,
    - deprecated paths are either removed or marked with a dated sunset policy.

### Proposed PR rollout (step-by-step)

- PR-6.1 — Core-service inventory + migration matrix freeze
  - produce a concrete table of all control-plane services, current receive/reply primitive, and target primitive (`try/timed recv`, `IpcCall/IpcReply`, notification path).
  - annotate owner + risk + test gate per service.
  - **Exit check:** matrix is checked in and referenced by Phase 6 docs. ✅ (pass 8)

- PR-6.2 — Remaining service receive-loop migration (timed/budgeted)
  - migrate each remaining service loop to budgeted receive helpers (nonblocking probe + timed wait fallback where allowed).
  - add per-service source guardrails blocking regression to legacy blocking `ipc_recv`.
  - **Exit check:** per-service migration tests and guardrails pass.

- PR-6.3 — Request/reply choreography replacement (`IpcCall/IpcReply`)
  - replace ad-hoc two-endpoint reply choreography in remaining core service RPC flows with reply-cap call/reply where semantically equivalent.
  - preserve compatibility shims only where replacement is not yet safe.
  - **Exit check:** call/reply lifecycle tests pass for migrated services (single-use + revocation + responder binding).

- PR-6.4 — Deprecation sunset policy + dated removal target
  - publish explicit deprecation timeline (target release/date) for legacy request/reply choreography and blocking receive usage in core services.
  - mark any temporary compatibility shims with sunset milestone.
  - **Exit check:** deprecation section includes concrete date/release and affected paths.

- PR-6.5 — Migration guide + final exit-gate bundle
  - publish operator/developer migration guide with per-service cutover status and compatibility window closure rules.
  - add a single Phase 6 gate suite that asserts all core control-plane services are migrated or have a dated sunset waiver.
  - **Exit check:** Phase 6 can be flipped from in-progress to complete once gate suite is green.

## Phase 6 artifacts (pass 8)

- Core-service inventory + migration matrix freeze (PR-6.1):
  - added `PHASE6_SERVICE_MIGRATION_MATRIX.md` with per-service current state, target primitive, owner, risk, status, and planned PR sequence.
  - matrix now serves as the canonical tracker for remaining Phase 6 implementation slices.

## Phase 6 artifacts (pass 9)

- PR-6.2 guardrail expansion slice:
  - `src/services/control_plane/mod.rs` now extends the control-plane source guardrail to include `init` and `process_manager` service sources in addition to VFS + supervisor.
  - the guardrail rejects regressions to legacy blocking `kernel.ipc_recv` usage across all current core control-plane service modules.

## Phase 6 artifacts (pass 10)

- PR-6.2 receive-loop migration slice:
  - `src/services/control_plane/process_manager/service.rs` now includes a kernel-IPC roundtrip loop (`run_request_loop_over_kernel_ipc`) that uses timed receive (`ipc_recv_with_deadline`) with explicit receive budget.
  - migration coverage now includes a dedicated process-manager kernel-IPC request-loop test to keep timed-recv path behavior under regression guard.

## Phase 6 artifacts (pass 11)

- PR-6.2 guardrail hardening for migrated process-manager path:
  - `src/services/control_plane/process_manager/service.rs` now includes a source-level canary requiring budgeted roundtrip helper usage and timed receive (`ipc_recv_with_deadline`) call-sites in the migrated kernel-IPC loop.

## Phase 6 artifacts (pass 13)

- PR-6.3 call/reply migration slice (process-manager):
  - `src/services/control_plane/process_manager/service.rs` kernel-IPC roundtrip path now uses reply-cap call/reply choreography (`create_reply_cap_for_caller` + `ipc_reply`) instead of ad-hoc dedicated server-send endpoint replies.
  - source-level guardrail now asserts presence of budgeted helper + timed receive + reply-cap reply path in migrated process-manager loop.

## Phase 6 artifacts (pass 14)

- PR-6.3 call/reply migration slice (VFS):
  - `src/services/control_plane/vfs/service.rs` kernel-IPC roundtrip path now uses reply-cap call/reply choreography (`create_reply_cap_for_caller` + `ipc_reply`) in place of ad-hoc dedicated server-send endpoint replies.
  - timed receive budget behavior (`ipc_recv_with_deadline`) remains in place for both server request receive and caller reply receive.

## Phase 6 artifacts (pass 15)

- PR-6.5 exit-gate bootstrap:
  - `src/services/control_plane/mod.rs` now includes a phase-6 exit-gate bundle canary that asserts current migration invariants across core control-plane services:
    - VFS: timed receive + reply-cap call/reply presence,
    - Supervisor: budgeted receive helper presence,
    - Process Manager: timed receive + reply-cap call/reply presence.

## Phase 6 artifacts (pass 16)

- PR-6.5 exit-gate report bootstrap:
  - added `PHASE6_EXIT_GATE_REPORT.md` with:
    - current gate checklist,
    - dated deprecation checkpoints,
    - draft dated waivers for remaining supervisor/init closure items,
    - explicit remaining-work list required before Phase 6 completion.

## Cross-phase quality gates

- ABI versioning and changelog updates per phase.
- Telemetry additions for each new IPC path.
- Deterministic tests for all races introduced by each phase.
- Security review checkpoint before enabling call/reply by default.

## Phase 0 artifacts

- Baseline gate doc: `PHASE0_IPC_BASELINE_GATES.md`
  - Locks round-trip endpoint IPC behavior.
  - Locks IRQ notification routing behavior.
  - Locks shared-memory transfer descriptor + auto-map/release behavior.

## Phase 1 artifacts

- Payload policy + benchmark matrix: `PHASE1_PAYLOAD_POLICY.md`
- Medium payload fragmentation design: `IPC_FRAGMENTATION_POLICY.md`
- Repro benchmark command:
  - `cargo test -q --test phase1_payload_bench -- --nocapture`

## Phase 2 artifacts

- Tick-budget timeout receive syscall semantics:
  - `src/kernel/syscall.rs` (`IpcRecvTimeout` now interprets `args[3]` as timeout ticks).
- Timeout expiry result semantics:
  - non-zero timeout expiry now returns `TimedOut` (distinct from `WouldBlock`).
- Timed wait-state + deadline wake integration:
  - `src/kernel/boot/ipc_state.rs` (`ipc_recv_with_deadline`, `ipc_send_with_deadline`, per-task IPC timeout markers, deadline scanner).
  - `src/kernel/boot/fault_state.rs` (timer interrupt now processes expired IPC deadlines).
- ABI doc update:
  - `SYSCALL_ABI.md` (`IpcRecvTimeout` args[3] documented as timeout ticks).

## Phase 3 artifacts

- `NotificationObject` no longer wraps `Endpoint`; it now uses a lightweight IRQ ring.
- IRQ routes queue raw IRQ codes and materialize `Message` only on receive boundary.

## Phase 4 artifacts (partial)

- Call/reply capability execution plan:
  - `PHASE4_CALL_REPLY_CAP_PLAN.md`
- Slice 1 implementation:
  - `CapObject::Reply` kernel object variant and generation-protected reply-cap record table.
  - `create_reply_cap_for_caller(...)` and single-use `ipc_reply(...)` path.
- Slice 2 implementation (partial):
  - `IpcCall` syscall ABI slot + kernel path that mints and transfers ephemeral reply cap.
- Slice 3 implementation (partial):
  - `IpcReply` syscall ABI slot + kernel path that consumes single-use reply cap and routes reply to the bound caller endpoint.
- Slice 4 implementation (partial):
  - reply-cap records are now revoked when the bound caller exits/is reaped/restarts to prevent late reply authority reuse.
  - call-minted reply caps now bind an expected responder task; off-path task use is rejected.
