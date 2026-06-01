# YARM Kernel Test-Writing Rules

**Scope:** Rules for writing hosted-dev (`cargo test --no-default-features`) kernel
tests. These rules were distilled from diagnosing 16 test failures that arose from
subtle interactions between the scheduler, address-space model, capability cspace
layout, and the hosted-dev user-memory HashMap. Violating them produces tests that
pass locally by luck and fail under minor state changes.

---

## Rule 1 — Caps are minted into the *current* task's cspace

`create_reply_cap_for_caller`, `materialize_capability`, and similar mint calls
write into the cspace of whatever task `current_tid()` returns at the time of the
call, **not** the task you specify as the logical caller.

**Consequence:** If you call `dispatch_next_task()` before minting a reply cap, the
cap lands in the newly-dispatched task's cspace, not the intended caller's.

**Rule:** Always mint reply caps and materialize caps **before** calling
`dispatch_next_task()` or any function that calls it internally (such as
`block_current_cpu()` + `dispatch_next_task()`). Structure tests as:

```rust
// CORRECT
let reply_cap = state.create_reply_cap_for_caller(caller_tid, recv_cap, None)?;
state.enqueue_current_cpu(caller_tid)?;
state.dispatch_next_task()?;
state.idle_re_enqueue_for_test()?;   // Rule 2

// WRONG — cap goes into whoever is current after dispatch
state.enqueue_current_cpu(caller_tid)?;
state.dispatch_next_task()?;
let reply_cap = state.create_reply_cap_for_caller(caller_tid, recv_cap, None)?;
```

---

## Rule 2 — Re-enqueue idle after every `dispatch_next_task()` call

`dispatch_next_task()` → `dispatch_next_current_cpu()` → `dispatch_next()` in the
scheduler calls `membership_remove(idle.tid)` when it displaces the idle task in
favour of a runnable task. This permanently removes idle from the scheduler
membership table until it is explicitly re-enqueued.

**Consequence:** Any subsequent blocking operation (IPC recv, futex wait, exit) that
calls `block_current_cpu()` + `dispatch_next_task()` will find an empty membership
table and panic (no ready task to dispatch).

**Rule:** After every `dispatch_next_task()` call in a test, immediately call
`idle_re_enqueue_for_test()`:

```rust
state.dispatch_next_task()?;
state.idle_re_enqueue_for_test()?;  // keep idle in membership table
```

`idle_re_enqueue_for_test()` is a `#[cfg(test)]` helper on `KernelState` that
wraps `enqueue_on_cpu(CpuId(0), 0)`.  It is defined in `scheduler_state.rs`.

Exception: if the very next line in the test exits the process and no further
blocking occurs, the re-enqueue can be omitted — but it is always safe to add it.

---

## Rule 3 — Hosted-dev user memory is not auto-initialized; write before read

The hosted-dev user-memory backing store is a `HashMap<(asid_id, phys_addr), u8>`.
Pages mapped into an address space have **no entries** in the HashMap until
explicitly written. Reads from unwritten locations return 0 (the HashMap default),
but operations that *validate* user memory (e.g. `futex_wait_current`,
`validate_user_access_for_asid`) may fail or return unexpected results when entries
are absent.

Additionally, the HashMap key uses the **physical address**, not the virtual address.
Two address spaces that map the same `MemoryObject` (i.e. the same physical frame)
share the same physical address but have different ASID keys — writing to one ASID
does **not** create an entry for the other ASID.

**Rule:**

1. Write all required bytes to user memory before invoking any syscall that reads
   or validates that memory.
2. If two tasks share a physical page via a shared `MemoryObject`, write the
   initial value through **each** ASID separately.

```rust
// CORRECT — both ASIDs get an entry for the shared physical page
state.write_user_memory(tid1, 0x1000, &val.to_ne_bytes())?;
state.write_user_memory(tid0, 0x1000, &val.to_ne_bytes())?;

// WRONG — task0's futex_wake validate call finds no entry for asid0
state.write_user_memory(tid1, 0x1000, &val.to_ne_bytes())?;
// ... then state.futex_wake(0x1000, 1) fails with TaskMissing
```

---

## Rule 4 — Every task that touches user memory must have an ASID bound

