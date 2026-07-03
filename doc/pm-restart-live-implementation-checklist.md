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

## SUP-L2 PM decode/validation-only status

SUP-L2 adds Process Manager dispatch recognition for `PROC_OP_PM_RESTART_V1`
only to decode the canonical `PmRestartRequestV1`, validate sender identity and
request policy, and encode canonical `PmRestartReplyV1` rejected/deferred
responses. The supervisor still does not send `PROC_OP_PM_RESTART_V1`; SUP-L2
therefore has no live supervisor PM restart IPC path.

The PM restart mechanism remains unavailable. Valid requests are deferred with no
replacement handle; they are never accepted and never report fake restart success.
SUP-L2 does not restart, spawn, tear down tasks, allocate address spaces,
mint/revoke caps, grant MMIO/IRQ/DMA, perform MMIO, or change syscall/process IPC
ABI counts. The next stage, SUP-L3, may add the supervisor send path, but still
must not execute restart.

## SUP-L3 supervisor PM restart client status

SUP-L3 adds the supervisor PM restart client send/receive path for
`PROC_OP_PM_RESTART_V1` when PM request/reply endpoint authority is configured.
The supervisor builds canonical `PmRestartRequestV1` payloads, sends them to PM,
and decodes canonical `PmRestartReplyV1` responses.

PM remains decode/validation/deferred only: no PM restart mechanism exists yet.
Deferred or rejected PM replies preserve dead/degraded supervisor state and block
repeat sends rather than clearing the failure. Any Accepted reply is treated as a
protocol violation until SUP-L4 and must not mark restart success. SUP-L3 does
not restart, spawn, tear down tasks, allocate address spaces, mint/revoke caps,
grant MMIO/IRQ/DMA, perform MMIO, change process opcode count, or change syscall
ABI. The next stage, SUP-L4, will implement one narrow PM restart mechanism.

## SUP-L4 narrow live restart prototype

SUP-L4 introduces the first narrow PM-owned live restart prototype behind an
explicit PM mechanism gate. The gate defaults off, so valid PM restart requests
continue to return Deferred/MechanismUnavailable unless a test/hosted policy
explicitly enables the SUP-L4 mechanism.

The supported target is intentionally limited to one PM-known direct-initrd
service class: lifecycle records with image id 6. PM already stores the target
TID, image id, parent TID, and lifecycle state for these records. The prototype
uses the existing PM-owned direct-initrd spawn path to create a replacement task
and records the replacement in PM lifecycle bookkeeping. It does not implement
broad restart for arbitrary services, dependency cascades, driver/resource
restart, real timer integration, resource cleanup, or final capability-bound
token authority.

Before mutation, PM still validates the verified supervisor sender, diagnostic
`supervisor_tid`, target lifecycle record, scoped/redacted token ownership,
optional token fingerprint, attempt count, restart reason, dependency blocker,
and startup-cap policy. Unsupported service classes, missing restart specs,
closed gates, bad senders, wrong token owners, raw/unscoped tokens, and exhausted
attempts produce Rejected/Deferred replies with zero replacement handles.

When the gate is on and the single supported service passes validation, PM marks
the restart operation in progress, reserves replacement accounting, builds the
replacement from the PM-known lifecycle spec, runs the existing PM-owned spawn
mechanism, records the replacement lifecycle entry, and replies Accepted only
after the replacement TID exists and has been recorded. Failures after reservation
enter rollback, clear the reservation, leave the old failed/degraded task state
truthful, and reply RolledBack/Deferred/Rejected without a replacement handle.

Supervisor Accepted handling is correspondingly gated. A reply is accepted only
when the request id and target TID match, the SUP-L4 acceptance gate is enabled,
and the replacement handle kind/value are nonzero. Valid Accepted replies clear
pending restart state and emit `SUPERVISOR_PM_RESTART_STATE_UPDATED`; mismatched
or zero-handle Accepted replies remain protocol violations.

