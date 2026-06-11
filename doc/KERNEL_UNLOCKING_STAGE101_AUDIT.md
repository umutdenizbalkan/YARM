// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

# YARM Kernel Unlocking — Stage 101 Audit

**Written at:** Stage 101 — Kernel unlocking restart / MUST_SMOKE policy +
syscall decomposition readiness + D1 cap-transfer audit.
**Baseline:** branch `claude/wizardly-sagan-SL81B` at commit `81e581f` (Stage
100 / Optional FS Milestone 1 declared).
**Branch:** `claude/eager-mendel-6oncuk`.

This stage is **audit / scaffold / documentation / source-label**. It must not
change kernel behavior. The Cycle 11 kernel-unlocking review recommends the
ordering captured below:

1. **D7** — MUST_SMOKE policy (this stage).
2. **D4** — `syscall.rs` decomposition map and (optional) tiny mechanical split
   before D1/D3 land (this stage codifies the map).
3. **D1** — cap-transfer recv split (audited here; not implemented).
4. **D3** — `VmAnonMap` two-phase live (deferred).
5. **D6** — per-CPU scheduler locking (deferred until IPC split is stable).

---

## 1. MUST_SMOKE policy summary

The full policy lives in `doc/AI_AGENT_RULES.md §13` and
`doc/KERNEL_TEST_RULES.md` (Stage 101.1). The triggers are repeated here for
reviewer convenience:

A stage/PR MUST include smoke results when its diff:

1. live-wires a new split path in `handle_trap_entry_shared` /
   `try_split_dispatch_into_frame` or any equivalent trap/syscall entry seam,
2. modifies IPC dequeue, sender-waiter, receiver-waiter, timeout, wakeup, or
   reply-delivery logic,
3. changes `entering_tid` / `exiting_tid` / `task_switched` /
   `current_tid_authoritative` behavior,
4. changes trap/syscall result writeback,
5. changes scheduler dispatch or block/wake behavior,
6. changes VM/TLB shootdown behavior.

Minimum accepted smoke: **x86_64 `-smp 1` core smoke**. Optional-FS strict
smoke is required additionally for FS-facing changes. `nonfatal=true` lines
must be filtered out of `fatal` / `panic` greps.

---

## 2. LIVE_TRAP_SMOKE label convention

The validation labels documented in `KERNEL_TEST_RULES.md` (Stage 101.2) mark
the call sites of the existing split / live-wired paths. The mapping for the
paths active at Stage 101 is:

| Stage | Site | File / function | Label |
|-------|------|----------------|-------|
| Stage 29 / 29A | NR 8 control-plane cnode-slots split dispatch | `kernel/syscall_split.rs::try_split_dispatch_into_frame` | `LIVE_TRAP_SMOKE_X86_64` |
| Stage 32 / 32B | NR 2 IpcRecv kernel-task queued-plain split | `kernel/syscall_split.rs::try_split_ipc_recv_queued_plain_into_frame` | `LIVE_TRAP_SMOKE_X86_64` |
| Stage 4C / 4D / 4J | IpcRecv queued-plain split-recv fast path | `kernel/syscall.rs::try_endpoint_split_recv` | `LIVE_OFF_TRAP` + `SPLIT_FAST_PATH_ONLY` |
| Stage 4E | IpcSend queued/plain fast path | `kernel/syscall.rs::handle_ipc_send` (call to `ipc_try_send_queued_plain_endpoint_only`) | `LIVE_OFF_TRAP` + `SPLIT_FAST_PATH_ONLY` |
| Stage 4K / 4O | recv-v2 blocked-receiver direct delivery | `kernel/syscall.rs::handle_ipc_send` (call to `complete_blocked_recv_for_waiter`) | `LIVE_OFF_TRAP` |
| Stage 4L | IpcCall to recv-v2 blocked receiver | `kernel/syscall.rs::handle_ipc_call` (call to `complete_blocked_recv_for_waiter`) | `LIVE_OFF_TRAP` |
| Stage 4M | IpcReply fast path | `kernel/syscall.rs::handle_ipc_reply` (call to `kernel.ipc_reply`) | `GLOBAL_LOCK_SLOW_PATH` (no split yet) |
| Stage 4N | recv-v2 cap-transfer delivery | `kernel/syscall.rs::handle_ipc_send` (cap-transfer branch under `complete_blocked_recv_for_waiter`) | `LIVE_OFF_TRAP` |
| Stage 4O | recv-v2 FLAG_CAP_TRANSFER delivery | same as 4K/4N (annotated branch) | `LIVE_OFF_TRAP` |
| Stage 36 | user-ASID plain recv split path | `kernel/recv_core.rs::try_recv_core_user_plain` (caller `try_split_recv_queued_plain_with_snapshot_locked`) | `LIVE_OFF_TRAP` |
| Stage 37 | user-ASID recv-v2 plain split path | `kernel/recv_core.rs::try_recv_core_user_plain_v2` (caller `try_split_recv_queued_plain_with_snapshot_locked`) | `LIVE_OFF_TRAP` |
| Stage 42+43 | cap-transfer–aware split-recv dequeue (helper) | `kernel/recv_core.rs::extract_cap_transfer_plan` (callers throughout `try_recv_core_*`) | `SPLIT_FAST_PATH_ONLY` |
| NR 30 | RecvSharedV3 partial split path | `kernel/syscall.rs::handle_recv_shared_v3` (call to `try_recv_core_user_plain`) | `SPLIT_FAST_PATH_ONLY` |

Stage 101 adds source-comment labels at each of those call sites (no behavior
change). Source-scan tests in `syscall_split.rs` / `syscall.rs` /
`recv_core.rs` assert each label is present.

---

## 3. `syscall.rs` decomposition map

`src/kernel/syscall.rs` is ~7,650 lines and 154 fns. The proposed
decomposition target (no code moves required in Stage 101) is:

| New module | Owns | Functions to move |
|------------|------|--------------------|
| `syscall/dispatch.rs` | Decode + route, syscall NR constants, the `Syscall` enum, `dispatch()` | `Syscall`, `Syscall::decode`, `SYSCALL_*_NR`, `SYSCALL_COUNT`, `dispatch()` (decode + match arm), `current_tid` helper |
| `syscall/ipc.rs` | NR 1/2/4/5/6/7 — high-level IPC syscalls | `handle_ipc_send`, `handle_ipc_recv`, `handle_ipc_recv_timeout`, `handle_ipc_call`, `handle_ipc_reply`, `handle_transfer_release`, `try_endpoint_split_recv` |
| `syscall/ipc_recv_core.rs` | NR 2/5/recv-v2 adapters over `recv_core` | `handle_ipc_recv_result`, `handle_ipc_recv_result_with_empty_error`, `materialize_received_message_cap`, `materialize_received_transfer_cap`, recv-v2 writeback adapters, `complete_blocked_recv_for_waiter` |
| `syscall/mm.rs` | NR 3/13/14 — VM syscalls | `handle_vm_map`, `handle_vm_anon_map`, `handle_vm_brk` |
| `syscall/cap.rs` | NR 8 — capability / control-plane | `handle_control_plane_set_cnode_slots` |
| `syscall/sched.rs` | NR 0/9/10/11 — scheduling | `handle_futex_wait`, `handle_futex_wake`, `handle_spawn_thread` (yield is inline in dispatch) |
| `syscall/process.rs` | NR 12/23/24/26/29 — process lifecycle | `handle_fork`, `handle_spawn_process`, `handle_spawn_process_from_user_buf`, `handle_spawn_from_initramfs_file`, `handle_spawn_from_memory_object`, related helpers (`spawn_image_path_for_image_id`, `pack_register_payload`, etc.) |
| `syscall/initramfs.rs` | NR 27/28 — initramfs syscalls | `handle_initramfs_read_chunk`, `handle_create_initramfs_file_slice_mo` |
| `syscall/debug.rs` | NR 15 — debug log | `handle_debug_log` |
| `syscall/recv_shared_v3.rs` | NR 30 — RecvSharedV3 adapter | `handle_recv_shared_v3`, parsing helpers, ABI helpers |

