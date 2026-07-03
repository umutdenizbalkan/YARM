<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Umut Deniz Balkan -->

# Process Manager restart contract plan (SUP-4)

SUP-4 is PM-side design/model-only. It does not add a global IPC ABI opcode, does
not send or receive a live restart IPC path, does not restart/spawn/tear down
any task, does not mint/revoke capabilities, does not allocate address spaces,
does not grant MMIO/IRQ/DMA, and does not perform MMIO.

## Current PM restart/spawn authority audit

| Question | Audit result |
|---|---|
| Existing spawn/restart mechanisms | PM already has production spawn paths for `SpawnV2`/`SpawnV5Cap` and runtime `spawn_process` / `spawn_process_with_startup_caps` wrappers. It also has `PROC_OP_EXECUTE_RESTART`, but that path is limited and returns unsupported unless a restart capability path is available. |
| Restart-token checks | PM stores `(tid, restart_token)` records via `PROC_OP_REGISTER_SUPERVISED_TASK`, serves token lookups, and checks token equality before `ExecuteRestart`. Tokens are still raw in the existing ABI and are not the future scoped-token contract. |
| Sender identity validation | Existing PM request decoding records `Message.sender_tid` for some requests and validates wrong-sender SpawnV5 replies in tests, but the legacy restart-token/register/execute path is not the final verified-supervisor restart authority model. |
| PM owns process creation | Yes. PM owns process creation/replacement mechanism boundaries; supervisor must remain requestor/advisor and must not spawn/restart directly. |
| Rollback/accounting primitives | PM has descriptive validation/accounting patterns in driver-manager-facing tests, but no live restart-specific rollback implementation. SUP-4 adds an inert PM restart accounting/rollback oracle only. |
| Restart-like opcode today | `PROC_OP_EXECUTE_RESTART` exists today. SUP-4 does not extend it or add a new opcode. The future opcode plan below is intentionally documentation/model-only. |
| Production-live vs test-only | Spawn paths are production-live. Many lifecycle/accounting assertions are hosted-test scaffolding. SUP-4 PM restart validation/accounting/reply descriptors are inert model code. |
| Unsafe/legacy not to reuse | Do not reuse raw/unscoped restart tokens, fake success paths, supervisor-side execution, direct cap IDs in payloads, or unsupported kernel-cap restart shortcuts as the future contract. |

## PM-side inert validation model

The PM model defines bounded descriptors: `PmRestartRequestDescriptor`,
`PmRestartValidationReport`, `PmRestartValidationEntry`,
`PmRestartValidationStatus`, `PmRestartValidationFailure`,
`PmRestartValidationPolicy`, `PmRestartAuthority`, `PmRestartTokenCheck`, and
`PmRestartSenderCheck`.

Validation checks request version, verified supervisor sender identity, restart
authority, target existence, scoped token ownership, attempt limits, reason policy,
already-restarting/running state, dependency blockers, resource preflight,
startup-cap layout support, rollback support, and fail-closed policy. Outcomes are
`WouldAccept`, `WouldReject`, `WouldDefer`, `UnsupportedVersion`,
`AlreadyRestarting`, and `NoSuchTarget`.

## PM-side accounting and rollback plan

`PmRestartAccountingPlan` is descriptive only. Reservations model old-task teardown,
replacement task slot, address-space slot, CNode/startup-cap slots, inherited
service caps, fault endpoint/restart-monitor slot, PM handle slot, and
init/supervisor notification slot.

Failure injection after replacement-task or startup-cap reservations produces
reverse-order rollback descriptors. The old task remains dead/degraded according
to policy; replacement partial state is described only. No task, address space,
capability, or resource is created or destroyed.

## Future reply descriptor

`PmRestartReplyDescriptor` maps inert validation/accounting into future reply
statuses: `Accepted`, `Rejected`, `Deferred`, `RolledBack`, `UnsupportedVersion`,
`AlreadyRestarting`, and `NoSuchTarget`. Replies include request ID, target task,
mock replacement handle on accepted requests, cleanup/accounting/startup-cap/
health-monitor status strings, rollback summary, failure reason, and retry tick
when deferred.

## Future opcode/ABI plan (not implemented)

Future live work may introduce names such as `PROC_OP_PM_RESTART_V1` and
`PROC_OP_PM_RESTART_REPLY_V1`, but SUP-4 deliberately does not add them to
`yarm-ipc-abi`. The future payload should be fixed-size/bounded, versioned, require
verified supervisor sender identity, use scoped/redacted token references or
capability-bound token authority, avoid cap IDs as payload authority, define
stable failure codes, require PM-owned rollback semantics, and include a
compatibility plan for unsupported versions.

## SUP-5 global PM restart IPC ABI RFC (proposed, not live)

