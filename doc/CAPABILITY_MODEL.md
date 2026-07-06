<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Umut Deniz Balkan -->

# YARM Capability Model

> **Ownership rule.** All capability documentation — rights model, CSpace
> access policy, lock-rank ordering, domain rules, rights-width audit —
> lives here. New capability fragment files are forbidden; update this doc
> instead. See `doc/DOCUMENTATION_MAP.md`. The locking architecture spec
> lives in `doc/KERNEL_LOCKING.md`; the kernel directive split status
> lives in `doc/KERNEL_UNLOCKING.md`.

---

## 1. CapRights — current allocation (frozen at `u8`)

`CapRights` is defined in `crates/yarm-kernel/src/capability.rs` as a
private `u8` newtype and is re-exported through the kernel and userspace
runtime facades. **All eight bits are currently assigned**:

| Bit | Mask | Right |
|-----|------|-------|
| 0 | `0x01` | `READ` |
| 1 | `0x02` | `WRITE` |
| 2 | `0x04` | `MAP` |
| 3 | `0x08` | `SEND` |
| 4 | `0x10` | `RECEIVE` |
| 5 | `0x20` | `SCHEDULE` |
| 6 | `0x40` | `SIGNAL` |
| 7 | `0x80` | `WAIT` |

There is no unused in-band bit. **A ninth right cannot be represented by
the current `u8` bitset.** Widening to `u16` is a documented future
audit; it stays deferred until a real ninth right is required.

### Public ABI surface for rights

The public IPC / syscall ABI mostly exposes **CapIDs**, not raw rights
masks:

- Syscall arguments and returns carry `CapId` values in integer registers.
- recv-v2 metadata (40 bytes) carries status / opcode / message flags,
  payload length, receiver-local transferred `CapId`, recv-meta flags,
  sender / status lanes; **no rights mask**.
- `Message` carries a transferred `CapId` (`u64`) plus message
  flags / opcode and payload bytes; **no rights mask**.
- Startup slots, SpawnV5, VFS grant replies, driver-manager grant replies
  carry CapIDs as `u64` / `u32` scalars or transferred caps; **not
  rights masks**.
- `yarm-ipc-abi` service protocols encode CapIDs and operation flags,
  **not** `CapRights` bitsets.

The main ABI-visible `CapRights` exposure is the **Rust API surface**:
`CapRights` is re-exported from `yarm-user-rt` and from the kernel
extraction bridge. `tests/extraction_bridge_tests.rs` asserts that
extracted / re-exported `CapRights` has the same size as
`yarm_kernel::capability::CapRights` — width changes are observable to
Rust users and compatibility tests even if the wire format keeps passing
only CapIDs.

### Rights-storage layout sensitivity

These types are layout-sensitive (not `repr(C)`, not directly copied to
userspace, but width / footprint matters):

- `Capability { object: CapObject, rights: CapRights }`
- `CapEntry` (embeds `Capability` + parent `CapId`)
- `CapSlot` (embeds `Option<CapEntry>` + generation)
- `CapabilitySpace` (all `CapSlot`s in allocator-backed CNode arrays)

`TransferEnvelope` and `ReplyCapRecord` do **not** store a standalone
rights field. Transfer materialization re-resolves the source capability
and grants rights from the current `Capability`; reply-cap records store
`CapObject` and `CapId` state, not a serialized rights mask.

---

## 2. Kernel CSpace access policy

### Policy

1. **Task-execution paths MUST use task-local capability lookup.** Any
   path acting on behalf of the currently running task (syscall IPC
   send / recv, map / unmap / protect, task-fault handling) must resolve
   capabilities from the **current task's CNode**.

2. **Global kernel capability access is allowed only for kernel-internal
   orchestration.** Examples: delegation records, driver runtime-cap
   revocation, transfer-envelope staging helpers that intentionally
   operate on globally minted capabilities.