Expected future QEMU markers for this prototype are:
`SUPERVISOR_PM_RESTART_SEND_BEGIN`, `PM_RESTART_V1_DECODE_OK`,
`PM_RESTART_SENDER_OK`, `PM_RESTART_TOKEN_OK`, `PM_RESTART_ACCOUNTING_BEGIN`,
`PM_RESTART_SPAWN_BEGIN`, `PM_RESTART_SPAWN_OK`, `PM_RESTART_REPLY_ACCEPTED`,
`SUPERVISOR_PM_RESTART_REPLY_RECV`, and
`SUPERVISOR_PM_RESTART_STATE_UPDATED`. SUP-L4 does not add process opcodes,
does not change syscall ABI, and keeps `PROCESS_IPC_OPCODE_COUNT == 16` and
`SYSCALL_COUNT == 31`.

## SUP-L4A hardening evidence: gated single-service restart proof

SUP-L4A does not broaden restart support. The only live prototype target remains
exactly the PM-known **direct-initrd image_id == 6** lifecycle class. In the
current bootstrap contract this image id represents the VFS direct-initrd service
class used by PM for startup-cap-sensitive service handoff; SUP-L4A treats the
numeric lifecycle record as the authority boundary and does not infer support for
image_id 7/8/9 or any generic lifecycle record.

Audit result:

1. **Supported service:** direct-initrd image_id == 6 only.
2. **Lifecycle storage:** PM stores the original and replacement records in the
   fixed `LifecycleTable` as `ServiceLifecycleRecord { tid, image_id, parent_tid,
   pm_service_send_cap, state }`.
3. **Spawn spec reconstruction:** PM reconstructs the replacement from PM-owned
   lifecycle metadata: `image_id`, `parent_tid`, and the existing direct-initrd
   spawn/load path. No payload TID or payload cap id is trusted as authority.
4. **Startup caps:** the SUP-L4A replacement uses the existing PM-owned
   direct-initrd spawn path and records `pm_service_send_cap = 0`; no driver,
   MMIO, IRQ, DMA, or broad startup-cap grant path is added.
5. **Security-relevant difference:** the replacement is intentionally narrower
   than broad service restart. It reuses only PM-known image/parent metadata and
   scoped/redacted token validation; cap-bound restart authority remains future
   work before any wider class can be enabled.
6. **Unsupported:** all non-image_id-6 services, non-direct-initrd sources,
   broad dependency cascades, final cap-bound restart tokens, resource cleanup,
   and generic restart-any-lifecycle-record remain unsupported.

Hosted evidence now covers the deterministic success path and negative/rollback
cases for the exact supported class: gate-off deferral, unsupported image,
missing/no target, untrusted sender, supervisor_tid spoofing, wrong token owner,
token fingerprint mismatch, raw/unscoped tokens, attempt-limit rejection,
dependency/startup-cap policy rejection, duplicate in-progress reservation,
rollback after reservation, rollback at spawn, rollback after replacement TID but
before lifecycle record, lifecycle-record failure, and modeled reply-construction
rollback. Rollback emits `PM_RESTART_ROLLBACK_BEGIN`,
`PM_RESTART_ROLLBACK_DONE`, and `PM_RESTART_REPLY_ROLLED_BACK`, clears the
reservation, returns no Accepted reply, and keeps replacement handles zero.

Supervisor acceptance remains gated and strict: Accepted replies update state only
when the supervisor acceptance gate is enabled and the reply request_id,
target_tid, replacement handle kind, and replacement handle value all validate.
Zero-handle Accepted replies, mismatched request_id/target_tid, and Accepted while
the gate is off remain protocol violations. Supervisor never executes restart
locally; PM remains the only mechanism owner.

QEMU readiness is marker-only for SUP-L4A unless a deterministic restart workload
is added. The future expected sequence is:
`SUPERVISOR_PM_RESTART_SEND_BEGIN`, `PM_RESTART_V1_DECODE_OK`,
`PM_RESTART_SENDER_OK`, `PM_RESTART_TOKEN_OK`, `PM_RESTART_ACCOUNTING_BEGIN`,
`PM_RESTART_SPAWN_BEGIN`, `PM_RESTART_SPAWN_OK`, `PM_RESTART_REPLY_ACCEPTED`,
`SUPERVISOR_PM_RESTART_REPLY_RECV`, and
`SUPERVISOR_PM_RESTART_STATE_UPDATED`. Full QEMU acceptance remains SUP-L5 before
any broadening beyond direct-initrd image_id == 6.

## SUP-L5 MissingImageId/MissingRestartSpec audit: crash-test workload blocked

