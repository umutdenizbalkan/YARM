<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Umut Deniz Balkan -->

# YARM IPC

> **Ownership rule.** All IPC documentation — message framing, fragmentation
> policy, shared-memory fastpath, throughput patterns, migration / phase
> history — lives here. New IPC fragment files are forbidden; update this doc
> instead. The per-syscall public ABI is in `doc/SYSCALL_ABI.md` (canonical);
> the typed wire codec versions are in `doc/VFS.md` §6. See
> `doc/DOCUMENTATION_MAP.md`.

For the finalized recv-v2 / reply-cap contract see `doc/ARCH_AARCH64.md` §4
(the portable AArch64 reference). For the kernel-side directive split
status (D1 / D2 / D5 routers and fallbacks) see `doc/KERNEL_UNLOCKING.md`
§2 and §6.

---

## 1. Frozen payload + framing policy

- **Inline payload capacity:** `Message::MAX_PAYLOAD = 128` bytes, frozen.
- Medium payloads (`129..=1024` bytes) use the fragmentation protocol (§2).
- Large payloads (`>1024` bytes) use the shared-memory descriptor path
  (`OPCODE_SHARED_MEM`) with auto-map on receive (§3).

Phase 1 benchmark snapshot (historical, see `doc/PROJECT_HISTORY.md`):
`inline64 = 94.96 ns/op`, `inline128 = 96.80 ns/op`,
`shared_desc = 80.93 ns/op`, `simulated_2x128 = 193.61 ns/op`.

---

## 2. Medium-payload fragmentation protocol

Each fragment is a normal `Message` payload with this fixed 12-byte prefix:

| Field | Type | Size |
|-------|------|------|
| `message_id` | `u32` | 4 |
| `fragment_index` | `u16` | 2 |
| `fragment_count` | `u16` | 2 |
| `fragment_len` | `u16` | 2 |
| `reserved` | `u16` | 2 |
| (data) | `[u8; fragment_len]` | up to 116 |

Usable fragment data per message: `MAX_PAYLOAD (128) − prefix (12) = 116`
bytes.

### Sender rules

1. Generate a non-zero `message_id` unique per sender endpoint stream.
2. Compute `fragment_count = ceil(total_len / 116)`.
3. Emit fragments in index order (`0..fragment_count-1`).
4. Use the consistent opcode for all fragments of the same logical message.

### Receiver rules

1. Group by `(sender_tid, opcode, message_id)`.
2. Reject duplicate fragment indexes.
3. Require all fragments to arrive before exposing the reassembled payload.
4. Drop partial assemblies on timeout / sender death / endpoint teardown.

---

## 3. Shared-memory fastpath

Receiver auto-map shared-memory delivery with explicit lifecycle and
revocation. The full phased plan is closed; current live behavior:

### Transfer object model

Dedicated kernel record: `(transfer_id, source_tid, receiver_tid,
endpoint_binding, memory_object_id / dma_region_id, byte_range, rights_mask,
generation)`. Explicit states: `Created → MappedReceiver → MappedBoth →
(Released | Revoked)`. Telemetry counters track creation /
materialization / revocation / failures and map/release parity.

### Receiver auto-map plumbing

On `IpcRecv` of `OPCODE_SHARED_MEM`:

1. Recv-side map request contract = `(target_VA, map_flags,
   optional fixed/anywhere policy)`.
2. Receiver pages are mapped automatically from the transferred capability
   according to policy.
3. Result metadata (`mapped_VA`, `mapped_length`, `transfer_id`) is returned
   through syscall return lanes.

Partial-map mid-range mapping faults trigger rollback (no half-mapped
state). The legacy descriptor return path remains as a compatibility
fallback behind an ABI gate.

### Sender / receiver dual-map + pinning lifecycle

- Pin/unpin rules pin shared transfer frames while either side holds active
  mappings.
- Map refcounts are updated for both sides and survive task scheduling /
  restart boundaries.