Status: **proposed / RFC-only**. The proposed request opcode name is
`PROC_OP_PM_RESTART_V1`; the proposed reply opcode name is
`PROC_OP_PM_RESTART_REPLY_V1`. SUP-5 does not add these names to
`yarm-ipc-abi`, does not allocate numeric opcode values, and does not wire PM or
supervisor runtime dispatch. Future live work requires explicit ABI approval.

The future ABI must be fixed-size and bounded. Every request carries a version
field and every reply must reject unsupported versions with an explicit
unsupported-version status rather than interpreting unknown payloads. The PM must
trust verified IPC sender metadata and endpoint/cap authority, never the
`supervisor_tid` payload field alone.

### Proposed request payload layout

| Field | Type | Meaning |
|---|---:|---|
| `version` | `u16` | Contract version; first proposed value is `1`. |
| `request_id` | `u64` | Supervisor-chosen correlation ID; not authority. |
| `supervisor_tid` | `u64` | Informational sender hint; PM must verify sender metadata separately. |
| `target_tid` | `u64` | Service/task PM is asked to restart. |
| `service_kind` | `u16` | Bounded service-kind enum agreed by ABI review. |
| `service_name_len` | `u8` | Length of bounded service-name bytes. |
| `service_name_bytes` | `[u8; 32]` | UTF-8/debug name only; not authority. |
| `restart_reason` | `u16` | Fault, normal-exit policy, crash loop, dependency failure, manual policy, or health timeout. |
| `attempt_count` | `u16` | Supervisor-observed attempt number for policy validation. |
| `due_tick` | `u64` | Monotonic supervisor tick at which restart became due. |
| `dependency_cause_tid` | `u64` | Failed dependency TID, or `0` when not dependency-caused. |
| `degraded_hint` | `u8` | Non-authoritative hint that supervisor marked the service degraded. |
| `policy_flags` | `u32` | Bounded flags for restart/no-duplicate/rate-limit policy. |
| `token_descriptor` | fixed descriptor | Scoped/capability-bound restart-token authority; raw tokens are invalid. |
| `startup_cap_policy` | fixed descriptor | Startup-cap delivery policy requested from PM. |
| `rollback_policy` | fixed descriptor | Required PM rollback behavior if replacement fails. |
| `health_monitor_policy` | fixed descriptor | Health registration/timeout policy after replacement. |

The request payload is a request for PM-owned mechanism only. It must not contain
local CapIds as transferable authority, process handles, address-space handles, or
MMIO/IRQ/DMA grants.

### Proposed reply payload layout

| Field | Type | Meaning |
|---|---:|---|
| `version` | `u16` | Reply contract version. |
| `request_id` | `u64` | Echo of the request ID. |
| `target_tid` | `u64` | Target service/task for the reply. |
| `status` | `u16` | Accepted, rejected, deferred, rolled-back, unsupported-version, already-restarting, or no-such-target. |
| `failure` | `u16` | Stable failure code when status is not accepted. |
| `replacement_handle_kind` | `u16` | Kind of PM-scoped replacement handle, or none. |
| `replacement_handle_value` | `u64` | PM-scoped opaque handle value; not a CapId and not a raw TID. |
| `cleanup_status` | `u16` | Old-task cleanup/teardown result descriptor. |
| `accounting_status` | `u16` | Resource-accounting result descriptor. |
| `startup_cap_status` | `u16` | Startup-cap delivery result descriptor. |
| `health_monitor_status` | `u16` | Health-monitor registration result descriptor. |
| `rollback_status` | `u16` | Rollback result when replacement partially failed. |
| `next_retry_tick` | `u64` | PM-requested retry tick when deferred, or `0`. |

Reply failure codes must distinguish missing restart authority, untrusted sender,
unsupported version, no such target, wrong token owner, raw/unscoped token,
restart-limit exceeded, duplicate running restart, dependency blocker, resource
preflight unavailable, startup-cap layout unsupported, rollback unsupported, and
fail-closed policy.

### Scoped / capability-bound token authority

Raw restart tokens are legacy and are not accepted by this future ABI. The future
token descriptor must be scoped to the target service/task and bound either to the
verified supervisor authority or to a capability-like authority validated by PM. PM
must not accept token authority from payload bytes alone. Verified sender identity
is mandatory, the token target must match `target_tid`, and the token cannot be
reused for dependents. A dependent restart requires that dependent service's own
token. Logs must use only redacted token references/fingerprints; raw, missing,
unscoped, or unsupported tokens fail closed.

### Verified supervisor endpoint authority

PM identifies the supervisor through verified IPC sender metadata plus the endpoint
or capability grant that carries restart-request authority. Arbitrary tasks that
can reach a PM endpoint are not restart authorities. PM must distinguish init,
supervisor, driver_manager, and ordinary services by verified identity and explicit
right, not by a claimed payload TID. Unknown or unauthorized senders reject with
`MissingRight`/`UntrustedSender` semantics and no restart side effects.

