<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Umut Deniz Balkan -->

# Supervisor service audit (SUP-1)

This audit covers the userspace supervisor service only. It intentionally does
not change kernel, architecture, syscall ABI, IPC ABI, capability, scheduler, VM,
trap, RPi5 boot, init bootstrap, or driver-manager DRS mechanisms.

## Claim verdicts

| # | Verdict | Notes |
|---|---|---|
| 1 | True, fixed | Production control messages now route to the same `handle_control_request` core used by hosted tests. Unknown or unauthorized opcodes return visible errors rather than being silently discarded. |
| 2 | True, fixed fail-closed | Runtime outbound/task-exit operations no longer return fake success when no real IPC/kernel mechanism is wired. They return errors and log `SUPERVISOR_INIT_ALERT_UNAVAILABLE`, `SUPERVISOR_TASK_EXIT_OP_UNAVAILABLE`, or `SUPERVISOR_RESTART_REQUEST_DEFERRED_NO_PM_OP`. |
| 3 | True, fixed | Dependent restart scheduling no longer falls back to the failed task restart token. Missing dependent tokens block that dependent restart and log `SUPERVISOR_DEPENDENT_RESTART_BLOCKED_NO_TOKEN`. |
| 4 | True, fixed as logical tick | Production advances a logical supervisor tick and executes due restarts through the shared restart executor. This is not wall-clock precision; it logs `SUPERVISOR_TICK_ADVANCE` and due execution/deferred markers. |
| 5 | True, fixed | Production `SUPERVISOR_OP_TASK_EXITED` now routes to `handle_task_exit`; it no longer registers the task token as a substitute for exit handling. |
| 6 | Partially true, improved | A shared `handle_supervisor_event` step core exists for production-style control/fault/tick events and hosted regression tests. The outer receive loops still differ because hosted tests use `KernelState` while production uses syscall IPC transport. |
| 7 | True, intentionally deferred | `RestartOwner::Init` self-supervision state replay is not automatic persistence. Current tests exercise explicit init replay behavior. Marker: `SUPERVISOR_STATE_REPLAY_DEFERRED`. |
| 8 | Partially true, documented | The loop still polls control then fault in the live path to avoid boot-chain risk. A future bounded fault-priority policy should process fault endpoint traffic before routine control once startup ordering is validated. |
| 9 | Partially true, improved | Touched critical paths now avoid fake success. Failed degraded-alert/task-dead outbound operations return errors and tests assert state is not committed as successful. Larger state normalization remains future work. |
| 10 | True, intentionally deferred | Kernel resource cleanup is not implemented in supervisor. Marker: `SUPERVISOR_RESOURCE_CLEANUP_DEFERRED_NO_PM_KERNEL_API`. |
| 11 | True, roadmap deferred | Health checks, crash dumps, configurable policies, resource cleanup API, alert rate limits, graceful shutdown, and metrics remain future supervisor work. Alerts must stay visible; production must not silently drop them. |
| 12 | True, fixed | Fault access wire values now use `FaultAccess::{Read,Write,Execute}` and tests cover encode/decode behavior. |
| 13 | Partially true, improved | Shared production-style handlers return `Result<SupervisorStepOutcome, KernelError>` or existing `Result` decisions. Broad unrelated error handling remains deferred. |
| 14 | Partially true, minimally addressed | No broad borrow-checker churn was done. The touched stale-token bug was fixed directly. |
| 15 | True, fixed where metadata exists | Control/fault handlers consume verified `Message.sender_tid`. Fault reports are accepted from kernel sender `0`, the claimed task, or registered process manager; mismatches fail closed with `SUPERVISOR_FAULT_SENDER_MISMATCH`. |
| 16 | True, intentionally deferred | Restart tokens still travel in regular IPC under current capability-channel assumptions. Logs do not print full restart tokens in new markers. TODO: PM-validated capability-bound restart requests. |
| 17 | Partially true, intentionally deferred | Query-status reply caps continue using the existing transferred-cap path. Additional cap-right validation requires kernel/runtime metadata not added here; unsupported cap-bearing extensions should fail closed. |
| 18 | True, fixed | Control messages are accepted only from init or the registered process manager. Unknown senders fail with `MissingRight` and log `SUPERVISOR_CONTROL_REJECT_UNTRUSTED_SENDER`. |

## Production/test shared path

The supervisor now has a shared production-style event entry point for control,
fault/task-exit, and logical tick events. Hosted tests can inject the same event
shapes without depending on the production syscall transport. The production loop
still owns endpoint polling and boot-chain pacing; the core handler owns policy,
sender validation, task-exit decisions, restart scheduling, and due-restart
execution.

## Sender identity model

Control messages are trusted only from:

- init (`init_tid`), and
- process manager after it has been registered as a supervised core service.

Fault/task-exit reports are trusted from:

- kernel-originated sender `0`,
- the claimed task itself, or
- the registered process manager.

Any other sender is rejected visibly. Payload TIDs are not trusted without sender
metadata agreement.

## Restart-token handling

A failed task's restart token is valid only for the failed task. Dependent
services must have their own restart token available from the task-exit ops / PM
lookup path. If the dependent token is absent, the supervisor leaves that
dependent unscheduled and emits a blocked/deferred marker rather than using the
wrong token.

## Production tick/backoff model

Production now advances a logical supervisor tick in the event loop and invokes
due restart execution. This is a cooperative progress tick, not a wall-clock
timer. Until a real timer endpoint exists, backoff precision is intentionally
logical and visible in logs.

## Deferred roadmap

1. Add a real timer endpoint and define tick-to-time semantics.
2. Replace cleartext restart tokens with PM-validated, capability-bound restart
   requests.
3. Add a PM/kernel resource cleanup API for memory, caps, IRQ, and IOVA cleanup.
4. Add state snapshot/replay for automatic supervisor self-recovery.
5. Add bounded fault-priority polling once boot-chain risk is validated.
6. Add health checks, crash dumps, alert rate limiting that remains visible,
   graceful shutdown, metrics, and configurable restart policies.

## SUP-2 supervisor ↔ PM restart-request contract

SUP-2 adds an inert, userspace-only restart-request model. It does not restart,
spawn, tear down, mint caps, revoke caps, grant MMIO/IRQ/DMA, allocate address
spaces, perform MMIO, or call a live Process Manager operation.

### Authority boundary

- Supervisor owns observation and policy: exits/faults, degradation decisions,
  backoff scheduling, dependency restart policy, init-alert construction, and
  inert PM restart-request construction.
- Process Manager owns mechanism: process creation/restart/teardown, restart-token
  validation, address-space setup, resource accounting, capability delivery, and
  task cleanup.
- Kernel owns low-level task, capability, VM, scheduler, and trap mechanisms.

### Inert request shape

`SupervisorRestartRequestBundle` is a bounded fixed-size collection of
`SupervisorRestartRequest` entries. Each entry records the supervised TID, service
kind/name, restart owner, restart reason, redacted `SupervisorRestartTokenRef`,
backoff due tick, attempt count, dependency cause, degraded flag, PM authority
marker, and mock request ID. It intentionally stores descriptive references only,
not live process handles or new capability IDs.

Restart reasons model fault, normal exit, crash loop, dependency failure, manual
policy, and health timeout cases. Request statuses distinguish
`WouldRequestPmRestart`, blockers such as `BlockedNoDependentToken` and
`BlockedRestartLimit`, `NoAction`, and `AlreadyPending`.

### Validation simulation

`SupervisorPmRestartValidationReport` simulates PM-side checks without sending IPC.
It verifies supervisor identity, request version, PM authority availability,
target-record existence, token ownership, attempt limits, dependency blockers, and
fail-closed policy. Outcomes are descriptive only: `WouldAccept`, `WouldReject`,
`Deferred`, `NoAction`, `AlreadyPending`, and `Unsupported`.

### Accounting and rollback simulation

`SupervisorPmRestartAccountingReport` simulates descriptive reservations for an
accepted restart request: restart slot, replacement task slot, address-space slot,
CNode slot, startup-cap delivery slot, health-monitor slot, and init-alert slot.
Failure injection produces reverse-order rollback descriptors only. No real PM,
kernel, cap, address-space, or task operation is performed.

### Runtime production behavior

Production runtime restart execution remains fail-closed. The supervisor may build
and log an inert request using `SUPERVISOR_PM_RESTART_REQUEST_BUILT`, but live
execution returns an explicit unavailable/deferred error and logs
`SUPERVISOR_PM_RESTART_EXEC_DEFERRED_NO_PM_OP`,
`SUPERVISOR_PM_RESTART_VALIDATION_DEFERRED`, and
`SUPERVISOR_PM_RESTART_ACCOUNTING_DEFERRED` until a real PM restart mechanism is
specified and wired.

### Token and sender security

Restart-token references are scoped to the target TID and log only a redacted
fingerprint. Dependent restarts must use the dependent service's token; the failed
service token is never a substitute. Control and fault paths continue to require
verified sender metadata as described above.

### Deferred live work

A real timer endpoint, PM restart IPC contract, PM resource-accounting API,
capability-bound token transport, live cleanup/reclamation, and automatic
supervisor state replay remain deferred.

## SUP-3 PM restart IPC contract and timer/backoff oracle

