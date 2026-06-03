<!-- SPDX-License-Identifier: Apache-2.0 -->

# Capability Domain Rules

This document records invariants that govern how the capability domain interacts
with the IPC, scheduler, and VM lock domains.  It is a companion to
`doc/KERNEL_LOCKING.md` and `doc/KERNEL_CSPACE_ACCESS_POLICY.md`.

---

## 1. Lock-rank contract

| Rank | Lock | Protected subsystem |
|------|------|---------------------|
| 1 | `scheduler_state` | Per-CPU runqueue, dispatch, preemption counters |
| 2 | `task_state_lock` | TCB allocation, task status, CPU affinity |
| 3 | `ipc_state_lock` | Endpoints, notifications, reply_caps, transfer_envelopes, cross_cpu_work |
| 4 | `capability_state_lock` | CNode spaces, process_cnodes, delegated_capability_links |
| 5 | `vm_state_lock` | Page tables, ASID map, TLB shootdown coordination |

**Always acquire locks in strictly ascending rank order.**  Any path that needs
both IPC (rank 3) and capability (rank 4) state must acquire IPC first.  The
two-phase create pattern (§3) is the canonical way to respect this order.

---

## 2. What may NOT happen under each lock

### Under `ipc_state_lock` (rank 3)
- No user-memory copy (`copy_from_user` / `copy_to_user`)
- No TrapFrame writes
- No cap mint, revoke, or materialization (`mint_capability_*`, `revoke_*`)
- No VM mapping operations
- No scheduler queue mutation (enqueue/dequeue)
- No TCB field mutation

### Under `capability_state_lock` (rank 4)
- No user-memory copy
- No TrapFrame writes
- No VM mapping operations
- No scheduler queue mutation
- No IPC endpoint mutation (send/recv/enqueue/dequeue)
- No TCB field mutation

### Under scheduler/task locks (ranks 1–2)
- No IPC endpoint mutation unless a documented rank order proves it safe
- No cap materialization unless a documented rank order proves it safe

---

## 3. Two-phase create pattern

Objects shared between the IPC and capability domains must be created in two
phases to avoid acquiring both locks simultaneously:

```
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

This is the pattern used by `create_endpoint` and `create_notification`.  At
call return, both domains are coherent: the IPC slot is occupied and the caps
are visible via `resolve_capability_for_task` and `capability_for_cnode`.

Do NOT acquire both locks simultaneously ("merge the phases") — this inverts
rank order (rank 4 would be held while rank 3 is acquired, or vice versa).

---

## 4. Reply-cap one-shot rule

A `ReplyCapRecord` in `IpcSubsystem.reply_caps` is consumed exactly once:

1. Created by `create_reply_cap_for_caller` under `ipc_state_lock`.
2. Consumed by `ipc_reply` under `ipc_state_lock` (slot set to `None`).
3. Revoked early by `revoke_reply_caps_for_caller` on task death/restart
   under `ipc_state_lock`.

After consumption or revocation the corresponding `CapObject::Reply` in the
caller's CNode is stale.  Attempts to use it must fail with
`KernelError::StaleCapability`.

---

## 5. Transfer-envelope cleanup ownership

`TransferEnvelope` objects in `IpcSubsystem.transfer_envelopes` follow this
ownership chain:

- Created and stashed under `ipc_state_lock` by the sender.
- Taken and consumed under `ipc_state_lock` by the receiver.
- Abandoned envelopes (sender dies before receiver takes) must be cleaned up
  by `maybe_cleanup_process_cnode_for_pid` (which drops the IPC-domain record)
  or by a future explicit sweep.

Transfer envelopes live in the IPC domain (rank 3).  The capabilities they
carry are materialized in the capability domain (rank 4) only after the
envelope is taken, using the two-phase pattern.

### 5.1 Materialize rollback on recv copy failure (Stage 20)

The recv-delivery paths materialize the transferred/reply cap into the
receiver's CNode (and consume the transfer envelope) **before** the
metadata/payload `copy_to_user` that may fault.  If that copy fails, the
message is dropped — so the materialized cap must be rolled back via
`rollback_materialized_recv_cap` (the inverse of the materialization mint):

- Reply cap → `fast_revoke_reply_cap_in_cnode` (no `cap_refcount`) + clear the
  global `waiter_cap_id` (`clear_reply_cap_waiter_cap`, generation-guarded). The
  `ReplyCapRecord` stays live; the reply remains re-deliverable.
- Transfer cap → `revoke_capability_in_cnode` (removes delegation link,
  decrements `cap_refcount`, reclaims if unreferenced).

Rollback is idempotent: a second call returns `false` (slot already cleared) and
never underflows `cap_refcount`.  The envelope itself is consumed exactly once;
the rollback does not (and cannot) resurrect it.

---

## 6. Cap slot ownership

- Each CNode slot is owned by exactly one task (identified by `CNodeId`).
- A slot is live as long as the `CapabilitySpace` entry is `Some` and the
  referenced IPC-domain object (endpoint / notification / reply) is present with
  matching generation.
- `fast_revoke_reply_cap_in_cnode` and `revoke_capability_in_cnode` are the
  only functions that may null a slot.  Direct `cspace.slots[i] = None` is
  forbidden outside these helpers.

---

## 7. No cap materialization under IPC lock

The IPC send/recv fast paths must not call `mint_capability_*` or
`revoke_capability_*` while holding `ipc_state_lock`.  Cap materialization
(reply-cap creation, delegation, revoke) must happen either before the IPC
lock is acquired or after it is released, using the two-phase pattern (§3).

---

## 8. Test rules

### 8.1 Direct cspace mutation in tests

`#[cfg(test)]` code may use the `cspace_for_cnode` / `cspace_for_cnode_mut`
helpers (which bypass the capability lock) only for introspection after the
operation under test has completed.  Tests must never call these helpers to
set up state that would later be observed by a production code path; use the
approved lifecycle helpers (`mint_capability_*`, `revoke_capability_*`) instead.

