# IPC Shared-Memory Fast Path Plan (Stepwise PR Roadmap)

This document breaks the end-to-end shared-memory IPC fast path into incremental
PRs so we can land and validate each piece safely.

## Goal

Implement automatic/map-on-receive shared-memory delivery that wires a transferred
region into both sender and receiver address spaces with explicit lifecycle and
revocation semantics suitable for sustained FS/network/display data-plane traffic.

## Non-goals (for this sequence)

- Replacing the existing inline IPC control-plane path.
- Reworking scheduler/endpoint semantics unrelated to data-plane transfers.

## Phase 1 — Transfer object model + metadata hardening

- [x] Introduce a dedicated shared-transfer kernel record type (transfer id, source tid, receiver tid, endpoint binding, memory object id / dma region id, byte range, rights mask, generation).
- [x] Extend descriptor validation (`offset`, `len`) with page alignment / overflow / bounds checks against the transferred memory object or DMA region.
- [x] Add explicit transfer states (`Created`, `MappedReceiver`, `MappedBoth`, `Released`, `Revoked`) and state transition guards.
- [x] Add telemetry counters for transfer creation/materialization/revocation/failures.

Acceptance checks:
- unit tests for malformed descriptors and illegal state transitions.
- property-style tests for generation/handle reuse safety.

## Phase 2 — Receiver auto-map plumbing

- [x] Add recv-side map request contract (target VA + map flags + optional fixed/anywhere policy).
- [x] On `IpcRecv` of `OPCODE_SHARED_MEM`, automatically map receiver pages from transferred capability according to policy.
- [x] Return mapping result metadata (mapped VA, mapped length, transfer id) through syscall return lanes.
- [x] Keep current descriptor return path as compatibility fallback behind a feature/ABI gate until migration is complete.

Acceptance checks:
- syscall tests proving map-on-receive success/failure behavior and deterministic error mapping.
- tests for partial-map rollback on mid-range mapping faults.

## Phase 3 — Sender/receiver dual-map + pinning lifecycle

- [x] Define pin/unpin rules for shared transfer frames while either side holds active mappings.
- [x] Ensure map refcounts are updated for both sides and survive task scheduling/restart boundaries.
- [x] Add unmap/release syscall path that drops active mapping references and transitions transfer state.

Acceptance checks:
- tests validating refcount stability under map/unmap races and task handoff.
- frame-reclamation tests proving no early free while mappings remain.

## Phase 4 — Revocation semantics + failure containment

- [ ] Integrate capability revocation with active shared-transfer mappings.
- [ ] Define deterministic behavior for revoked transfer while mapped (read-fault, write-fault, or forced unmap policy).
- [ ] Emit supervisor-visible fault/revocation events for observability.

Acceptance checks:
- targeted revocation tests (source revoke, receiver revoke, endpoint teardown).
- fault-policy tests verifying architecture-consistent behavior.

## Phase 5 — Throughput path tuning

- [ ] Add batching/ring usage guidance for FS/network/display servers.
- [ ] Minimize syscall overhead on steady-state transfer reuse.
- [ ] Add fast-path telemetry and benchmark harnesses for large read/write, packet RX/TX, and framebuffer update flows.

Acceptance checks:
- benchmark gates with minimum throughput/latency thresholds.
- regression checks ensuring control-plane IPC latency remains stable.

## Phase 6 — Contract freeze + migration cleanup

- [ ] Freeze shared-transfer ABI fields and lifecycle semantics.
- [ ] Remove compatibility fallback once all consumers migrate to auto-map path.
- [ ] Publish integration guides for FS/network/display servers.

Acceptance checks:
- ABI freeze document update.
- CI coverage for migrated servers and shared-transfer conformance suite.

## PR slicing guidance

Each PR in this sequence should:

1. Ship exactly one phase (or a clearly bounded subset of a phase).
2. Include focused tests for newly introduced state transitions and failure paths.
3. Preserve backward compatibility until Phase 6 cleanup.
4. Update this checklist with completed items and links to landed commits/PRs.

## Progress notes

- Phase 1 landed in PRs up to commit `5460f97`.
- Phase 2 landed in PRs up to commit `7d1ab28`.
- Current pass extends Phase 3 with transfer-release syscall plumbing and process-cleanup mapping/envelope purge hooks.
