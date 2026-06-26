<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Umut Deniz Balkan -->

# PM restart live-implementation checklist and conformance matrix (SUP-6)

SUP-6 is not a live restart implementation. It adds review and conformance
artifacts only. It SUP-L1 adds only global IPC ABI opcode reservations, does not change syscall
ABI, does not wire supervisor-to-PM restart IPC, and does not create/restart/tear
down tasks, allocate address spaces, mint/revoke capabilities, grant MMIO/IRQ/DMA,
or perform MMIO. Future SUP-7/live work must pass this checklist before enabling
any runtime path.

## Live implementation review checklist

### 1. ABI approval gate

- [ ] Numeric opcode assignment is reviewed and approved; proposed names remain
      `PROC_OP_PM_RESTART_V1` and `PROC_OP_PM_RESTART_REPLY_V1` until allocation.
- [ ] Request/reply versioning is explicit and starts at version `1`.
- [ ] Request/reply payload sizes are fixed and bounded; no heap-sized fields.
- [ ] Unsupported versions return `UnsupportedVersion` without interpreting the
      rest of the payload.
- [ ] Compatibility policy documents old supervisor/new PM and new supervisor/old
      PM behavior.

### 2. Sender authority

- [ ] PM uses verified IPC sender metadata for supervisor identity.
- [ ] Restart authority is carried by the supervisor endpoint/cap grant, not by a
      payload TID.
- [ ] Arbitrary PM clients are rejected even if they can send to a PM endpoint.
- [ ] Payload `supervisor_tid` is informational only and is never trusted as
      authority.

### 3. Restart token authority

- [ ] Raw/unscoped restart tokens are not accepted by the live ABI.
- [ ] Token authority is bound to the target service/task.
- [ ] Dependent restart uses the dependent service's own token only.
- [ ] Logs use redacted token fingerprints/references only.

### 4. PM validation

- [ ] Target exists in the PM lifecycle table.
- [ ] Target is restartable under PM policy.
- [ ] Restart reason is allowed by policy.
- [ ] Attempt count is within limit.
- [ ] Dependency blockers are enforced.
- [ ] Resource preflight is available.
- [ ] Startup-cap layout is supported.
- [ ] Rollback policy is supported.

### 5. PM accounting

- [ ] Replacement task slot reserved/described.
- [ ] Address-space slot reserved/described.
- [ ] CNode/startup-cap slots reserved/described.
- [ ] Inherited service caps accounted.
- [ ] Fault/restart monitor slot accounted.
- [ ] PM handle reserved/described.
- [ ] Init/supervisor notification slot accounted.

### 6. PM rollback

- [ ] Rollback runs in reverse reservation order.
- [ ] Old task state after restart failure is explicit: dead, degraded, or policy
      retained.
- [ ] Partial replacement task state is cleaned up by PM.
- [ ] Capability/resource rollback is PM-owned.
- [ ] Init/supervisor are alerted after rollback; rollback is never reported as
      restart success.

### 7. Timer/backoff

- [ ] Runtime uses a real timer endpoint or PM/kernel timer source.
- [ ] Restart is never executed before due tick.
- [ ] Timer unavailable defers restart rather than executing immediately.
- [ ] Backoff saturates/caps safely.
- [ ] Crash-loop alerts and PM requests are rate limited.

### 8. Production observability

- [ ] Structured markers exist for accepted, rejected, deferred, and rolled-back
      outcomes.
- [ ] Full restart tokens are never logged.
- [ ] Metrics hooks exist for attempts, rejects, deferrals, rollbacks, and
      rate-limited alerts.
- [ ] PM and supervisor logs include request IDs and target TIDs without leaking
      capability-local identifiers as authority.

### 9. Security

- [ ] Spoofed sender is rejected.
- [ ] Wrong token owner is rejected.
- [ ] Replay is detected through request/token generation or PM nonce state.
- [ ] Duplicate restart while already pending/running is rejected or reported as
      `AlreadyRestarting`.
- [ ] Already-running targets are handled according to explicit policy.

### 10. Rollout gates

- [ ] SUP model/oracle tests pass.
- [ ] SUP-6 conformance matrix tests pass.
- [ ] QEMU boot chain is unaffected.
- [ ] Rollback injection tests pass.
- [ ] Fail-closed fallback is tested for missing PM client, missing timer, missing
      token authority, and unsupported version.