### 8.2 Correct pattern for two-phase tests

When a test needs to verify that an object is visible in both domains after a
two-phase create, do NOT nest `state.with_ipc_state` calls inside a
`state.with_capability_state` closure (or vice versa) — that would re-enter the
lock from inside the closure and deadlock.  Instead:

```rust
// Good: sequential, each closure released before the next.
let (ep_idx, send_cap, recv_cap) = state.create_endpoint(4).expect("endpoint");
let ep_present = state.with_ipc_state(|ipc| ipc.endpoints[ep_idx].is_some());
assert!(ep_present);
let cap = state.resolve_capability_for_task(0, send_cap).expect("cap");
```

### 8.3 Grant-with-rights test pattern

When testing attenuation or widening rejection, mint the source cap first, then
call `grant_capability_task_to_task_with_rights` as a separate statement:

```rust
let src_cap = state.mint_capability_for_current_context(...).expect("mint");
let result = state.grant_capability_task_to_task_with_rights(0, src_cap, 1, rights);
```

Nesting `mint_capability_for_current_context` inside the `grant` call argument
list triggers a double-mutable-borrow error because both calls borrow `state`.

### 8.4 Common mistakes

| Mistake | Consequence | Correct pattern |
|---------|-------------|-----------------|
| `CapObject::ReplyCap { .. }` | Compile error — variant is `Reply` | `CapObject::Reply { .. }` |
| Nested `state.method()` in argument to another `state.method()` | E0499 double-mutable-borrow | Split into separate `let` bindings |
| Direct `self.ipc.*` in a new scheduler helper | Bypasses `ipc_state_lock` | Wrap in `with_ipc_state` / `with_ipc_state_mut` |
| Acquiring capability lock then IPC lock | Lock-rank inversion (deadlock) | Use two-phase pattern: IPC first, capability second |

---

## 9. Audit status

| Domain | Files audited | Result |
|--------|--------------|--------|
| Capability state (`self.capability.*`) | `capability_state.rs`, `capability_lifecycle_state.rs`, `cnode_state.rs`, `delegation_state.rs`, `capability_service_state.rs`, `task_core_state.rs` | **CLEAN** — all production accesses through `with_capability_state*`; two `#[cfg(test)]` exceptions acceptable |
| IPC state (`self.ipc.*`) | All `src/kernel/boot/` files | **CLEAN after fixes** — two direct-access bugs found and fixed in `scheduler_state.rs` |
| Scheduler state | `scheduler_state.rs`, `fault_state.rs` | **CLEAN** — no inappropriate cross-domain direct accesses |
| IPC cap-transfer / reply-cap materialize | `transfer_state.rs`, `ipc_state.rs`, `syscall.rs` | **CLEAN after Stage 20 fix** — recv copy-fault now rolls back the materialized cap (§5.1); no mint/revoke under `ipc_state_lock` |

Last audited: Stage 20 (2026-06-03).
