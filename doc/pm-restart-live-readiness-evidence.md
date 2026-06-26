<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Umut Deniz Balkan -->

# SUP-10 PM restart live-readiness evidence pack

SUP-10 is an evidence and exact future-diff planning stage only. It does not
allocate live restart opcodes, update `yarm-ipc-abi`, wire PM dispatch, wire a
supervisor PM restart send path, or perform restart, spawn, teardown,
address-space, capability, MMIO, DMA, IRQ, or resource-cleanup work.

## Evidence summary from SUP-1 through SUP-9

| Item | Artifact path | Evidence type | Invariant proven | Missing before live enablement |
|---|---|---|---|---|
| Production supervisor fail-closed behavior | `crates/yarm-control-plane-servers/src/control_plane/supervisor/service.rs`, `doc/supervisor-audit.md` | runtime markers + tests/docs | unavailable PM/timer operations remain visible errors/deferred, not fake success | live PM endpoint, timer source, fail-closed integration tests |
| Supervisor restart request model | `crates/yarm-control-plane-servers/src/control_plane/supervisor/service.rs` | inert model + hosted tests | supervisor can construct bounded restart intent without executing restart | live PM client and reply-driven state update evidence |
| Supervisor to PM contract model | `doc/supervisor-pm-restart-contract.md`, `doc/process-manager-restart-contract.md` | contract docs + descriptor tests | request/reply shape, authority boundary, token redaction, and timer semantics are modeled | ABI promotion and dispatch/client tests |
| PM validation/accounting/reply oracle | `crates/yarm-control-plane-servers/src/control_plane/process_manager/service.rs` | inert oracle + tests | PM-side acceptance/rejection/accounting/rollback semantics are modeled | live PM mechanism implementation and rollback injection evidence |
| Global ABI RFC | `doc/process-manager-restart-contract.md` | RFC/spec | proposed opcodes, payloads, statuses, failures, and compatibility rules are documented | reviewed opcode allocation and live ABI diff |
| SUP-6 conformance matrix | `doc/pm-restart-live-implementation-checklist.md` | checklist/matrix | live behavior rows have model/oracle/golden/future-test coverage | live dispatch/client conformance results |
| Non-dispatching codec and golden vectors | `crates/yarm-control-plane-servers/src/control_plane/process_manager/restart_abi_review.rs` | review-only codec + tests | fixed request/reply encoding, malformed rejection, and golden vectors are testable | promotion into live ABI namespace and live decode tests |
| Frozen offsets and reserved policy | `doc/process-manager-restart-contract.md` | SUP-8 signoff table | Request V1 is 110 bytes, Reply V1 is 50 bytes, reserved bytes must be zero | reviewer signoff and live ABI compatibility review |
| Promotion dry-run plan | `doc/pm-restart-live-promotion-plan.md` | step-by-step future plan | future live PR sequence is mechanically reviewable | execution of the future plan and attached evidence |
| Readiness report | `crates/yarm-control-plane-servers/src/control_plane/mod.rs` | test-only dry-run model | current package is ready for ABI review only, not live enablement | GoForLiveEnablement is intentionally absent until live evidence exists |

## Exact future live PR diff plan

### 1. `yarm-ipc-abi` / process IPC ABI

- Allocate opcode `15`/`16` or approved alternatives for the PM restart request and
  reply.
- Update the production process IPC opcode count in the same PR.
- Add live ABI encode/decode tests for Request V1, Reply V1, malformed payloads,
  unsupported versions, invalid enum values, and nonzero reserved bytes.
- Preserve the legacy `PROC_OP_EXECUTE_RESTART` separation; the future PM restart
  request must not silently replace or reinterpret that existing opcode.

### 2. Process Manager

- Add an explicit dispatch match arm for the approved restart request opcode.
- Decode Request V1 with the checked SUP-7 codec or its reviewed live successor.
- Validate verified supervisor sender metadata before trusting any payload field.
- Validate scoped/capability-bound token ownership and reject raw/unscoped token
  authority.
- Call the PM validation, accounting, mechanism, and rollback implementation.
- Encode Reply V1 for accept/reject/defer/rollback/unsupported/no-target cases.
- Never accept raw token authority and never log full token material.

### 3. Supervisor

- Add a PM client path behind an explicit feature/rollout gate.
- Build Request V1 from the SUP-2/SUP-3 restart model.
- Send only when a verified PM endpoint/cap authority exists.
- Decode Reply V1 and update supervisor state only from the PM reply.
- Preserve fail-closed fallback and deferred markers when the PM endpoint, timer,
  or authority is missing.

### 4. PM mechanism

- Validate target state, restartability, restart limit, dependency blockers, and
  token ownership.
- Define old-task state handling before and after successful replacement.
- Reserve replacement task, address-space, CNode, startup-cap, and health-monitor
  resources according to PM-owned accounting rules.
- Implement reverse-order rollback for partial failures.
- Register health monitoring and send init/supervisor notifications after final
  accounting state is known.

### 5. Timer/backoff

- Replace production logical-tick-only placeholder with a timer endpoint or
  PM/kernel timer source.
- Preserve the no-execution-before-`due_tick` invariant.
- Defer when the timer source is unavailable.
- Add crash-loop rate limiting without silently dropping required alert state.

### 6. Tests, scripts, and docs

- Add hosted ABI, PM dispatch, supervisor client, validation, accounting, rollback,
  and reply tests.