- The unmap / release syscall path drops active mapping references and
  transitions transfer state.

### Telemetry contract

Track `shared_mem_bytes_mapped`, `shared_mem_bytes_released`, and
`transfer_release_calls`. If `_mapped` grows much faster than `_released`
under steady load, tune ring depth and release cadence.

Reclamation tests must prove no early free while mappings remain.

---

## 4. Throughput patterns (FS / network / display)

### Common contract

- Prefer **one long-lived data endpoint per producer/consumer pair**.
- Use a **ring descriptor in shared memory** (`head`, `tail`, `entries[]`)
  and transfer only capabilities for reusable page-aligned regions.
- Receiver calls `IpcRecv` with an auto-map target VA and keeps the mapping
  hot until ring pressure requires recycling.
- Recycle with `TransferRelease` fast path (`ptr=0`, `len=0`) when an
  active transfer mapping record exists.

### FS servers (large read / write)

- Batch adjacent file blocks into **64 KiB+ transfer windows** when
  possible.
- Keep **2–4 in-flight transfer regions per client** to overlap disk and
  user-copy completion.
- Ring watermarking: low watermark → request refill; high watermark → stop
  issuing new read windows.

### Network servers (RX / TX)

- Use fixed-size packet slot rings (MTU-sized or jumbo-sized classes).
- Reserve separate RX / TX rings to avoid cross-direction cache thrash.
- Return consumed RX slots in batches (every `N` packets or every poll
  tick).

### Display servers (framebuffer updates)

- Prefer tile / dirty-rect rings over full-frame transfers.
- Use stable backing mappings for frequently updated regions.
- Batch tile commit notifications so one control message can acknowledge
  multiple transfer ids.

---

## 5. Shared-IPC migration ownership

- **ABI opcode/payload ownership:** `crates/yarm-ipc-abi`.
- **Shared service-side helper / runtime glue:** `crates/yarm-srv-common`.
- **Service implementation ownership:** extracted server crates
  (`yarm-*-servers`).

### Migration rule

When migrating an IPC surface:

1. Define / freeze the request+reply codec in `yarm-ipc-abi`.
2. Use shared decode / reply helpers from `yarm-srv-common` where
   applicable.
3. Keep policy / orchestration in service crates, **not** kernel.
4. Add deterministic tests in the owning service crate.

### Shared-memory flow expectations

For transfer-cap / shared-memory flows:

1. Receive / map through the current IPC contract.
2. Consume in a bounded region.
3. Release transfer mapping (`TransferRelease`) to avoid leaks / drift.

### Gate expectations

- `scripts/phase7-shared-ipc-gates.sh` is the shared-IPC migration check.
- Map / release parity must remain green in canary tests
  (`transfer_records_created == transfer_records_revoked`;
  `shared_mem_bytes_mapped == shared_mem_bytes_released`).

---

## 6. Finalized IPC ABI summary

(Full contract: `doc/ARCH_AARCH64.md` §4; per-syscall ABI: `doc/SYSCALL_ABI.md`.)

- **`ipc_call`** is send / queue only. No inline syscall reply consumption.
- **`ipc_recv_v2`** ABI: `ret0` carries syscall success / error only; all
  metadata in `IpcRecvMetaV2` (out-meta only); no inline reply prefix
  stripping for plain replies.
- **Portable blocked recv-v2 completion** with delivery-time payload + 40-byte
  meta copy; one-shot message consumption; no syscall replay; no retry
  workaround.
- **`ipc_reply`** completes blocked recv-v2 waiters directly; no duplicate
  enqueue on the reply path.
- **Reply-cap materialization:** receiver-local CapIDs only; reply caps are
  one-shot; raw reply handles are never exposed to userspace.
- **`recv_shared_v3`** ABI offsets are frozen (see `doc/KERNEL_UNLOCKING.md`
  §3).