### PM rollback and accounting invariants

PM must validate request authority, target existence, token ownership, policy, and
resource preflight before teardown or replacement. Where policy requires preserving
service availability, PM should reserve replacement resources before irreversible
teardown. Any partial replacement failure rolls back in reverse reservation order:
replacement partial state, CNode/startup-cap state, address-space state, inherited
service caps, fault endpoint/restart monitor state, health-monitor state, and
notification state. The old task remains dead or degraded according to PM policy;
PM, not supervisor, owns replacement cleanup and resource reclamation. PM must alert
init/supervisor after rollback rather than reporting restart success.

### Timer, backoff, and crash-loop semantics

Supervisor logical ticks are the current placeholder and are not wall-clock time.
A live ABI user must pair restart execution with a real timer endpoint or PM/kernel
timer source. PM must not execute a restart before `due_tick`; timer unavailability
defers restart rather than executing immediately. Backoff must saturate or cap
instead of wrapping, crash-loop alerting must be rate limited, and PM may reject or
defer requests that arrive too frequently. The supervisor keeps the service
degraded until PM accepts the restart or policy gives up.

### Source guardrail expectations

Until the live ABI is explicitly approved, source tests must keep proving that
`PROC_OP_PM_RESTART_V1` and `PROC_OP_PM_RESTART_REPLY_V1` are absent from the
global IPC ABI, syscall count remains unchanged, model code does not call live
restart/spawn/teardown/cap/resource operations, token logging stays redacted, and
the production runtime retains deferred/no-PM-client markers.

## Deferred live work

Live PM restart requires a new ABI review, verified supervisor endpoint authority,
capability-bound token validation, PM-owned teardown/replacement/resource
accounting, rollback implementation, startup-cap delivery, health-monitor
registration, and reply delivery. None are implemented by SUP-4/SUP-5/SUP-6/SUP-7.

## SUP-6 live implementation checklist link

SUP-6 adds `doc/pm-restart-live-implementation-checklist.md` as the review
checklist and conformance matrix for future live PM restart work. It is not live
implementation: proposed opcode names remain unallocated, global IPC ABI remains
unchanged, and future SUP-7/live work must pass the checklist before enabling any
runtime restart path.

## SUP-7 non-dispatching codec review artifacts

SUP-7 adds fixed-size request/reply codec structs and little-endian encode/decode
helpers in `crates/yarm-control-plane-servers/src/control_plane/process_manager/restart_abi_review.rs`.
The module is review-only, compiled behind the test/`hosted-dev` gate, and is not
referenced by PM runtime dispatch. Future live implementation must explicitly
promote the reviewed codec into `yarm-ipc-abi`, assign numeric opcodes, and add
dispatch in a separate approved live-ABI PR.

The request codec mirrors the SUP-5 request layout with bounded 32-byte service
name storage, scoped/redacted token descriptor, restart reason, attempt/backoff
fields, dependency cause, degraded hint, and startup-cap/rollback/health policy
descriptors. Decode rejects malformed length, unsupported version, invalid enum
values, oversized names, raw/unscoped tokens, and nonzero reserved fields.

The reply codec mirrors the SUP-5 reply layout with accepted/rejected/deferred/
rolled-back/unsupported/already-restarting/no-such-target statuses, failure code,
mock replacement-handle descriptor fields, accounting/status descriptors, rollback
status, and retry tick. Golden vectors cover valid request, accepted reply, wrong
token rejection, timer-unavailable deferral, rolled-back reply, and unsupported
version reply.

Candidate opcode names remain `PROC_OP_PM_RESTART_V1` and
`PROC_OP_PM_RESTART_REPLY_V1`. ABI-review candidate numeric values are `15` for
the request and `16` for the reply because the live process IPC opcode count is
currently 14, but these numbers are **not allocated**, are not present in
`yarm-ipc-abi`, and remain absent from PM/supervisor runtime dispatch.

## SUP-8 ABI-review signoff package

SUP-8 freezes the review codec layout for signoff, but it is still non-live. The
codec remains in `restart_abi_review.rs`, allocated opcodes `15`/`16` now
unallocated, no `yarm-ipc-abi` constants exist for them, and no PM dispatch or
supervisor send path exists. Promotion requires an explicit future SUP-live stage
that updates the global ABI, dispatch, smoke evidence, and this documentation in
one reviewed PR.

### Request V1 frozen layout