SUP-L5 audited the deterministic crash-test restart workload request and does **not**
add `crash_test_srv` in this change. No crash-test image id is selected in SUP-L5
because the current safe image-id contract has no unused direct-initrd service id
that can be assigned without touching kernel or architecture code, and the hard
rules forbid those changes.

Spawn/image-id audit result:

1. **Image id mappings:** PM classifies image ids `1..=6` as direct-initrd and
   `7..=12` as VFS-backed. The concrete PM path table maps image ids 4, 5, and 6
   to `/initramfs/sbin/initramfs_srv`, `/initramfs/sbin/devfs_srv`, and
   `/initramfs/sbin/vfs_server`; ids 7 through 12 are late VFS-backed services.
2. **Currently used image ids:** init currently spawns image ids 4, 5, 6, 7, 8,
   9 and optional/test-gated filesystems at ids 10, 11, and 12. Existing PM
   lifecycle bootstrap also seeds supervisor/init metadata outside the free
   service range.
3. **Crash-test image id:** none selected. Reusing image id 6 would make the VFS
   service crash and would violate the warning not to repurpose a core service;
   assigning id 13 would require a new spawn/load mapping and restart metadata
   path that is not present under the no-kernel/no-arch/no-slot-invention rules.
4. **PM spawn path:** current PM spawn uses `PROC_OP_SPAWN_V5_CAP`, then either
   the direct-initrd kernel-backed spawn path for ids 1..=6 or the VFS-backed
   `pm_vfs_spawn_inline` path for ids 7..=12. SUP-L5 does not duplicate or bypass
   this path.
5. **Lifecycle records:** PM records spawned services in `LifecycleTable` as
   `ServiceLifecycleRecord { tid, image_id, parent_tid, pm_service_send_cap,
   state }`.
6. **Restart metadata:** the current narrow SUP-L4/SUP-L4A replacement path only
   has enough PM-owned direct-initrd metadata for the explicitly gated supported
   class. It does not store a separate crash-test restart spec, generation
   counter, delay/mode config, or cap-bound restart authority for a new dummy
   service.
7. **Startup caps:** normal services rely on the existing PM/init startup-cap
   conventions; SUP-L5 does not hard-code CNode slots, fabricate caps, or install
   endpoint caps manually.
8. **Replacement helper:** the only allowed replacement helper remains the
   existing PM-owned direct-initrd helper used by the gated SUP-L4 path; no
   generic restart-any-image helper is introduced.
9. **Sufficiency:** current PM state is insufficient to restart a new dummy
   service without inventing image-id mapping or restart metadata, so the correct
   outcome is `MissingImageId` / `MissingRestartSpec` and no `Accepted` reply.

Fail-closed behavior for this stage: the crash-test service is not staged into
normal boot, no QEMU crash-restart smoke script is added, and no marker-count
claim is made. The expected future workload remains: four
`CRASH_TEST_SRV_ENTRY` markers for generations 1..4, exactly three
`PM_RESTART_REPLY_ACCEPTED` markers, exactly three
`SUPERVISOR_PM_RESTART_STATE_UPDATED` markers, then one
`SUPERVISOR_RESTART_LIMIT_EXCEEDED attempts=3` and one
`SUPERVISOR_SERVICE_DEGRADED_FINAL`. Until a safe image id and PM-owned restart
metadata are added, `PM_RESTART_REPLY_ACCEPTED` must not appear for a crash-test
image.

Guardrails remain unchanged: `PROCESS_IPC_OPCODE_COUNT == 16`,
`SYSCALL_COUNT == 31`, no kernel/arch/RPi5/driver-manager DRS changes, no broad
restart-any-image support, no manual cap/slot invention, no raw token authority,
and no supervisor-side restart execution.

## SUP-L5A safe crash-test image metadata: test-gated image id 13

SUP-L5A resolves the SUP-L5 MissingImageId planning blocker without adding the
`crash_test_srv` service binary and without changing normal boot. The selected
future crash-test image id is `CRASH_TEST_SRV_IMAGE_ID = 13`; it is unique relative
to the existing direct-initrd ids `1..=6` and VFS-backed ids `7..=12`, and image
id 6 remains `vfs_server`.

The mapping is test-gated only. PM records source/docs markers
`CRASH_TEST_IMAGE_ID_ASSIGNED image_id=13` and `CRASH_TEST_IMAGE_GATED`, and the
path descriptor is `/initramfs/sbin/crash_test_srv`, but PM exposes that mapping
only when the supervisor restart test gate is enabled. Normal production boot and
RPi5 profiles do not spawn or stage `crash_test_srv`.