### Regression coverage (regression-set)

- `recv_v2_blocked_waiter_direct_delivery_consumes_exactly_once`
- `ipc_reply_wakes_blocked_recv_v2_waiter_without_duplicate_enqueue`
- `recv_v2_reports_metadata_only_via_out_meta_and_preserves_plain_reply_payload`
- `recv_v2_materializes_reply_cap_once_per_message`

---

## 7. Phase 5 — shared-memory transfer hardening artifacts

These invariants remain live:

- `IpcSend` large-payload transfer path requires transfer-cap rights
  `READ | MAP` before descriptor send.
- Repeated rejection due to missing transfer rights leaves
  `transfer_records_created` unchanged (`0`).
- Shared-memory recv validation / map failures revoke materialized
  transfer caps (no leaked receiver-local cap on failure).
- Receiver mapping-intent validation: `read` required; unknown bits
  rejected; write-intent rejected unless materialized transfer cap
  includes `WRITE`; read-only intent attenuates receiver-local cap to
  `READ | MAP` (drops `WRITE`).
- Repeated recv map-intent / write-intent failures keep
  `shared_mem_bytes_mapped` and `_released` at `0` (no accounting drift).
- Process-cleanup purge of active shared-memory transfer mappings records
  `shared_mem_bytes_released`.
- Direct transfer-cap revoke force-unmap records released-byte telemetry.
- Mixed cleanup / revoke keeps both invariants stable:
  `transfer_records_revoked >= transfer_records_created` (no stale
  records); `shared_mem_bytes_mapped == shared_mem_bytes_released`.

---

## 8. Reply caps, shared regions, direct IPC, reply timeout, and server death

> Consolidated from the per-stage reports for Stages 198C – 200D, which were deleted
> after migration (inventory: `doc/DOCUMENTATION_MAP.md` §"Consolidation Pass 6").
> Lock classification for every path below: `doc/KERNEL_UNLOCK_AUDIT.md` §2.

### 8.1 Reply-cap semantics (authoritative kernel model)

**Creation.** `create_reply_cap_for_caller[_in_cnode]` (`src/kernel/boot/ipc_state.rs`)
mints a one-shot reply capability into the **caller's** cspace and records a `Reply`
record identified by `{ index, generation }`. The record captures the caller's
incarnation ASID and, when a responder is bound, the responder's incarnation ASID.
An **unbound** record (responder `None`) carries no replier incarnation.

**Identity is generation-bearing, never numeric-TID-only.** A record is valid only
while both the recorded `{ tid, asid }` incarnations still match; a reused TID with a
new ASID does not inherit authority. This is the `numeric_tid_only_authority=0`
invariant.

**Invocation.** `ipc_reply(reply_cap, msg)` is **one-shot**. The second reply through
the same cap — or through an alias produced by `grant_capability_task_to_task`, which
resolves to the *same* reply record/object — is rejected canonically with
`InvalidCapability` or `StaleCapability`, and wakes nobody. Aliases never multiply
authority: `duplicate_replies=0`, `duplicate_wakes=0`.

**Transfer.** Reply caps move on the split delivery path; receiver-local CapIDs only,
raw reply handles are never exposed to userspace.

**Queued reply-cap enqueue is UNSUPPORTED.** The queued envelope stores a flag-based
routing gate rather than kernel-derived object identity; the accepted redesign is
*typed queued transfer envelope carrying kernel-derived object identity*, with
liveness revalidation (`capability_object_live`, else `InvalidCapability`) **before**
minting the receiver-local SEND-only cap. Until that lands, the queued reply-cap class
has zero live cells by policy, not by omission.

### 8.2 Shared-region `IpcSend` — the large-transfer contract

