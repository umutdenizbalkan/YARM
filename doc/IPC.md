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
   with unrelated links created on both sides of the armed transaction.

3. **First ServerDies live cell — x86_64, EARNED.** Historical seal name
   `STAGE_200D2B1C_X86_64_SERVER_DIES_SEAL`; canonically a **Stage 199D server-crash-cleanup
   increment** (it also exercises the reply-object-cleanup element canonical 202D owns).
   Exact commit `f5669cb55325ac58aba6a15207a89c95ad8cad3d`, tree
   `e2fd0b5c7a82dc6c8c422d5c6db242473533a9a6`, one fresh boot, clean tree frozen and
   re-checked after every phase.

   ```
   STAGE_200D2B1C_X86_64_SERVER_DIES_SEAL arch=x86_64
     sha=f5669cb55325ac58aba6a15207a89c95ad8cad3d
     tree=e2fd0b5c7a82dc6c8c422d5c6db242473533a9a6
     live_cells=1 caller_wakes=1 peer_death_winners=1
     exit_returns=0 feature_off_oracle_literals=0 result=ok
   ```

   Live evidence for the accounting model, both tiers:

   * **Scoped vector `[1, 1, 1, 1, 1, 1, 1, 1, 1]`** with `result_before_enqueue=1` —
     `IPC_SERVER_DEATH_TRANSITION_AUDIT`. All nine transitions once, in order.
   * **Quiescent system balance `created=54 closed=54 live_links=0`** —
     `IPC_SERVER_DEATH_LINK_BALANCE_QUIESCENT … scope=system result=ok`. **54 is the exact
     count that used to be reported as the leak** (`created=54 detached=1 result=fail`): the
     links were always being closed, the ordinary terminal path simply never counted its
     closes.
   * Scope armed by observation, not inference — `IPC_SERVER_DEATH_SCOPE_ARMED
     record_index=1 record_generation=17 server_tid=10008 server_asid=1 link_present=1`.
   * **Owner revalidation executed for the first time in QEMU** —
     `EXIT_TASK_OWNER_REVALIDATED arch=x86_64 cpu=0 prepared=idle committed=replacement
     next_tid=1 advances=1 broad_lock=0 result=ok`, and the **replacement return** is
     correct: `EXIT_TASK_COMMON_EPILOGUE_OWNER … owner=replacement clears=1
     frame_committed=1`. This closed the Stage 200D-2B1D4 hang, in which the epilogue
     committed a stale `owner=idle` decision taken before the drains published the wake.
   * `IPC_SERVER_DEATH_TERMINAL_CLAIM terminal=PeerDeath result=won` →
     `IPC_SERVER_DEATH_USER_VALIDATED result=ServerDied code=10 continuations=1`;
     `reply_aliases_invalid=1 reply_copies=0`; survivor and health attested
     (`SURVIVOR_PROGRESS_OK … yields=64`, `SYSTEM_HEALTH_OK`).
   * Zero `result=fail` in the whole 14215-line log; all eighteen forbidden markers absent;
     one boot banner; `caller_wakes=1`, `peer_death_winners=1`.

   **This is one cell on one architecture.** AArch64 and RISC-V ServerDies cells remain
   unearned, and canonical 199D is not sealed.

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

**The direct-IPC acknowledgement store is bounded, endpoint-indexed, generation-bearing
and multi-pair** (Stage 199D; `src/kernel/direct_ack_store.rs`). It replaces the former
single-outstanding-pair slot with the store the Stage 199A2D1 race model specified.
`ipccall_direct_ack` and `ipcreply_direct_ack` are now endpoint-keyed views over one
`DirectAckStore` instance each; neither holds any synchronisation of its own.

*Model.* A fixed array of `DIRECT_ACK_STORE_CAPACITY` slots — no allocation, no unbounded
growth. Each slot runs `Vacant → Reserved → Committed → Consumed`, with `cancel`
(`Reserved → Vacant`) and `restore` (`Consumed → Committed`, same publication only).

* **reserve** binds one slot to one `(endpoint_index, endpoint_generation)` pair and one
  waiter incarnation `{tid, asid}`. It publishes nothing an observer can consume, and it
  is the ONLY refusal point — capacity is refused here, **before** any irreversible
  publication, so a refusal costs nothing to unwind.
* **commit** is the single irreversible publication. It consumes the reservation token by
  value (so one reservation commits at most once) and requires the committed fields to
  name exactly the endpoint and waiter incarnation the reservation was taken for.
* **consume** is the exactly-once ownership transfer, keyed by the EXACT endpoint
  incarnation and optionally by the exact waiter incarnation. Absent, stale-generation,
  foreign-waiter, not-yet-committed and already-consumed attempts are each refused
  fail-closed, counted separately, and mutate nothing.
* **cancel** returns an uncommitted reservation to `Vacant` with every identity wiped —
  a rollback that leaks neither a slot nor a waiter.

*Identity and staleness.* Each slot carries a per-slot generation bumped on every entry
into `Reserved`, so a reservation token that outlived its slot can neither commit nor
cancel someone else's pair. A recycled endpoint slot (same index, newer generation) and a
recycled thread id in another address space are both DIFFERENT incarnations and are
refused.

*Fail-closed refusals.* The former overwrite fuse survives as the refusal of a SECOND LIVE
pair on the SAME endpoint: the already-published acknowledgement is preserved untouched and
`IPCCALL_DIRECT_ACK_OVERWRITE_FUSE` / `IPCREPLY_DIRECT_ACK_OVERWRITE_FUSE` is marked
(`overwrite_fuse_count()`, still 0 in the sealed flow). Capacity exhaustion marks
`IPCCALL_DIRECT_ACK_CAPACITY_REFUSED` / `IPCREPLY_DIRECT_ACK_CAPACITY_REFUSED`. **Hosted
builds keep last-writer-wins** in the `publish` convenience wrapper, because the wiring
fixtures share the process-global stores.

*Concurrency.* Commit publishes the slot state with `Release` after the fields; consume is
an `AcqRel`/`Acquire` compare-exchange; readers `Acquire` the state first. The endpoint
uniqueness decision and the slot allocation are taken together under a leaf admission
guard, because they touch different words and no CAS ordering alone can make them one
decision; every other operation is lock-free. Deterministic hosted races cover two and
`DIRECT_ACK_STORE_CAPACITY` simultaneous pairs, contended consumption, capacity
exhaustion, same-endpoint reservation, stale/foreign consumption, and reserve→cancel
rollback (`stage199d_multi_pair_races`).

