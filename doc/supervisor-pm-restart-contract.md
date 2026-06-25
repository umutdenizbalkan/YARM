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
