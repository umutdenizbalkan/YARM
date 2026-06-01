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