The shared-region transfer is **NOT** selected by an inline `OPCODE_SHARED_MEM`
message. A small inline cap-transfer message is decoded by the kernel as an *ordinary*
inline cap transfer. The kernel selects the shared-region path purely by the
**large-transfer form of `IpcSend`**: `arg(LEN) > Message::MAX_PAYLOAD`.
`OPCODE_SHARED_MEM` is *produced by the kernel* on that path (`handle_ipc_send`), not
supplied by userspace.

| Item | Value / source |
|------|----------------|
| syscall | `IpcSend` = `SYSCALL_IPC_SEND_NR` = **1** |
| arg 0 | `SYSCALL_ARG_CAP` — SEND cap to the endpoint the receiver recv-v2-blocks on |
| arg 1 | `SYSCALL_ARG_PTR` — byte **offset** into the source shared-region object |
| arg 2 | `SYSCALL_ARG_LEN` — region byte length |
| arg 5 | `SYSCALL_ARG_TRANSFER_CAP` (`= TRAPFRAME_ARG_REGS − 1 = 6 − 1`, all three arches) — the init-local source cap |
| args 3 / 4 | inline payload 0 (unused here) / send-timeout ticks (0 = non-blocking) |
| **path selector** | `len > Message::MAX_PAYLOAD` (**128**, `crates/yarm-kernel/src/ipc.rs:90`) selects the large shared-region branch. `len ≤ 128` takes the ELSE branch — an ordinary `OPCODE_INLINE` cap transfer. The region length **must** exceed 128. |
| source object forms | `CapObject::MemoryObject` or `CapObject::DmaRegion` only; anything else → `WrongObject`. `validate_shared_mem_transfer_rights` gates required rights. |
| region validation | `validate_user_region(offset, len)`: `offset < KERNEL_SPACE_BASE`, no overflow, `offset + len ≤ KERNEL_SPACE_BASE` |
| **descriptor layout** | `SharedMemoryRegion::ENCODED_LEN` = **16 bytes**, little-endian: `offset: u64` at bytes 0..8, `len: u64` at bytes 8..16 |
| envelope | exactly **one** `TransferEnvelope`; the source cap is delegated/duplicated, **not moved** — it stays valid in the sender's CNode |
| pre-ack behavior | if the receiver is not yet an authoritatively-committed recv-v2 waiter the direct producer fails closed with retryable `SyscallError::WouldBlock` — no mutation, source cap and envelope preserved, parent retries |

**Transaction and cleanup ownership — protocol A (executor-owned).** One owner performs
cleanup; teardown matching is generation-bearing; partial-map progress is tracked so
cancellation checkpoints can unwind exactly the pages mapped. Invariants proven by the
hosted seals: `orphan_pages=0`, `duplicate_unmaps=0`, `duplicate_revokes=0`,
`duplicate_pin_releases=0`, `stale_publications=0`, `leaked_transactions=0`.

**Live status:** direct class sealed on all three architectures —
`SECOND_COHORT_SHARED_REGION_DIRECT_MATRIX_SEAL arches=3 classes=1 live_cells=3
fuse_trips=0 duplicate_wakes=0 result=ok`. The **enqueue** class is hosted-sealed only
(`SECOND_COHORT_SHARED_REGION_ENQUEUE_HOSTED_SEAL cases=23 … result=ok`) and has zero
live cells.

### 8.3 Direct `IpcCall` / `IpcReply` (NR 6 / NR 7) — off-lock transaction

The off-lock direct request/reply transaction is **implemented and live-proven, but
proof-gated and default-OFF** (`ipccall_direct_proof_enabled()`,
`src/kernel/boot/mod.rs:3095`). A normal boot never takes it. See
`doc/KERNEL_UNLOCKING.md` §0 blocker 3.

Transaction shape: reserve → caller-copy → exact-waiter claim → record `Consumed` →
single enqueue. Race outcomes (deterministic, hosted):

* **Reply vs caller exit** — reserve → caller `mark_task_dead` → commit ⇒ `commit=GoneDead`;
  stale authority restored 0, caller wakes 0, record left `Reserved` 0, reply alias usable 0.