**Scope.** This lifts the ack store's own one-pair limit. It does NOT widen who may use
the off-lock NR6/NR7 path: the proof gate and the oracle endpoint confinement are
unchanged, and no production default was flipped.

### 8.6.1 The canonical receiver-visible delivery projection

A sender frames a message one way; a receiver observes another. `ipc_send` / `ipc_call`
prepend a 2-byte little-endian **application opcode** to the payload, because the kernel ABI
carries no opcode lane — so the kernel must un-frame it and report the application opcode
through the recv-v2 metadata instead. Userspace decodes **exclusively** from that metadata
(`ipc_recv_v2` takes `opcode = meta.opcode` and `payload[..meta.payload_len]`), so getting
the projection wrong does not fail loudly: it hands the receiver a payload shifted by two
bytes and an opcode of 0.

`src/kernel/syscall/ipc_recv_core.rs` holds the **single canonical rule**:

* `RecvDelivery` — the receiver-visible projection: `app_opcode`, `app_payload`,
  `stripped_prefix`, `raw_flags`, `sender_tid`.
* `project_recv_delivery(&Message)` / `project_recv_delivery_parts(opcode, flags, sender,
  raw)` — the projection itself; the `_parts` form serves producers that have not
  materialized a `Message` (the off-lock direct NR6 transaction).
* `should_strip_inline_opcode_prefix_parts` — the framing predicate.
  `ipc_abi::should_strip_inline_opcode_prefix` delegates to it, so the header predicate and
  the payload projection cannot drift apart.
* `encode_blocked_waiter_meta` — the blocked-waiter metadata encoder (`status` and
  `msg_flags` are 0), shared by all three blocked-waiter producers.
* `RecvDelivery::reply_cap_recv_meta_flags` — the reply-cap `recv_meta_flags` word.
  `FLAG_CAP_TRANSFER_PLAIN` deliberately does not set it; only `FLAG_REPLY_CAP` denotes a
  reply cap.

**Malformed / too-short disposition.** The prefix is stripped only when the sender framed
one AND the raw payload is at least 2 bytes. A framed message with a shorter payload has no
prefix to read, so the projection falls back to the sender's own `opcode` and the verbatim
payload rather than fabricating an opcode from a truncated read.
`RecvDelivery::inline_prefix_malformed()` names that case. This is the historical behaviour
of every legacy site and is part of the frozen contract — it must not be turned into a
rejection.

**Producers that project through it:** the four blocked-waiter completions in `syscall.rs`,
the immediate full-recv path in `syscall/ipc.rs`, the deferred reply-cap producer, and the
off-lock direct NR6 transaction.

**Stage 199D conformance record.** The direct NR6 transaction previously delivered the raw
wire frame and reported `OPCODE_INLINE` with the unstripped length — the oracle's contract,
not the production one, and the oracle server reparsed the prefix itself so nothing caught
it. It now projects the header words it *would* have framed
(`OPCODE_INLINE` + `FLAG_REPLY_CAP`), copies `delivery.app_payload`, and encodes through the
shared blocked-waiter encoder, making its delivery byte-identical to the legacy one. The
oracle server was rewritten to consume the ordinary production `ipc_recv_v2` contract with
no manual prefix reparse.

Proved by `stage199d_delivery_projection_differential`: the same message fed through BOTH
deliveries — a real `IpcCall` trap for the legacy side, `drain_direct_request_post_work` for
the direct side — with every receiver-visible field compared (`status`, `opcode`, `flags`,
`payload_len`, `cap_id`, `recv_meta_flags`, `sender_tid`), the payload bytes read back from
the receiver's own address space, and the reply-cap identity. Cases: empty application data,
nonzero opcode, zero opcode, the maximum framed inline payload (126 data bytes in a 128-byte
frame), and the malformed too-short prefix.

**The NR7 reply direction was already conforming** — reply messages carry no opcode prefix,
so its `OPCODE_INLINE`/verbatim encoding already matched `Message::new`.

#### Replacement live seal (current)

The NR6/NR7 direct round trip has been re-earned twice: once for delivery conformance
(HARD-STOP A) and again for the successful-return ABI (HARD-STOP C, which changed the NR6
`ret2` lane). The seal below is the current one.

| Field | Value |
|---|---|
| Seal | `STAGE_199_IPCCALL_REPLY_DIRECT_LIVE_SEAL arch=x86_64 classes=2 live_cells=2 duplicate_replies=0 duplicate_wakes=0 result=ok` |
| Exact commit | `a4bb63e3e83e93ecc3e9e33582e493c8b37c33fe` |
| Exact tree | `2f0fddddfccd2b5018c761a32967804d8f64ea95` |
| Runner | `scripts/qemu-ipccall-reply-direct-x86_64-smoke.sh`, QEMU 8.2.2 TCG, `-smp 1`, `yarm.x86_64_ipccall_direct_oracle=1` |
| Supersedes | `458bb3d4` (delivery conformance, pre-`ret2`-parity) and `2c07ac96` (pre-conformance NR6 delivery) |

**Return-lane parity attested live** — the successful NR6 call returned the legacy
transfer-cap sentinel, not 0:

```
IPCCALL_DIRECT_ORACLE_CLIENT_CALL_RET2 ret2=18446744073709551615
    expected=18446744073709551615 ret2_ok=1 result=ok
```

`18446744073709551615` is `u64::MAX` = `SYSCALL_NO_TRANSFER_CAP`. The round-trip completion
is gated on this attestation, and the runner requires the marker with that exact value.

Conformance evidence from that boot log — the two numbers that were wrong before:

```
IPCCALL_DIRECT_ORACLE_SERVER_RECV opcode=1543 opcode_ok=1 data_ok=1 plen=8 reply_cap=1114117 reply_cap_ok=1
```

`opcode=1543` is `0x0607`, the **application** opcode (it was `0` before the fix), and
`plen=8` is the **stripped** application length (it was `10`, the framed length). The
`framed_ok=` marker of the old reparsing server is absent from the log entirely. Both kernel
classes and the userspace completion appear exactly once:

```
IPCCALL_DIRECT_REQUEST_OK arch=x86_64 source_copy_offlock=1 reply_cap=1 server_wakes=1
IPCREPLY_DIRECT_OK       arch=x86_64 source_copy_offlock=1 caller_wakes=1 one_shot=1
X86_IPCREPLY_DIRECT_SEND attempts=1 early_retries=0 result=ok
X86_IPCCALL_DIRECT_ROUNDTRIP_DONE request_ok=1 reply_ok=1 duplicate_reply=rejected
    server_wakes=1 caller_wakes=1 client_continuations=1 server_continuations=1 result=ok
YARM_BOOT_OK present_cpus=1 present_bitmap=0x1 online_cpus=1
```

The seal covers the NR6/NR7 **oracle-confined** round trip only. It is **not** a Stage 199D
stage seal, and no production default was flipped.

### 8.6.2 The direct-transaction disposition contract

Every direct NR6/NR7 outcome is classified, never discarded. `src/kernel/direct_disposition.rs`
maps each transaction result onto exactly one of:

* **`Completed`** — keep the existing successful frame encoding;
* **`DeclinedBeforeMutation`** — nothing was delivered and nothing observable was left
  behind, so the split helper returns `None` and the legacy path runs;
* **`Failed(SyscallError)`** — the legacy path must NOT run; the canonical error is encoded
  with `frame.set_err(err.code())`, byte-for-byte how the global-lock handler encodes a
  `SyscallError`.

The mapping is pure and **exhaustive with no wildcard arm**, so a new error variant is a
compile error until its disposition is decided. Fallback is admissible only when all six of
these hold of the state the transaction leaves: no reply record reserved or committed, no
capability minted or installed, no user payload/meta copied, no waiter or run-queue change,
no acknowledgement committed as a delivery, no wake published. Any *attempted* user copy
disqualifies fallback — a faulted copy may have written a prefix of its bytes.

Past the publication line — the endpoint waiter claimed, the request bytes in the receiver's
address space, or the receiver committed `Runnable` — every variant is `Failed`
unconditionally, because running legacy afterwards would deliver the same message twice.

The reply direction's `WaiterLost` was split into `WaiterLost` (the pre-reserve, pre-copy
check — fallback-safe) and `WaiterLostAfterCopy` (the claim, which runs after both copies
have landed — never a fallback), because one variant cannot carry two post-states.

Full per-variant tables, the legacy equivalent of each, and the cases where honest parity
does not exist are in `doc/KERNEL_UNLOCK_AUDIT.md` §6.1.2.

### 8.6.3 The successful-return lane contract

Legacy `handle_ipc_call` **and** `handle_ipc_reply` both end their success path with

```text
frame.set_ok(0, 0, 0);
encode_transfer_cap_ret(frame, None)?;   // → set_ret2(SYSCALL_NO_TRANSFER_CAP)
```

so a successful NR6 or NR7 returns `error = 0`, `ret0 = 0`, `ret1 = 0`,
`ret2 = SYSCALL_NO_TRANSFER_CAP` (`u64::MAX`) — *not* zero. The direct path wrote
`set_ok(0, 0, 0)` alone, leaving `ret2 = 0`: a silent divergence on the **successful** path,
in both directions (the NR7 audit did not confirm the prior assumption that it already
matched).

`direct_disposition::apply_direct_disposition` is now the single frame-encoding site for both
directions:

* **Completed** → `set_ok(0, 0, SYSCALL_NO_TRANSFER_CAP as usize)`, taking the sentinel from
  the same constant `encode_transfer_cap_ret` writes rather than duplicating the literal;
* **Failed** → `set_err`, which zeroes `ret0`/`ret1`/`ret2` before setting the code, so a
  failure can never leave a stale success value in any lane — including the sentinel;
* **DeclinedBeforeMutation** → nothing is written; the legacy fallback gets a pristine frame,
  arguments and syscall number included.

Neither split helper writes a return lane after the drain. Proved by
`stage199d_delivery_projection_differential::injection`: a successful legacy `IpcCall` trap
supplies the empirical lane baseline and every lane is compared against the production
encoder's output (fed a poisoned frame first, so parity cannot be an accident of zeroes); the
shared legacy encoding is pinned at source level for both handlers; every `Failed` arm is
asserted to clear all three lanes starting from a full success frame; and a decline is
asserted to modify nothing. Attested live — see the seal above.

The endpoint confinement remains load-bearing for scope, and the proof gate remains
proof-only; no production default has been flipped. See `doc/KERNEL_UNLOCK_AUDIT.md` §6.1.

### 8.6.4 Direct-IPC eligibility and observability

**Eligibility** (`src/kernel/direct_eligibility.rs`) is one pure, exhaustive classification
over facts the call site gathers — no wildcard arm, so a new decline reason cannot be added
without deciding what it means.

NR6 is eligible only when the send cap resolves **with `SEND` rights**, names an `Endpoint`,
the endpoint **incarnation is current** (slot occupied and generation matched), the mode is
`Buffered`, and the message shape is one the transaction supports. **`Synchronous` endpoints
decline before any mutation** and fall through to the legacy rendezvous path — the direct
transaction claims a waiter and delivers straight into the receiver's address space, and does
not reproduce the scheduling-level rendezvous. The mode check runs *before* the confinement
check, so the decline stays meaningful once the confinement is eventually removed.

NR7 eligibility is tied to a live one-shot `Reply` object and its exact caller /
reply-endpoint incarnation, with **no `EndpointMode` requirement**: NR7 does not send to an
endpoint, so the endpoint's queueing discipline never applies. The facts type has no place to
express a mode, and a test pins that the classifier never mentions one.

**Counters** (`src/kernel/direct_ipc_counters.rs`) are per-direction and observational —
relaxed atomics incremented after a decision is already made, never read back into one. The
terminal-bucket invariant is:

```text
attempts = declined_preflight        (ineligible)
         + declined_pre_transaction  (eligible, but no ack / copy fault / no snapshot)
         + completed
         + failed (Σ failed_by_error_code)
         + legacy_fallback_after_decline
```

with `eligible = attempts - declined_preflight` and `declined_ineligible_mode` the
`Synchronous` subset of the preflight declines. The acknowledgement store reports
reserve/commit/consume/cancel, live occupancy, an occupancy high-watermark bounded by
`DIRECT_ACK_STORE_CAPACITY`, and every fail-closed fuse (capacity refusal, overwrite fuse,
stale / foreign / duplicate / not-committed consumes).