3. **All global access must use explicit helper APIs.** Never direct
   `self.cspace.*`:
   - `kernel_global_capability(...)`
   - `kernel_global_capability_has_right(...)`
   - `revoke_kernel_global_capability(...)`

This naming is intentional: reviewers can spot global-kernel access at a
glance and decide if it is justified.

---

## 3. Capability domain — lock-rank contract

| Rank | Lock | Protected subsystem |
|------|------|---------------------|
| 1 | `scheduler_state` | Per-CPU runqueue, dispatch, preemption counters |
| 2 | `task_state_lock` | TCB allocation, task status, CPU affinity |
| 3 | `ipc_state_lock` | Endpoints, notifications, `reply_caps`, `transfer_envelopes`, `cross_cpu_work` |
| 4 | `capability_state_lock` | CNode spaces, `process_cnodes`, `delegated_capability_links` |
| 5 | `vm_state_lock` | Page tables, ASID map, TLB shootdown coordination |

**Always acquire locks in strictly ascending rank order.** Any path that
needs both IPC (rank 3) and capability (rank 4) state must acquire IPC
first. The two-phase create pattern (§5) is the canonical way to respect
this order.

**Split-mut seam (Stage 186A, infrastructure only).** The capability domain
(rank 4) now has a `SharedKernel` split-mut seam,
`with_capability_state_split_mut`, exposing only `&mut CapabilitySubsystem`
under `capability_state_lock` — completing the per-domain seam set (ranks 1–6,
see `doc/KERNEL_LOCKING.md §0.1`). It is `M2_SEAM_HELPER_ONLY`: **no live
capability/cnode path is migrated onto it yet.** When a future vertical slice
does use it, the rank order above is what makes it safe — a caller holding no
IPC (rank 3) lock invokes it *after* dropping `ipc_state_lock`, so cap
materialization never runs under the IPC lock (§8). This seam does not change
the current locking behaviour of any live path.

**Cap-transfer materialize is not cap-only (Stage 186D-prereq, HARD-STOP).** An
attempt to migrate the received-cap materialization engine onto this rank-4 seam
was audited and stopped: materializing a received transfer/reply cap spans four
subsystems, not one. `task_cnode` fuses task (rank 2) + capability (rank 4);
`capability_object_live` reads IPC (rank 3) for endpoint/notification objects;
`mint_capability_in_cnode` installs the cnode slot (rank 4) **and** bumps the
memory-object `cap_refcount` (rank 6) in the *same* critical section — splitting
them opens a reclaim race (object freed while a fresh cnode slot references it);
and the reply arm records the waiter cap under IPC (rank 3) *after* the rank-4
mint. `with_capability_state_split_mut` exposes only `&mut CapabilitySubsystem`,
so it cannot carry any of these. A cap-transfer seam therefore requires a joint
capability↔memory decomposition first; deferred as `CAP_TRANSFER_SEAM_DEFERRED`.
Pinned by `stage186d_cap_transfer_engine_seam_entanglement`.

**Atomic capability↔memory mint (Stage 186D-proper, infrastructure only).** The
mint/refcount half of that joint decomposition now exists as seam-only
infrastructure: `SharedKernel::mint_capability_with_memory_ref_split` (in
`boot/cap_memory_mint_split.rs`) mints a cap into an existing cnode while keeping
the referenced memory-object `cap_refcount` (rank 6) and the published cnode slot
(rank 4) consistent, using **Model A — pre-bump then install**: bump the object's
`cap_refcount` under the memory seam (object protected before any slot references
it), then publish the slot under the capability seam with a fresh receiver-local
`CapId`, rolling the refcount back if the publish fails. The two critical sections
are disjoint, so it holds only one subsystem lock at a time (deadlock-free) and
never takes `ipc_state_lock` — no cap materialization under IPC, no cap→IPC rank
inversion. It takes an object+rights `Capability` (never echoes a sender-local
CapId as authority) and returns `StaleCapability`/`CapabilityFull`/`TaskMissing` as
real errors. `M2_SEAM_HELPER_ONLY`: **not wired live** — it does not by itself
convert `ipc_reply` or retire the lock, and does not solve the reply-cap IPC
rank-inversion blocker. It is the atomic-mint building block for a future
cap-transfer seam. Guarded by `stage186d_proper_cap_memory_mint_atomicity`.