Stage 101 chooses **Option A: no mechanical movement**. Only labels and the
decomposition map are added. A future Stage 102+ may perform Option B (one
small mechanical split, zero behavior change) — but **never** combined with
D1, D3, or D6 changes.

### 3.1 Tests guarding the decomposition map

Stage 101 adds source-scan tests that assert:

- `SYSCALL_COUNT` is `31` (already guarded by `stage81b_syscall_count_remains_31`).
- `Syscall::RecvSharedV3` remains a dispatch arm in `dispatch()`.
- `Syscall::ControlPlaneSetCnodeSlots` remains a dispatch arm and remains
  whitelisted in `syscall_split.rs::classify_split_eligible_nr_only`.
- `doc/KERNEL_UNLOCKING_STAGE101_AUDIT.md` exists and contains the map above
  (a tiny source-scan test in `syscall.rs` cross-checks the doc).

---

## 4. D1 cap-transfer recv split — pre-audit only (not implemented)

### 4.1 What "D1" means

D1 is the proposed kernel-unlocking step that performs the receive-side
**cap materialization** outside the global `KernelState` lock, in three phases:

- **Phase A — ipc(3):** dequeue message + extract `RecvCapTransferPlan` under
  `ipc_state_lock` (rank 3).
- **Phase B — cap(4):** materialize the transferred cap into the receiver's
  cnode under `capability_state_lock` (rank 4).
- **Phase C — no lock:** trapframe / message writeback (user-memory copy).

### 4.2 Current Stage 100 plumbing

The cap-transfer path is already **scaffolded but materialized on the
global-lock path**. The relevant types and functions are:

- `kernel/recv_core.rs::RecvCapTransferPlan` (`raw_handle`, `is_reply_cap`).
- `kernel/recv_core.rs::extract_cap_transfer_plan(msg)` — pure function,
  helper-only. Called inside the three `try_recv_core_*` paths to populate
  `RecvDelivery.cap_transfer`.
- `kernel/syscall.rs::materialize_received_message_cap(...)` — delegation /
  direct-mint path. Today runs **inside the same `&mut KernelState`** as the
  rest of the dispatch (global lock held).
- `kernel/syscall.rs::materialize_received_transfer_cap(...)` — pure-transfer
  variant used by the recv-v2 blocked-receiver fast path
  (`complete_blocked_recv_for_waiter`).
- `kernel/syscall.rs::rollback_materialized_recv_cap(...)` — undo on
  writeback failure (called from `try_split_recv_queued_plain_with_snapshot_locked`
  when meta-copy / payload-undersized).

`FLAG_CAP_TRANSFER_PLAIN` is currently routed through
`materialize_received_message_cap`'s **transfer** arm (treated identically to
`FLAG_CAP_TRANSFER`). Both rely on `take_transfer_envelope` +
`grant_task_to_task_with_rights`. Neither path is split-wired yet: the
materialization is performed under the global lock that is taken by
`handle_ipc_recv_result_with_empty_error` and friends.

### 4.3 Audit questions

The audit in Stage 101 must answer the following, scoped to **kernel** code
(no userspace impact):

#### Q1 — Does `materialize_capability`-equivalent touch only the capability domain?

The kernel does **not** expose a function literally called
`materialize_capability`. The current names are:

- `materialize_received_message_cap` (`syscall.rs:446`),
- `materialize_received_transfer_cap` (`syscall.rs:419`).

Both functions today call into:

- `kernel.take_transfer_envelope(...)` — reads from
  `ipc.active_transfer_mappings` (IPC domain, rank 3).
- `kernel.resolve_capability_for_task(...)` — reads from capability domain
  (rank 4).
- `kernel.capability_service_mut().grant_task_to_task_with_rights(...)` —
  mutates capability domain (rank 4), records a delegation link in
  `capability.delegated_capability_links` (rank 4).
- `kernel.capability_object_live(...)` — reads capability/reply registries
  (rank 4).
- `kernel.task_cnode(...)` — reads task→cnode mapping (task domain, rank 2,
  or capability domain depending on call site).
- `kernel.mint_capability_in_cnode(...)` — mutates capability domain (rank 4).
- `record_reply_cap_record(...)` (reply-cap path) — mutates IPC reply registry
  (rank 3).

**Conclusion:** The transfer-arm (delegation) path is **capability-domain
only** (rank 4) for the mutation phase, but it **reads** from the IPC domain
(rank 3) via `take_transfer_envelope`. The reply-arm (direct-mint) path
touches both rank 3 (IPC reply registry) and rank 4 (capability domain).

D1 must respect this: Phase A (ipc/rank 3) extracts the envelope and the plan;
Phase B (cap/rank 4) does the grant/mint. Reply-arm Phase B additionally needs
to write the reply-cap record back into the IPC reply registry under rank 3 —
i.e. it is **not a pure rank-4 mutation** and must be carefully split.

#### Q2 — Does `delegate_capability`-equivalent touch only capability domain?

The kernel does **not** have a function called `delegate_capability` either.
The equivalent service is `CapabilityService::grant_task_to_task_with_rights`
(used by both Phase 2A `IPC_GRANT_RO` and our materialize path) plus the
delegation-link record `record_delegated_capability_link`. Both run inside
`capability_state_lock` and touch only capability-domain fields.

#### Q3 — Do either touch ipc / task / scheduler / vm / memory / global KernelState?

- **ipc:** transfer-arm reads `ipc.active_transfer_mappings` (rank 3). The
  reply-arm additionally records into `ipc.reply_cap_records` (rank 3).
- **task:** both arms read `task_cnode(receiver_tid)` (task-domain map, rank 2
  for the read path).
- **scheduler:** none.
- **vm:** none.
- **memory:** `adjust_memory_object_cap_refcount` (memory-domain mutation,
  separate lock) is called by `mint_capability_in_cnode` on success.