The attestation is emitted from the existing off-lock `DebugLog` observation point — no new
emission site — and is bounded to **two latched samples per direction**: a `first` census as
soon as that direction is attempted at all (the only sample an ordinary confinement-declining
boot gets), and a `settled` sample once that direction produced a terminal beyond a preflight
decline. The latches are per-direction because a reply necessarily follows its request.

Observed on a live oracle boot:

```
IPC_DIRECT_PATH_COUNTERS phase=settled dir=nr6 attempts=55 eligible=1
    declined_ineligible_mode=0 declined_preflight=54 declined_pre_txn=0 completed=1
    failed=0 legacy_fallback=0 balanced=1
IPC_DIRECT_PATH_COUNTERS phase=settled dir=nr7 attempts=55 eligible=2
    declined_ineligible_mode=0 declined_preflight=53 declined_pre_txn=1 completed=1
    failed=0 legacy_fallback=0 balanced=1
IPC_DIRECT_ACK_COUNTERS phase=settled dir=nr6 reserve=1 commit=1 consume=1 cancel=0
    live=0 high_watermark=1 capacity=8
```

The 54/53 preflight declines are the ordinary service chain being turned away by the endpoint
confinement — exactly the census a production audit wants. The single NR7
`declined_pre_txn` is the oracle server's bounded pre-acknowledgement retry, and the one
`duplicate` fuse trip on the NR7 store is the oracle's deliberate duplicate-reply rejection.

**`declined_pre_transaction` exists because a live boot found it missing.** The first
attestation showed NR7 with `eligible=2` but only one terminal, `balanced=0`: eligible
attempts that stopped at the ack claim were reaching no bucket at all. The invariant caught a
real hole in its own model — the kind of thing only a real boot produces.

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

### 8.6.5 The x86_64 production-default enablement — HELD OFF

Every dependency on the proof oracle has been removed from admission, eligibility and
acknowledgement publication. The enablement is now a single expression:

```rust
// src/kernel/boot/mod.rs
pub const fn ipccall_direct_production_enabled() -> bool { false }
//                                                        ^^^^^ → cfg!(target_arch = "x86_64")
```

It is **deliberately not flipped**. The flip was implemented, built and run on a normal
feature-off x86_64 boot (no direct-IPC oracle knob), and hard-stopped on a service-chain
regression. The A/B is exact — same tree, same artifacts, one constant:

| `ipccall_direct_production_enabled()` | `scripts/qemu-x86_64-core-smoke.sh` |
| --- | --- |
| `false` (held off) | `all 6 service entries present exactly once` |
| `cfg!(target_arch = "x86_64")` | 6 service entries **missing**, `PM_ELF_ZC_FAIL`, boot times out (status=124) |

Four defects, all masked until now by the oracle's endpoint confinement:

**1. Capability transfer is silently dropped — this is the one that breaks the boot.**
Legacy `ipc_reply` reads `SYSCALL_ARG_TRANSFER_CAP`, validates it and stashes a transfer
handle so the caller's `recv` installs the capability. The direct NR7 path
(`try_split_ipcreply_direct_into_frame`) reads only `CAP`/`PTR`/`LEN`; neither the
eligibility contract nor `ipccall_direct_txn` mentions a transferred capability anywhere.
A cap-bearing reply taken by the direct path therefore loses the capability. Live chain:

```
USER_LOG tid=3 msg=PM_VFS_REPLY_FULL op=25 len=12 transferred_cap=0
USER_LOG tid=3 msg=PM_ELF_ZC_FAIL image_id=7 reason=grant_ro_unsupported
USER_LOG tid=3 msg=PM_VFS_SPAWN_FAIL_DETAIL image_id=7 site=mo_create err=grant_ro_unsupported
```

VFS's read-only memory-object grant never reaches PM, so blkcache, virtio-blk and the
driver manager never spawn. **Fix direction:** the eligibility contract must decline any
reply carrying a transfer cap (mutation-free, falls through to legacy) — or the transaction
must reproduce the stash. Declining is the smaller, provable step.

**2. The acknowledgement store has no production release path.** A pair is published
whenever a task commits a blocking recv-v2, and cleared only by a direct delivery's
`consume`. Publication is driven by *blocking*; consumption is driven by *direct delivery* —
the two are not paired. Any recv satisfied by the legacy path, a timeout, or server death
orphans a `Committed` slot permanently. `release_endpoint_index` exists but has no
production caller; under confinement one endpoint published and always consumed, so the
gap never showed.

**3. Orphans trip the overwrite fuse.** Re-blocking on an endpoint that still holds an
orphan is refused as `EndpointAlreadyLive`: **17 trips on one short boot** (7
`IPCCALL_DIRECT_ACK_OVERWRITE_FUSE slot=server`, 10 `IPCREPLY_DIRECT_ACK_OVERWRITE_FUSE
slot=caller`).

**4. Capacity pressure is structural.** `DIRECT_ACK_STORE_CAPACITY` is 8, and a normal boot
keeps more than 8 servers blocked in recv-v2 at once. Once the store fills, publication is
refused and the direct path degrades to permanent legacy fallback with no signal but the
counter.

Blockers 2–4 are one defect's symptoms (an unpaired lifecycle). Blocker 1 is independent.

**The quiescent attestation is what caught this**, and it is retained. It fires once, from
the off-lock `DebugLog` point, only after `INIT_SPAWN_V5_REPLY_RECV_OK` establishes that the
service chain is healthy, and checks eleven invariants per direction: attempts equal all
terminal buckets; `completed > 0`; no confinement decline; no fallback after a terminal;
ack live occupancy zero; `reserve = commit + cancel`; `commit = consume`; high-watermark
within capacity; and every fuse clear. On the attempted flip it reported, correctly:

```
IPC_DIRECT_PRODUCTION_QUIESCENT dir=nr6 attempts=2 completed=1 failed=0 preflight=0
    pre_txn=1 fallback=0 not_admitted=0 terminals=1 eligibility=1 completed_gt0=1
    no_confinement=1 no_late_fallback=1 result=fail
IPC_DIRECT_PRODUCTION_ACK_QUIESCENT dir=nr6 reserve=4 commit=4 consume=1 cancel=0 live=3
    high_watermark=3 capacity=8 live_zero=0 reserve_resolves=1 commit_consumed=0
    watermark_ok=1 fuses_clear=0
IPC_DIRECT_PRODUCTION_QUIESCENT_SEAL nr6_ok=0 nr7_ok=0 result=fail
```

