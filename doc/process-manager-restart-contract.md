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

## Deferred live work

Live PM restart requires a new ABI review, verified supervisor endpoint authority,
capability-bound token validation, PM-owned teardown/replacement/resource
accounting, rollback implementation, startup-cap delivery, health-monitor
registration, and reply delivery. None are implemented by SUP-4.