**Conclusion:** the materialization functions are **multi-domain**: they read
from IPC (rank 3), read from task (rank 2), mutate capability (rank 4), and
mutate memory-object refcounts. They are **not** "rank-4 only". D1 must
schedule them so that each domain's lock is acquired and released in
ascending order; the existing global-lock path provides this trivially
because every operation runs under the same `&mut KernelState`.

#### Q4 — Is D1 safe to implement as A/B/C?

Yes, provided the implementation:

- **Phase A (rank 3):** under `ipc_state_lock`, dequeue the message via
  `ipc_try_recv_queued_with_cap_transfer`, extract `RecvCapTransferPlan` via
  `extract_cap_transfer_plan`, take the transfer envelope via
  `take_transfer_envelope`. All rank-3 reads happen here. Release rank 3.
- **Phase B (rank 4 + rank 2 read + rank 3 write for reply-arm):** under
  `capability_state_lock`, perform `grant_task_to_task_with_rights` or
  `mint_capability_in_cnode`. Read `task_cnode(receiver)` via a snapshot
  taken in Phase A (do NOT re-acquire rank 2 mid-phase). For the reply-arm,
  the reply-cap record write (rank 3) must be deferred to a small Phase B′
  that re-acquires rank 3 briefly with no other lock held.
- **Phase C (no lock):** trapframe writeback / payload copy. On failure,
  invoke `rollback_materialized_recv_cap` (rank 4) to undo Phase B (and
  Phase B′ for reply-arm).

The same multi-phase shape is already used by the recv-core split path
(plan_recv_core → try_recv_core_* → execute_user_asid_plain_writeback). The
delta for D1 is **adding** Phase B between the dequeue and the writeback.

#### Q5 — Rollback behavior on Phase C failure

`rollback_materialized_recv_cap` already exists and is exercised by the
Stage 36/37/42+43 callers. It handles:

- delegation-arm: revoke the freshly-minted descendant cap via
  `revoke_capability_in_cnode`, decrement the source delegation link.
- reply-arm: fast-revoke the freshly-minted reply slot via
  `fast_revoke_reply_cap_in_cnode`, clear the reply-cap record.

D1 must call this on `UndersizedBuffer` / `MetaCopyFault` /
`PayloadUndersized`, **before** returning the error to userspace, matching
the existing global-lock semantics. **No additional rollback is needed for
Phase A**: the IPC dequeue is the canonical commit point; once dequeued, the
message is gone whether or not the cap materializes.

If Phase B itself fails (e.g. `CapabilityFull`), the receiver loses the
transferred cap but must still receive the message bytes. This matches the
current global-lock behavior. The error is encoded into the trapframe and
the message is delivered with `transferred_cap = None`.

#### Q6 — Does `FLAG_CAP_TRANSFER_PLAIN` fall back to global-lock today?

**Yes.** All three `try_recv_core_*` adapter functions populate
`RecvDelivery.cap_transfer` whenever the flag is set, but the actual
materialization is performed by the caller
(`try_split_recv_queued_plain_with_snapshot_locked` in `syscall.rs`) by
calling `materialize_received_message_cap` — which **today** runs while the
caller still holds the global `&mut KernelState` (the
`SharedKernel::with(...)` closure body). The split-recv path therefore
delivers cap-transfer messages **correctly**, but the materialization itself
is **not** lock-split yet — it runs against the same monolithic lock as the
old path. D1 is the work to move materialization out of that closure.

The `FallbackReason::CapTransfer` enum variant is documented as "no longer
produced by `try_recv_core_*` in Stage 42+43" — it is reserved for the
sender-waiter-with-cap-transfer fallback that is still global-locked.

#### Q7 — Queue-head starvation risk?

In Stage 42+43, the split-recv adapters call
`ipc_try_recv_queued_with_cap_transfer`, which dequeues whatever is at the
head of the queue (plain **or** cap-flagged) and returns it as
`RecvDelivery`. The cap-transfer arm of the adapter completes
materialization on the **same** dispatch call (Phase B in D1), so there is
**no risk of head-of-line blocking** at the IPC queue level — a cap-transfer
message at the head is consumed in the same dispatch as a plain message
would be.

The risk is more subtle: under D1, Phase B (rank 4) is **longer** than the
plain-recv fast path (which has no Phase B), so the receiver's syscall
takes longer. This is intrinsic and is the cost of cap materialization. It
does not block other CPUs from making progress on their own dispatches
because each CPU holds rank 4 for **its own** materialization only.

### 4.4 D1 readiness verdict

D1 is **safe to implement** in a future stage with the A/B/C decomposition
in §4.4. Stage 101 does **not** live-wire it. Stage 102+ should:

1. Add a per-phase plan type analogous to `RecvCapTransferPlan` that captures
   the snapshot of `task_cnode(receiver)` taken under rank 2 in Phase A.
2. Split `materialize_received_message_cap` into a Phase B function that
   takes only `(plan, snapshot)` arguments and acquires only rank 4 (+ a tiny
   rank-3 reply-record write for the reply-arm).
3. Wire the live split call site to call Phase A → Phase B → Phase C in
   sequence with explicit lock-release points.
4. Add a smoke test (x86_64 -smp 1) that exercises the cap-transfer path
   under the new split wiring.

Stage 101 source-scan tests assert that the audit conclusions above are not
silently invalidated:

- `materialize_received_message_cap` and `materialize_received_transfer_cap`
  both exist.
- `RecvCapTransferPlan` exists and is referenced by all three
  `try_recv_core_*` functions.
- `FallbackReason::CapTransfer` remains a `FallbackReason` variant.

### 4.5 `CapRights` width — C6 deferral

Widening `CapRights` (review-finding C6) is **deferred to Stage 102 / 103**.
No trivial isolated test-only assertion is added in Stage 101.

---

## 5. Decomposition scaffold status pointer

See `doc/DECOMPOSITION_SCAFFOLD_STATUS.md` for the canonical table of
plan/scaffold types and their state (live / helper-only / fallback-only /
deferred / obsolete).

---

## 6. Unsafe split-helper guard audit

### 6.1 Helpers that project raw pointers from `SharedKernel` / `KernelState`

The `unsafe fn *_from_raw(state: *const KernelState, ...)` family in
`src/kernel/boot/orchestrator_state.rs`. Each function takes a raw pointer to
the entire `KernelState` and uses `core::ptr::addr_of!` (`addr_of_mut!` for
the mut variants) to derive raw field pointers without forming a `&mut` to
the whole struct.