SUP-3 adds design/model-only descriptors for the future supervisor → PM restart
IPC contract. `SupervisorPmRestartContract`, `SupervisorPmRestartRequestV1`, and
`SupervisorPmRestartReplyV1` describe the versioned request/reply shape without
adding a global IPC ABI opcode or sending PM IPC. Mapping from SUP-2 restart
requests preserves target TID, service metadata, restart reason, attempt count,
due tick, dependency cause, degraded hint, mock request ID, and redacted token
reference. Blocked, missing-token, restart-limit, no-action, already-pending, and
PM-authority-unavailable entries remain non-sendable/deferred descriptors.

Timer semantics remain explicit: production is `LogicalTickOnly`, not wall-clock.
Future execution requires a timer endpoint or PM/kernel timer source. Backoff is
monotonic in supervisor tick domain, capped/saturating on overflow, and deferred
when a future timer endpoint is unavailable. Due restarts are evaluated only after
a tick/timer event.

The PM reply model is inert: accepted replies record mock replacement handles,
rejected replies keep the restart blocked/degraded in the model, deferred replies
produce retry ticks, rolled-back replies mark degraded rollback failure, and
invalid versions are rejected. No live PM handle, task restart, cleanup, cap, VM,
or resource operation is performed.

Runtime remains fail-closed and logs `SUPERVISOR_PM_RESTART_CONTRACT_BUILT`,
`SUPERVISOR_PM_RESTART_IPC_DEFERRED_NO_PM_CLIENT`,
`SUPERVISOR_TIMER_ENDPOINT_DEFERRED`, and
`SUPERVISOR_BACKOFF_LOGICAL_TICK_ONLY` until the live PM client and timer source
exist.

## SUP-4 PM-side restart validation/accounting oracle

SUP-4 defines the PM side of the future restart contract as an inert oracle. It
adds PM-local descriptors for validation, accounting/rollback planning, and reply
construction without changing global IPC ABI or adding live PM restart behavior.
PM validation owns verified supervisor identity checks, scoped-token ownership,
target existence, attempt limits, reason policy, dependency blockers, resource
preflight, startup-cap layout support, rollback support, and fail-closed policy.
PM accounting models only descriptive reservations and reverse-order rollback
steps. The future opcode names remain documentation-only; no `yarm-ipc-abi`
constant is added at this stage.

## SUP-5 restart IPC ABI RFC guardrails

SUP-5 adds the reviewed global PM restart IPC ABI as RFC text only. Proposed names
`PROC_OP_PM_RESTART_V1` and `PROC_OP_PM_RESTART_REPLY_V1` remain absent from the
production global IPC ABI, syscall count remains unchanged, and source guardrails
continue to require redacted token refs, no dependent-token fallback, no live PM
IPC/restart/spawn/teardown/cap/resource calls in model regions, and deferred
production markers until live ABI approval.

## SUP-6 live implementation review matrix

SUP-6 remains design/test-artifact-only. It adds the PM restart live enablement
checklist and conformance matrix, plus guardrails that keep proposed opcode names
absent from the live global IPC ABI, preserve process IPC opcode count and
`SYSCALL_COUNT`, require inert model regions, and require redacted token/dependent
token protections before future SUP-7/live work.

## SUP-7 non-dispatching codec review

SUP-7 adds review-only PM restart request/reply codecs and golden vectors behind
the test/`hosted-dev` gate. The live global IPC ABI remains unchanged, PM runtime
dispatch and supervisor send paths remain absent, process IPC opcode count remains
14, and production restart behavior remains deferred/fail-closed.

## SUP-8 ABI-review signoff package

SUP-8 freezes the non-live codec offsets/lengths, reserved-field policy, reviewer
signoff checklist, golden-vector table, and promotion guardrails. Candidate
opcodes `15`/`16` remain unallocated, the live global IPC ABI remains unchanged,
PM dispatch and supervisor send paths remain absent, and production restart remains
fail-closed/deferred.

## SUP-9 pre-live promotion dry-run

SUP-9 adds `doc/pm-restart-live-promotion-plan.md` and a dry-run readiness model.
It is not live implementation: candidate opcodes remain unallocated, global IPC
ABI and syscall ABI remain unchanged, PM dispatch and supervisor send remain
absent, and restart/spawn/teardown/cap/resource behavior remains disabled.

## SUP-10 live-readiness evidence pack

SUP-10 adds `doc/pm-restart-live-readiness-evidence.md` and a GoForAbiReview-only
source guard. It is evidence/diff planning only: live global IPC ABI, syscall ABI,
PM dispatch, supervisor send, and restart/spawn/teardown/cap/resource behavior
remain unchanged and disabled.

## SUP-11 runtime cleanup (2026-06-26)