* **Request vs server exit** — claim server waiter → server `mark_task_dead` → commit ⇒
  `commit=GoneDead`; cap minted into replacement CNode 0, reply-record leak 0, server wake 0.

**Achieved-invariant seal (Stage 199A2A):**

```
STAGE_199A2A_INCARNATION_SAFE_REPLY_RECORD_SEAL numeric_tid_only_authority=0 request_copy_before_record_reserve=1 reply_claim_before_source_copy=0 leaked_reply_records_on_fault=0 duplicate_replies=0 duplicate_wakes=0 result=ok
```

**Honestly deferred seal — the broad-lock payload copy was NOT removed.** Preserved
verbatim because it records a still-open blocker:

```
STAGE_199_IPCCALL_REPLY_OFFLOCK_SEAL request_copy_under_lock=1 reply_copy_under_lock=1 request_copy_before_record_reserve=1 reply_claim_before_source_copy=0 numeric_tid_only_authority=0 duplicate_replies=0 duplicate_wakes=0 leaked_reply_records=0 result=deferred reason=broad_lock_payload_copy_needs_pre_global_lock_split_seam_which_is_hosted_disabled_and_block_dispatch_switch_required
```

Two source-grounded reasons, both still true:

1. The only way to copy user memory off the broad `&mut KernelState` is the
   pre-global-lock split seam, whose copy helper `copy_from_user_asid_split_read` is
   `#[cfg(not(feature = "hosted-dev"))]`-only — the `hosted-dev` stubs return `None`.
   An off-lock copy for NR 6 / NR 7 therefore cannot be exercised, let alone proven,
   in a hosted test.
2. NR 6 / NR 7 block the caller and drive a queue-advancing dispatch — exactly the
   `switch_required` case the codebase already defers for `FutexWait`
   (`GLOBAL_LOCK_RETIRE_CLASS_DEFERRED class=FutexWait
   reason=block_dispatch_switch_required_needs_global_lock`).

**Live evidence (x86_64, SMP=2, all knob-gated):**
`STAGE_199_X86_DIRECT_IPC_FINAL_SEAL functional_smp1=1 ap_dispatch_smp2=1
cross_cpu_request_smp2=1 cross_cpu_reply_smp2=1 request_user_consumed=1
reply_user_consumed=1 trap_depth_errors=0 wrong_current_task=0 duplicate_replies=0
duplicate_wakes=0 overwrite_fuse_trips=0 result=ok`. A duplicate NR 7 through the
consumed reply cap is refused: `IPCREPLY_DIRECT_SMP_DUPLICATE_REFUSED
reason=consumed_barrier reply_copies=1 caller_wakes=1 ipis=1`.

### 8.4 Reply timeout — terminal ownership and the three-architecture matrix

**Terminal ownership (Stage 200A).** Reply, timeout and peer-death compete for a single
terminal claim on the reply slot. The winner is recorded once; the loser observes the
terminal state and takes no action. Terminal state is generation-bearing and integrates
with the existing reply reservation, so a slot/generation reuse cannot resurrect a stale
claim.

**Deadline tokens (Stage 200B).** Deadlines live in a generation-bearing token store;
arm / fire / cancel are idempotent against slot and generation reuse. Reply-wins
disarms the deadline via `disarm_deadline_after_terminal_completion`; timeout-wins
rejects the late reply. `late_reply_successes=0`, `late_timeout_claims=0`.

**Completion transaction (Stage 200C).** The completion runs as a narrow transaction:
`IPC_REPLY_TIMEOUT_LOCK_STATUS … completion_transaction_narrow=1`. On x86_64 the
deadline **scan** is also off the broad lock (`scan_broad_lock=0`,
`GLOBAL_LOCK_RETIRE_CLASS_DONE arch=x86_64 class=IpcReplyTimeout result=ok`); on
AArch64 and RISC-V the scan is still `scan_broad_lock=1`, and the broad-lock fallback
`run_reply_timeout_completion_locked` (`src/runtime.rs:3725`) survives.

