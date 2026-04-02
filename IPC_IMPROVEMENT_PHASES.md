<!-- SPDX-License-Identifier: Apache-2.0 -->

# IPC Improvement Phases

This plan breaks the IPC hardening work into incremental, reviewable phases.

## Implementation status (current branch)

- ✅ **Phase 0 — Baseline and rollback guardrails** (implemented in this pass).
- 🟡 **Phase 1 — Payload capacity and framing policy** (partially implemented; payload lane raised, policy/benchmark matrix still pending).
- ⏳ **Phases 2–6** (not implemented yet).

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

## Phase 6 — Service migration and deprecation

- Migrate core services to timed call/reply and lightweight notifications.
- Deprecate legacy patterns (ad-hoc reply endpoints where replaceable).
- Publish migration guide with ABI/version matrix and compatibility windows.

**Exit criteria**
- Core control-plane services run on new primitives.
- Deprecated paths are either removed or formally sunset with dates.

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