Syscall paths that validate user memory (futex, shared-mem send, transfer, etc.)
check `task_asid(tid)` and return `TaskMissing` or `InvalidArgument` if no ASID is
bound. In hosted-dev, spawning a task does not auto-assign an ASID.

**Rule:** Call `bind_task_asid(tid, asid)` for every task that will participate in
a memory-validating syscall path, including the idle task (TID=0) if it will be the
scheduler's *current* task when such a path runs (e.g. during `futex_wake` after a
blocking wait displaced idle to current).

```rust
let (asid0, aspace_cap0) = state.create_user_address_space()?;
state.bind_task_asid(0, asid0)?;   // idle task needs asid for futex_wake path
let (asid1, aspace_cap1) = state.create_user_address_space()?;
state.bind_task_asid(1, asid1)?;
```

---

## Rule 5 — Scheduler status checks must accept `Running | Runnable`

After `exit_task(tid, code)` or `futex_wake` wakes a waiter, the newly dispatched
task may be in `TaskStatus::Running` (it is the current task on the CPU) or
`TaskStatus::Runnable` (in the run queue), depending on whether the test inspects
status before or after a subsequent `yield_current`. Either status is correct.

**Rule:** Use `matches!` with both variants:

```rust
assert!(matches!(
    state.task_status(tid),
    Some(TaskStatus::Runnable) | Some(TaskStatus::Running)
));
```

Never assert only `Runnable` after a wake or exit path — the task may already be
current.

---

## Rule 6 — `mark_task_dead` does not dispatch; `exit_task` does

`mark_task_dead(tid)` cleans up the task's capability cspace (calling
`maybe_cleanup_process_cnode_for_pid`) and marks the task dead, but does **not**
call `block_current_cpu()` or `dispatch_next_task()`. The dead task remains the
scheduler's current task until explicitly dispatched away.

`exit_task(tid, code)` (when `tid == current_tid()`) calls `block_current_cpu()`
then `dispatch_next_task()` — it leaves the scheduler in a dispatched state.

**Consequence for cap tests:** If you call `dispatch_next_task()` before
`mark_task_dead(tid)` with the intent of making `tid` non-current during the dead
marking, the dead task's cspace has already been cleaned up by the time any cap
assertions run. In particular, reply caps minted into that cspace will be revoked
as part of the cspace cleanup — this is the *intended* behavior under test.

**Rule:** For `mark_task_dead` tests, leave the target task as current (do not
dispatch away first). Call `mark_task_dead` directly. Do not add unnecessary
`enqueue_current_cpu` + `dispatch_next_task` before the mark-dead call.

```rust
// CORRECT — task1 is current; mark_task_dead cleans task1's cspace
let reply_cap = state.create_reply_cap_for_caller(ThreadId(1), recv_cap, None)?;
state.mark_task_dead(1)?;
// reply_cap should now be revoked (test asserts this)

// WRONG — extra dispatch corrupts the test: task1 is no longer current, so
// create_reply_cap_for_caller puts the cap in whoever is current after dispatch
state.enqueue_current_cpu(1)?;
state.dispatch_next_task()?;
let reply_cap = state.create_reply_cap_for_caller(ThreadId(1), recv_cap, None)?;
state.mark_task_dead(1)?;
```

---

## Rule 7 — Capacity constants must fit within address-space bookkeeping limits

Hosted-dev builds inflate some pool sizes for test coverage (e.g. `MAX_COW_PAGES`,
`MAX_NOTIFICATIONS`). Tests that iterate up to a pool capacity can exhaust the
address-space mapping table (`MAX_MAPPINGS`) before reaching the pool limit.

`MAX_MAPPINGS = 128` is the address-space bookkeeping limit. A COW exhaustion test
that maps up to `MAX_COW_PAGES / 2 + 1` pages will fail with `VmError` if
`MAX_COW_PAGES > 254`.

**Rule:** When adding or changing a hosted-dev capacity constant, verify that any
test which counts up to `N/2 + 1` pages fits within `MAX_MAPPINGS - 1 = 127`.
The formula is:

```
hosted-dev capacity / 2 + 1  ≤  MAX_MAPPINGS - 1
hosted-dev capacity  ≤  2 * (MAX_MAPPINGS - 2)  =  252
```