The direction-level counters are healthy — `not_admitted=0` proves the confinement really
is gone, and `completed=1` proves ordinary feature-off production traffic took the direct
path. It is the **ack lifecycle** that fails: `commit=4 consume=1 live=3`.

**No production-default live seal is issued.** AArch64 and RISC-V are untouched and remain
proof-gated. The oracle regression (`live_cells=2 result=ok`) and the feature-off boot both
pass with the flip held off.

### 8.6.6 Transfer-cap safety and the authoritative acknowledgement lease

Blockers 1–3 of §8.6.5 are closed. Blocker 4 (capacity) remains, and the flip stays held off.

#### Transfer-cap safety (blocker 1)

`DirectReplyFacts` now carries `transfer_cap_present`, and
`DirectReplyEligibility::TransferCapUnsupported` declines a cap-bearing reply **before any
mutation**, falling through to the legacy path which does the transfer. Direct capability
transfer is still unimplemented; declining is the entire fix.

Three properties make this structural rather than careful:

* **One presence predicate.** `ipc_abi::transfer_cap_arg_present` is the single place the
  `SYSCALL_NO_TRANSFER_CAP` sentinel is tested, and the legacy `transfer_cap_arg` decode is
  built on it. So the direct contract and `ipc_reply` cannot disagree about what "cap-bearing"
  means — including the trap that a raw `0` **is** a capability id, not an absence.
* **The check precedes every capability resolution.** It runs before the reply object is
  resolved, so a cap-bearing reply never reaches an endpoint incarnation, an acknowledgement
  claim, a payload copy, or the transaction. Only the two checks that need no capability at all
  (payload length, requester availability) outrank it.
* **The transaction has no transfer machinery at all.** `ipccall_direct.rs` and
  `ipccall_direct_txn.rs` contain no `transfer_cap`, `stash_transfer_handle` or
  `validate_transfer_cap` — which is *why* the decline is necessary rather than merely prudent.

NR6 needs no equivalent: `IpcCall` **repurposes** the `SYSCALL_ARG_TRANSFER_CAP` slot for the
reply-endpoint receive capability, and the direct request path reads that slot the same way
`handle_ipc_call` does, so no capability transfer is in flight on the request side.

#### The acknowledgement lease is owned by the endpoint waiter lifecycle (blockers 2–3)

Publication was driven by *blocking* and consumption by *delivery*, and the two were never
paired. The lease now ends when its waiter is removed, for any reason.

`DirectAckStore::release(endpoint, waiter)` is the non-direct terminal edge. It adds a fourth
slot state, `Released` — terminal, spent, never consumed — and is exact in all four components
(endpoint index, endpoint generation, waiter TID, waiter ASID). Only `Released(seq)` mutates:
`Absent`, `StaleGeneration`, `ForeignWaiter`, `NotCommitted`, `AlreadyConsumed` and
`AlreadyReleased` all leave the slot untouched.

**Centralized in the waiter primitives, not at the terminal branches.** Every canonical closing
edge in the kernel — direct consume, legacy delivery, timeout, cancellation, task/server death,
endpoint destruction, stale waiter cleanup — funnels through exactly three `IpcSubsystem`
methods:

| primitive | closing edges it serves |
| --- | --- |
| `take_endpoint_waiter` | direct claim, `wake_waiter_for_endpoint`, `ipc_reply` wake plan, shared-region claim |
| `clear_endpoint_waiter_if_identity` | legacy split-send delivery, recv-v2 finalize, sync rendezvous handoff |
| `clear_endpoint_waiters_for_identity` | task/server death, timeout batch, cancellation, stale cleanup |

`IpcSubsystem::release_direct_ack_lease` is called from those three and nowhere else, reading
the live `endpoint_generations` entry so the release is generation-bearing. A guard pins that no
terminal branch anywhere releases a lease by hand: coverage is a property of the structure, and
a new removal path gets it for free.

**The two terminal edges are mutually exclusive.** `consume` refuses a `Released` slot
(`AckConsume::AlreadyReleased`, counted as a crossed terminal) — which is what stops a delivery
into a departed waiter's buffers. `release` on a `Consumed` slot reports `AlreadyConsumed` and
retires nothing; that ordering is the ordinary trace of a successful direct delivery, counted
separately as `release_after_consume`. A deterministic 200-run race pins that exactly one of the
two mutates, in either order.

`release` takes the leaf admission guard, because it must be excluded against slot **recycling**:
a release that observed a slot as `Committed` must never retire a different pair that was
reserved and re-committed into that slot meanwhile. The common case — a removal on an endpoint
that never published — is resolved lock-free before the guard is taken, so ordinary waiter
removals pay nothing. A second 200-run race pins that a stale release cannot touch a recycled
pair.

#### Live evidence, feature-off x86_64 boot, flip temporarily enabled

| | first attempt (§8.6.5) | with this increment |
| --- | --- | --- |
| service entries | 6 **missing**, boot timed out | **all 6 present exactly once** |
| `PM_ELF_ZC_FAIL` | 1 (`grant_ro_unsupported`) | **0** |
| overwrite fuse | **17** trips | **0** |
| leases retired by their waiter | n/a (no release edge) | NR6 **52**, NR7 **64** |
| cap-bearing replies declined | n/a (silently robbed) | **10** |

```
IPC_DIRECT_PRODUCTION_QUIESCENT dir=nr6 attempts=54 completed=53 failed=0 preflight=0
    pre_txn=1 fallback=0 not_admitted=0 transfer_cap=0
IPC_DIRECT_PRODUCTION_ACK_QUIESCENT dir=nr6 reserve=113 commit=113 consume=53 release=52
    rel_after_consume=53 cancel=0 live=8 spent=0 high_watermark=8 capacity=8
IPC_DIRECT_PRODUCTION_QUIESCENT dir=nr7 attempts=53 completed=41 failed=0 preflight=10
    pre_txn=2 fallback=0 not_admitted=0 transfer_cap=10
IPC_DIRECT_PRODUCTION_ACK_QUIESCENT dir=nr7 reserve=113 commit=113 consume=41 release=64
    rel_after_consume=41 cancel=0 live=8 spent=0 high_watermark=8 capacity=8
```

**The genuine post-release high-watermark is 8 — the full capacity — and there is exactly one
`CAPACITY_REFUSED` per store.** The eight live leases are **not** orphans; the accounting is
exact in both directions:

```text
reserve == consume + release + cancel + live
    nr6:  113 == 53 + 52 + 0 + 8
    nr7:  113 == 41 + 64 + 0 + 8
```

Ten distinct servers blocked in recv-v2 over the boot, and at `INIT_IDLE_PARK_BEGIN` the
resident services (PM, VFS, ramfs, ext4, blkcache, virtio-blk, driver manager, init) are all
parked holding legitimate leases. The store is simply **saturated**: a ninth parked server gets
no lease and its endpoint falls back to legacy. That is blocker 4 exactly, and resizing is out
of scope for this increment.

#### Two measurement corrections the live boot forced

* **The quiescent trigger moved to `INIT_IDLE_PARK_BEGIN`.** At the previous trigger
  (`INIT_SPAWN_V5_REPLY_RECV_OK`) the boot measured `high_watermark=2` and then went on to
  saturate all eight slots — an early sample reported as a settled one. The bounded
  per-direction census still fires at first use, so a boot that never reaches the park loses no
  diagnostic.
* **`live == 0` is not a valid quiescence requirement** for a running microkernel: a healthy
  system always has its resident services parked in recv-v2, each holding a legitimate lease.
  It is still reported, but `QuiescentVerdict::ok` now requires `no_orphaned_lease`
  (`reserve == consume + release + cancel + live`) — the leak invariant that holds at any
  instant — instead of `ack_live_zero` and `terminal_edges_balance`.

Kernel log lines are capped at `PRB_MSG_MAX = 192` bytes and truncate silently, so the quiescent
attestation is split into four lines per direction; the first version clipped its own verdict
flags.

### 8.6.7 The structural capacity, the waiter census, and the production default

Blocker 4 is closed and the x86_64 production default is **ON**.

#### Capacity is derived, not chosen

`DIRECT_ACK_STORE_CAPACITY` is now `crate::kernel::boot::ENDPOINT_WAITER_SLOTS` — the length of
the authoritative endpoint receive-waiter table — with two compile-time assertions pinning
`>=` and `==` against it. A lease exists exactly while an endpoint receive-waiter does, so the
bound is not merely sufficient, it is exact. The magic 8 is gone and no number was read off a
boot to replace it.

The store is now **one slot per endpoint index**: the slot's position *is* the endpoint index.
That makes several previously-defended properties structural instead:

| property | before | now |
| --- | --- | --- |
| endpoint uniqueness | leaf admission spinlock over check+allocate | impossible — an index has one slot |
| capacity exhaustion | real, and hit on a live boot | impossible for a valid index |
| reserve | lock + scan + allocation policy | one compare-exchange on one word |
| release vs. slot recycling | needed the admission guard | no slot is ever reused by another endpoint |
| lookup | linear scan | `slots[endpoint_index]` |

The admission spinlock is gone, so **every** store operation is lock-free. Occupancy is
maintained incrementally (a scan would be O(table) on the blocking-recv path);
`live_pair_count_scan` is the independent derivation the tests compare it against on every
transition. `AckReserveError::CapacityExhausted` survives only as a fail-closed guard for an
endpoint index outside the table, which cannot arise.

#### The independent waiter census

`src/kernel/direct_ack_census.rs` measures the same population from the other side. The store
can balance its own books while being wrong — capacity 8 did exactly that, refusing a ninth
parked server a lease while every one of its counters read clean — so the census reads the
endpoint receive-waiter table instead, and **is not bounded by the store's capacity**. A guard
pins that it never references the store it audits.

* **Running census** — current and high-watermark endpoint receive-waiter counts, maintained by
  the waiter table's own mutators (one link site, and the same three removal primitives that
  retire the lease), so the watermark is a true peak rather than whatever a log happened to
  sample.
* **Final bijection** — two passes over the endpoint table at `INIT_IDLE_PARK_BEGIN`, matching
  live leases against eligible waiters on the complete
  `{endpoint_index, endpoint_generation, waiter_tid, waiter_asid}` identity. Eligibility is
  re-derived from committed state (`recv_abi == RecvV2`, non-null payload and metadata
  destinations, endpoint admitted), not remembered, so a lease issued against a waiter that
  never qualified is detectable. It runs on the IPC (rank 3) and task (rank 2) **split seams,
  one index at a time and never simultaneously** — it adds no broad-lock acquisition, and
  `tests/broad_lock_census_guard.rs` enforces that.

`LeaseBijection::ok()` requires equal populations *and* zero violations: a count-only check
would miss a store that held one extra lease and dropped another waiter's.

#### The flip is healthy — and HELD OFF on a fifth blocker

With `ipccall_direct_production_enabled()` temporarily enabled, the normal feature-off x86_64
boot is fully healthy:

```text
YARM_BOOT_OK present_cpus=1 present_bitmap=0x1 online_cpus=1
[ok] x86_64 core smoke: all 6 service entries present exactly once
[ok] Phase 3B: PM_ELF_ZC_FAIL count=0

IPC_DIRECT_PRODUCTION_QUIESCENT dir=nr6 attempts=54 completed=53 failed=0 preflight=0
    pre_txn=1 fallback=0 not_admitted=0 transfer_cap=0
IPC_DIRECT_PRODUCTION_QUIESCENT_FLAGS dir=nr6 terminals=1 eligibility=1 completed_gt0=1
    no_confinement=1 no_late_fallback=1 result=ok
IPC_DIRECT_PRODUCTION_ACK_QUIESCENT dir=nr6 reserve=114 commit=114 consume=53 release=52
    rel_after_consume=53 cancel=0 live=9 spent=3 high_watermark=9 capacity=256
IPC_DIRECT_PRODUCTION_ACK_FLAGS dir=nr6 live_zero=0 reserve_resolves=1 no_orphan=1
    terminal_edges=0 exclusive=1 watermark_ok=1 fuses_clear=1
IPC_DIRECT_PRODUCTION_QUIESCENT dir=nr7 attempts=53 completed=41 failed=0 preflight=10
    pre_txn=2 fallback=0 not_admitted=0 transfer_cap=10
IPC_DIRECT_PRODUCTION_ACK_QUIESCENT dir=nr7 reserve=114 commit=114 consume=41 release=64
    rel_after_consume=41 cancel=0 live=9 spent=0 high_watermark=9 capacity=256
IPC_DIRECT_WAITER_CENSUS dir=nr6 waiters_current=10 waiters_high_watermark=10
    ack_capacity=256 eligible=9 live_leases=9
IPC_DIRECT_WAITER_BIJECTION dir=nr6 waiters_without_lease=0 leases_without_waiter=0
    identity_mismatch=0 generation_mismatch=0 duplicate_incarnation=0 result=ok
IPC_DIRECT_WAITER_BIJECTION dir=nr7 ... result=ok
IPC_DIRECT_PRODUCTION_QUIESCENT_SEAL nr6_ok=1 nr7_ok=1 census_ok=1 result=ok
```