SUP-L5A selects the existing VFS-backed PM spawn path for the future crash-test
service because direct-initrd ids are occupied and broadening the direct-initrd
range would affect bootstrap behavior. The metadata references the existing
`pm_vfs_spawn_inline` path and does not duplicate spawn logic.

The PM-owned restart metadata is bounded and descriptor-only. A crash-test restart
spec records:

- original image id 13;
- service name `crash_test_srv`;
- parent/supervisor owner TIDs;
- default `max_restarts = 3`;
- scoped/redacted token fingerprint descriptor;
- load source `Vfs` and the gated path descriptor.

The spec deliberately does **not** store local CapIds as authority, hard-coded
CNode slots, endpoint-cap installations, manually fabricated caps, or driver /
MMIO / IRQ / DMA resource descriptors. Missing spec remains `MissingRestartSpec`,
and the crash-test image is unavailable unless the restart-test gate is enabled.
SUP-L4 production restart support remains narrow: image id 6 is still the existing
gated SUP-L4 class, and image id 13 is restart-test-only future metadata. There is
no generic restart-any-image or restart-any-lifecycle path.

No crash-test workload is added in SUP-L5A. A future SUP-L5B should add the
`crash_test_srv` binary and manifest/profile staging under the same test gate. A
future SUP-L6 should add the deterministic QEMU restart-count smoke that expects
four crash-test generations, exactly three Accepted PM restart replies, and the
final restart-limit/degraded markers.

### SUP-L5B crash-test binary and gated staging

SUP-L5B adds the `crash_test_srv` userspace binary and test-only staging for the SUP-L5A image descriptor. The service image remains `CRASH_TEST_SRV_IMAGE_ID = 13` and resolves to `/initramfs/sbin/crash_test_srv` only when the supervisor restart test gate (`YARM_SUPERVISOR_RESTART_TEST=1` or equivalent runtime policy `yarm.supervisor_restart_test=1`) is enabled. Normal boot, service-core startup, RPi5 profiles, driver/resource services, and `vfs_server` image_id 6 are unchanged.

The binary emits `CRASH_TEST_SRV_ENTRY`, `CRASH_TEST_SRV_READY`, waits using the existing userspace yield path with a small deterministic SUP-L5B delay, emits `CRASH_TEST_SRV_FAULT_NOW`, and deliberately faults in userspace. No new syscall ABI, IPC ABI, argv/env ABI, startup-slot layout, CNode slot, capability fabrication, endpoint installation, MMIO/IRQ/DMA grant, or PM spawn path is introduced. The hosted build can compile the binary; the freestanding image is staged only by the gated QEMU artifact scripts.

The Process Manager integration keeps image_id 13 on the existing VFS-backed spawn helper path when and only when supervisor restart testing is enabled. With the gate off, image_id 13 remains unavailable and fail-closed. With the gate on but the binary missing, PM/VFS loading must fail as a missing-image or missing-restart-spec style error and must not return `Accepted`. Restart support remains narrow: the existing SUP-L4 supported class plus crash-test image_id 13 under the restart-test gate only; there is no restart-any-image or restart-any-lifecycle path.

SUP-L6 will add the deterministic QEMU restart-count proof. The expected future marker-count acceptance is: `CRASH_TEST_SRV_ENTRY` = 4, `CRASH_TEST_SRV_READY` = 4, `CRASH_TEST_SRV_EXIT_NOW` or `CRASH_TEST_SRV_FAULT_NOW` = 4, `PM_RESTART_REPLY_ACCEPTED` = 3, `SUPERVISOR_PM_RESTART_STATE_UPDATED` = 3, `SUPERVISOR_RESTART_LIMIT_EXCEEDED` = 1, `SUPERVISOR_SERVICE_DEGRADED_FINAL` = 1, and `PM_RESTART_REPLY_ACCEPTED` must not appear 4 times.

### SUP-L6 deterministic QEMU crash restart smoke oracle