**Three-architecture live matrix — 6/6 cells earned.**
`STAGE_200_IPC_REPLY_TIMEOUT_MATRIX_SEAL` (commit `72a4ebf`,
`scripts/qemu-ipc-reply-timeout-matrix-smoke.sh`) freezes timeout-wins and reply-wins on
x86_64, AArch64 and RISC-V from one clean exact commit:

* **Architecture-local selector decoding.** Slot-5 scenario pairs overlap (AArch64 8/9,
  RISC-V 9/10, x86_64 10/11), so a number alone never identifies a scenario.
  `yarm_ipc_abi::ipc_reply_timeout_abi` owns one typed decoder
  (`IpcReplyTimeoutScenario`, `ipc_reply_timeout_scenario_for_current_arch`) whose
  current-arch base is `cfg`-selected, plus the matching encoder the kernel uses to
  publish slot 5. A selector belonging only to another architecture decodes to `None`,
  never to the wrong scenario.
* **Per-cell evidence.** Reply-wins additionally proves a duplicate NR 7 through the
  consumed reply cap is rejected (`duplicate_reply=rejected`). RISC-V timeout-wins
  proves its return chain with three independently observed values of `a0`:
  `RISCV_BLOCKED_RETURN_PUBLISHED` (stored TCB lanes) →
  `RISCV_BLOCKED_SYSCALL_COMPLETION_DELIVERED` (final sret frame) →
  `USER_RT_RECV_AFTER_SYSCALL` (first userspace observation), all required to be 9, in
  that order.
* **Single-boot proof.** A one-shot `YARM_BOOT_INSTANCE` marker carries a per-boot nonce
  read from the architecture's free-running counter. Runners assert one QEMU launch, one
  firmware banner, one kernel entry, one boot completion, and exactly one **distinct**
  nonce — the last is what separates one boot from two that produced identical-looking
  lines.

### 8.5 Server death (`ServerDies`) — mechanism landed, zero live cells

The server-death terminal mechanism and its deferred post-lock completion are landed and
hosted-proven (`drain_server_death_post_work`, `src/arch/trap_entry.rs:1129`). The
liveness foundation defines **nine transition classes**, **fifteen hard-fail literals**,
**twenty-four deterministic races** and **nine wiring guards**
(`src/kernel/boot/mod.rs:3985`–`4152`).

**No ServerDies live cell has been earned on any architecture.** Four live attempts,
none sealed. Two defects were found live:

1. **Restore-owner staleness (fixed, hosted-only).** x86_64 resolves the restore owner
   **in-lock**, strictly before the post-lock drains — and the drains are exactly where
   wakes are published. The server-death drain made a caller runnable and enqueued it;
   the epilogue committed the earlier `owner=idle` decision anyway; the CPU halted
   holding an idle frame while a runnable task existed, re-idling on every tick until the
   boot timed out. The violated invariant: *a restore-owner decision taken before the
   post-lock drains must be re-validated after them.* AArch64 and RISC-V consume the
   disposition **after** the drains, so they never had this gap.
   Fixed by `revalidate_idle_owner_after_drains` (`src/runtime.rs:665`, wired at
   `src/arch/x86_64/descriptor_tables.rs:1324`) with the typed outcome:

   ```rust
   enum OwnerRevalidation { Idle, Replacement(u64), RestoreFailed { tid: u64, rolled_back: bool } }
   enum OwnerCommit       { Idle, Replacement(u64), FailClosed(u64) }
   ```

   Only `RestoreFailed { rolled_back: false }` maps to `FailClosed`, which takes the
   existing fatal architecture path rather than a second policy. **This fix has never run
   in QEMU.** The `FailClosed` arm is a currently-unreachable backstop.