**53 NR6 and 41 NR7 ordinary syscalls were serviced entirely off-lock** — the split-dispatch
counts (`YARM_LOCK_SPLIT_DISPATCH nr=6` ×53, `nr=7` ×41) match `completed` exactly, with **zero**
broad-lock NR6/NR7 entries. Zero capacity refusals, zero overwrite-fuse trips, zero stale,
foreign, duplicate or crossed terminals, and an exact lease/waiter bijection in both directions.
The x86 direct NR6/NR7 oracle regression passes with the flip on (`live_cells=2 result=ok`).

The ten `transfer_cap` declines are cap-bearing replies correctly staying on the mutation-free
legacy path — the population that used to be silently robbed.

Two readings that are *not* violations and are reported rather than gated: `live=9` (nine
resident services legitimately parked in recv-v2, each holding a valid lease) and
`terminal_edges=0` (the same fact). `no_orphan=1` —
`reserve == consume + release + cancel + live`, exactly 114 == 53+52+0+9 and 114 == 41+64+0+9 —
is the leak invariant that holds on a running system. The census independently confirms it:
10 waiters, 9 eligible, 9 leases, zero mismatches.

**No seal is issued, and the constant is restored to `false`, because the ServerDies regression
fails.** `SharedKernel::register_server_reply_link_split` — the direct NR6 transaction's
reverse-link installation — writes `tcb.server_reply_link` but does **not** stamp
`server_dies_counters::note_link_created`, while its legacy twin
`KernelState::register_server_reply_link` does. With the direct path as the production default,
every request installs a link the system-wide leak accounting never counts as created while the
close edge still counts:

```text
IPC_SERVER_DEATH_LINK_LEAK created=0 closed=13 scope=system result=fail
STAGE_200D2B1C_X86_64_SERVER_DIES_SEAL arch=x86_64 result=fail
```

The links themselves are installed and closed correctly — this is an instrumentation gap in the
split twin, not a link leak. It is not cosmetic: while it is open, the attestation that would
detect a *real* reverse-link leak is blind on the production path, which is precisely what the
ServerDies cell exists to guarantee. The fix is to stamp the creation edge in the split twin as
the legacy one does and re-run the regression.

### 8.6.8 Reverse-link creation parity — and the close edge that mirrors it

**Creation parity is done.** `KernelState::register_server_reply_link` and
`SharedKernel::register_server_reply_link_split` carried independent copies of the same
installation decision and drifted: the split twin installed the link without stamping
`server_dies_counters::note_link_created`, while the legacy one stamped it. Both now delegate to
`boot::install_server_reply_link` — one status gate, one set of match arms, one stamp — so the
accounting is identical by construction rather than by inspection.

| case | installs | returns | stamps |
| --- | --- | --- | --- |
| no link present | yes | `true` | **yes** |
| identical link already present (duplicate retry) | no | `true` | no |
| a DIFFERENT live link present | no | `false` | no |
| incarnation has committed to exit | no | `false` | no |

A missing or foreign TCB never reaches the decision — both callers resolve the exact
`{tid, asid}` incarnation first. The stamp sits in the `None =>` arm *after* the write, so it is
unreachable without a genuine installation; guards pin that the stamp exists exactly once in the
tree, follows the write, and is preceded by every refusal arm.

Live effect: `created` went from **0 to 54** on the ServerDies boot.

#### Close parity, delivered the same way

All four closing paths — `KernelState::detach_server_reply_link_exact`,
`KernelState::take_server_reply_link`, `SharedKernel::unregister_server_reply_link_split` and
the reply-timeout domain's `rt_detach_server_link` — now delegate to
`boot::close_server_reply_link`. Two of the four used to remove links without stamping at all,
which is why unifying creation produced `created=54 closed=13`.

| selector | used by |
| --- | --- |
| `Exact { record_index, record_generation }` | every ordinary terminal close |
| `Any` | the exiting-server take path, and only it |

| case | removes | outcome | stamps |
| --- | --- | --- | --- |
| `Exact`, link matches the record | yes | `Closed(link)` | **yes** |
| `Any`, a link is present | yes | `Closed(link)` | **yes** |
| no link present (absent, or a repeat) | no | `AlreadyAbsent` | no |
| `Exact`, same slot at a later generation | no | `StaleRecordGeneration` | no |
| `Exact`, a different record is live | no | `DifferentLiveLink` | no |

The stamp follows a genuine `Some(link) → None` mutation and is attributed by the **removed**
link's record identity, not by the selector, so the `Any` path reports the record it actually
closed. Every caller's return contract is preserved exactly, including the split seam's
defensive server re-verify, which stays at that call site. There is now exactly one link-close
mutation and one `note_link_closed` call in the tree, mirroring the one install mutation and one
`note_link_created` call; guards pin both.

Live: `IPC_SERVER_DEATH_LINK_BALANCE_QUIESCENT created=54 closed=54 live_links=0 scope=system`,
with the ServerDies scoped vector `[1, 1, 1, 1, 1, 1, 1, 1, 1]`, one PeerDeath winner and one
caller wake.

### 8.6.9 Terminal-arbitrated replies are ineligible — and the production default is ON

A reply whose record is arbitrated by an armed terminal-ownership / reply-timeout cell must
reserve the terminal *before* its caller copy and commit it *after*, so a concurrent timeout
claimant provably loses. That lease — `reserve_reply_win_before_copy` → delivery →
`commit_reply_win_after_delivery`, with `rollback_reply_win` on a retryable fault — lives only
on the legacy reply path. Servicing an arbitrated reply off-lock therefore lost the race the
caller was promised: the reply reserved, rolled back, and the timeout's deferred path completed.

Rather than port the lease, the arbitrated population is now **explicitly ineligible** for
direct NR7. Porting it is recorded as future canonical 199E work.