SUP-L6 adds `scripts/qemu-supervisor-crash-restart-smoke.sh` as the deterministic x86_64 QEMU oracle for the gated `crash_test_srv` workload. The script builds/stages artifacts with `YARM_SUPERVISOR_RESTART_TEST=1` and `SUPERVISOR_RESTART_TEST=1`, boots with `yarm.supervisor_restart_test=1 yarm.crash_test_max_restarts=3 yarm.crash_test_delay_ms=1000`, normalizes the serial log, scans fatal markers, writes a marker snapshot, and requires exact restart-count markers.

The required SUP-L6 oracle is: `CRASH_TEST_SRV_ENTRY` = 4, `CRASH_TEST_SRV_READY` = 4, either `CRASH_TEST_SRV_EXIT_NOW` = 4 or `CRASH_TEST_SRV_FAULT_NOW` = 4, `PM_RESTART_REPLY_ACCEPTED` = 3, `SUPERVISOR_PM_RESTART_STATE_UPDATED` = 3, `SUPERVISOR_RESTART_LIMIT_EXCEEDED` = 1, and `SUPERVISOR_SERVICE_DEGRADED_FINAL` = 1. The script also requires the real PM/supervisor send, decode, validation, reservation, spawn, accepted-reply, and final degraded path markers and fails if accepted/state-update counts reach 4 or more.

The audit still identifies a runtime blocker for claiming a passing SUP-L6 proof in this change: the gate is staged into the QEMU artifact/profile and kernel command line, but there is not yet a safe end-to-end runtime propagation/registration path that enables PM `supervisor_restart_test_enabled`, PM restart mechanism gate, supervisor acceptance gate, initial `crash_test_srv` spawn, supervised registration, scoped restart token provisioning, and max_restarts=3 from boot configuration. Therefore the script is an exact fail-closed oracle; if those runtime markers are absent it reports the missing gate/registration path instead of faking success. Normal boot remains unaffected, image_id 13 remains test-gated, image_id 6 remains `vfs_server`, and broad restart-any-image/lifecycle support remains forbidden.

Manual/demo 30-second timing remains separate from the short smoke oracle. Remaining gaps before a passing SUP-L6 proof are cap-bound restart authority finalization, real timer endpoint integration, dependency cascade policy, resource cleanup, and the safe runtime gate/registration handoff described above.

### SUP-L6B gated runtime handoff for crash-test restart smoke

SUP-L6B wires the smallest runtime handoff needed for the existing SUP-L6 oracle without broadening restart support. The restart-test gate is carried by the gated artifact build environment (`YARM_SUPERVISOR_RESTART_TEST=1` / `SUPERVISOR_RESTART_TEST=1`) and the QEMU command line still includes `yarm.supervisor_restart_test=1` for audit visibility. Normal builds leave the compile-time gate off, so normal boot does not spawn or register `crash_test_srv`.

When the gate is on, init emits `INIT_SUPERVISOR_RESTART_TEST_GATE_ON`, requests the existing PM SpawnV5 path for image_id 13 (`INIT_CRASH_TEST_SPAWN_REQUEST image_id=13`), and registers the resulting TID with supervisor using the existing registration IPC with `max_restarts = 3`. PM emits `PM_SUPERVISOR_RESTART_TEST_GATE_ON`, routes image_id 13 through the existing VFS-backed PM spawn helper, records lifecycle metadata, and records a bounded scoped/redacted restart-token descriptor for the target TID. Supervisor emits `SUPERVISOR_RESTART_TEST_GATE_ON`, enables PM accepted-reply handling only under the test gate, records the crash-test policy, accepts PM replacements only through the PM reply path, and moves the tracked TID to the replacement handle after a valid accepted reply.

Restart-count semantics remain: the initial incarnation is not a restart; `max_restarts = 3` permits exactly three Accepted replacements; after the fourth incarnation exits or faults, supervisor emits `SUPERVISOR_RESTART_LIMIT_EXCEEDED attempts=3` and `SUPERVISOR_SERVICE_DEGRADED_FINAL`. The SUP-L6 QEMU oracle remains unchanged and must still prove `CRASH_TEST_SRV_ENTRY = 4`, `PM_RESTART_REPLY_ACCEPTED = 3`, and `SUPERVISOR_PM_RESTART_STATE_UPDATED = 3`.