| Helper | Field(s) touched | Lock domain (intended) |
|--------|------------------|------------------------|
| `fault_split_mut_ptrs_from_raw` | `fault_state_lock`, `faults` | fault (own lock) |
| `telemetry_split_mut_ptrs_from_raw` | `telemetry_state_lock`, `telemetry` | telemetry (own lock) |
| `task_asid_for_tid_from_raw` | `task_state_lock`, `tcbs` | task (rank 2) |
| `task_class_from_raw` | `task_state_lock`, `tcbs`, `task_classes` | task (rank 2) |
| `task_exists_from_raw` | `task_state_lock`, `tcbs` | task (rank 2) |
| `cnode_slot_capacity_from_raw` | `capability_state_lock`, `capability` | capability (rank 4) |
| `process_id_from_raw` | `task_state_lock`, `tcbs` | task (rank 2) |
| `is_group_leader_from_raw` | `task_state_lock`, `tcbs` | task (rank 2) |
| `notification_waiter_count_from_raw` | `ipc_state_lock`, `ipc` | IPC (rank 3) |
| `cnode_registered_from_raw` | `capability_state_lock`, `capability` | capability (rank 4) |

The `SharedKernel` wrappers in `src/runtime.rs` always call these inside
`SAFETY:` comments asserting that `state.data_ptr()` is the stable storage
owned by the same `SharedKernel`, so no aliasing `&mut KernelState` can be
live at the same time (guarded in debug by
`BOOT_RAW_BORROW_ACTIVE` from §5.0 in runtime.rs).

### 6.2 Could a debug assertion verify the corresponding lock?

Each `*_from_raw` helper takes the per-domain `SpinLock` guard internally
(e.g. `lock_ref.lock()`), so the lock IS held during the read. A
`debug_assert!` is therefore redundant for those helpers — the lock guard
type itself is the assertion.

The helpers that could benefit from `#[track_caller]` debug assertions:

- `borrow_kernel_for_boot` (already debug-guarded via `BOOT_RAW_BORROW_ACTIVE`).
- Any future helper that derives a raw pointer without re-acquiring a per-domain
  lock should add a `debug_assert!(boot_raw_borrow_is_active())` so the boot
  raw-borrow contract is enforced.

### 6.3 Risk / lock-domain ownership

No helpers currently lack a clear lock-domain owner. The raw-pointer pattern
is structurally safe because:

1. The `KernelState` storage is owned by `SharedKernel::state: SpinLock<KernelState>`.
2. `data_ptr()` returns the storage pointer of that lock without taking it.
3. Each `*_from_raw` helper takes its **per-domain** sub-lock via `addr_of!`
   before reading the data, so two helpers for different domains can run on
   different CPUs without conflict.

The risk surface is **adding a new helper that forgets to take its per-domain
lock**. Mitigation: any new helper must follow the existing pattern (lock-ref
via `addr_of!`, `.lock()` before any data read) AND be reviewed against this
audit.

### 6.4 TODO comments

No source changes proposed in Stage 101 for this section. Future TODOs may
go on the helpers if the lock-domain pattern evolves.

---

## 7. `boot/tests.rs` and `syscall.rs` maintainability

`src/kernel/boot/tests.rs` is ~31,600 lines and contains:

- capability tests (revoke/delegate/grant/cspace).
- IPC tests (send/recv/reply/call/cap-transfer/timeout/recv-v2/recv-shared-v3).
- VM tests (anon map, brk, map_shared_region, two-phase unmap, TLB shootdown).
- scheduler tests (membership, dispatch, idle re-enqueue).
- spawn / process tests (SpawnV5, fork, spawn-from-memory-object).
- fault / fatal-trap tests.

A mechanical split into `boot/tests/<subsystem>.rs` (one file per subsystem)
is the recommended future stage. It must be:

- pure file moves, no logic changes;
- preserves all `#[test]` annotations;
- preserves all `#[cfg(...)]` attributes verbatim;
- updates `boot/mod.rs` to declare the new submodules.

**Suggested stage:** Stage 105 — mechanical boot/tests.rs split. Not combined
with D1, D3, or D6.

`src/kernel/syscall.rs` follows the decomposition map in §3. Same rules:
mechanical only, not combined with behavior changes.

---

## 8. Stage 100 FS baseline preservation

Stage 101 must preserve the Stage 100 / Optional FS Milestone 1 baseline.
Source-scan tests assert:

- `INIT_SPAWN_RAMFS_SRV` remains `true`.
- `INIT_SPAWN_FAT_SRV` remains `false` in the default optional-fs profile.
- `INIT_SPAWN_EXT4_SRV` remains `true`.
- `VFS_EXT4_LIVE_MOUNT_ENABLED` remains `true`.
- `VFS_FAT_LIVE_MOUNT_ENABLED` remains `false` by default.
- `VFS_FAT_SHARED_IO_ENABLED` remains `false`.

These are already covered by existing yarm-fs-servers tests (572 pass) and
yarm-control-plane-servers tests (130 pass). Stage 101 does not touch any
filesystem-facing source.

---

## 9. Recommended Stage 102 task

**Stage 102 — D4 mechanical syscall split (optional) + D1 Phase A/B/C scaffold.**

Two parallel tracks, chosen at PR time:

- **D4 track:** mechanical move of one small group from `syscall.rs` per the
  map in §3 (e.g. `syscall/debug.rs` for NR 15 only). Zero behavior change;
  all source-scan tests updated; CI smoke required because trap result
  writeback is touched at the dispatch seam.
- **D1 track:** add Phase A/B/C plan types and a feature-gated split call
  site for the cap-transfer recv path. Live-wire **only** behind a Cargo
  feature; default-off in CI; required smoke when enabled.

Whichever track lands first must NOT be combined with the other or with VM /
scheduler / SMP work.

---

## 10. Final invariants reaffirmed

- `SYSCALL_COUNT = 31` (guarded by existing source-scan test).
- `STARTUP_SLOT_COUNT = 18`.
- SpawnV5 ABI unchanged.
- `recv_shared_v3` ABI offsets unchanged.
- Image IDs 7–12 frozen.
- Optional-FS smoke markers and forbidden markers unchanged.
- `VFS_SUPERVISOR_TASK_EXIT_NOTIFICATION_ENABLED = false`.
- No deadline-0 required replies anywhere.
- No new behavior changes in Stage 101.

---

## 11. Stage 102 — Mechanical syscall decomposition (first split landed)

Stage 102 executed the first mechanical move from the §3 decomposition map.
Zero behavior change; all moved code is byte-identical except for
`pub(super)` visibility on the moved function signatures.

### 11.1 Module layout decision

`src/kernel/syscall.rs` **remains the parent module** (not converted to
`syscall/mod.rs`). Reasons:

- `scripts/check-kernel-arch-boundary.sh` and
  `scripts/phase7-shared-ipc-gates.sh` reference `src/kernel/syscall.rs` by
  path.
- ~10 `include_str!("syscall.rs")` source-scan tests reference the file
  relative to its own directory.
- Rust supports the `syscall.rs` + `syscall/` directory layout natively, so
  keeping the parent file avoids touching any external reference.

### 11.2 What moved in Stage 102

| New module | Contents | Lines | Visibility |
|------------|----------|-------|------------|
| `src/kernel/syscall/debug.rs` | `handle_debug_log` (NR 15) | ~55 | `pub(super)` |
| `src/kernel/syscall/initramfs.rs` | `handle_initramfs_read_chunk` (NR 27), `handle_create_initramfs_file_slice_mo` (NR 28) | ~256 | `pub(super)` |