**First cap-transfer materialization seam slice (Stage 186D2, infrastructure
only).** Built on the atomic mint: `SharedKernel::materialize_received_cap_snapshot_split`
and `materialize_received_message_cap_routed_split` (in
`boot/cap_transfer_materialize_split.rs`) take a plain, IPC-lock-free
`TransferCapSnapshot { receiver_cnode, object, rights }` — captured *after* the
transfer envelope was consumed under `ipc_state_lock` — and mint an ordinary
object cap into the receiver's cnode via `mint_capability_with_memory_ref_split`.
No `ipc_state_lock`, no broad `&mut KernelState`, no cap→IPC rank inversion. The
snapshot carries object identity + rights, **never** a sender-local `CapId` (local
CapIds are not transferable authority); the receiver-local CapId is freshly minted.
Reply objects (`CapObject::Reply`) route to an explicit `DeferredReplyCap`
(`reply_cap_ipc_rank_inversion`) — never faked as seam-supported, no reply-cap
success marker. `StaleCapability`/`CapabilityFull`/`TaskMissing` are real errors;
`WrongObject`/`MissingRight` are upstream. `M2_SEAM_HELPER_ONLY`: **not wired
live**, and **not yet a live-equivalent** of `grant_task_to_task_with_rights` — it
does not yet record the source→dest delegation link (revocation propagation); that
rank-4 follow-on must land before any live-wiring. It does not by itself convert
`ipc_reply` or retire the lock. Guarded by
`stage186d2_cap_transfer_materialize_seam_first_slice`.

**Delegation-link seam (Stage 186D3, infrastructure only).** Ordinary
cap-transfer materialization becomes seam **live-equivalent** by recording the
sender→receiver delegation link the legacy grant records (so revoking a source
cap propagates to the derived receiver cap). The link is pure capability-domain
(rank 4) metadata (`delegated_capability_links`), recorded via
`SharedKernel::record_cap_delegation_link_split` under the rank-4 capability seam
only — no IPC/task/memory lock.
`materialize_received_cap_snapshot_with_delegation_split` mints via the Stage
186D2 seam (atomic mint), records the link (when `source_tid != dest_tid`), and on
record failure rolls the mint fully back via `rollback_minted_cap_split` (clear
receiver cnode slot rank 4 → drop `cap_refcount` + reclaim rank 6 — slot cleared
before refcount, so no live slot references a dropped-refcount object; no stale
slot, no stale delegation edge, no refcount leak). The `TransferCapDelegation`
carries `source_cap` as a recorded revoke edge only — never resolved-to-mint,
never receiver authority. Reply objects stay `DeferredReplyCap`
(`reply_cap_ipc_rank_inversion`) and are never delegated (reply caps are one-shot,
outside the delegation tree). `StaleCapability`/`CapabilityFull`/`TaskMissing`
remain real errors. `M2_SEAM_HELPER_ONLY`: **not wired live**; it does not by
itself convert `ipc_reply` or retire the lock. Guarded by
`stage186d3_cap_transfer_delegation_link_seam`.

**Live-wiring hard stop (Stage 186D4).** Wiring the ordinary cap-transfer seam
into the live recv path was audited and **stopped**: both live materialization
sites run inside a `with`/`with_cpu` closure holding the global
`SpinLock<KernelState>` with a live `&mut KernelState`, and the `SharedKernel`
seam derives `&mut Subsystem` from `self.state.data_ptr()` — calling it there
would alias the global-lock `&mut` (UB). Releasing the global lock before
materialize is broad IPC decomposition (Stage 187), out of scope. The seam stays
helper-only; no live seam marker is emitted. Reply-cap materialization, `ipc_reply`
conversion, and full global-lock retirement remain deferred. Pinned by
`stage186d4_ordinary_cap_transfer_live_wiring_hard_stop`.

