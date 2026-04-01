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

- [x] Integrate capability revocation with active shared-transfer mappings.
- [x] Define deterministic behavior for revoked transfer while mapped (read-fault, write-fault, or forced unmap policy).
- [x] Emit supervisor-visible fault/revocation events for observability.

Acceptance checks:
- targeted revocation tests (source revoke, receiver revoke, endpoint teardown).
- fault-policy tests verifying architecture-consistent behavior.

## Phase 5 — Throughput path tuning

- [x] Add batching/ring usage guidance for FS/network/display servers.
- [x] Minimize syscall overhead on steady-state transfer reuse.
- [x] Add fast-path telemetry and benchmark harnesses for large read/write, packet RX/TX, and framebuffer update flows.

Acceptance checks:
- benchmark gates with minimum throughput/latency thresholds.
- regression checks ensuring control-plane IPC latency remains stable.

## Phase 6 — Contract freeze + migration cleanup

- [x] Freeze shared-transfer ABI fields and lifecycle semantics.
- [x] Remove compatibility fallback once all consumers migrate to auto-map path.
- [x] Publish integration guides for FS/network/display servers.

Acceptance checks:
- ABI freeze document update.
- CI coverage for migrated servers and shared-transfer conformance suite.

## Phase 7 — Runtime hardening + CI enforcement

- [x] Add canary/runtime assertions for shared-memory map/release parity under repeated load.
- [x] Add CI gate script for shared-memory fast-path migration invariants and required tests.
- [x] Document Phase 7 rollout checks and escalation signals.

Acceptance checks:
- deterministic canary test demonstrates zero map/release byte drift.
- CI gate fails when migration docs/checkpoints/tests are missing.

## PR slicing guidance

Each PR in this sequence should:

1. Ship exactly one phase (or a clearly bounded subset of a phase).
2. Include focused tests for newly introduced state transitions and failure paths.
3. Preserve backward compatibility until Phase 6 cleanup.
4. Update this checklist with completed items and links to landed commits/PRs.

## Progress notes

- Phase 1 landed in PRs up to commit `5460f97`.
- Phase 2 landed in PRs up to commit `7d1ab28`.
- Current pass completes Phase 4 by emitting structured transfer-revocation events to the supervisor fault endpoint while preserving forced-unmap semantics.
- Phase 5 started with transfer-volume telemetry (`shared_mem_bytes_mapped`, `shared_mem_bytes_released`, `transfer_release_calls`) and a repeated map/release throughput smoke harness in syscall tests.
- Phase 5 completed with batching/ring usage guidance (`SHARED_IPC_THROUGHPUT_GUIDE.md`) and a `TransferRelease` active-mapping fast path (`ptr=0`, `len=0`) that avoids userspace rematerialization of mapping bounds on steady-state recycle loops.
- Phase 6 freezes the ABI/behavior at v6, removes user-mode descriptor-only shared-memory recv fallback, and publishes migration guidance in `SHARED_IPC_MIGRATION_GUIDE.md`.
- Phase 7 adds runtime hardening canaries for shared-memory map/release parity and introduces a dedicated CI gate script (`scripts/phase7-shared-ipc-gates.sh`) to enforce migration/test/doc invariants.
- Post-fastpath follow-up wired synchronous IPC handoff switches into measured scheduler telemetry (`scheduler_dispatch_calls`, `scheduler_yield_calls`, `scheduler_context_switches`, `scheduler_fastpath_handoffs`) and added regression assertions.
- Post-fastpath follow-up added delegation-bundle helper APIs (`delegate_driver_bundle_checked`, `redelegate_driver_bundle`) plus stale-cap regression coverage for helper-driven redelegation.
- Post-fastpath follow-up extended typed process/VFS codec freeze with stable golden vectors and CI enforcement (`scripts/check-proc-vfs-codec-freeze.sh`).
- Post-fastpath follow-up added extra HAL conformance targets (riscv64/x86_64/aarch64 baseline) with enforcement via `scripts/check-hal-conformance-targets.sh`.
- Post-fastpath follow-up expanded deterministic end-to-end server flow tests (process-manager + VFS + notification routing) with replay-stability assertions in `tests/kernel_scenarios.rs`.