2. **`LINK_LEAK created=54 detached=1 result=fail` — RESOLVED (Stage 199D increment).**
   It was never a resource leak: the ordinary reply path does close its links, through
   `finalize_server_reply_link_for_record`. One pair of counters was being asked to answer
   two different questions and could answer neither. `LinkCreated` was recorded by
   `register_server_reply_link` for **every** bound `IpcCall` in the system; `LinkDetached`
   only by `take_server_reply_link`, the exit path; and the ordinary terminal close
   (`detach_server_reply_link_exact`) recorded **nothing at all**, so the pair was not even
   a valid global leak invariant. `audit_success_path` then compared a system-wide creation
   count against one death's detach count and additionally demanded every class equal 1 —
   true only when the direct transaction was the sole link creator and the counters were
   reset per hosted case. `reset_instance()` had no live caller, so a real boot accumulated
   from boot.

   The two questions are now separated.

   **Tier 1 — system-wide link-lifecycle totals** (`links_created` / `links_closed`).
   Incremented by every genuine installation and by every genuine removal at **both**
   closing edges. This is the real reverse-link leak invariant, and it is what the
   `IPC_SERVER_DEATH_LINK_LEAK` literal now compares:
   `IPC_SERVER_DEATH_LINK_LEAK created=<n> closed=<m> scope=system result=fail`. A link
   created anywhere and never closed still fails the audit.

   **Tier 2 — the nine-vector, scoped to exactly one armed ServerDies transaction**,
   identified by the reply record `{index, generation}` it owns. Unrelated earlier or later
   calls carry a different record identity and cannot move it, so the expectation is
   `LinkCreated = 1` and `LinkDetached = 1` regardless of how much unrelated IPC the boot
   performs.

   The scope is armed at `register_reply_receive_deadline` — the point at which the
   transaction first becomes identifiable, and the same site that already records the
   ServerDies stale token. The reverse link is installed earlier, when the reply record is
   created, so the creation edge cannot scope itself; instead the arm **observes live
   state**, resolving the record's bound replier and reading its TCB link back
   (`IPC_SERVER_DEATH_SCOPE_ARMED … link_present=<0|1>`). That keeps `LinkCreated` a
   genuine observation rather than an inference from the later detach: an armed record that
   owns no reverse link leaves it at 0 and fails the audit, which is exactly the defect
   Stage 200D-2B1D-x86 hit live (`DEFERRED_RESERVED` reached, `LINK_CAPTURED` absent).

   Fail-closed behaviour is preserved throughout: a second close of the armed record leaves
   the class visibly `>1`; a close for a different record while armed is reported
   (`IPC_SERVER_DEATH_FOREIGN_LINK_CLOSE … counted=0`) and not counted; a stale record
   generation or stale server incarnation detaches nothing and counts nothing; a **losing**
   terminal path (reply wins) closes the armed link so `LinkDetached = 1` while
   `PeerDeathWinner = 0`, and the existing record-leak literal fires; and an unarmed
   instance fails outright (`IPC_SERVER_DEATH_SCOPE_UNARMED`). A second, different arm is
   refused (`IPC_SERVER_DEATH_SCOPE_CONFLICT`), so two overlapping scenarios can never
   share a vector.

   Ten focused hosted cases pin this (`d01`–`d10`), including the original reproduction
   with unrelated links created on both sides of the armed transaction. **No live cell is
   claimed** — this is a hosted accounting repair, and the ServerDies live cells remain
   unearned on all three architectures.

### 8.6 Preserved contracts from the deleted stage reports

Facts that existed only in per-stage files and have no other home.