**Recv delivery boundary split (Stage 187A, LIVE).** The queued-split recv path
now drops the broad `&mut KernelState` **before** the user-space writeback:
Phase A (under the lock) dequeues, materializes the cap via the **legacy** routed
engine (reply-cap arm unchanged — still never seam-materialized), applies the
sender wake, and snapshots the delivery by value; Phase B copies through the
Stage 186E `copy_to_user_split` seam after the borrow is dead; Phase C rolls back
the receiver-local cap (§58) on writeback failure via a brief global re-entry.
Cap materialization itself still runs under the global lock — the 186D2/186D3
cap-transfer seam is **not** wired yet; this boundary is what makes wiring it a
mechanical follow-on. No cap is ever materialized under `ipc_state_lock`; no
sender-local CapId crosses the boundary as authority. Guarded by
`stage187a_ipc_recv_delivery_boundary_split`.

**Cap-transfer seam LIVE on the recv boundary (Stage 187B).** Ordinary
(non-reply, non-shared-region) transferred caps received on the 187A queued-split
boundary by a user task are now materialized through the 186D2/186D3 seam — the
first live use of the cap-transfer materialization + delegation seam. Phase A
(under the lock) consumes the transfer envelope once and snapshots
object+rights+cnode + the delegation identity by value (`source_cap` is the
delegation parent edge only, never receiver authority); Phase B (after the borrow
drops) runs `materialize_received_message_cap_routed_with_delegation_split`
(atomic cap↔memory mint + delegation link), commits the fresh receiver-local
CapId, wakes the sender, then copies via the 186E seam. A writeback failure rolls
the cap back (revoke + delegation removal + refcount drop). Reply caps stay on the
legacy in-lock router (`reply_cap_ipc_rank_inversion` — reply-cap materialization
remains deferred); shared-region and kernel-register receivers also stay legacy.
No cap is materialized under `ipc_state_lock`. Guarded by
`stage187b_ordinary_cap_transfer_seam_live_on_recv_boundary`.

**`ipc_reply` retry hard stop (Stage 187C).** Retrying the `ipc_reply` conversion
with the 187A/187B boundary was audited and **stopped**: `ipc_reply`'s reply
payload copy and any cap materialization to the caller happen inside
`complete_blocked_recv_for_waiter` — the shared blocked-waiter delivery path
(reply/send/fault) that runs under the broad `&mut KernelState` and was not
boundary-split. Splitting it is out-of-scope blocked-waiter decomposition; a
reply-with-cap additionally hits the unsolved `reply_cap_ipc_rank_inversion`
(mint at capability rank 4 then `set_reply_cap_waiter_cap` at IPC rank 3). The
reply-cap consume/revoke/enqueue/wake call no seam and need no boundary.
Reply-cap materialization remains deferred. Pinned by
`stage187c_ipc_reply_retry_hard_stop`.

### What may NOT happen under each lock

#### Under `ipc_state_lock` (rank 3)

- No user-memory copy (`copy_from_user` / `copy_to_user`).
- No `TrapFrame` writes.
- No cap mint, revoke, or materialization (`mint_capability_*`,
  `revoke_*`).
- No VM mapping operations.
- No scheduler queue mutation (enqueue / dequeue).
- No TCB field mutation.

#### Under `capability_state_lock` (rank 4)

- No user-memory copy.
- No `TrapFrame` writes.
- No VM mapping operations.
- No scheduler queue mutation.
- No IPC endpoint mutation (send / recv / enqueue / dequeue).
- No TCB field mutation.

#### Under scheduler / task locks (ranks 1–2)