## Conformance matrix

| Future live behavior | Existing SUP model/oracle | Future live test name | Expected PM reply | Expected supervisor state | Rollback expectation | Blocker before live? |
|---|---|---|---|---|---|---|
| Valid supervisor request | SUP-2 request + SUP-4 validation/accounting | `pm_restart_live_valid_supervisor_request_accepts` | `Accepted` | restart accepted/requested | none | yes |
| Untrusted sender | SUP-1/SUP-4 sender checks | `pm_restart_live_untrusted_sender_rejected` | `Rejected/MissingRight` | remains degraded/dead | none | yes |
| Wrong token owner | SUP-2 token rule + SUP-4 token check | `pm_restart_live_wrong_token_owner_rejected` | `Rejected/WrongTokenOwner` | blocked/degraded | none | yes |
| Raw token | SUP-5 scoped-token RFC | `pm_restart_live_raw_token_rejected` | `Rejected/RawTokenUnsupported` | blocked/degraded | none | yes |
| Unknown target | SUP-4 target-exists check | `pm_restart_live_unknown_target_no_such_target` | `NoSuchTarget` | blocked/degraded | none | yes |
| Restart limit exceeded | SUP-2/SUP-4 attempt policy | `pm_restart_live_restart_limit_rejected` | `Rejected/RestartLimitExceeded` | policy gives up/degraded | none | yes |
| Dependency blocker | SUP-1/SUP-2 dependency policy | `pm_restart_live_dependency_blocker_deferred` | `Deferred/DependencyBlocked` | deferred/degraded | none | yes |
| Resource preflight unavailable | SUP-4 resource preflight | `pm_restart_live_resource_preflight_deferred` | `Deferred/ResourceUnavailable` | deferred/degraded | none | yes |
| Startup-cap layout unsupported | SUP-4 startup-cap layout check | `pm_restart_live_startup_cap_layout_rejected` | `Rejected/StartupCapLayoutUnsupported` | blocked/degraded | none | yes |
| Rollback failure after replacement task | SUP-4 failure injection | `pm_restart_live_rollback_after_replacement_task` | `RolledBack` | degraded/dead | replacement slot rollback | yes |
| Rollback failure after startup caps | SUP-4 failure injection | `pm_restart_live_rollback_after_startup_cap` | `RolledBack` | degraded/dead | startup-cap/CNode reverse rollback | yes |
| Unsupported version | SUP-3/SUP-4 version checks | `pm_restart_live_unsupported_version_rejected` | `UnsupportedVersion` | no state change/degraded | none | yes |
| Timer unavailable | SUP-3 timer model | `pm_restart_live_timer_unavailable_deferred` | `Deferred` | deferred/degraded | none | yes |
| Duplicate while already restarting | SUP-4 already-restarting check | `pm_restart_live_duplicate_already_restarting` | `AlreadyRestarting` | already pending/degraded | none | yes |
| Already running duplicate | SUP-4 duplicate-running policy | `pm_restart_live_already_running_duplicate_rejected` | `Rejected/DuplicateRunningRestart` | unchanged | none | yes |
| Rollback alert delivery | SUP-4 reply model + SUP-5 observability | `pm_restart_live_rollback_alerts_init_supervisor` | `RolledBack` | degraded with alert pending/delivered | reverse rollback plus alert | yes |

Every matrix row must have either a direct hosted model/oracle assertion or a
source guard before live enablement. A live implementation must not claim success
for a row unless the PM reply, supervisor state, and rollback expectation match
this table.

## Numeric opcode candidates (not allocated)

The candidate opcode names are `PROC_OP_PM_RESTART_V1` and
`PROC_OP_PM_RESTART_REPLY_V1`. Numeric values were **not allocated** in SUP-6; SUP-L1 allocates 15/16.
Candidate selection for a future ABI review must:

- avoid collision with the current process IPC opcode count of 14;
- reserve a contiguous request/reply pair only after review;
- update `yarm-ipc-abi` and all dispatch tests in the same live-ABI PR;
- include unsupported-version compatibility tests;
- keep old `PROC_OP_EXECUTE_RESTART` legacy behavior separate until explicitly
  retired or bridged.