Keep hosted-dev `MAX_COW_PAGES ≤ 100` (current value) to leave headroom for test
setups that consume a few mapping slots before the exhaustion loop.

---

## Summary Table

| Rule | Key point |
|------|-----------|
| 1. Cspace-at-mint | Mint caps **before** `dispatch_next_task` |
| 2. Idle re-enqueue | After every `dispatch_next_task`, call `idle_re_enqueue_for_test()` |
| 3. HashMap init | Write user memory **before** any syscall reads it; write per-ASID |
| 4. ASID binding | Bind ASID to every task (including TID=0) that touches user memory |
| 5. Running\|Runnable | After wake/exit, assert `Running \| Runnable`, not just `Runnable` |
| 6. mark vs exit | `mark_task_dead` leaves task current; `exit_task` dispatches away |
| 7. Capacity fit | Hosted-dev pool sizes must not exceed `2*(MAX_MAPPINGS-2)` |
| 7. Endpoint caps in correct cnode | Create endpoints while the task that needs send_cap is current; grant recv to waiters |
| 8. supervisor endpoint vs fault handler | `set_supervisor_endpoint` and `set_fault_handler` are distinct; a test that checks fault reports must set `set_fault_handler`, not `set_supervisor_endpoint` |
| 9. send_message_to_endpoint_and_wake | Call directly (it is `pub(crate)`); do not inline the enqueue+wake pattern in tests |
| 10. task_class consistency | After `register_task`, verify both `task_status` (from `with_tcbs`) and `task_class` (from `task_classes`) are Some |

---

## Rule 7 — Endpoint capabilities must be in the current task's cnode

When a test creates an endpoint and then dispatches to another task, the caps are in
the **original current task's** cnode.  If the new current task tries to use the cap,
the capability resolution will fail.

**Pattern:**

```rust
// Create endpoint while task 0 (idle) is current — both caps go into task 0's cnode.
let (_eid, send_cap, recv_cap_t0) = state.create_endpoint(4).expect("ep");

// Grant the cap the receiver needs.
let recv_cap_t1 = state
    .grant_capability_task_to_task(0, recv_cap_t0, 1)
    .expect("grant recv");

// Now dispatch to task 1.
state.enqueue_current_cpu(1).expect("enqueue");
state.dispatch_next_task().expect("dispatch");
// task 1 uses recv_cap_t1; task 0 uses send_cap after task 1 blocks.
```

---

## Rule 8 — supervisor_endpoint vs fault_handler_endpoint

`set_supervisor_endpoint(recv_cap)` sets `FaultSubsystem::supervisor_endpoint`.
`set_fault_handler(recv_cap)` sets `FaultSubsystem::fault_handler_endpoint`.

They are independent.  A test verifying that `emit_fault_report_for_fault` delivers a
message must call `set_fault_handler`, not `set_supervisor_endpoint`.  A test
verifying that `report_task_exit_to_supervisor` delivers must call
`set_supervisor_endpoint`.

---

## Rule 9 — send_message_to_endpoint_and_wake usage

The helper is `pub(crate)` and can be called directly in tests:

```rust
state.send_message_to_endpoint_and_wake(endpoint_idx, msg).expect("send");
```

Do not reproduce the `with_ipc_state_mut` + `wake_waiter_for_endpoint` pattern inline
in new tests — use the helper to keep tests and the documented pattern in sync.

---

## Rule 10 — TCB and task_class consistency after register_task

After `register_task` (or any `register_task_with_class*` variant), both the TCB
slot (visible via `with_tcbs`) and the task class (visible via `task_class()`) must
be `Some`.  If a test depends on `task_class()` returning `None` to check that a
task is not yet registered, it must not call `register_task` beforehand.

```rust
state.register_task(42).expect("register");
assert!(state.task_status(42).is_some(), "tcb exists");
assert_eq!(state.task_class(42), Some(TaskClass::App), "class set");
```


---

## Rule 11 — VM domain: with_user_spaces required for all address-space reads/writes

All production access to `user_spaces` (the `AddressSpaceManager`) must go through
the `with_user_spaces` / `with_user_spaces_mut` wrappers (vm lock, rank 5).  Tests
may access `state.user_spaces` directly (single-threaded, no rank needed), but new
production code must never bypass the wrapper.

