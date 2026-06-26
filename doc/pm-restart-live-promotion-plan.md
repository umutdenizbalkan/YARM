<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Umut Deniz Balkan -->

# SUP-9 PM restart live-promotion dry-run plan

SUP-9 is a **planning and readiness dry-run only**. It does not allocate live
opcodes, does not change `yarm-ipc-abi`, does not wire PM dispatch, does not wire
a supervisor PM restart send path, and does not perform restart, spawn, teardown,
capability, address-space, MMIO, or resource-cleanup operations.

The future live stage must satisfy this plan together with the SUP-8 signoff
package before any runtime path is enabled.

## Future SUP-live promotion sequence

### 1. ABI approval

1. Approve numeric opcode allocation for the request and reply opcodes. SUP-9
   keeps allocated values `15` and `16` explicitly reserved.
2. Re-approve Request V1 at 110 bytes and Reply V1 at 50 bytes from
   `doc/process-manager-restart-contract.md`.
3. Promote the reviewed codec into the global process IPC ABI source only after
   the signoff checklist is complete.
4. Update the process IPC opcode count in the same PR that adds the live opcode.
5. Add unsupported-version and nonzero-reserved decode tests in the live ABI test
   suite before enabling dispatch.

### 2. PM dispatch wiring

1. Add a PM dispatch arm for the approved request opcode.
2. Decode using the checked Request V1 codec; malformed, unsupported-version,
   invalid-enum, raw-token, and nonzero-reserved inputs must fail closed.
3. Use verified IPC sender metadata; never trust `supervisor_tid` from payload as
   authority.
4. Call the PM validation/accounting path before any irreversible mechanism step.
5. Return a checked Reply V1 payload for accepted, rejected, deferred,
   unsupported-version, already-restarting, no-such-target, and rolled-back cases.

### 3. Supervisor PM client wiring

1. Build the request from the SUP-2/SUP-3 supervisor model state.
2. Encode Request V1 only after the target has its own scoped restart-token
   authority.
3. Send to the PM endpoint only if the supervisor has a verified PM client/cap
   authority; missing authority must remain fail-closed.
4. Decode Reply V1 and update supervisor state only from the PM reply status.
5. Preserve token redaction and never log raw restart-token bytes.

### 4. PM mechanism implementation

1. Validate target existence, restart policy, and scoped token ownership.
2. Preflight resource/accounting availability.
3. Reserve replacement resources before irreversible teardown where policy
   requires it.
4. Create the replacement process using PM-owned spawn/restart mechanisms only.
5. Deliver startup caps through the approved capability-transfer/startup path.
6. Register health/fault monitoring.
7. Roll back in reverse order on injected or real failure.
8. Construct and send Reply V1 after accounting/rollback state is known.

### 5. Timer/backoff integration

1. Replace production logical-tick-only behavior with a timer endpoint or
   PM/kernel timer source.
2. Preserve the no-execute-before-due-tick rule.
3. Timer unavailable must defer, not execute immediately.
4. Keep capped/saturating backoff.
5. Rate-limit crash-loop alerts without silently dropping required failure
   notification state.

### 6. Rollout

1. Keep the live path behind an explicit feature or rollout gate until all hosted
   and QEMU evidence is attached.
2. Run hosted model, codec, dispatch, and rollback-injection tests.
3. Run x86_64 and AArch64 boot smokes.
4. Verify fail-closed fallback when PM endpoint/timer/restart authority is absent.
5. Update docs from RFC/review status to live status in the same PR that enables
   runtime dispatch.

## Promotion PR checklist