The dispatch arms in `dispatch()` are textually unchanged; the parent
re-imports the handlers via `use self::debug::...` / `use self::initramfs::...`.

The `mod debug;` / `mod initramfs;` declarations sit AFTER the
`syscall_trace!` macro definition — required by textual macro scoping
(`debug.rs` invokes `syscall_trace!`, which expands to a reference to
`AARCH64_SYSCALL_TRACE`, imported in `debug.rs` via `use super::`).

### 11.3 Visibility compromises

None beyond `pub(super)` on the three moved handler fns. The private parent
helpers the moved code uses (`current_tid`, `validate_user_region`,
`PM_BOOTSTRAP_TID`, `INITRAMFS_READ_CHUNK_TRACE`, `AARCH64_SYSCALL_TRACE`)
remain private to the `syscall` parent module — child modules access them
through normal Rust descendant-privacy via `use super::`.

### 11.4 What remains in the parent (and why)

| Group | Status | Blocker for moving |
|-------|--------|--------------------|
| NR constants + `Syscall` enum + `dispatch()` | stays for now | many `include_str!` scans + scripts reference syscall.rs content; move to `dispatch.rs` is safe but big-diff; do after IPC group |
| IPC handlers (NR 1/2/4/5/6/7) | stays | densely interdependent helpers (`stash_transfer_handle`, `complete_blocked_recv_for_waiter`, `materialize_received_*`, recv-result writeback chain); this is the **D1 landing area** — do not churn it before D1 lands |
| recv-core adapters (`try_split_recv_*`) | stays | called from `runtime.rs` via `crate::kernel::syscall::` paths; safe to move later with re-export |
| VM handlers (NR 3/13/14) | stays | D3 landing area; same churn-avoidance argument |
| cap/control-plane (NR 8) | stays | tiny; tied to syscall_split.rs whitelist tests |
| sched (NR 0/9/10/11) | stays | small; trivial future move |
| process/spawn (NR 12/23/24/26/29) | stays | largest group; depends on ELF/CPIO/zero-copy helpers; mechanical but big-diff |
| recv_shared_v3 (NR 30) | stays | parse/encode helpers are self-contained, but the handler shares recv-core adapter calls with the Stage 36 path; candidate for the **next** split |
| `#[cfg(test)] mod tests` | stays | include_str!-based source scans assume current layout |

### 11.5 Recommended next split target

`syscall/recv_shared_v3.rs` — the NR 30 parse/encode helpers
(`parse_v3_request_bytes`, output-encode helper, object-kind mapping) plus
`handle_recv_shared_v3` form a mostly self-contained group (~700 lines).
After that: `syscall/sched.rs` (futex/spawn-thread, trivial), then
`syscall/process.rs` (big but mechanical).

**Do NOT move the IPC group before D1 lands** — moving it would force every
D1 review diff to cross a file rename, defeating the reviewability goal.

### 11.6 Stage 103 D1 landing area (exact files/functions)

D1 (cap-transfer recv split, Phase A/B/C per §4.4) will touch exactly:

- `src/kernel/recv_core.rs`
  - `RecvCapTransferPlan` (extend with receiver-cnode snapshot)
  - `extract_cap_transfer_plan` (unchanged or extended)
  - `try_recv_core_kernel_plain` / `try_recv_core_user_plain` /
    `try_recv_core_user_plain_v2` (Phase A: snapshot capture)
- `src/kernel/syscall.rs`
  - `materialize_received_message_cap` (split into Phase B function)
  - `materialize_received_transfer_cap` (recv-v2 path equivalent)
  - `try_split_recv_queued_plain_with_snapshot_locked` (writeback site;
    Phase B call moves out of the global-lock closure)
  - `rollback_materialized_recv_cap` callers (Phase C failure handling)
- `src/runtime.rs`
  - the `SharedKernel` seam that today wraps the whole recv in `with(...)`
    (Phase A/B/C lock-release points)
- `src/kernel/boot/capability_lifecycle_state.rs`
  - `mint_capability_in_cnode` (Phase B rank-4 mutation; unchanged but its
    lock-domain contract is the Phase B safety argument)

Stage 102 deliberately did NOT touch any of these functions, so the Stage
103 diff will be reviewable in isolation.

### 11.7 Stage 102 test additions

4 new source-scan/runtime tests in `kernel::syscall::tests`:

1. `stage102_split_modules_exist_and_host_moved_handlers`
2. `stage102_dispatch_arms_unchanged_for_moved_handlers`
3. `stage102_moved_modules_do_not_define_abi_constants`
4. `stage102_dispatch_runtime_routing_for_moved_handlers` — runtime proof
   that NR 15 and NR 27 still route through `dispatch()` to the moved
   bodies with identical results (ok-0 fast path; MissingRight gate).

---

## 12. Stage 103 — D1 cap-transfer recv split scaffold (helper-only)

Stage 103 builds the Phase A / B / C scaffold for the D1 cap-transfer recv
split as **helper-only / default-off**. No live behavior change.

### 12.1 New module: `src/kernel/cap_transfer_split.rs`

| Item | Purpose |
|------|---------|
| `CapTransferRecvClass` | Pure flag classification of a delivered `Message` into `None` / `Transfer { raw_handle }` / `Reply { raw_handle }`. |
| `CapTransferRecvSnapshot` | Phase A output: `handle`, `endpoint`, `source_tid`, `source_cap`, `receiver_tid`, `source_rights`. All `Copy`. |
| `CapTransferMaterializeOutcome` | Phase B output: receiver-local `CapId`. |
| `CapTransferSplitResult` | Combined entry-point outcome enum: `None` / `Materialized(u64)` / `FallbackRequired` / `Failed(SyscallError)`. |
| `phase_a_take_transfer_envelope` | Phase A: IPC rank 3 (`take_transfer_envelope`) + capability rank 4 read (`resolve_capability_for_task`). |
| `phase_b_materialize_transfer_cap` | Phase B: capability rank 4 mutate (`grant_task_to_task_with_rights`). |
| `materialize_split_transfer_cap_equivalent` | Combined A → B entry; equivalence-tested against the global-lock direct-call sequence. |

All three helper fns are labeled `D1_HELPER_ONLY` / `D1_DEFAULT_OFF` /
`FALLBACK_GLOBAL_LOCK`. A source-scan test
(`stage103_helper_only_no_live_call_sites`) asserts they are not called from
`syscall.rs` or `runtime.rs`.

### 12.2 Supported case (helper-only)

- **`FLAG_CAP_TRANSFER` + non-reply (delegation path)** — full A → B handled.
- **`FLAG_CAP_TRANSFER_PLAIN`** — same `Transfer` class as `FLAG_CAP_TRANSFER`;
  same A → B path; same observable outcome. (`materialize_received_message_cap`
  unifies these two flags into a single `materialize_received_transfer_cap`
  call, and the helper mirrors that.)
- **Plain (no flag, no `transferred_cap`)** — returns `None`.