| Field | Offset | Length | Type / endian | Valid range | Rejection behavior | Status | Authority-bearing? |
|---|---:|---:|---|---|---|---|---|
| `version` | 0 | 2 | `u16` LE | `1` | reject `UnsupportedVersion` | current | descriptive |
| `request_id` | 2 | 8 | `u64` LE | any | none; correlation only | current | no |
| `supervisor_tid` | 10 | 8 | `u64` LE | any; verified externally | payload never trusted as authority | current | descriptive only |
| `target_tid` | 18 | 8 | `u64` LE | known PM target | PM rejects unknown target | current | identifies target, not authority |
| `service_kind` | 26 | 2 | `u16` LE | reviewed enum values | future live invalid enum rejects | current | descriptive |
| `service_name_len` | 28 | 1 | `u8` | `0..=32` | reject `OversizedServiceName` | current | no |
| `service_name` | 29 | 32 | bytes | first `service_name_len` bytes meaningful | decode is deterministic; name is debug only | current | no |
| `restart_reason` | 61 | 2 | `u16` LE | `1..=6` | reject `InvalidEnum` | current | policy input |
| `attempt_count` | 63 | 2 | `u16` LE | policy-bounded | PM rejects over limit | current | policy input |
| `due_tick` | 65 | 8 | `u64` LE | monotonic tick domain | PM/supervisor defer if timer unavailable | current | no |
| `dependency_cause_tid` | 73 | 8 | `u64` LE | `0` or known TID | PM may reject/defer blocker | current | no |
| `degraded_hint` | 81 | 1 | `u8` bool | `0` or `1` | future live invalid bool rejects | current | descriptive |
| `policy_flags` | 82 | 4 | `u32` LE | review flags; ignore-safe only when documented | unknown authority-bearing flags must fail closed in live ABI | future-policy | descriptive only |
| `token.owner_tid` | 86 | 8 | `u64` LE | must equal target for scoped token | reject wrong owner | current | token descriptor input |
| `token.redacted_fingerprint` | 94 | 2 | `u16` LE | redacted fingerprint only | raw token material is not accepted | current | no raw authority |
| `token.scoped` | 96 | 1 | `u8` bool | must be `1` | reject `RawOrUnscopedToken` | current | authority hint checked by PM state |
| `token.reserved` | 97 | 1 | reserved byte | must be `0` | reject `NonzeroReserved` | reserved | no |
| `startup_cap_policy` | 98 | 4 | `u32` LE | reviewed descriptor | unsupported layout rejects | future-policy | descriptive |
| `rollback_policy` | 102 | 4 | `u32` LE | reviewed descriptor | unsupported rollback rejects | future-policy | descriptive |
| `health_monitor_policy` | 106 | 4 | `u32` LE | reviewed descriptor | unsupported policy rejects/defer | future-policy | descriptive |

Request V1 total length is frozen at 110 bytes.

### Reply V1 frozen layout

| Field | Offset | Length | Type / endian | Valid range | Rejection behavior | Status | Authority-bearing? |
|---|---:|---:|---|---|---|---|---|
| `version` | 0 | 2 | `u16` LE | `1` | reject `UnsupportedVersion` | current | no |
| `request_id` | 2 | 8 | `u64` LE | any | correlation only | current | no |
| `target_tid` | 10 | 8 | `u64` LE | requested target | mismatch rejected by caller policy | current | identifies target |
| `status` | 18 | 2 | `u16` LE | `1..=7` | reject `InvalidEnum` | current | result descriptor |
| `failure` | 20 | 2 | `u16` LE | `0..=10` | reject `InvalidEnum` | current | result descriptor |
| `replacement_handle_kind` | 22 | 2 | `u16` LE | reviewed PM handle kind | unsupported kind rejects in live caller | future-policy | PM-scoped descriptor only |
| `replacement_handle_value` | 24 | 8 | `u64` LE | opaque PM handle | not a CapId/TID authority | future-policy | PM-scoped descriptor only |
| `cleanup_status` | 32 | 2 | `u16` LE | reviewed status | unknown rejects/fails closed in live caller | future-policy | descriptive |
| `accounting_status` | 34 | 2 | `u16` LE | reviewed status | unknown rejects/fails closed in live caller | future-policy | descriptive |
| `startup_cap_status` | 36 | 2 | `u16` LE | reviewed status | unknown rejects/fails closed in live caller | future-policy | descriptive |
| `health_monitor_status` | 38 | 2 | `u16` LE | reviewed status | unknown rejects/fails closed in live caller | future-policy | descriptive |
| `rollback_status` | 40 | 2 | `u16` LE | reviewed status | unknown rejects/fails closed in live caller | future-policy | descriptive |
| `next_retry_tick` | 42 | 8 | `u64` LE | `0` or monotonic retry tick | caller defers, never executes early | current | no |

Reply V1 total length is frozen at 50 bytes. Reply V1 currently has no reserved
bytes; any future reserved field requires a version bump or explicit compatibility
rule.

### Reserved field and flag policy