- No IPC endpoint mutation unless a documented rank order proves it safe.
- No cap materialization unless a documented rank order proves it safe.

---

## 4. Two-phase create pattern

Objects shared between the IPC and capability domains must be created in
two phases to avoid acquiring both locks simultaneously:

```text
Phase 1 — under ipc_state_lock (rank 3):
  - Find a free slot in the appropriate IPC array.
  - Bump the generation counter.
  - Store the new object.
  - Capture (slot_index, generation).
  - Release ipc_state_lock.

Phase 2 — under capability_state_lock (rank 4, acquired separately):
  - Mint one or more capabilities referencing (slot_index, generation).
  - Return the CapId(s) to the caller.
```

This is the pattern used by `create_endpoint` and `create_notification`.
At return, both domains are coherent: the IPC slot is occupied and the
caps are visible via `resolve_capability_for_task` and
`capability_for_cnode`.

Do **NOT** acquire both locks simultaneously ("merge the phases") — this
inverts rank order.

---

## 5. Reply-cap one-shot rule

A `ReplyCapRecord` in `IpcSubsystem.reply_caps` is consumed exactly once:

1. Created by `create_reply_cap_for_caller` under `ipc_state_lock`.
2. Consumed by `ipc_reply` under `ipc_state_lock` (slot set to `None`).
3. Revoked early by `revoke_reply_caps_for_caller` on task death / restart
   under `ipc_state_lock`.

After consumption or revocation, the corresponding `CapObject::Reply` in
the caller's CNode is stale. Attempts to use it must fail with
`KernelError::StaleCapability`.

---

## 6. Transfer-envelope cleanup ownership

`TransferEnvelope` objects in `IpcSubsystem.transfer_envelopes` follow
this ownership chain:

- Created and stashed under `ipc_state_lock` by the sender.
- Taken and consumed under `ipc_state_lock` by the receiver.
- Abandoned envelopes (sender dies before receiver takes) must be cleaned
  up under `ipc_state_lock`.

The recv-delivery paths materialize the transferred / reply cap into the
receiver's CNode (and consume the transfer envelope) **before** the
metadata / payload `copy_to_user` that may fault. If that copy fails, the
message is dropped — so the materialized cap must be rolled back via
`rollback_materialized_recv_cap`:

- **Reply cap** → `fast_revoke_reply_cap_in_cnode` (no `cap_refcount`) +
  clear the global `waiter_cap_id` (`clear_reply_cap_waiter_cap`,
  generation-guarded). The `ReplyCapRecord` stays live; the reply remains
  re-deliverable.
- **Transfer cap** → `revoke_capability_in_cnode` (removes delegation
  link, decrements `cap_refcount`, reclaims if unreferenced).

Rollback is **idempotent**: a second call returns `false` (slot already
cleared) and never underflows `cap_refcount`. The envelope itself is
consumed exactly once.

---

## 7. Cap slot ownership

- Each CNode slot is owned by exactly one task (identified by `CNodeId`).
- A slot is live as long as the `CapabilitySpace` entry is `Some` AND the
  referenced IPC-domain object (endpoint / notification / reply) is
  present with matching generation.
- `fast_revoke_reply_cap_in_cnode` and `revoke_capability_in_cnode` are
  the **only** functions that may null a slot. Direct
  `cspace.slots[i] = None` is forbidden outside these helpers.

---

## 8. No cap materialization under IPC lock

The IPC send / recv fast paths must **not** call `mint_capability_*` or
`revoke_capability_*` while holding `ipc_state_lock`. Cap materialization
(reply-cap creation, delegation, revoke) must happen either before the
IPC lock is acquired or after it is released, using the two-phase pattern
(§4).

---

## 9. Test rules

### 9.1 Direct cspace mutation in tests

`#[cfg(test)]` code may use the `cspace_for_cnode` /
`cspace_for_cnode_mut` helpers (which bypass the capability lock) only
for **introspection after the operation under test has completed**. Tests
must never call these helpers to set up state that would later be
observed by a production code path; use the approved lifecycle helpers
(`mint_capability_*`, `revoke_capability_*`).