```rust
// correct (production):
let mapped = state.with_user_spaces(|spaces| {
    spaces.get(asid).and_then(|aspace| aspace.resolve(virt)).is_some()
});

// wrong (Bug G pattern — avoid in production):
// let aspace = self.user_spaces.get(asid).ok_or(...)?;
```

---

## Rule 12 — Memory domain: with_memory_state_mut required for memory object mutations

All mutations to `memory.memory_objects`, `memory.frame_allocator`, and other
`MemorySubsystem` fields must go through `with_memory_state_mut` (rank 6).
Read-only queries use `with_memory_state`.  Tests that need the raw state may use
the accessor but should verify the result via `with_memory_state` where possible.

```rust
// correct:
state.note_mapping_inserted(phys);  // internally uses with_memory_state_mut
let rc = state.with_memory_state(|memory| {
    memory.memory_objects.iter().flatten()
        .find(|obj| obj.phys == phys)
        .map(|obj| obj.map_refcount)
});

// wrong (Bug E pattern — avoid):
// self.memory.memory_objects[slot].as_mut().map(|obj| obj.map_refcount = ...)
```

---

## Rule 13 — Lock-rank interleaving: vm (5) then memory (6) is the only valid order

Operations that touch both the vm domain and the memory domain must acquire the
locks in rank order: finish all `with_user_spaces_mut` work (acquire rank 5, release
rank 5) before starting `with_memory_state_mut` work (rank 6).  Nesting them is
forbidden.

`map_user_page_in_asid_raw` is the canonical example:
1. `with_user_spaces_mut` — page table mutation (rank 5, released)
2. `note_mapping_inserted` — map_refcount update (rank 6, released)
3. `request_live_asid_shootdown` — IPC notification (rank 3, released)

Tests that verify map→refcount consistency must not hold any lock across the two
operations.


---

## Rule 14 — Task domain: use `with_tcb_mut` for all TCB field mutations in production code

All production mutations of `ThreadControlBlock` fields must go through
`with_tcb_mut(tid, |tcb| { ... })`, which acquires the task lock (rank 2) for
the duration of the closure.

The old `tcb_mut(tid) -> Option<&mut ThreadControlBlock>` method is restricted to
`#[cfg(test)]` after Stage 4T+4.  Do not use it in production paths.

```rust
// correct:
state.with_tcb_mut(tid, |tcb| {
    tcb.fault_policy_override = Some(policy);
});

// wrong (Bug I pattern — avoid):
// let tcb = state.tcb_mut(tid).ok_or(...)?;
// tcb.fault_policy_override = Some(policy);
```

---

## Rule 15 — Memory domain: user_memory hashmap mutations must use `with_memory_state_mut`

In hosted-dev, `write_user_byte` / `read_user_byte` access `memory.user_memory`.
These must go through `with_memory_state_mut` / `with_memory_state` (rank 6).

Direct `self.memory.user_memory` access is forbidden in production code (Bug H pattern).

---

## Rule 16 — Split-read helpers must be verified against global-lock reads

Every new `*_split_read` helper on `SharedKernel` must have a test that:
1. Mutates the relevant state (via `split_mut` or `kernel.with(|state| ...)`).
2. Reads via the split-read helper.
3. Reads via `kernel.with(|state| state.xxx())` (global lock path).
4. Asserts both return the same value.

This ensures the split helper sees the same subsystem lock and accesses the correct
field — no stale reads, no wrong pointer arithmetic.

```rust
// correct pattern:
kernel.record_fault_split_mut(fault);
assert_eq!(
    kernel.last_fault_split_read(),
    kernel.with(|state| state.last_fault())
);

// wrong — only checks one path:
// assert_eq!(kernel.last_fault_split_read(), Some(fault));
```

---

## Rule 17 — Split-read helpers must not hold outer SharedKernel lock

A `*_split_read` helper must acquire ONLY its own subsystem lock — never the outer
`SharedKernel` `SpinLock<KernelState>`. Callers must not hold any lock of rank ≤ the
subsystem rank when invoking the helper (per the lock-rank hierarchy in §2).

Document this constraint in the helper's inline comment, following the pattern in
`runtime.rs::with_fault_split_read` / `with_telemetry_split_read`.