All reserved bytes must encode as zero, and decode must reject nonzero reserved
bytes. Unknown enum values fail closed. Unknown flags fail closed unless the
signoff table marks them ignore-safe and documents why they are not authority.
Future extension requires a version bump or an explicit compatibility rule.
Candidate opcode values `15`/`16` remain unallocated until live ABI approval.

### Reviewer signoff checklist

SUP-L1 promotes this codec into `yarm-ipc-abi`; runtime dispatch remains disabled until every
item below is explicitly signed off:

- [ ] opcode numeric allocation;
- [ ] wire layout offsets and byte lengths;
- [ ] little-endian encoding;
- [ ] bounded string behavior;
- [ ] enum/status/failure behavior;
- [ ] reserved fields and future-use policy;
- [ ] token authority model;
- [ ] verified supervisor sender model;
- [ ] PM rollback/accounting invariants;
- [ ] timer/backoff semantics;
- [ ] unsupported-version behavior;
- [ ] QEMU x86_64 and AArch64 boot smoke results;
- [ ] rollback injection tests;
- [ ] security review.

### Golden-vector signoff table

| Vector | Payload kind | Length | Status/failure | Matrix row | Valid/malformed | Expected decode |
|---|---|---:|---|---|---|---|
| valid restart request | request | 110 | n/a | valid supervisor request | valid | request roundtrip |
| accepted reply | reply | 50 | `Accepted/None` | valid supervisor request | valid | reply roundtrip |
| untrusted sender reply | reply | 50 | `Rejected/MissingRight` | untrusted sender | valid | reply roundtrip |
| wrong-token reply | reply | 50 | `Rejected/WrongTokenOwner` | wrong token owner | valid | reply roundtrip |
| raw-token reply | reply | 50 | `Rejected/RawTokenUnsupported` | raw token | valid | reply roundtrip |
| unknown-target reply | reply | 50 | `NoSuchTarget/None` | unknown target | valid | reply roundtrip |
| restart-limit reply | reply | 50 | `Rejected/RestartLimitExceeded` | restart limit exceeded | valid | reply roundtrip |
| dependency-blocker reply | reply | 50 | `Deferred/DependencyBlocked` | dependency blocker | valid | reply roundtrip |
| resource-unavailable reply | reply | 50 | `Deferred/ResourceUnavailable` | resource unavailable | valid | reply roundtrip |
| startup-cap unsupported | PM oracle | n/a | `Rejected/StartupCapLayoutUnsupported` | startup-cap unsupported | valid oracle | oracle rejects |
| rollback-failure reply | reply | 50 | `RolledBack/RollbackFailed` | rollback failure | valid | reply roundtrip |
| unsupported-version reply | reply | 50 | `UnsupportedVersion/UnsupportedVersion` | unsupported version | valid | reply roundtrip |
| timer-unavailable reply | reply | 50 | `Deferred/TimerUnavailable` | timer unavailable | valid | reply roundtrip |
| already-restarting reply | reply | 50 | `AlreadyRestarting/None` | already restarting | valid | reply roundtrip |
| already-running duplicate | PM oracle | n/a | `Rejected/DuplicateRunningRestart` | already running duplicate | valid oracle | oracle rejects |
| rollback alert delivery | supervisor model | n/a | rolled-back alert/degraded | rollback alert delivery | valid model | alert/degraded state |
| truncated request | request | `<110` | n/a | malformed input | malformed | `Malformed` |
| invalid enum request | request | 110 | n/a | malformed input | malformed | `InvalidEnum` |
| raw/unscoped token request | request | 110 | n/a | raw token | malformed | `RawOrUnscopedToken` |
| nonzero reserved request | request | 110 | n/a | reserved policy | malformed | `NonzeroReserved` |

### Conformance matrix completeness

Every SUP-6 row now has one of: a SUP-7 golden vector, an existing PM oracle test,
an existing supervisor model test, or a documented future live test. Startup-cap
unsupported, already-running duplicate, and rollback alert delivery remain covered
by oracle/model rows rather than wire-only vectors because live dispatch is still
disabled.

## SUP-9 pre-live promotion dry-run

SUP-9 adds `doc/pm-restart-live-promotion-plan.md` as a planning-only promotion
sequence for a future live stage. It does not allocate opcodes, update
`yarm-ipc-abi`, add PM dispatch, add supervisor send, or perform restart/spawn/
teardown/cap/resource work. Future live work must satisfy both the SUP-8 signoff
package and the SUP-9 promotion checklist before enabling runtime dispatch.

## SUP-10 live-readiness evidence pack

SUP-10 adds `doc/pm-restart-live-readiness-evidence.md` as evidence and exact
future-diff planning only. It does not enable live ABI/runtime behavior; future
SUP-live work must explicitly change status from proposed/review-only to live and
satisfy the SUP-8, SUP-9, and SUP-10 checklists before adding opcodes or dispatch.

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