### 9.2 Correct pattern for two-phase tests

Do **NOT** nest `state.with_ipc_state` calls inside a
`state.with_capability_state` closure (or vice versa) — that re-enters
the lock from inside the closure and deadlocks.

```rust
// Good: sequential, each closure released before the next.
let (ep_idx, send_cap, recv_cap) = state.create_endpoint(4).expect("endpoint");
let ep_present = state.with_ipc_state(|ipc| ipc.endpoints[ep_idx].is_some());
assert!(ep_present);
let cap = state.resolve_capability_for_task(0, send_cap).expect("cap");
```

### 9.3 Grant-with-rights test pattern

When testing attenuation or widening rejection, mint the source cap
first, then call `grant_capability_task_to_task_with_rights` as a
separate statement (nesting triggers E0499 double-mutable-borrow).

### 9.4 Common mistakes

| Mistake | Consequence | Correct |
|---------|-------------|---------|
| `CapObject::ReplyCap { .. }` | Compile error — variant is `Reply` | `CapObject::Reply { .. }` |
| Nested `state.method()` in argument to another `state.method()` | E0499 double-mutable-borrow | Split into separate `let` bindings |
| Direct `self.ipc.*` in a new scheduler helper | Bypasses `ipc_state_lock` | Wrap in `with_ipc_state` / `with_ipc_state_mut` |
| Acquire capability lock then IPC lock | Lock-rank inversion (deadlock) | Two-phase: IPC first, capability second |

---

## 10. Capability error semantics

These errors are **fatal unless explicitly specified otherwise** (see
`doc/AI_AGENT_RULES.md` §1.7):

- `MissingRight` — caller lacks the required right.
- `WrongObject` — cap refers to the wrong kernel object type.
- `StaleCapability` — cap has been consumed or revoked.
- `MaterializeFailed` — cap could not be installed in the receiver
  cspace.

The only permitted recovery is explicit fallback logic documented in the
relevant milestone (e.g. Phase 3A falls back to Phase 2B on `Unsupported`
from `VFS_OP_FILE_GRANT_RO` — see `doc/PROJECT_HISTORY.md`). All other
capability errors must propagate as hard failures.

---

## 11. Capability transfer rules

(Full agent-facing contract: `doc/AI_AGENT_RULES.md` §1.)

- **Never encode local CapIDs in payload bytes as authority.** CapIDs are
  cspace-local; embedding a cap ID in an IPC message payload and
  treating it as transferable authority is wrong.
- **Authority transfer must use the real IPC transferred-cap path:**
  sender sets `FLAG_CAP_TRANSFER_PLAIN` in the IPC flags word and places
  the local cap ID in the designated transfer field; kernel stashes the
  cap on the pending IPC and strips it from the sender's cspace;
  receiver finds the cap materialized into its cspace via
  `received.transferred_cap`.
- **Use `FLAG_CAP_TRANSFER_PLAIN = 1 << 2` for reply-with-cap.** It does
  not strip an opcode prefix from the payload. Do **not** use the older
  `FLAG_CAP_TRANSFER` for plain replies (it triggers opcode-prefix
  stripping).
- **Reply caps are one-shot and non-delegatable.** A reply cap created by
  `ipc_call` is consumed exactly once by `ipc_reply`. It cannot be
  delegated, stored, or used to send additional messages. A second
  reply returns `StaleCapability`.
- **Reply-cap cleanup uses fast revoke** (`IPC_FAST_REVOKE`). Do not
  traverse the general revocation / delegation graph for reply-cap
  cleanup.

---

## 12. Authoring rule

Future capability-model changes update **this file** and the
`crates/yarm-kernel/src/capability.rs` source. Do **not** create new
`CAPABILITY_*` / `CSPACE_*` fragment files.
