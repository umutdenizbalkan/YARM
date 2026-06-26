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