### SUP-L6D gated image_id 13 spawn policy fix

SUP-L6C runtime evidence proved `crash_test_srv` reaches PM as valid ELF bytes (`len = 16744`, first4 ELF magic). SUP-L6D therefore treats the first concrete blocker as the PM/kernel spawn policy boundary: kernel `spawn_from_memory_object` and user-buffer spawn consult the fixed kernel image-path policy table, which still rejects image_id 13 with `InvalidArgs` because kernel/arch changes are out of scope for the crash-test rollout.

The PM fix is deliberately narrow and test-gated. For image_id 13 only when the supervisor restart-test gate is enabled, PM logs `PM_SPAWN_FROM_MO_ENTER image_id=13` and `PM_SPAWN_FROM_MO_POLICY image_id=13 allowed=1 reason=restart-test-gate`, skips the MemoryObject path whose kernel policy table cannot name image_id 13, and uses the already-loaded crash-test ELF bytes with the existing PM-owned VFS-backed user-buffer spawn path. The syscall compatibility image-id used for the kernel path-policy label remains bounded and internal; PM lifecycle/restart metadata still records the original crash-test image_id 13. Gate-off image_id 13 remains rejected, and the production 7..=12 VFS range is not broadened.

Failure reporting is also tightened: `InvalidArgs` from the spawn syscall is no longer collapsed into an unqualified `TableFull`, and crash-test failures emit `PM_SPAWN_FROM_MO_FAIL_DETAIL` plus `PM_SPAWN_FROM_MO_TABLE_STATS` for PM lifecycle capacity. The SUP-L6 marker-count oracle remains unchanged and must still prove exactly four crash-test entries, three accepted PM restarts, three supervisor state updates, and one final degraded/give-up marker.

### SUP-L6E crash-test supervisor registration send-cap blocker

SUP-L6D moved the runtime proof past VFS/ELF/image_id 13 spawn: PM now records `PM_CRASH_TEST_SPAWN_OK`, lifecycle metadata, and the scoped restart-token descriptor, and the crash-test service reaches userspace. SUP-L6E identifies the next blocker as registration delivery from init to supervisor: init's runtime startup context does not currently contain `supervisor_control_send_ep`, so the crash-test path emits `INIT_SUPERVISOR_CONTROL_SEND_CAP_MISSING reason=startup-slot-empty` and `INIT_CRASH_TEST_REGISTER_FAIL tid=<tid> reason=no-supervisor-send-cap` instead of sending a registration.

The code now distinguishes init-side send-cap extraction from supervisor-side acceptance. If the existing supervisor control SEND cap is present, init logs `INIT_SUPERVISOR_CONTROL_SEND_CAP_PRESENT`, sends the existing `RegisterDriverRequest` through that cap, and emits `INIT_CRASH_TEST_REGISTER_OK` only for local IPC send success. Supervisor emits `SUPERVISOR_CRASH_TEST_REGISTER_BEGIN`, `SUPERVISOR_CRASH_TEST_REGISTER_OK`, `SUPERVISOR_CRASH_TEST_POLICY`, and `SUPERVISOR_CRASH_TEST_RESTART_TOKEN_READY` only after its control receive path accepts the registration. No new cap slot, endpoint, CNode slot, or bootstrap capability is introduced in SUP-L6E.

The remaining blocker, if the missing-cap marker persists in QEMU, is a startup-handoff gap for the existing supervisor control SEND cap. The next task must fix that handoff using an already-defined capability path or explicitly report `MissingSupervisorControlSendCap`; it must not add kernel/bootstrap slots in this stage. The SUP-L6 marker-count oracle remains unchanged.

### SUP-L6F supervisor control SEND startup handoff audit

SUP-L6F confirms the crash-test registration blocker is the existing supervisor-control SEND startup handoff into init. The startup ABI already defines `STARTUP_SLOT_SUPERVISOR_CONTROL_SEND_EP` and `StartupContext::supervisor_control_send_ep`; init now logs the raw slot value with `INIT_STARTUP_SLOT_SUPERVISOR_CONTROL_SEND raw=<n>` before decoding it. A raw zero value is reported as `INIT_SUPERVISOR_CONTROL_SEND_CAP_MISSING reason=zero` and `reason=startup-slot-empty`; a nonzero value that still fails `StartupContext` decoding is reported as `reason=decode`.

No new startup slot, endpoint, CNode slot, or fabricated cap is introduced. If QEMU continues to show raw zero, the exact deferred blocker is production/bootstrap provisioning of the already-defined supervisor control SEND cap into init's existing startup slot. Fixing that requires the existing startup handoff producer; SUP-L6F does not broaden restart support, weaken the oracle, or fake registration success.