### 12.3 Fallback cases (not implemented)

| Case | Why fallback |
|------|--------------|
| `FLAG_REPLY_CAP` | Reply-cap Phase B is rank-4 `mint_capability_in_cnode` followed by a **rank-3** `set_reply_cap_waiter_cap` — rank inversion. Live wiring requires either a dedicated rank-3 B′ phase reacquired after B (extra lock cycles) or a deferred-plan write applied by the caller alongside the scheduler wake plan. Either choice is a live-IPC timing change and requires QEMU smoke. |
| Sender-waiter with cap-transfer | `RecvOutcome::FallbackRequired(FallbackReason::SenderWaiterWake)` already routes this to the global lock for "complex" cap-flagged refills (`recv_core.rs` §FallbackReason::SenderWaiterWake comment). D1 inherits the same fallback. |
| Mapped / shared receive (`FLAG_CAP_TRANSFER` with `OPCODE_SHARED_MEM`) | The envelope has a `shared_region`; Phase A's pin-refcount adjust is already in the right domain, but the receiver-side `register_active_transfer_mapping` + VM map require additional domain handling. Out of scope for Stage 103. |

### 12.4 Domain / rank order used by the helper

| Phase | Lock acquired | Lock released | Operations |
|-------|---------------|---------------|------------|
| Phase A.1 | IPC rank 3 (+ memory refcount) | end of `take_transfer_envelope` | dequeue envelope |
| Phase A.2 | capability rank 4 (read) | end of `resolve_capability_for_task` | snapshot source rights |
| Phase B | capability rank 4 (mutate) + memory refcount + delegation links | end of `grant_task_to_task_with_rights` | mint receiver cap |
| Phase C | none | — | writeback (caller drives) |

The helper today runs all three phases under a single `&mut KernelState`, so
the in-process lock count is unchanged from the global-lock path. Stage 104
will replace the closure boundary with explicit `SharedKernel` lock-release
points between Phase A.2 → Phase B (rank 4 read → rank 4 mutate is rank-safe
to chain; rank 3 → rank 4 is the actual decomposition gain).

### 12.5 Rollback / error behavior

- **Phase A failure** (envelope already gone / endpoint mismatch / receiver
  mismatch / source-cap resolve failure): no state changed; envelope remains
  consumed only if `take_transfer_envelope` succeeded (it didn't in this
  case). Same observable outcome as the global-lock path.
- **Phase B failure** (`grant_task_to_task_with_rights` returns
  `CapabilityFull` / `MissingRight` / `TaskMissing`): envelope is **already
  consumed** by Phase A — exactly matching the global-lock contract today
  (no envelope rollback on materialize failure; the message is delivered
  with `transferred_cap = None` or the syscall returns the error).
- **Writeback (Phase C) failure** drives `rollback_materialized_recv_cap`,
  unchanged from existing live code in `try_split_recv_queued_plain_with_snapshot_locked`.

### 12.6 Equivalence proof

Test `stage103_equivalence_split_matches_direct_take_plus_grant` builds two
independent kernel states with byte-identical setup. State A runs the helper
end-to-end; State B calls `take_transfer_envelope` +
`resolve_capability_for_task` + `grant_task_to_task_with_rights` directly
(the exact sequence inside `materialize_received_transfer_cap`). The test
asserts:

- minted `CapId` raw values are equal,
- receiver cnode slot objects are equal,
- receiver cnode slot rights are equal.

Plus four additional behavior-equivalence tests:

- `stage103_equivalence_plain_message_returns_none`
- `stage103_equivalence_reply_cap_message_returns_fallback_required` (also
  verifies the envelope is **not** consumed on fallback)
- `stage103_equivalence_no_envelope_returns_invalid_capability` (matches the
  global-lock `SyscallError::InvalidCapability` exactly)
- `stage103_phase_b_mints_attenuated_cap_in_receiver_cnode` (matches the
  attenuation contract of `grant_task_to_task_with_rights`)

### 12.7 Live-wire blockers (Stage 104 prerequisites)

1. **SharedKernel seam.** `runtime.rs` must expose two split-mut entry
   points, one per phase. The current `SharedKernel::with(...)` closure
   holds the global lock for the duration of the whole materialize. Phase A
   and Phase B must each take their own narrow rank-3 / rank-4 closure to
   actually realize the split.
2. **Reply-cap rank 3↔4 design.** Choose between (a) explicit rank-3 B′
   phase, or (b) deferred reply-record write. Both are observable timing
   changes.
3. **QEMU x86_64 `-smp 1` smoke.** MUST_SMOKE (`AI_AGENT_RULES.md §13`)
   triggers because the change modifies IPC reply-delivery / cap-transfer
   logic. Stage 103 was developed without QEMU; Stage 104 needs an
   environment with QEMU available.
4. **Sender-waiter-with-cap-transfer.** `FallbackReason::SenderWaiterWake`
   currently routes complex cap-flagged refills to the global lock. D1 may
   absorb the plain subset; the complex subset stays fallback.

### 12.8 Stage 103 invariants reaffirmed (superseded by §13 for D1 status)

- `SYSCALL_COUNT = 31`, `STARTUP_SLOT_COUNT = 18`, SpawnV5 ABI,
  `recv_shared_v3` ABI offsets, image IDs 7–12 — **unchanged**.
- Stage 100 FS gates (`INIT_SPAWN_RAMFS_SRV=true`, `INIT_SPAWN_FAT_SRV=false`,
  `INIT_SPAWN_EXT4_SRV=true`, `VFS_EXT4_LIVE_MOUNT_ENABLED=true`,
  `VFS_FAT_LIVE_MOUNT_ENABLED=false`, `VFS_FAT_SHARED_IO_ENABLED=false`) —
  **unchanged**.
- `VFS_SUPERVISOR_TASK_EXIT_NOTIFICATION_ENABLED = false` — **unchanged**.
- Stage 101 LIVE_TRAP_SMOKE labels — **unchanged**.
- Stage 102 syscall module split (`syscall/debug.rs`, `syscall/initramfs.rs`)
  — **unchanged**.
- Global-lock fallback `materialize_received_message_cap` — **unchanged and
  still authoritative**.

---

## 13. Stage 104 / Kernel Unlocking Pass 1 — D1 live-wired (transfer arm), D5 deferred

Stage 104 live-wires the Stage 103 D1 engine for the supported transfer-cap
receive case and defers D5 (reply-cap split) with an exact blocker.

**NOT SMOKE-ACCEPTED:** developed in an environment without QEMU. Per
MUST_SMOKE (`AI_AGENT_RULES.md §13`) this branch requires x86_64 `-smp 1`
core smoke and optional-FS strict smoke before merge acceptance — this is a
live IPC semantics change (routing), even though the materialization sequence
is equivalence-proven byte-identical.

### 13.1 What is live now (D1)

`syscall.rs::materialize_received_message_cap_routed` (labels:
`D1_LIVE_SPLIT` + `FALLBACK_GLOBAL_LOCK`) is called from exactly two
delivery seams:

1. `complete_blocked_recv_for_waiter` — recv-v2 blocked-receiver delivery
   (Stage 4K/4O seam, used by IpcSend and IpcCall split deliveries).
2. `try_split_recv_queued_plain_with_snapshot_locked` — queued split-recv
   (Stage 36/37/42+43 seam).

Supported scope routed through `cap_transfer_split::materialize_split_transfer_cap_equivalent`:
- `FLAG_CAP_TRANSFER`, non-reply, `opcode != OPCODE_SHARED_MEM`
- `FLAG_CAP_TRANSFER_PLAIN`, non-reply, `opcode != OPCODE_SHARED_MEM`
- plain messages short-circuit to `None`

Each successful routed materialization increments the new
`IpcPathTelemetry::d1_split_materializations` counter and emits
`YARM_D1_SPLIT_MATERIALIZE` (additive marker). Failure logging is
byte-identical to the canonical transfer arm
(`IPC_RECV_CAP_MATERIALIZE_FAILED kind=transfer ...`).

### 13.2 What stays on the global-lock fallback

| Case | Why |
|------|-----|
| `FLAG_REPLY_CAP` | D5 deferred — see §13.4 |
| `OPCODE_SHARED_MEM` transfers | receiver-side mapping obligations outside the materialize step |
| Sender-waiter cap-transfer refills | `recv_core.rs FallbackReason::SenderWaiterWake` unchanged |
| Legacy full recv path (`handle_ipc_recv_result_with_empty_error`) | intentionally unrouted this pass — canonical helper called directly |
| NR 30 RecvSharedV3 handler | intentionally unrouted this pass |
| Any `FallbackRequired` outcome | router falls through to `materialize_received_message_cap` |

The canonical `materialize_received_message_cap` is **unchanged** and remains
the authoritative implementation for all fallback cases.

### 13.3 Rank order and the hosted-dev lock-order note

Routed sequence (all sequential acquire/release, never nested):
Phase A.1 IPC rank 3 (`take_transfer_envelope`) → Phase A.2 capability rank 4
read (`resolve_capability_for_task`) → Phase B capability rank 4 mutate
(`grant_task_to_task_with_rights`) → telemetry IPC rank 3
(`note_d1_split_materialize`).

The final rank-4 → rank-3 sequential transition triggers the debug-only
hosted-dev `YARM_LOCK_ORDER_WARN` diagnostic in unit tests. This is expected:
the tracker flags any descending sequential pair, the locks are never held
simultaneously (no deadlock possibility), and the canonical reply arm
produces the same pattern (`mint` rank 4 → `set_reply_cap_waiter_cap` rank
3). The warn is compiled out of production builds (`hosted-dev` +
`debug_assertions` only) and no smoke script greps it.

### 13.4 D5 reply-cap split — deferred with exact blocker

Lock-touch map of the reply arm: `take_transfer_envelope` (rank 3) →
`capability_object_live` (rank 4 read) → `task_cnode` (rank 2) →
`mint_capability_in_cnode` (rank 4 mutate) → `set_reply_cap_waiter_cap`
(rank 3 mutate).

Sequential acquisition is deadlock-free; the blocker is the **atomicity
window** between the rank-4 mint and the rank-3 record write: outside the
global lock another CPU could revoke/consume the reply object in that window,
leaving a minted-but-unrecorded reply cap (leak) or a generation-guarded
record-set that silently no-ops. Safe D5 design (Pass 2):

1. mint under rank 4 (existing code),
2. change `set_reply_cap_waiter_cap` to return `bool` (its generation guard
   already detects the stale case — the signal is just discarded today),
3. on `false`, roll back the mint via `rollback_materialized_recv_cap`.

This requires a signature change plus a new rollback branch on live reply
traffic — gated on QEMU smoke. Deferred.

### 13.5 CapRights widening — explicitly deferred

Stage 104 does not touch the capability layout. `CapRights` widening (C6)
remains deferred to a later stage; nothing in the D1 transfer arm requires
it (rights pass through `Capability::derive` unchanged).

### 13.6 D4 — no further module moves this pass

No additional syscall module splits were performed: mixing mechanical churn
with a semantic IPC change would defeat the per-PR reviewability that D4
exists to provide. The Stage 102 split (`syscall/debug.rs`,
`syscall/initramfs.rs`) is unchanged.

### 13.7 Stage 104 test additions (net +6: 8 new, 2 Stage 103 tests replaced)

In `kernel::cap_transfer_split::tests`:
- `stage104_live_wire_call_sites_present` (replaces
  `stage103_helper_only_no_live_call_sites` per the Stage 103 test rule):
  router defined; split engine called; both routed seams present; canonical
  helper retained at definition + router fallback + legacy path + NR 30.
- `stage104_validation_labels_present` (replaces
  `stage103_validation_labels_present`): `D1_LIVE_SPLIT`,
  `FALLBACK_GLOBAL_LOCK`, and the `NOT SMOKE-ACCEPTED` disclosure.

In `kernel::syscall::tests`:
- `stage104_router_supported_transfer_routes_through_split_engine`
  (telemetry proves routing; cap present in receiver cnode)
- `stage104_router_transfer_plain_also_routes_through_split_engine`
- `stage104_router_shared_mem_opcode_stays_on_canonical_path` (telemetry 0)
- `stage104_router_reply_cap_falls_back_to_canonical_path` (telemetry 0;
  canonical WrongObject; envelope consumed per canonical contract)
- `stage104_router_equivalence_with_canonical_for_supported_case`
  (byte-equal CapId, slot object, slot rights, memory-object cap_refcount,
  delegation-link count across two identical states)
- `stage104_router_materialize_failure_error_matches_canonical`
  (source cap revoked after stash: identical error + identical envelope
  consumption on both paths)

Stage 103's remaining 12 tests (classification, Phase A/B, equivalence)
continue to pass and remain the equivalence contract.

### 13.8 Recommended Pass 2

1. **Smoke-accept Pass 1**: run x86_64 `-smp 1` core smoke + optional-FS
   strict smoke on this branch in a QEMU-capable environment. Watch for
   `YARM_D1_SPLIT_MATERIALIZE` on PM/VFS cap-transfer traffic and verify
   `INIT_SPAWN_V5_WRONG_SENDER_REPLY count=0` is preserved.
2. **D5 reply-cap split** per the §13.4 design (bool-returning record set +
   mint rollback), smoke-gated.
3. **D2 (IPC recv blocking split)** after D5: register recv waiters under
   IPC lock; global lock only for the scheduler block/dispatch step.
4. Optional D4 continuation: `syscall/recv_shared_v3.rs` mechanical move
   (separate PR from any semantic change).

---

## 14. Stage 105 / Pass 2 — D5 reply-cap split live-wired

D5 splits the reply-cap recv materialization out of the global lock using a
fallible Phase B' record-write with explicit mint rollback.

**NOT SMOKE-ACCEPTED:** developed without QEMU. Per MUST_SMOKE the branch
requires x86_64 `-smp 1` core smoke and optional-FS strict smoke before
merge acceptance.