**Ordinary-cap transfer is COPY / DELEGATION, not move.** The source capability is
recorded only as a delegation-tree **parent edge** and is **never revoked**; the
destination rights **equal** the source rights (no attenuation). The delivery layer
resolves the full freshly-minted capability
(`resolved_capability_split(receiver_cnode, cap)`), compares the full `CapObject` for
identity, and only then attests destination rights against the canonical transfer result.
Live attestation, direct and queued, on all three architectures:
`IPC_ORDINARY_CAP_RIGHTS receiver_tid=<t> dst_rights=Some(<r>) expected_rights=<r>
rights_ok=1 object_endpoint=1 reply_object=0 generation=<g>` and
`IPCSEND_ORDINARY_CAP_RIGHTS_OK arch=<a> class=<class> source_semantics=copy …`.

**Reply-delivery ordering is fixed and load-bearing.** Under the internal
`SpinLock<KernelState>` a delivery copies the reply bytes, **then** transitions the record
`Reserved → Consumed`, **then** — strictly last and non-fallibly — enqueues the caller:

```
reply bytes copied → record Consumed → scheduler enqueue → resumed caller observes
    reply bytes AND a Consumed (non-invokable) record
```

Only the reservation **owner** may consume or release; an alias fails closed. The caller
cannot dispatch before **both** the reply bytes and the `Consumed` barrier are visible;
the enqueue publishes the completed wake state to whichever CPU dispatches.

**The direct-IPC acknowledgement slot is single-outstanding-pair, by design.** The two ack
modules are classified ORACLE-ONLY / SINGLE-OUTSTANDING-PAIR proof infrastructure. On a
**real** build each `publish` carries a fail-closed **overwrite fuse**: it refuses to
overwrite an active (`VALID && !CLAIMED`) acknowledgement — the second-simultaneous-pair
condition — preserving the active ack and marking
`IPCCALL_DIRECT_ACK_OVERWRITE_FUSE` / `IPCREPLY_DIRECT_ACK_OVERWRITE_FUSE`. Endpoint
confinement plus the single provisioning slot already hold the system to one pair; the
fuse is defence in depth and never trips in the sealed flow (`overwrite_fuse_trips=0`).
**Hosted builds keep last-writer-wins**, because the wiring fixtures share the
process-global statics.
The memory-ordering audit confirmed one outstanding pair is handed across CPUs correctly
(Release → Acquire; a monotone `SEQ` defeats stale restore), so no single pair can lose an
ack under valid cross-CPU sequencing. Replacing the slot with an **endpoint-indexed,
generation-bearing bounded store** is required only to support genuine **multi-pair**
production concurrency — it is a prerequisite for concurrent direct IPC, not for the
current sealed flow.

**Retirement-seal isolation model — Model 1, serialized master.**
`scripts/qemu-combined-retirement-seal.sh` runs the first-cohort (12), plain (6) and
ordinary-cap (6) seals **strictly sequentially** — one QEMU at a time — each with a unique
per-run `LOGDIR` and `ORACLE_RUN_ID`, so there is no CPU/memory starvation and no shared
log, artifact or socket contention. Audited shared resources: target dirs (per-arch,
fixed), QEMU serial (per-boot stdio, no sockets), oracle scratch logs (per-invocation
`ORACLE_SCRATCH_DIR`), timeout state (per inner script), manifests (per-arch).
`scripts/qemu-retirement-seal-isolation.sh` runs the combined seal 3× consecutively and
requires each cohort cell exactly once per run, no inner timeout (exit 124), and every
run's `COMBINED_RETIREMENT_SEAL result=ok`, emitting
`RETIREMENT_SEAL_ISOLATION serialized=1 repeated_runs=3 successful_runs=3
contaminated_runs=0 timeout_runs=0 result=ok`.

---

## 9. Authoring rule

Future IPC changes must update this doc and the relevant per-syscall ABI
in `doc/SYSCALL_ABI.md` (or the typed codec in `doc/VFS.md` §6).
Do **not** create new `IPC_*` / `SHARED_IPC_*` / `STAGE_*` fragment files —
`tests/doc_fragmentation_guard.rs` enforces this. Closed
phase / milestone outcomes belong in `doc/PROJECT_HISTORY.md`.