### SUP-L6G startup slot 4 provisioning audit

SUP-L6G confirms the real QEMU startup payload still leaves init's existing `STARTUP_SLOT_SUPERVISOR_CONTROL_SEND_EP` raw value at zero. The existing startup ABI and init decode path are correct, and the architecture boot paths already create the supervisor control endpoint and populate `sup_args[4]` for the supervisor process. The missing production handoff is that those same architecture boot paths do not populate `init_args[4]` for init, so init cannot send the crash-test registration and correctly fails closed with `INIT_CRASH_TEST_REGISTER_FAIL tid=<tid> reason=no-supervisor-send-cap`.

No new startup slot, endpoint, CNode slot, raw cap number, or fabricated cap is added. Because the only production writer of these initial QEMU startup arguments is under `src/arch/*/boot.rs` and this stage is constrained not to touch `src/arch/`, SUP-L6G precisely defers the fix: the existing supervisor control SEND cap must be granted to init and written to init startup slot 4 by the existing architecture bootstrap/startup-cap provisioning path. The SUP-L6 marker-count oracle remains unchanged, and restart execution is still blocked until registration is delivered and accepted.

### SUP-L6H init-local supervisor control SEND slot provisioning

SUP-L6H completes the already-defined startup slot 4 provisioning. The architecture bootstrap paths that create the existing supervisor control endpoint now grant a distinct SEND capability for that endpoint into init's CNode and write that init-local cap ID to `init_args[4]` (`STARTUP_SLOT_SUPERVISOR_CONTROL_SEND_EP`). This is not a raw copy of the supervisor's slot-4 cap: supervisor still receives its own SEND cap in `sup_args[4]`, and init receives a separately granted local SEND cap to the same endpoint.

The minimal fix is applied consistently in `src/arch/x86_64/boot.rs`, `src/arch/aarch64/boot.rs`, and `src/arch/riscv64/boot.rs` because all three production bootstrap paths shared the same supervisor-control endpoint/startup contract. No startup slot, syscall, IPC opcode, endpoint, CNode layout, driver resource grant, restart policy, or SUP-L6 marker-count oracle is changed. The next QEMU run should move from `INIT_STARTUP_SLOT_SUPERVISOR_CONTROL_SEND raw=0` to a nonzero raw slot and then test whether init registration reaches supervisor before the crash-test fault.

### SUP-L6I supervisor control opcode 0 normalization

SUP-L6H fixed the startup slot 4 handoff: init now receives an init-local supervisor-control SEND cap and `ipc_send` succeeds. SUP-L6I identifies the next blocker as inline IPC framing on the supervisor control receive path: init built the existing `SUPERVISOR_OP_REGISTER_DRIVER` (`0x41`) message, but the legacy `ipc_send` receive path can expose it to supervisor as metadata opcode `0` (`OPCODE_INLINE`) with the application opcode prefixed in the first two payload bytes. Supervisor previously dispatched directly on metadata opcode 0 and returned `WrongObject` before decoding the registration.

SUP-L6I keeps the existing registration opcode and payload layout. Init now logs `INIT_CRASH_TEST_REGISTER_META` and the first eight payload bytes before sending. Supervisor logs sender/opcode/length plus payload bytes, normalizes only inline-framed control messages by extracting the first two payload bytes as the existing registration opcode, and emits precise `SUPERVISOR_CONTROL_*` diagnostics for sender acceptance, dispatch, and `WrongObject` sub-sites. No new IPC opcode, startup slot, cap path, restart authority, or SUP-L6 oracle count is introduced.

### SUP-L6J crash-test fault delivery routing

SUP-L6I made the crash-test supervisor registration path complete before the deliberate fault. SUP-L6J identifies the next blocker as kernel task-fault delivery: the trap path emitted `TASK_FAULT_CURRENT tid=<tid>` but the page-fault report helper only targeted the optional legacy `fault_handler_endpoint`. In the QEMU startup path the supervisor fault/task-exit endpoint is registered as the existing `supervisor_endpoint`, so no `SUPERVISOR_FAULT_REPORT_RECV` marker appeared even though the service was registered.

SUP-L6J keeps the fault authority model kernel-owned: crash_test_srv does not self-report and payload TIDs are still validated by supervisor sender checks. The kernel fault report helper now emits `TASK_FAULT_REPORT_BEGIN`, falls back from `fault_handler_endpoint` to the existing `supervisor_endpoint` when no explicit fault handler is installed, and reports either `TASK_FAULT_REPORT_SENT tid=<tid> target=supervisor` or a precise fail/no-route marker. Supervisor logs `SUPERVISOR_FAULT_SENDER_OK`, `SUPERVISOR_FAULT_REPORT_ACCEPTED`, and `SUPERVISOR_RESTART_DUE` after an accepted report schedules the first restart attempt. No syscall, IPC opcode, startup slot, cap path, broad restart policy, or SUP-L6 marker-count oracle is changed.

