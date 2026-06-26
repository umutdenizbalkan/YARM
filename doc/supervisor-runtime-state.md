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