### 14.1 The atomicity solution (Pass 1's exact blocker)

Pass 1 blocked D5 on the rank-4 mint → rank-3 `set_reply_cap_waiter_cap`
race window. Pass 2 solves it as the audit doc §12.7 predicted:

1. New fallible primitive `KernelState::try_set_reply_cap_waiter_cap`
   returns `ReplyRecordSetOutcome::{Set, IndexOutOfRange, GenerationMismatch,
   SlotEmpty}`. The existing `set_reply_cap_waiter_cap` becomes a thin
   wrapper that discards the outcome (still used by the canonical reply arm
   under the global lock, where staleness is unreachable).
2. New Phase B' helper `phase_b_prime_record_reply_cap` calls the fallible
   primitive and, on any non-`Set` outcome, drives mint rollback via
   `rollback_materialized_recv_cap` (the existing helper with
   `is_reply_cap=true`) and surfaces `SyscallError::WrongObject` — the same
   error the canonical path produces for `KernelError::StaleCapability`.
3. New telemetry: `IpcPathTelemetry::d5_split_reply_materializations` and
   `d5_split_reply_rollbacks`. Smoke logs additionally see
   `YARM_D5_SPLIT_MATERIALIZE` (success), `YARM_D5_SPLIT_RECORD` (Phase B'
   success), `YARM_D5_SPLIT_RECORD_ROLLBACK reason=...` (Phase B' rollback),
   and `D5_REPLY_RECORD_SET_STALE reason=...` (from the fallible primitive).

### 14.2 What is live now (D5)

`syscall.rs::materialize_received_message_cap_routed` (label
`D1_LIVE_SPLIT`) is extended with a D5 reply-cap arm after the D1 transfer
arm. Both arms route from exactly the same two delivery seams:

1. `complete_blocked_recv_for_waiter` — recv-v2 blocked-receiver delivery
2. `try_split_recv_queued_plain_with_snapshot_locked` — queued split-recv

Supported scope routed through
`cap_transfer_split::materialize_split_reply_cap_equivalent`:
- `FLAG_REPLY_CAP`, `opcode != OPCODE_SHARED_MEM`

### 14.3 What stays on the global-lock fallback

| Case | Why |
|------|-----|
| Reply-cap with `OPCODE_SHARED_MEM` | shared-region semantics outside D5 scope |
| Sender-waiter cap-transfer refills | `recv_core.rs FallbackReason::SenderWaiterWake` unchanged |
| Legacy full recv path (`handle_ipc_recv_result_with_empty_error`) | intentionally unrouted (canonical helper called directly) |
| NR 30 RecvSharedV3 handler | intentionally unrouted |

The canonical `materialize_received_message_cap` reply arm is **unchanged**
and remains authoritative.

### 14.4 Rank order and the atomicity proof

D5 routed sequence (sequential acquire/release, never nested):

| Phase | Lock | Operations |
|-------|------|------------|
| A.1 | IPC rank 3 | `take_transfer_envelope` |
| A.2 | capability rank 4 read | `capability_object_live` |
| A.3 | task rank 2 read | `task_cnode(receiver)` |
| B | capability rank 4 mutate | `mint_capability_in_cnode` |
| B' | IPC rank 3 mutate | `try_set_reply_cap_waiter_cap` (+ rollback) |
| C | none | writeback (caller) |

**Atomicity proof:** between Phase B (rank 4 mint) and Phase B' (rank 3
record set), any other CPU that revokes/reuses the reply record changes the
slot's generation or sets the slot to `None`. Phase B's fallible primitive
detects both modes and returns a non-`Set` outcome. The Phase B' rollback
revokes the freshly-minted slot via `fast_revoke_reply_cap_in_cnode` and
clears the (now-stale) `waiter_cap_id` via `clear_reply_cap_waiter_cap`.
Net effect: the receiver cnode contains exactly what it would have if the
revoke had landed BEFORE the mint, and the receiver gets `WrongObject` — the
same error code the canonical path produces for that exact race outcome
under the global lock. Mint leak: impossible by construction.

### 14.5 Stage 105 test additions (net +6: 7 new, 1 renamed)

`kernel::syscall::tests`:
- `stage105_router_reply_cap_routes_through_d5_split_engine` — telemetry
  proves routing; minted cap object matches the captured reply object.
- `stage105_router_reply_cap_equivalence_with_canonical_for_supported_case`
  — two identical states; byte-equal CapId, slot object, slot rights.
- `stage105_router_reply_cap_stale_record_rolls_back_mint` — manual race
  injection: `phase_a_take_reply_envelope` → revoke record →
  `phase_b_mint_reply_cap` → `phase_b_prime_record_reply_cap` returns
  `WrongObject`; receiver cnode slot is gone.
- `stage105_router_reply_cap_phase_a_failure_does_not_count_rollback` —
  bogus handle: telemetry stays at 0 success / 0 rollback.
- `stage105_phase_b_prime_rollback_increments_rollback_telemetry` —
  success-path telemetry; source-scan that the engine wrapper calls
  `note_d5_split_reply_rollback` on Failed.
- `stage105_router_reply_cap_wrong_object_caught_by_d5_phase_a` (renamed
  from Stage 104's `_falls_back_to_canonical_path`): non-Reply envelope
  under FLAG_REPLY_CAP — D5 Phase A returns `WrongObject`; telemetry stays 0.
- `stage105_canonical_reply_arm_remains_authoritative` — canonical helper
  retained at ≥ 4 sites; the discarding `set_reply_cap_waiter_cap` wrapper
  is still used by the canonical arm.

### 14.6 Invariants reaffirmed

- `SYSCALL_COUNT = 31`, `STARTUP_SLOT_COUNT = 18`, SpawnV5 ABI,
  `recv_shared_v3` ABI offsets, image IDs 7–12 — **unchanged**.
- Stage 100 FS gates — **unchanged**.
- D1 transfer arm unchanged. Test counts: workspace lib 1323/0 (+6 net);
  yarm-fs-servers 572/0; yarm-control-plane-servers 130/0.

### 14.7 D2 and D3 — Pass 2 status

**D2 (IPC recv blocking precursor):** scope is large enough that
implementing it without QEMU smoke would re-create the Pass 1 D5 risk
posture. Pass 2 defers D2 with the audit captured in §15 below (helper-only
scaffold + no-lost-wakeup audit, no live wiring).

**D3 (VmAnonMap/VmBrk two-phase split):** existing plan types
(`VmAnonMapProgressPlan`, `VmAnonMapRollbackTlbPlan`, `TlbShootdownWaitPlan`,
`VmBrkShrinkTlbPlan`) are already consumed inside the global-lock
`handle_vm_anon_map` / `handle_vm_brk` paths (see scaffold status §2). The
live-wire delta is plumbing the SharedKernel seam to release between PTE
mutation, TLB shootdown, and frame reclaim. Same QEMU smoke gate as D2 and
D5: deferred this pass. Audit at §16.