SUP-11 is a production supervisor runtime cleanup, not the live PM restart
implementation. The runtime now keeps one restart execution path: authoritative
fault/task-exit reports schedule restart state through `handle_task_exit`, and
actual execution is considered only by the due-restart sweep. The direct
fault-handler execute-restart bypass is disabled so fault and task-exited paths
share backoff and cannot double-attempt a restart.

Production restart execution remains fail-closed because no live PM client or
restart opcode is allocated. When the due-restart sweep reaches such a record it
marks the record as `RestartBlockedNoPmClient`, leaves the task dead/degraded or
pending, and emits a single structured
`SUPERVISOR_RESTART_EXEC_DEFERRED_NO_PM_CLIENT` line with tid, service, reason,
due tick, attempt count, and state. Later loop iterations do not repeatedly call
the unavailable PM path or recreate the old multi-line deferred spam pattern.

Bad fault senders are rejected with `SUPERVISOR_FAULT_SENDER_REJECTED` but no
longer abort the live loop with `continue`; normal tick advancement and due
restart maintenance still run after rejection. Authoritative production
fault/task-exit reports are accepted only from a kernel-origin fault endpoint
sender, the registered Process Manager, or another explicit trusted lifecycle
authority. Claimed-task self-reporting is not treated as authoritative fault
origin; it must be modeled separately as health/degraded reporting if needed.

Remaining live gaps are unchanged: a real PM restart client, timer endpoint,
cap-bound restart token, and PM cleanup/rollback are still absent. SUP-11 does
not allocate live PM restart opcodes, wire supervisor-to-PM restart IPC, spawn or
tear down tasks, allocate address spaces, mint/revoke capabilities, grant
MMIO/IRQ/DMA, perform MMIO, or change syscall/global IPC ABI.

## SUP-12 mechanical restart-model extraction (2026-06-26)

SUP-12 is mechanical extraction only. The SUP-2 through SUP-10 non-live restart
contract/model/readiness definitions and helper methods are isolated in
`crates/yarm-control-plane-servers/src/control_plane/supervisor/restart_model.rs`,
which is compiled only for hosted-dev/test guardrails. The production
`service.rs` hot path keeps SUP-11 runtime state, scheduling, fault validation,
run-loop, fail-closed deferred restart markers, and runtime ops.

Production behavior remains unchanged from SUP-11: no live PM restart client,
PM dispatch, supervisor PM restart send path, restart/spawn/teardown,
capability/resource behavior, MMIO/IRQ/DMA grant, syscall ABI change, or global
IPC ABI change exists. Future live work should start at SUP-L1 rather than adding
more review-only model expansion.

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

## SUP-L3A client semantics hardening

SUP-L3A is a supervisor PM-restart client semantics hardening stage only. It does
not add PM restart execution and does not change the global process IPC ABI:
`PROC_OP_PM_RESTART_V1` remains 15, `PROC_OP_PM_RESTART_REPLY_V1` remains 16,
and `PROCESS_IPC_OPCODE_COUNT` remains 16.

The supervisor now maps PM restart client outcomes through a typed internal
result instead of generic syscall/control-plane errors. Deferred PM replies,
rejected PM replies, accepted-reply protocol violations, malformed replies,
transport send failures, missing PM clients, and request-build failures remain
separate states. Deferred/rejected replies preserve the dead/degraded service
state and do not clear restart state as success.

Supervisor PM restart request IDs are monotonic/generation-style IDs allocated by
the supervisor for each due request. Repeated requests for the same target receive
distinct request IDs, overflow fails closed before issuing a request, and a
synchronous PM reply must match both request ID and target TID before supervisor
state changes.

Request construction derives service kind and bounded service name from the
managed service record. Restart reason is derived from the scheduled event:
fault reports encode Fault, normal task exits encode NormalExit, and dependency
restarts encode DependencyFailed with `dependency_cause_tid` set to the failed
service TID. SUP-L3A continues to encode only a scoped/redacted token descriptor;
raw tokens and local CapIds are not encoded as restart authority, missing tokens
prevent sends, and live capability-bound restart authority remains SUP-L4-or-later
work.

Reply validation is fail-closed: malformed replies, mismatched request IDs,
mismatched target TIDs, Accepted status, or nonzero replacement handles on
Deferred/Rejected replies are protocol violations or malformed outcomes. Valid PM
requests still defer with mechanism unavailable; no Accepted success path or
replacement handle is honored in SUP-L3A.

The due-restart runtime state machine is explicit and non-overlapping:
PendingDue, BlockedNoPmClient, PmDeferred, PmRejected, PmClientSendFailed,
ProtocolViolation, and AwaitingMechanismUnavailable. PM-deferred/rejected states
avoid busy-loop resends and preserve fail-closed behavior until a later staged
mechanism is implemented.

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
