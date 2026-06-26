<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Umut Deniz Balkan -->

# Supervisor ↔ Process Manager restart contract (SUP-3)

SUP-3 is design/model-only. It defines the future supervisor-to-Process Manager
restart IPC contract descriptors and timer/backoff semantics, but it does not add
a global IPC ABI opcode, send PM IPC, restart/spawn/tear down tasks, mint/revoke
capabilities, grant MMIO/IRQ/DMA, perform MMIO, or clean kernel resources.

## Authority boundary

- **Supervisor:** owns supervision policy, observed exits/faults, restart
  scheduling, dependency policy, degraded state, alert construction, and inert PM
  restart-request construction.
- **Process Manager:** owns restart mechanism, token validation, process
  replacement, teardown, address-space setup, resource accounting, capability
  mint/revoke/delivery, task cleanup, and real handles.
- **Kernel:** owns low-level task, capability, VM, scheduler, and trap mechanisms.

## Versioned request descriptor

`SupervisorPmRestartContract` fixes the model version and bounded wire limits for
future PM-facing restart IPC. `SupervisorPmRestartRequestV1` is the local oracle
for the eventual request shape. It includes:

- contract version and verified-supervisor identity requirement;
- target service TID, kind, and name;
- scoped redacted restart-token reference;
- restart reason, attempt count, due tick, dependency cause, and degraded hint;
- policy flags;
- requested startup-capability behavior;
- requested health-monitor behavior;
- rollback expectation; and
- mock request ID.

Only `SupervisorRestartRequestStatus::WouldRequestPmRestart` with a scoped token
reference maps to `SupervisorPmRestartDescriptorStatus::Sendable`. Blocked,
missing-token, restart-limit, no-action, already-pending, and PM-authority
unavailable requests remain non-sendable or deferred descriptors.

## Versioned reply descriptor and reply model

`SupervisorPmRestartReplyV1` models the future PM reply. It includes accepted,
rejected, deferred, rolled-back, and unsupported statuses; mock replacement handle;
old-task cleanup status; accounting status; startup-cap delivery status;
health-monitor registration status; rollback status; failure reason; and optional
next retry tick.

`apply_pm_restart_reply_model` is descriptive only:

- accepted records a mock replacement handle;
- rejected/unsupported records blocked/degraded model state;
- deferred schedules a retry tick from the reply;
- rolled back records degraded rollback failure;
- invalid version is rejected.

No real PM handle, task TID replacement, capability, or kernel state is created.

## Timer and backoff semantics

Current production uses `SupervisorTimerMode::LogicalTickOnly`; it is not a
wall-clock timer. Future runtimes should use a timer endpoint or PM/kernel timer
source. Backoff due ticks are monotonic in the supervisor tick domain and due
restarts must be evaluated only after a timer/tick event.

`compute_backoff_decision` models exponential growth by attempt count, caps the
backoff at a configured maximum, and fails closed by deferring when the future
timer endpoint is unavailable. Overflow saturates to a capped decision instead of
wrapping. Timer failure must defer restart execution rather than running a restart
early, and repeated crashes must not flood PM or init alerts.

## Production runtime behavior

Production may build/log the descriptor with
`SUPERVISOR_PM_RESTART_CONTRACT_BUILT`, but live PM restart IPC remains deferred
with `SUPERVISOR_PM_RESTART_IPC_DEFERRED_NO_PM_CLIENT`. Runtime also emits
`SUPERVISOR_TIMER_ENDPOINT_DEFERRED` and `SUPERVISOR_BACKOFF_LOGICAL_TICK_ONLY`
while the logical tick path is the only available timing source. The live restart
operation still returns an explicit unavailable/deferred error.

## Deferred live work

Before live PM wiring, future work must define the real PM IPC opcode/reply ABI,
verified sender contract, capability-bound token transport, PM resource cleanup
and rollback APIs, timer endpoint semantics, alert rate limiting, and supervisor
state replay. None of those mechanisms are implemented by SUP-3.

## SUP-4 PM-side oracle dependency

SUP-4 adds the PM-side acceptance oracle for this supervisor contract. The
supervisor remains the requestor and policy owner; PM remains the only component
that may eventually execute restart mechanism. The future supervisor request
shape in this document must validate against PM-side `PmRestartRequestDescriptor`
semantics before any live PM client is wired.

## SUP-5 RFC cross-link

The reviewed global PM restart IPC ABI is now specified as an RFC-only section in
`doc/process-manager-restart-contract.md`. SUP-5 remains non-live: supervisor is
still only the requestor, PM is the only future executor, and kernel mechanisms
remain external. Future live work requires explicit ABI approval before adding
`PROC_OP_PM_RESTART_V1` or wiring any PM client.

## SUP-6 conformance handoff

SUP-6 adds the live-implementation review checklist and conformance matrix in
`doc/pm-restart-live-implementation-checklist.md`. The supervisor remains only the
requestor until future live work passes that matrix, including verified sender,
scoped-token, fail-closed fallback, timer/backoff, and rollback-injection gates.

## SUP-7 codec-review handoff

SUP-7 prepares non-dispatching PM restart ABI codecs behind the review gate. The
supervisor still does not send PM restart IPC; production restart remains
fail-closed/deferred until a future live-ABI PR promotes the codec and passes the
SUP-6 conformance matrix.

## SUP-8 promotion gate

SUP-8 adds the ABI-review signoff package but does not change supervisor runtime
behavior. The supervisor remains requestor-only and must not send PM restart IPC
until a future SUP-live stage promotes the codec, passes signoff, and preserves
fail-closed fallback.

## SUP-9 dry-run promotion plan

SUP-9 links the supervisor contract to `doc/pm-restart-live-promotion-plan.md`.
The supervisor remains requestor-only and production restart remains fail-closed;
a future live stage must satisfy the SUP-8 signoff package and SUP-9 checklist
before any supervisor PM restart send path is enabled.

## SUP-10 evidence pack

SUP-10 links this supervisor contract to `doc/pm-restart-live-readiness-evidence.md`.
The supervisor remains requestor-only and fail-closed; any future SUP-live stage
must explicitly change the contract from proposed to live after satisfying the
SUP-8, SUP-9, and SUP-10 gates.

## SUP-11 runtime boundary update (2026-06-26)

The SUP-2 through SUP-10 restart contract/model remains non-live. SUP-11 moves
production behavior toward the modeled contract by removing the direct
fault-handler PM execute-restart bypass, but it intentionally does not wire a
live PM restart send path. Helpers that query or execute PM restart IPC are
legacy/deferred scaffolding and must not be called by production fault/task-exit
restart execution until a future task introduces the real contract-compliant PM
client, opcodes, cap-bound token flow, cleanup, and rollback semantics.

Production restart records now use a blocked/deferred runtime state when the due
sweep finds no PM client. This preserves fail-closed behavior without
busy-looping or clearing service state as if restart succeeded. New runtime
markers are `SUPERVISOR_RESTART_SCHEDULED`, `SUPERVISOR_RESTART_DUE_CHECK`, and
`SUPERVISOR_RESTART_EXEC_DEFERRED_NO_PM_CLIENT`.

## SUP-12 model extraction boundary (2026-06-26)

The restart contract/model code now lives in the gated supervisor
`restart_model.rs` module for hosted-dev/test/docs guardrails. This is a
mechanical location change only: the contract remains non-live, no PM IPC or
runtime dispatch is enabled, and production restart execution remains the SUP-11
fail-closed due-restart path.

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
