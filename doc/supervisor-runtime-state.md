<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Umut Deniz Balkan -->

# Supervisor runtime state after SUP-11

SUP-11 is a runtime cleanup only. It does not implement live PM restart.

Runtime restart flow:

1. verified kernel fault-endpoint delivery or registered PM lifecycle delivery is
   decoded;
2. `handle_task_exit` records exit state and schedules a restart or marks the
   service degraded/dead;
3. `execute_due_restarts` is the only execution gate when the logical due tick is
   reached;
4. because no live PM client/opcode exists, due execution transitions the record
   to `RestartBlockedNoPmClient` and logs one structured deferred line.

Invalid fault senders are rejected without scheduling restart and without
skipping later loop maintenance. Claimed-task self-reporting is not authoritative
for fault/task-exit delivery.

Direct PM restart helper calls remain disabled/deferred for production restart
execution. Future live work must provide a real PM client, timer endpoint,
cap-bound token, cleanup/rollback, and contract-compliant accounting before any
actual restart/spawn/teardown/resource changes are permitted.

## SUP-12 note

SUP-12 mechanically moves non-live restart contract/model/readiness code out of
`service.rs` into the gated `restart_model.rs` module. Runtime state and behavior
remain SUP-11 fail-closed: scheduling, due checks, blocked/no-PM-client state,
invalid sender rejection, and compact deferred logging are unchanged. Future live
restart work should begin at SUP-L1.

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