#### The fact is canonical and exact

`SharedKernel::reply_record_terminal_arbitrated_split_read(index, generation)` reads the
authoritative `reply_terminal_ownership` cell — co-located with, and indexed identically to,
`reply_caps`. Never an oracle selector, a marker or a counter. A cell arbitrates this reply only
when its epoch is non-zero (a vacant cell is epoch 0 with `TerminalIdentity::ZERO`) **and** its
immutable identity names this exact record index **and** generation, so a cell armed for a
previous occupant of a recycled slot correctly arbitrates nothing.

**It is not a TOCTOU test**, for two independent reasons:

1. **Internal consistency.** The record generation and the terminal cell are read under ONE
   rank-3 acquisition, so the pair cannot be torn — the generation cannot advance between
   reading it and reading the cell armed for it.
2. **Arming strictly precedes reply deliverability.** The cell is armed by
   `maybe_arm_reply_timeout_oracle` at the caller's blocking-recv publication
   (`IPC_RECV_BLOCK_REGISTER`), which happens *before* the blocked-caller acknowledgement is
   published at `IPC_RECV_BLOCKED_STATE_SAVE`. The direct NR7 path cannot reach eligibility for
   a record whose caller has not yet published that acknowledgement. A cell therefore cannot go
   unarmed → armed for this incarnation between the read and the transaction: either the arming
   already happened, or the reply is not deliverable yet at all.

#### The decline precedes every mutation

`DirectReplyEligibility::TerminalArbitrationUnsupported` is returned as soon as the record
identity is known — before the endpoint is even resolved, and therefore before the
acknowledgement claim, any record reservation or consumption, the payload/meta copy, any waiter
mutation or wake, the reverse-link close, and any direct transaction call, all of which happen
only on the `Eligible` arm. It carries no error and no endpoint, so the legacy path returns the
real result. Its own counter, `declined_terminal_arbitration`, is a breakdown of
`declined_preflight` rather than a new bucket, so the terminal-balance invariant is untouched.

#### FIRST x86_64 NR6/NR7 PRODUCTION-DEFAULT LIVE SEAL

Exact commit `0b5ec254`. `ipccall_direct_production_enabled()` is
`cfg!(target_arch = "x86_64")`.

**1. Normal feature-off x86 core boot**

```text
YARM_BOOT_OK present_cpus=1 present_bitmap=0x1 online_cpus=1
[ok] x86_64 core smoke: all 6 service entries present exactly once
[ok] Phase 3B: PM_ELF_ZC_FAIL count=0
IPC_DIRECT_PRODUCTION_QUIESCENT dir=nr7 attempts=53 completed=41 failed=0 preflight=10
    pre_txn=2 fallback=0 not_admitted=0 transfer_cap=10 arbitrated=0
IPC_DIRECT_WAITER_BIJECTION dir=nr6 ... result=ok
IPC_DIRECT_WAITER_BIJECTION dir=nr7 ... result=ok
IPC_DIRECT_PRODUCTION_QUIESCENT_SEAL nr6_ok=1 nr7_ok=1 census_ok=1 result=ok
YARM_LOCK_SPLIT_DISPATCH nr=6 ×53   nr=7 ×41   (zero broad-lock NR6/NR7 entries)
```

**2. Direct NR6/NR7 oracle regression** —
`STAGE_199_IPCCALL_REPLY_DIRECT_LIVE_SEAL classes=2 live_cells=2 duplicate_replies=0
duplicate_wakes=0 result=ok`

**3. ServerDies regression**

```text
IPC_SERVER_DEATH_TRANSITION_AUDIT vector=[1, 1, 1, 1, 1, 1, 1, 1, 1] result_before_enqueue=1 result=ok
IPC_SERVER_DEATH_LINK_BALANCE_QUIESCENT created=54 closed=54 live_links=0 scope=system result=ok
STAGE_200D2B1C_X86_64_SERVER_DIES_SEAL live_cells=1 caller_wakes=1 peer_death_winners=1 result=ok
```

**4. x86 reply-timeout matrix, both cells** — zero `[fail]` lines.

```text
reply-wins    IPC_REPLY_WIN_RESERVE = 1        IPC_REPLY_WIN_ROLLBACK = 0
              IPC_REPLY_TIMEOUT_DEFERRED = 0
              IPC_REPLY_BEATS_TIMEOUT_OK arch=x86_64 terminal=Reply reply_copies=1
                  deadline_disarmed=1 late_timeout_claims=0 caller_wakes=1 result=ok
              IPC_DIRECT_PRODUCTION_QUIESCENT dir=nr7 ... arbitrated=1
              IPC_DIRECT_PRODUCTION_QUIESCENT_SEAL nr6_ok=1 nr7_ok=1 census_ok=1 result=ok

timeout-wins  IPC_REPLY_TIMEOUT_OK arch=x86_64 terminal=Timeout timeout_result=TimedOut
                  caller_wakes=1 reply_aliases_invalid=1 late_reply_successes=0 result=ok
              X86_IPC_REPLY_TIMEOUT_DONE caller_result=TimedOut late_reply=rejected result=ok
```

The `arbitrated=1` on the reply-wins boot is the whole causal chain in one number: exactly one
NR7 reply — the arbitrated one — declined to legacy, legacy took the terminal lease, and the
reply provably beat the timeout with zero rollback and zero deferred completion, while the other
41 ordinary replies still went off-lock.

Zero `result=fail`, leak, duplicate, stale or fatal markers in any run.

> The AArch64 and RISC-V matrix cells could not be executed: `qemu-system-aarch64` and
> `qemu-system-riscv64` are not installed in this environment. Neither architecture was changed
> — both still resolve to the proof gate and the oracle confinement — so their behaviour is
> byte-identical by construction, but the 3-arch matrix seal remains unearned here.

AArch64 and RISC-V are untouched and remain proof-gated.

AArch64 and RISC-V are untouched and remain proof-gated.

---

## 9. Authoring rule

Future IPC changes must update this doc and the relevant per-syscall ABI
in `doc/SYSCALL_ABI.md` (or the typed codec in `doc/VFS.md` §6).
Do **not** create new `IPC_*` / `SHARED_IPC_*` / `STAGE_*` fragment files —
`tests/doc_fragmentation_guard.rs` enforces this. Closed
phase / milestone outcomes belong in `doc/PROJECT_HISTORY.md`.
