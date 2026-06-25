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
codec remains in `restart_abi_review.rs`, candidate opcodes `15`/`16` remain
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

No PR may promote this codec into `yarm-ipc-abi` or runtime dispatch until every
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