- Add rollback-injection tests for each planned failure point.
- Add x86_64 and AArch64 boot smokes for accepted, rejected, deferred, and rollback
  paths.
- Update docs from RFC/proposed/review-only status to live status in the same PR
  that enables dispatch.

## Readiness evidence matrix

| Requirement | Current artifact | Current status | Missing live evidence | Blocker before live | Future test name |
|---|---|---|---|---|---|
| verified supervisor sender | supervisor/PM models and docs | Modeled | verified IPC metadata path in PM dispatch | yes | `pm_restart_live_rejects_unverified_sender` |
| scoped token | SUP-2/SUP-7 token refs/codecs | Modeled | capability-bound token materialization | yes | `pm_restart_live_accepts_scoped_token` |
| wrong sender rejection | conformance matrix and oracle | Proven | live PM dispatch rejection | yes | `pm_restart_live_wrong_sender_rejected` |
| wrong token owner rejection | codec/oracle vectors | Proven | live token lookup and rejection | yes | `pm_restart_live_wrong_token_owner_rejected` |
| raw token rejection | codec decode/oracle vectors | Proven | live raw-token rejection | yes | `pm_restart_live_raw_token_rejected` |
| unknown target | PM oracle | Modeled | live PM target lookup | yes | `pm_restart_live_unknown_target_no_such_target` |
| restart limit | PM/supervisor models | Modeled | live attempt counter enforcement | yes | `pm_restart_live_restart_limit_rejected` |
| dependency blocker | supervisor request model and reply vectors | Modeled | live dependency blocker propagation | yes | `pm_restart_live_dependency_blocker_deferred` |
| resource unavailable | PM accounting oracle | Modeled | live resource preflight failure | yes | `pm_restart_live_resource_unavailable_deferred` |
| startup-cap unsupported | PM oracle row | Modeled | live startup-cap layout check | yes | `pm_restart_live_startup_cap_unsupported` |
| rollback after replacement task | rollback plan | Documented | live rollback injection | yes | `pm_restart_live_rollback_after_task_slot` |
| rollback after startup-cap | rollback plan | Documented | live startup-cap rollback injection | yes | `pm_restart_live_rollback_after_startup_cap` |
| timer unavailable | timer/backoff model | Modeled | live timer endpoint unavailable path | yes | `pm_restart_live_timer_unavailable_deferred` |
| crash-loop rate limit | docs/checklists | Documented | live alert/PM rate-limit behavior | yes | `pm_restart_live_crash_loop_rate_limited` |
| accepted restart | PM oracle/accounting model | Modeled | live replacement process and reply | yes | `pm_restart_live_accepted_replaces_task` |
| unsupported version | codec/golden vectors | Proven | live ABI dispatch decode rejection | yes | `pm_restart_live_unsupported_version_rejected` |
| already restarting | PM oracle/golden vector | Modeled | live in-flight restart tracking | yes | `pm_restart_live_already_restarting` |
| supervisor fail-closed fallback | supervisor runtime markers/tests | Proven | live PM-client missing-endpoint regression | yes | `supervisor_pm_restart_missing_endpoint_fails_closed` |
| x86_64 boot unchanged | future QEMU plan | Missing | x86_64 smoke log | yes | `qemu_x86_64_pm_restart_live_boot_smoke` |
| AArch64 boot unchanged | future QEMU plan | Missing | AArch64 smoke log | yes | `qemu_aarch64_pm_restart_live_boot_smoke` |

## Go/no-go report model

The SUP-10 dry-run report status is **GoForAbiReview** only. It is not a live
readiness signal and intentionally has no `GoForLiveEnablement` state.

Required checks:

- SUP-8 frozen request and reply sizes are present.
- SUP-9 promotion plan is present.
- candidate restart opcodes are absent from live ABI.
- production process IPC opcode count is still 14.
- `SYSCALL_COUNT` is still 31.
- PM dispatch is absent.
- supervisor send is absent.
- production fail-closed marker remains present.
- rollback-injection plan is present.
- QEMU acceptance plan is present.

## Future rollback-injection scripts and markers

Future scripts, not added in SUP-10:

- `scripts/qemu-supervisor-pm-restart-accepted-smoke.sh`
- `scripts/qemu-supervisor-pm-restart-wrong-sender-smoke.sh`
- `scripts/qemu-supervisor-pm-restart-wrong-token-smoke.sh`
- `scripts/qemu-supervisor-pm-restart-rollback-smoke.sh`
- `scripts/qemu-supervisor-pm-restart-timer-deferred-smoke.sh`

Future-only markers that must **not** appear in current runtime logs yet:

- `SUPERVISOR_PM_RESTART_SEND_BEGIN`
- `PM_RESTART_V1_DECODE_OK`
- `PM_RESTART_SENDER_OK`
- `PM_RESTART_TOKEN_OK`
- `PM_RESTART_ACCOUNTING_BEGIN`
- `PM_RESTART_ROLLBACK_BEGIN`
- `PM_RESTART_REPLY_ACCEPTED`
- `PM_RESTART_REPLY_REJECTED`
- `PM_RESTART_REPLY_DEFERRED`
- `SUPERVISOR_PM_RESTART_REPLY_RECV`
- `SUPERVISOR_PM_RESTART_STATE_UPDATED`

Current runtime must continue to emit only deferred/unavailable production restart
markers until a future SUP-live stage explicitly enables the PM client and PM
restart dispatch.

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