| Step | Expected future files | Tests required | Security invariant | Rollback invariant | Live-enable blocker | Acceptance evidence |
|---|---|---|---|---|---|---|
| ABI numeric allocation | `yarm-ipc-abi` / process IPC ABI source | ABI encode/decode, opcode-count update, unsupported-version | no opcode without approval | n/a | missing signoff | reviewed ABI diff |
| PM dispatch | PM service dispatch | dispatch decode/reject/accept tests | verified sender, no payload TID trust | no mechanism before validation | missing sender metadata | hosted PM dispatch tests |
| Supervisor PM client | supervisor PM client/runtime path | missing-endpoint fail-closed, reply handling | only verified PM endpoint, no raw token logs | state updates only from reply | endpoint/cap absent | hosted supervisor client tests |
| PM accounting/rollback | PM accounting/restart implementation | reservation and rollback injection tests | PM owns resource authority | reverse-order rollback | rollback gap | rollback report artifacts |
| Timer/backoff | timer endpoint or PM/kernel timer integration | no-execute-before-due, timer-unavailable defer | crash-loop throttling | restart stays deferred on timer failure | no timer policy | timer tests and logs |
| Docs/status | contract, checklist, audit docs | doc guard tests | authority boundary documented | rollback behavior documented | stale RFC status | docs PR diff |
| QEMU smokes | smoke scripts if needed | x86_64 and AArch64 normal/restart/reject/rollback smokes | wrong sender/token rejected | injected failures cleaned/degraded | smoke failure | attached logs |

## Dry-run readiness model

The SUP-9 readiness model is intentionally conservative. A complete review package
returns `ReadyForReviewOnly`, never `ReadyForLive`, while live prerequisites remain
absent. Missing artifacts are reported explicitly.

The readiness checks are:

- SUP-8 frozen Request V1 size is documented.
- SUP-8 frozen Reply V1 size is documented.
- SUP-6 conformance matrix exists.
- SUP-8 reserved-field policy exists.
- codec golden vectors exist.
- candidate opcodes are documented as unallocated.
- live ABI opcodes remain absent.
- PM dispatch remains absent.
- supervisor runtime fail-closed/no-PM-client marker remains present.

## Future rollback-injection test plan

| Injection point | Expected PM reply | Expected supervisor state | Expected PM rollback steps | Expected logs/markers | Old task state | Replacement cleanup |
|---|---|---|---|---|---|---|
| after opcode decode | `Rejected` / decode failure | unchanged/degraded if already dead | none | decode failure marker | unchanged | none |
| after sender validation | `Rejected/MissingRight` | unchanged | none | untrusted sender marker | unchanged | none |
| after resource preflight | `Deferred/ResourceUnavailable` | restart pending/deferred | none | resource unavailable marker | dead/degraded | none |
| after replacement task reservation | `RolledBack/RollbackFailed` if cleanup fails, else `Deferred`/failure | dead/degraded | release replacement slot | rollback marker | dead/degraded | replacement slot released |
| after address-space reservation | `RolledBack` or `Deferred` | dead/degraded | release address-space, replacement slot | rollback marker | dead/degraded | AS and slot released |
| after CNode/startup-cap reservation | `RolledBack` or `Deferred` | dead/degraded | release CNode/startup-cap, AS, replacement slot | cap rollback marker | dead/degraded | partial caps revoked/released |
| after health-monitor registration | `RolledBack` or `Deferred` | dead/degraded | unregister health monitor, release caps/AS/slot | health rollback marker | dead/degraded | monitor and replacement cleaned |
| after reply construction | reply retry or supervisor-visible deferred error | state not marked restarted without accepted reply | roll back if reply cannot be delivered under policy | reply failure marker | dead/degraded unless accepted earlier | policy cleanup |
| while notifying supervisor/init | accepted/deferred plus alert failure marker | degraded/alert pending | no extra mechanism rollback unless policy requires | init/supervisor alert unavailable | policy-defined | replacement kept only if accepted |

## Future QEMU acceptance plan

Before live enablement, collect and attach smoke evidence for:

- x86_64 normal boot unchanged;
- AArch64 normal boot unchanged;
- supervisor restart request accepted path;
- wrong sender rejected path;
- wrong token rejected path;
- rollback-injection path;
- timer-unavailable deferred path;
- crash-loop rate-limit path.

## SUP-9 status

SUP-9 creates this promotion plan and dry-run readiness model only. Future live
work must explicitly satisfy SUP-8 and SUP-9 checklists before enabling any
runtime path. PM remains the executor, supervisor remains the requestor, and the
kernel remains the low-level mechanism provider.

## SUP-10 evidence pack link

SUP-10 adds `doc/pm-restart-live-readiness-evidence.md` with the live-readiness
evidence matrix, go/no-go report model, and exact future diff plan. SUP-10 is not
live implementation and does not allocate opcodes, dispatch PM restart requests,
or enable supervisor PM restart sends.

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
