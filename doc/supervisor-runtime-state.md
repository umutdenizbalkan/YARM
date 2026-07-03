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

### SUP-L6E crash-test registration state

The runtime state machine now treats crash-test registration as a separate pre-restart handoff. Init first verifies whether the existing supervisor control SEND cap is present in startup context; absence is logged as `INIT_SUPERVISOR_CONTROL_SEND_CAP_MISSING reason=startup-slot-empty` and blocks registration before any restart scheduling can occur.

When the cap is present, init sends the existing `RegisterDriverRequest` and supervisor records the crash-test service only after receiving and validating that control message. After registration, fault handling emits `SUPERVISOR_FAULT_REPORT_RECV`, `SUPERVISOR_FAULT_REPORT_ACCEPTED`, and `SUPERVISOR_HANDLE_TASK_EXIT_BEGIN` before the existing `SUPERVISOR_RESTART_SCHEDULED attempt=1 max=3` marker can appear. The exact SUP-L6 restart-count oracle is unchanged.

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