Guardrails remain unchanged: image_id 13 is test-gated, image_id 6 remains `vfs_server`, there is no generic restart-any-image/lifecycle path, no new IPC opcodes or syscall ABI changes are introduced, no CNode slots or endpoint caps are invented, no driver/resource/MMIO/IRQ/DMA grants are added, PM remains the restart mechanism owner, and supervisor does not execute restarts locally. If QEMU is unavailable in the agent environment, the user can run `scripts/qemu-supervisor-crash-restart-smoke.sh x86_64` locally to prove or expose the next real runtime bug.

### SUP-L6C runtime load-path proof markers for crash-test image 13

SUP-L6C does not change the restart ABI, restart counts, or supported-service scope. It narrows diagnosis of the gated `crash_test_srv` QEMU blocker to the runtime file-load path after user-side evidence proved that the host-built ELF and the staged CPIO copy are non-empty, byte-identical, and begin with ELF magic.

The runtime must now prove each handoff boundary explicitly: initramfs CPIO indexing logs `INITRAMFS_CPIO_ENTRY_COUNT`; crash-test lookup logs `INITRAMFS_LOOKUP_BEGIN` / `INITRAMFS_LOOKUP_HIT`; initramfs reads or file-grant handling log `INITRAMFS_READ_DONE` and `INITRAMFS_READ_ELF_MAGIC_OK`; PM logs `PM_VFS_SPAWN_LOAD_REPLY`, `PM_VFS_SPAWN_LOAD_FIRST4`, and `PM_VFS_SPAWN_ELF_MAGIC_OK`; and failures are classified with `PM_VFS_SPAWN_FAIL_DETAIL site=<reply_decode|elf_parse|mo_create|spawn_from_mo>` instead of collapsing every failure to `Malformed`.

The decision tree is unchanged: lookup miss means fix path normalization or the CPIO inode table; wrong hit size means fix CPIO indexing; bad initramfs first4 means fix runtime CPIO offset/read; good initramfs first4 but bad PM first4/len means fix VFS reply/copy/decode; good PM first4 and length with parse failure means fix ELF parsing only; `mo_create` and `spawn_from_mo` sites remain precise runtime blockers. No slot/cap fabrication, broad restart-any-image support, or kernel/arch changes are introduced.

## SUP-L6P — runtime-authoritative PM restart sender validation

SUP-L6O fixed the accepted-fault token path: supervisor now preserves a
recorded restart token when present, falls back to the read-only Process Manager
restart-token query, stores a successful PM-query token in the managed-service
record, emits `SUPERVISOR_RESTART_TOKEN_STATE ... present=1 source=record|pm-query`,
and only then enters the task-exit/restart scheduling path.

SUP-L6P fixes the next live blocker in PM restart execution. The root cause was
that `handle_pm_restart_v1` trusted a stale hardcoded supervisor TID (`4`) while
the production boot observed by the crash-restart smoke ran the real supervisor
as TID `2`. The request's `sender_tid=2` is kernel-authenticated IPC metadata,
and the restart payload's `supervisor_tid=2` matched it, so the anti-spoofing
cross-check passed; only the stale trusted-supervisor comparison rejected the
request.

The correct rule is now explicit: PM restart execution trusts the
runtime-authoritative supervisor TID stored in PM runtime state from the startup
lifecycle handoff (`startup_context().supervisor_tid`). If that value is absent,
restart execution fails closed with `PM_RESTART_SENDER_REJECTED ...
reason=trusted_supervisor_unknown`. If present, `sender_tid` must equal the
trusted runtime supervisor TID, and any nonzero payload `supervisor_tid` must
still equal `sender_tid` as an anti-spoofing cross-check. Rejections are marked
with `PM_RESTART_SENDER_CHECK_BEGIN`, `PM_RESTART_SENDER_REJECTED ...
reason=untrusted_supervisor`, or `PM_RESTART_SENDER_REJECTED ...
reason=trusted_supervisor_unknown`; accepted senders emit `PM_RESTART_SENDER_OK`.

Restart-token query behavior remains read-only/open as implemented by SUP-L6O
and is not treated as execution authority. PM remains authoritative for restart
execution; the supervisor does not locally spawn or restart tasks. The crash-test
restart remains gated and narrow, with no broad restart-any-image support. This
change does not modify kernel code, architecture code, syscall ABI, RPi5 behavior,
driver-manager DRS behavior, or the PM restart codec layout. The frozen counts
remain `PROC_OP_PM_RESTART_V1 = 15`, `PROC_OP_PM_RESTART_REPLY_V1 = 16`,
`PROCESS_IPC_OPCODE_COUNT = 16`, and `SYSCALL_COUNT = 31`.

