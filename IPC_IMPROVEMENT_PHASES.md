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
- ⏳ **Phase 6** (not implemented yet).

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
