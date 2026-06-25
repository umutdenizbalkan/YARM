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