## SUP-L6Q — PM trusted-supervisor runtime wiring

User-local QEMU after SUP-L6P proved the sender-validation logic was fail-closed
but not yet wired: PM logged `PM_RESTART_SENDER_CHECK_BEGIN sender_tid=2
payload_supervisor_tid=2 trusted_supervisor_tid=0` followed by
`PM_RESTART_SENDER_REJECTED ... reason=trusted_supervisor_unknown`. The exact
reason is that PM's startup slot 9 (`startup_context().supervisor_tid`) is zero
in this boot; that slot is only populated for tasks that receive it, and PM's
existing diagnostics already treated zero as missing/unknown while seeding the
supervisor lifecycle record from the deterministic bootstrap lifecycle order.

SUP-L6Q wires `ProcessService::trusted_supervisor_tid` from that existing
runtime lifecycle source. PM still first probes `startup_context` and emits
`PM_RESTART_TRUSTED_SUPERVISOR_INIT_BEGIN/OK/UNKNOWN`; when the startup slot is
unknown, PM uses the already-existing bootstrap lifecycle handoff that records
supervisor image_id 1 as the task spawned immediately before PM. Updates emit
`PM_RESTART_TRUSTED_SUPERVISOR_UPDATE_OK old=0 new=<tid>
source=lifecycle_bootstrap_order`; zero or conflicting updates are rejected with
`PM_RESTART_TRUSTED_SUPERVISOR_UPDATE_REJECTED`.

The expected user-local smoke sender check is now
`PM_RESTART_SENDER_CHECK_BEGIN sender_tid=2 payload_supervisor_tid=2
trusted_supervisor_tid=2`, followed by `PM_RESTART_SENDER_OK sender_tid=2`. PM
accepts sender TID 2 only because runtime lifecycle state identified TID 2 as
the supervisor; task 2 is not hardcoded as restart authority. Unknown trusted
supervisor still fails closed, arbitrary senders remain rejected, the payload
`supervisor_tid` anti-spoof check remains, token query stays read-only/non-
authorizing, crash-test restart remains gated/narrow, supervisor still does not
locally spawn or restart tasks, and PM remains the restart authority. No kernel,
syscall ABI, RPi5, driver-manager DRS, PM restart codec, or arch behavior
changes. Frozen counts remain `PROC_OP_PM_RESTART_V1 = 15`,
`PROC_OP_PM_RESTART_REPLY_V1 = 16`, `PROCESS_IPC_OPCODE_COUNT = 16`, and
`SYSCALL_COUNT = 31`; request/reply codec lengths remain 110 and 50 bytes.

## SUP-L7A — supervisor PM restart reply receive and state update

User-local QEMU after SUP-L6Q reached `PM_RESTART_SENDER_OK`,
`PM_RESTART_VALIDATE_OK`, `PM_RESTART_SPAWN_OK`, and
`PM_RESTART_REPLY_ACCEPTED request_id=1 target_tid=<old>`, proving PM restart
execution accepted the runtime supervisor and spawned the first replacement. The
next blocker was supervisor-side reply handling: `SUPERVISOR_PM_RESTART_REPLY_RECV`,
`SUPERVISOR_PM_RESTART_REPLY_ACCEPTED`, and
`SUPERVISOR_PM_RESTART_STATE_UPDATED` were absent, so the supervisor did not
record the replacement lineage or drive attempts 2/3 and final degraded state.

SUP-L7A fixes the first reply-path blocker. The supervisor PM restart client had
sent with `ipc_call` and then performed an immediate zero-deadline receive on the
reply cap. That could observe no message before PM ran and sent its reply, so the
client returned a send failure before logging `SUPERVISOR_PM_RESTART_REPLY_RECV`.
The client now emits `SUPERVISOR_PM_RESTART_REPLY_WAIT_BEGIN` and waits on the
existing PM reply cap with `ipc_recv_v2`, then validates opcode/length
(`PROC_OP_PM_RESTART_REPLY_V1`, 50 bytes), decodes with the frozen reply codec,
checks request_id and target_tid, accepts only a scoped task-TID replacement
handle, and emits `SUPERVISOR_PM_RESTART_REPLY_DECODE_OK` and
`SUPERVISOR_PM_RESTART_REPLY_ACCEPTED` before the state machine records the
replacement TID and emits `SUPERVISOR_PM_RESTART_STATE_UPDATED`.