### SUP-L6L fault-report endpoint/enqueue proof

SUP-L6K/SUP-L6L found that `TASK_FAULT_REPORT_SENT tid=<tid> target=supervisor` alone was not enough proof: the next QEMU run showed supervisor polling its fault receive cap on endpoint index 4 and blocking with no `SUPERVISOR_FAULT_REPORT_RECV`. SUP-L6L therefore instruments the existing kernel fault-report enqueue path with endpoint identity, queue state, sender metadata, enqueue begin/ok/fail, and supervisor fault receive-cap endpoint markers.

The intended delivery path remains unchanged: the kernel-owned fault report uses sender_tid 0, the existing supervisor fault/task-exit endpoint, and normal endpoint queue semantics. Success is now logged only after enqueue proof (`TASK_FAULT_REPORT_ENQUEUE_OK`) and queue-state-after diagnostics. If endpoint identity still mismatches in QEMU, the new markers identify the exact endpoint/generation used by the kernel report versus the endpoint/generation resolved from the supervisor fault receive cap. No syscall, IPC opcode, startup slot, cap path, broad restart policy, or SUP-L6 marker-count oracle is changed.

### SUP-L6M blocked supervisor fault recv completion

SUP-L6L proved the fault-report endpoint identities matched: supervisor was blocked on endpoint 3 and the kernel-origin fault report targeted endpoint 3. The failing state was the delivery helper path, not the endpoint route: the old kernel-origin helper enqueued the fault report, removed/woke the waiter, and left `queued=1`, so supervisor resumed with an `Internal` recv error before `IPC_RECV_BLOCKED_*` completion markers could appear.

SUP-L6M chooses the direct blocked-recv completion path. When a recv-v2 waiter is already registered on the supervisor fault endpoint, the kernel fault-report helper now reuses the normal `complete_blocked_recv_for_waiter` semantics: copy the fault-report payload and metadata with `sender_tid=0`, clear the endpoint waiter, wake the supervisor, and leave no queued stranded report. The queue/retry path remains only for the no-waiter or non-recv-v2 fallback. No syscall, IPC opcode, startup slot, cap path, broad restart policy, user self-report, PM Accepted marker, or SUP-L6 marker-count oracle is changed.

### SUP-L6N accepted-fault restart scheduling

SUP-L6M made the kernel-origin crash-test fault report reach the supervisor and pass
sender validation. SUP-L6N identifies the next blocker as the supervisor
post-acceptance token path: the runtime accepted the fault report, but its
restart-token lookup used a plain send/receive sequence rather than the existing
PM request/reply-cap call convention, so PM did not receive the reply capability
needed to answer and `handle_task_exit` was never reached.

SUP-L6N keeps PM Accepted semantics unchanged and fixes only that post-acceptance
bridge: the supervisor now queries the restart token through the existing PM
request/reply-cap `ipc_call` path, logs managed-record and token state, and then
calls the existing `handle_task_exit` / `schedule_restart_with_reason` path for
attempt 1. The change adds diagnostics for missing records, missing tokens,
handle-exit errors, and schedule failures; it does not add opcodes, startup
slots, capabilities, supervisor-local restart execution, broader restart policy,
or any change to the exact SUP-L6 marker-count oracle. If scheduling succeeds but
PM restart send/decode is the next blocker, the next expected markers are
`SUPERVISOR_RESTART_DUE` and then `SUPERVISOR_PM_RESTART_SEND_BEGIN` or a precise
PM-client failure marker.

### SUP-L6O restart-token state after accepted fault

SUP-L6N proved that the supervisor reaches the post-fault accepted path, but the
first accepted crash-test fault still stopped before `handle_task_exit` because
`SUPERVISOR_CRASH_TEST_RESTART_TOKEN_READY` was only a diagnostic registration
marker: registration did not place a token in the managed record, and the PM token
query used a non-blocking reply poll that could report missing before PM's reply
was delivered.

SUP-L6O keeps token authority scoped to the existing PM restart-token mechanism.
The accepted-fault path now uses a record token first when present, otherwise
sends the existing `PROC_OP_TASK_RESTART_TOKEN` query to PM, blocks on the existing
reply-cap receive path until the PM reply is available, decodes the reply before
reporting missing, stores a successful PM token in the managed record, and only
then calls `handle_task_exit`. The supervisor logs query begin/call/reply/decode
markers plus token source (`record` or `pm-query`) and continues to use the
existing scheduler. No new opcode, startup slot, cap path, PM Accepted shortcut,
supervisor-local restart execution, or SUP-L6 marker-count oracle change is made.

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