## Capability-bound token materialization requirements

- PM or an explicitly trusted lifecycle authority creates restart authority when a
  service becomes supervised.
- PM stores token/cap ownership with the target service lifecycle record.
- PM verifies token scope, generation, target TID, supervisor authority, and replay
  state before accepting a restart request.
- Supervisor references restart authority through a redacted/scoped descriptor or
  capability-bound handle; raw token bytes are not logged or trusted from payload.
- Token revocation occurs when the service exits permanently, is unsupervised, or
  its lifecycle generation changes.
- Replay is prevented by generation/nonce state tied to PM lifecycle records and
  request IDs.
- Dependent service tokens are distinct from the failed service token; dependent
  restart cannot reuse the failed task's authority.
- Logs include redacted fingerprints only and must not include raw token material
  or cspace-local CapIds as authority.

## Before live enablement

Future SUP-7/live work must not enable runtime restart until all of the following
are complete:

- [ ] ABI numeric assignment approved and documented.
- [ ] PM verified sender path implemented and tested.
- [ ] Scoped/capability-bound token validation implemented and tested.
- [ ] PM accounting and rollback implemented with failure injection.
- [ ] Timer endpoint available, or an explicit no-timer policy accepted and tested.
- [ ] Supervisor production PM client implemented with fail-closed fallback.
- [ ] Rollback injection hosted tests pass.
- [ ] Rollback injection QEMU tests or documented smoke-equivalent tests pass.
- [ ] x86_64 and AArch64 boot smokes are unaffected.
- [ ] Docs are updated from RFC/proposed status to live status in the same PR that
      enables runtime dispatch.

## SUP-7 codec golden vector linkage

SUP-7 adds review-only codec golden vectors that map back to the conformance
matrix rows: valid supervisor request, untrusted sender, wrong token owner, raw
token, unknown target, restart limit exceeded, dependency blocker, resource
unavailable, rollback failure, unsupported version, timer unavailable, and already
restarting. The codec remains non-dispatching until all matrix rows have either
hosted oracle coverage or live conformance coverage.

## SUP-8 signoff package linkage

SUP-8 freezes the codec layout tables, reserved-field policy, reviewer signoff
checklist, and golden-vector signoff table in `doc/process-manager-restart-contract.md`.
The package is still non-live: candidate opcodes `15`/`16` remain unallocated,
PM dispatch remains absent, and supervisor PM restart send remains absent until a
future SUP-live promotion stage satisfies every signoff item.

## SUP-9 promotion dry-run link

SUP-9 adds `doc/pm-restart-live-promotion-plan.md` with the future promotion
sequence, promotion PR checklist, dry-run readiness model, rollback-injection
plan, and QEMU acceptance plan. SUP-9 is not live implementation; future SUP-live
work must satisfy SUP-8 and SUP-9 before adding opcodes, PM dispatch, supervisor
send, or PM restart mechanisms.

## SUP-10 evidence pack link

SUP-10 adds `doc/pm-restart-live-readiness-evidence.md`, mapping current evidence
to missing live proof and spelling out the exact future diff plan. SUP-10 does
not enable live ABI/runtime behavior; future SUP-live work must explicitly change
status from proposed to live and satisfy all checklists.

## SUP-L1 ABI reservation status

SUP-L1 allocates the global process IPC ABI constants `PROC_OP_PM_RESTART_V1 = 15`
and `PROC_OP_PM_RESTART_REPLY_V1 = 16` and promotes the reviewed fixed-size
Request V1 / Reply V1 codecs into the shared process IPC ABI layer. Before
SUP-L1 the process IPC opcode count was 14; after SUP-L1 it is 16 because the
restart request/reply numbers are allocated.

This is an ABI reservation/promotion only. PM runtime dispatch remains disabled,
the supervisor PM restart send path remains disabled, and the PM restart
mechanism remains unimplemented. PM must reject/defer any restart request until
later live-gated work. The next stage, SUP-L2, is limited to PM decode and
validation only and still must not restart, spawn, tear down tasks, allocate
address spaces, mint/revoke caps, grant MMIO/IRQ/DMA, perform MMIO, or fake PM
restart success.