`PM_RESTART_REPLY_ACCEPTED request_id=... target_tid=...` means PM accepted the
restart operation and built an accepted reply; the PM runtime loop now also logs
`PM_RESTART_REPLY_SEND_BEGIN` and `PM_RESTART_REPLY_SEND_OK` when it sends that
reply through the kernel reply capability. PM remains restart authority, the
supervisor remains policy/state owner and does not locally spawn or restart, the
crash-test restart remains gated/narrow, and token query remains read-only and
non-authorizing. No kernel, syscall ABI, arch, RPi5, driver-manager DRS, or PM
restart codec layout changes; `PROC_OP_PM_RESTART_V1 = 15`,
`PROC_OP_PM_RESTART_REPLY_V1 = 16`, `PROCESS_IPC_OPCODE_COUNT = 16`,
`SYSCALL_COUNT = 31`, and request/reply lengths 110/50 remain frozen.

## SUP-L7B — PM restart reply wire-shape convention

User-local QEMU after SUP-L7A proved reply delivery reached the supervisor:
`SUPERVISOR_PM_RESTART_REPLY_RECV tid=10008 request_id=1` appeared after
`PM_RESTART_REPLY_SEND_OK request_id=1 target_tid=10008`. The next blocker was
reply shape validation: the supervisor received `opcode=0 len=50`, rejected the
message before codec decode, and therefore did not emit
`SUPERVISOR_PM_RESTART_REPLY_DECODE_OK`, `SUPERVISOR_PM_RESTART_REPLY_ACCEPTED`,
or `SUPERVISOR_PM_RESTART_STATE_UPDATED`.

SUP-L7B audits this as a userspace reply-cap wire convention, not a PM codec
length bug. `ProcessService::pm_restart_reply_with_handle` builds a message with
`PROC_OP_PM_RESTART_REPLY_V1` and a 50-byte payload, but the existing
`ipc_reply` syscall wrapper passes only the payload pointer/length and transfer
cap to the kernel. The kernel `handle_ipc_reply` reconstructs no-cap replies with
`Message::new(...)`, so reply-cap deliveries have IPC opcode `0` while preserving
the exact 50-byte PM restart reply payload. Changing that would be a syscall IPC
ABI semantic change, which SUP-L7B does not do.

Therefore the strict supervisor shape rule is: reply-cap IPC opcode `0`, ABI
opcode `PROC_OP_PM_RESTART_REPLY_V1 = 16` as the decoded payload type, and
payload length `PM_RESTART_REPLY_V1_LEN = 50`. The supervisor now emits
`SUPERVISOR_PM_RESTART_REPLY_SHAPE_OK opcode=0 abi_opcode=16 len=50` for this
valid reply-cap shape and still emits `SUPERVISOR_PM_RESTART_REPLY_SHAPE_FAIL`
and `SUPERVISOR_PM_RESTART_REPLY_DECODE_FAIL reason=shape` for wrong opcode or
wrong length. It then decodes the frozen 50-byte codec, rejects request_id or
target_tid mismatch, rejects zero or non-task-TID replacement handles, records
the replacement TID in the managed record, and emits
`SUPERVISOR_PM_RESTART_STATE_UPDATED tid=<replacement> replacement_tid=<replacement> attempt=<n>`.

`PM_RESTART_REPLY_ACCEPTED request_id=... target_tid=...` continues to mean PM
accepted the restart operation and built an accepted reply. `PM_RESTART_REPLY_SEND_OK
request_id=... target_tid=... opcode=0 abi_opcode=16 len=50` means the actual
reply-cap IPC send completed using the established reply syscall convention.
PM remains restart authority, the supervisor remains policy/state owner and does
not locally spawn or restart, crash-test restart remains gated/narrow, and token
query remains read-only/non-authorizing. No kernel, syscall ABI, arch, RPi5,
driver-manager DRS, or PM restart codec layout changes; `PROC_OP_PM_RESTART_V1 =
15`, `PROC_OP_PM_RESTART_REPLY_V1 = 16`, `PROCESS_IPC_OPCODE_COUNT = 16`,
`SYSCALL_COUNT = 31`, and request/reply lengths 110/50 remain frozen.
