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

---

## Rule 18 — Arch-boundary split-read TID tests must prove task-switch detection

When a split-read helper is used at an arch/trap boundary to determine whether a
task switch occurred (`entering_tid != exiting_tid`), the test suite must cover
three distinct scheduler states:

1. **Task-switch occurred**: enqueue ≥2 tasks before the first dispatch so that a
   subsequent `yield_current` dispatches a *different* task. Assert
   `exiting_tid ≠ entering_tid` and `task_switched = true`.

2. **No task switch**: dispatch a single task with no competitor in the run queue.
   Assert `exiting_tid == entering_tid` and `task_switched = false`.

3. **Offline CPU**: pass an out-of-range `CpuId` (e.g. `CpuId(255)`) to the helper.
   Assert `None` is returned (same as the conservative `with_cpu` path for an
   offline CPU).

**Dual-enqueue pattern** (required for case 1): enqueue **both** tasks before
calling `dispatch_next_task()` for the first time:

```rust
// CORRECT — task 82 is in the run queue when task 81 yields
state.register_task(81).expect("task81");
state.register_task(82).expect("task82");
state.enqueue_current_cpu(81).expect("enqueue 81");
state.enqueue_current_cpu(82).expect("enqueue 82");
state.dispatch_next_task().expect("dispatch to 81");
// ...
state.yield_current().expect("yield 81");
// exiting_tid == Some(82) ≠ Some(81) == entering_tid  ✓

// WRONG — task 81 re-dispatches because task 82 was never queued before dispatch
state.register_task(81).expect("task81");
state.enqueue_current_cpu(81).expect("enqueue 81");
state.dispatch_next_task().expect("dispatch to 81");
state.register_task(82).expect("task82");
state.enqueue_current_cpu(82).expect("enqueue 82");
state.yield_current().expect("yield 81");
// Both entering_tid and exiting_tid == Some(81) — WRONG assertion would fail
```

The WRONG pattern fails because `dispatch_next_task` removes the idle task (TID 0)
from the membership table; when only task 81 remains, `yield_current` re-dispatches
task 81 and `exiting_tid == entering_tid`.

**Equivalence test** (companion to case 1): every arch-boundary split-read conversion
must include a test that calls both `split_read` and the conservative
`with_cpu → current_tid` path on the same scheduler state and asserts they produce
the same value (Rule 16 pattern extended to arch-boundary callers).

---

## Rule 19 — Fatal-trap snapshot tests must cover TID, ASID, and offline-CPU cases

When a `FatalTrapReadSnapshot` (or equivalent composite split-read snapshot) is
introduced to replace a global-lock acquisition in a fatal error log path, the test
suite must cover three invariants:

1. **TID leg**: `snapshot.current_tid` must equal `current_tid_split_read(cpu).unwrap_or(0)`
   on the same scheduler state after a task has been dispatched.  Assert the concrete
   expected TID value.

2. **ASID leg**: `snapshot.current_asid` must equal both `task_asid_for_tid_split_read(tid)`
   and the global-lock `task_asid(tid).map(|a| a.0 as u64).unwrap_or(0)` for the same task.
   If no ASID is bound, all three must be `0`.

3. **Offline CPU**: `fatal_trap_read_snapshot(CpuId(N))` for an offline or nonexistent CPU
   must return `current_tid = 0` and `current_asid = 0` — the safe zero-fill sentinel used
   by the log function when no task is running.

**Rationale**: The fatal-trap log path is a diagnostic-only, never-returns path.
Incorrect values silently produce a misleading log; only tests can catch this.  The
offline-CPU case exercises the `unwrap_or(0)` sentinel; the ASID case exercises the
rank-2 task-lock path; the TID case exercises the rank-1 scheduler-lock path.

---

## Rule 20 — Arch-boundary trap-path conversions require smoke-level acceptance, not only unit-test value-equivalence

When a `with_cpu` call at an arch/trap boundary is replaced with a split-read helper
(removing a global-lock acquisition), the change is **not complete** until the
x86_64 service-chain smoke test confirms correct behavior end-to-end:

**Required smoke health signals** (x86_64 core smoke):
- `service_entries` ≥ 1 (at least one service endpoint reached)
- `driver/blkcache/virtio READY` all present
- `PM_ELF_ZC_DONE image_id 7/8/9` total 3
- `real_fatal_ish = 0` and `x86_fallback = 0`
- No repeated `SCHED_ENTER_IDLE_HLT` after the first task's initial syscall

**Rationale**: Stage 4T+6 converted x86_64 entering_tid and exiting_tid reads from
`with_cpu(cpu, |k| k.current_tid()).unwrap_or(None)` to `current_tid_split_read(cpu)`.
Unit tests proved the two paths return identical values for all reachable scheduler
states. Yet the x86_64 smoke test showed the service chain stalling after the conversion
(service_entries=0, repeated SCHED_ENTER_IDLE_HLT after TID 2's STARTUP_INSTALL_FINAL
syscall) — a failure invisible to unit tests. The conversion was reverted (Stage 4T+6R).

**Corollary**: Unit tests proving return-value equivalence are necessary but NOT
sufficient for trap-boundary conversions. They do not cover hardware-level lock ordering,
memory visibility guarantees on real CPUs, timing effects, or interactions between the
converted path and the broader service-chain bootstrap sequence.

**For the x86_64 shared trap path specifically**: all `with_cpu` calls at the
entering_tid and exiting_tid sites are classified **Class F** (global lock required).
Do not attempt to convert them without a successful end-to-end smoke run.

---

## Rule N+1 — x86_64 bootstrap timer: do not tick/yield before `BOOTSTRAP_SCHEDULER_READY`

**Invariant**: On x86_64 bare metal, the LAPIC timer IRQ may fire during
`bootstrap_first_user_task` (ELF loading takes >800 ms; timer deadline is 800 ms).
At that point, `bootstrap_first_user_task` holds a raw `&mut KernelState` from
`borrow_kernel_for_boot()`, which bypasses `SpinLock<KernelState>`. If the timer
ISR simultaneously acquires the SpinLock via `shared.with_cpu()`, both hold mutable
aliases to the same memory — undefined behavior. Ticking or yielding in this window
corrupts scheduler state and causes the kernel to idle instead of entering userspace.

**Guard**: `BOOTSTRAP_SCHEDULER_READY: AtomicBool` in
`src/arch/x86_64/descriptor_tables.rs` starts `false`. The timer ISR checks it
immediately after `acknowledge_interrupt`. While `false`, the ISR does EOI + re-arm
only and returns. `signal_bootstrap_scheduler_ready()` sets it to `true` (Release)
after bootstrap and secondary-CPU release complete.

**Rule**: Never call `tick_scheduler_timer()`, `yield_current()`, or any function
that modifies scheduler/task state from the x86_64 timer ISR path until
`bootstrap_scheduler_is_ready()` returns `true`. The EOI-only guard must come first.

**Corollary**: `borrow_kernel_for_boot()` is safe only if no concurrent path acquires
`SharedKernel::with_cpu()` for the same `KernelState`. On x86_64, the bootstrap timer
guard enforces this; do not widen the window between STI and the guard being set.

---

## Rule N+2 — Phase BT2: arm the BSP LAPIC timer ONLY after `signal_bootstrap_scheduler_ready()`

**Root cause (BT2)**: Prior to this fix, the BSP LAPIC timer was armed in two places:
1. `init_lapic_mmio_base()` in `src/arch/x86_64/irq.rs` (during LAPIC SVR configuration)
2. `run_with_prepared_kernel()` in `src/arch/x86_64/boot.rs` (before `run(kernel)`)

Both armings occurred before `bootstrap_first_user_task()` began loading ELF images.
At LAPIC divide-by-16 with a 50,000,000-tick deadline (~800 ms/fire in QEMU), loading
three ELF images takes ~17 s, so the timer ISR fired 21 times during bootstrap. Each
fire entered `yarm_x86_dispatch_trap_from_stub`, which calls `shared.with_cpu()` three
times (entering_tid, dispatch, exiting_tid). Each `with_cpu()` call acquired a second
`&mut KernelState` while `borrow_kernel_for_boot()`'s raw `&mut` alias was still live.
Under Rust's aliasing rules this is undefined behaviour; the optimizer corrupted TCB
tables and page-table entries, causing `bootstrap_first_user_task()` to hang and
`signal_bootstrap_scheduler_ready()` to never be called.

Observed smoke: `X86_BOOTSTRAP_TIMER_IRQ_EOI_ONLY: 21`, `X86_BOOTSTRAP_SCHEDULER_READY: 0`,
`ENTER_USER: 0`, `service_entries: 0`.

**Fix (D)**: Remove both early timer armings. Arm the BSP LAPIC timer exactly once via
`start_bsp_periodic_timer(kernel)`, called in `run_scheduler_loop()` immediately after
`signal_bootstrap_scheduler_ready()` returns. This eliminates the aliasing window
entirely. The BT1 EOI-only guard is preserved as defence-in-depth but fires zero times.

**Expected smoke markers after fix**:
- `X86_BOOTSTRAP_TIMER_IRQ_EOI_ONLY: 0` (was 21 — timer not armed during bootstrap)
- `X86_BOOTSTRAP_SCHEDULER_READY: 1` (was 0 — bootstrap now completes)
- `X86_BOOTSTRAP_TIMER_STARTED: 1` (new marker — timer armed after signal)
- `ENTER_USER: ≥1` (was 0 — userspace tasks now reached)

**Rule**: The BSP LAPIC timer must NOT be armed in `init_lapic_mmio_base()` or anywhere
before `signal_bootstrap_scheduler_ready()` completes. The single authorised arming site
is `start_bsp_periodic_timer(kernel)` in `src/arch/boot_entry.rs`.

**Test**: `init_lapic_does_not_arm_timer_before_signal` in `src/arch/x86_64/irq.rs`
asserts that the LAPIC timer initial-count register remains 0 after `init_lapic_mmio_base()`.
`bootstrap_scheduler_ready_gates_timer_isr_scheduling` in
`src/arch/x86_64/descriptor_tables.rs` asserts idempotent signal semantics.

**Files changed by BT2**:
- `src/arch/x86_64/irq.rs` — removed timer arm from `init_lapic_mmio_base()`; updated test
- `src/arch/x86_64/boot.rs` — removed `program_timer_deadline_current_cpu()` from `run_with_prepared_kernel()`
- `src/arch/boot_entry.rs` — added `start_bsp_periodic_timer(kernel)` function
- `src/bin/kernel_boot.rs` — call `start_bsp_periodic_timer(kernel)` after `signal_bootstrap_scheduler_ready()`
- `src/arch/x86_64/descriptor_tables.rs` — added BT2 comment block and test
- `doc/KERNEL_LOCKING.md` — added Section 15 for Phase BT2

---

## Rule N+3 — Stage 5A: split-read helpers must prove equivalence with a globally-locked test

**Rule**: Every new `SharedKernel::*_split_read` or `*_split_mut` helper added
under a domain sub-lock must be accompanied by an equivalence test that:
1. Reads the same value via `kernel.with(|state| ...)` (globally-locked path).
2. Reads the same value via the new split-read helper.
3. Asserts they are equal.

This pattern was established for fault (Stage 4T+5) and telemetry (Stage 4T+5),
and extended for task class and capability slot capacity in Stage 5A.

**Rationale**: Split-read helpers bypass the global `SpinLock<KernelState>`.
Without an explicit equivalence test, it is easy to accidentally write a helper
that acquires the wrong sub-lock, accesses the wrong field, or has an off-by-one
in array indexing — all of which would return a silently wrong value that no type
check or unit test would catch. The equivalence test is the only proof that the
split-read observes the same shared state as the canonical global-lock path.

**Requirement**: For each new `*_split_read` helper, the equivalence test must:
- Cover the absent/None case (before the target entity exists).
- Cover the present/Some case (after creation or mutation via global lock).
- Use `assert_eq!` with a descriptive message naming the helper.

**Examples** (Stage 5A, `runtime.rs`):
- `task_class_split_read_matches_global`
- `task_exists_split_read_matches_global`
- `cnode_slot_capacity_split_read_matches_global`

---

## Rule N+4 — Stage 5A: do not add split-read helpers for trap-boundary-sensitive reads

**Rule**: The following operations must NEVER be converted to split-read helpers
on `SharedKernel`, even if the underlying data is technically readable under a
domain sub-lock:

1. **x86_64 entering_tid / exiting_tid** in `yarm_x86_dispatch_trap_from_stub` (Class F).
   Stage 4T+6 attempted this and was smoke-broken. The x86_64 trap path's
   `with_cpu()` calls at these sites must remain globally locked.

2. **Any TrapFrame read or write** (Class F). TrapFrame register writeback
   semantics require the global lock's exclusivity guarantee.

3. **Scheduler yield/dispatch operations** (Class I). `yield_current`,
   `dispatch_next_task`, `dispatch_ready_task`, and `enter_dispatched_user_task_if_available`
   modify runqueues atomically with status updates. Split-mut here risks a
   task becoming runnable but not enqueued (or vice versa).

4. **x86_64 / AArch64 boot or timer paths** (Class J). BSP LAPIC timer arming,
   BT1/BT2 guards, and AArch64 boot ERET paths must not be touched.

**Corollary**: A split-read helper is safe if and only if:
- It acquires exactly the domain lock(s) for the data it reads.
- It returns a `Copy` snapshot (no borrowed references out of the lock scope).
- It is not on any path that also writes a TrapFrame, drives a scheduler state
  machine, or touches arch/boot/timer state.
- It has a passing equivalence test (Rule N+3).

---

## Rule N+5 — Stage 5A: lock-domain rank ordering for split-read helpers

**Rule**: When writing a new `*_from_raw` function in `orchestrator_state.rs`
that acquires a domain sub-lock, it must:

1. Acquire domain locks in **strictly increasing rank order** (scheduler=1,
   task=2, ipc=3, capability=4, vm=5, memory=6, driver=7, fault=8, restart=9,
   telemetry=10, boot_config=11).
2. Release each lock before acquiring the next — no simultaneous multi-domain
   lock holding unless a dedicated multi-lock helper exists (e.g.,
   `with_task_then_capability`).
3. Include a `// Lock-order domain: <name> (rank N)` comment before the lock
   acquisition, matching the `debug_lock_order_note` domain string.
4. If the function accesses multiple fields protected by the same lock (e.g.,
   `tcbs` + `task_classes` both under `task_state_lock`), note this explicitly
   in the SAFETY comment: `// task_state_lock protects both tcbs and task_classes`.

**Example** (correct):
```rust
pub(crate) unsafe fn task_class_from_raw(state: *const KernelState, tid: u64) -> Option<TaskClass> {
    // Lock-order domain: task (rank 2)
    // task_state_lock protects both tcbs and task_classes.
    let lock_ref = unsafe { &*core::ptr::addr_of!((*state).task_state_lock) };
    let _guard = lock_ref.lock();
    ...
}
```

**Violation example** (do NOT do):
```rust
// WRONG: acquires task (rank 2) AFTER ipc (rank 3) — rank inversion.
let _ipc = self.ipc_state_lock.lock();
let task_class = self.task_class(tid);  // acquires task_state_lock (rank 2 < 3)
```

---

## Rule N+6 — Stage 5B: plan-first decomposition for multi-domain syscall handlers

**Rule**: Any syscall handler that reads from one lock domain and mutates
another must use a plan-first pattern:

1. **Read phase**: snapshot all task-domain data (rank 2) into a `*Plan` struct
   before any mutation. In the current implementation this happens inside the
   global lock; when the global lock is removed, the read phase moves before
   `with_cpu()` using split-reads on `SharedKernel`.
2. **Mutation phase**: use only the plan snapshot for task-domain data; do not
   re-acquire the task lock inside a capability or memory closure.
3. The `*_planned` variant of the function must accept `&*Plan` and must not
   call any `with_tcbs` / `task_class` / `process_id` accessor internally —
   only capability or memory domain accessors.
4. Add a comment on the plan-build step:
   ```
   // Stage 5B plan-first: snapshot task domain (rank N) before <other> mutation (rank M).
   // When the global lock is removed, this read moves to before the with_cpu() call
   // via split-read on SharedKernel.
   ```
5. `VmAnonMapPlan` and any other plan struct for TLB-touching syscalls are
   **scaffolding only** until x86_64 smoke approval is obtained. Do not
   implement live conversion for TLB-adjacent paths without that approval.

**Corollary for plan structs**:
- Plan structs live in `src/kernel/boot/mod.rs` alongside other plan enums.
- Plan structs derive `Debug, Clone, Copy, PartialEq, Eq`.
- Each plan struct must have a documented lock-domain flow in `KERNEL_LOCKING.md §17`.

---

## Rule N+7 — Stage 5B: equivalence tests for Stage 5B split-read helpers

**Rule**: Every new `*_split_read` helper added in Stage 5B must have a
corresponding equivalence test in `runtime.rs` that:

1. Verifies the split-read result matches the globally-locked accessor for both
   absent and present TIDs/PIDs.
2. Checks at least one non-trivial case (e.g., a group-leader vs. a non-existent
   task, a registered vs. unregistered TID).
3. Is named `<helper_name>_matches_global` by convention.

This is an extension of Rule N+3 to Stage 5B helpers.

---

## Rule N+8 — Stage 5C: VmAnonMap plan/rollback/TLB rules

### N+8.1 — Plan structs for TLB-touching syscalls are scaffolding until smoke approved

`VmAnonMapPlan` and `VmAnonMapValidatedArgs` exist as scaffolding only. Do not
wire them into `handle_vm_anon_map` (the production syscall handler) without:

1. Resolving all three documented blockers (§18.2 of `KERNEL_LOCKING.md`):
   - Rollback TLB busy-wait (ipc rank 3) ordering
   - Per-iteration loop state captured in plan
   - x86_64 smoke approval obtained
2. x86_64 smoke testing of the new code path.

Both structs carry `#[cfg_attr(not(test), allow(dead_code))]` for this reason.
Do not remove that attribute until live conversion is approved.

### N+8.2 — Rollback helpers must be tested without triggering full mid-loop failure

`unmap_user_page_in_asid` is the rollback building block for the planned
`VmAnonMap` path. Because triggering a mid-loop allocation failure in
hosted-dev (which would require filling all 512 `MAX_MEMORY_OBJECTS` slots) is
impractical, test the rollback mechanism directly:

1. Map a page successfully.
2. Call `unmap_user_page_in_asid` explicitly and assert it returns `Some(mapping)`.
3. Confirm both current-ASID and explicit-ASID checks return false after the unmap.

Do **not** attempt to drive `handle_vm_anon_map` into mid-loop failure via
syscall frames — the state setup cost is prohibitive and would couple tests to
an internal allocation limit.

### N+8.3 — Unmap idempotency must be tested explicitly

`unmap_user_page_in_asid` on a never-mapped page must return `Ok(None)` without
error. This invariant must be confirmed by a dedicated test because the rollback
loop in the planned path may call unmap on pages that were never successfully
mapped (if the failure occurs before a map completes).

The idempotency test pattern:
```rust
let result = state.unmap_user_page_in_asid(asid, virt);
assert!(result.is_ok(), "unmap of unmapped page must not return Err");
assert_eq!(result.unwrap(), None, "unmap of unmapped page must return None");
```

### N+8.4 — Stack guard bypass tests must use the syscall frame path

Tests that verify the stack guard condition (`write && !execute`) must drive
`handle_vm_anon_map` via a real syscall frame (using `vm_anon_map_frame` +
`handle_trap(Trap::Syscall, Some(&mut frame))` + `syscall_succeeded`). Do not
call internal guard-check methods directly — the guard is conditioned on the
`PageFlags` computed from the `prot` argument, so driving via the syscall ABI
is the only way to confirm end-to-end behavior.

| `prot` value | `write` | `execute` | guard fires? |
|-------------|---------|-----------|--------------|
| `0x2` (PROT_WRITE) | true | false | **yes** — guard active |
| `0x4` (PROT_EXEC) | false | true | **no** — write=false |
| `0x6` (PROT_WRITE\|PROT_EXEC) | true | true | **no** — execute=true disarms |

### N+8.5 — Explicit-ASID helpers must be equivalence-tested against current-ASID path

`map_user_page_in_asid_with_caps`, `is_user_page_mapped_in_asid`, and
`unmap_user_page_in_asid` are the explicit-ASID building blocks. For each one,
there must be a test that confirms:

- The explicit-ASID helper produces the same result as the current-ASID path
  when both target the same address space.
- An address that was not mapped returns a consistent negative result from both
  the current-ASID and explicit-ASID check.

Use `setup_task0_with_known_asid()` (which returns `(KernelState, Asid)`) rather
than `setup_task0_with_asid()` (which discards the ASID) for tests that need
the ASID value.

### N+8.6 — TLB live-conversion claim prohibited without smoke

No test, doc, or commit message may claim that `handle_vm_anon_map` has been
live-converted to plan-first unless all three blockers from §18.2 of
`KERNEL_LOCKING.md` are resolved **and** x86_64 smoke approval is recorded.
Stage 5C status is: helpers-only, scaffolding only, no live conversion.

---

## Rule N+9 — Stage 5D: TLB shootdown plan, rollback progress, and VmBrk lazy-unmap rules

### N+9.1 — TlbShootdownRequestPlan tests must use the compute helper, not internal fields

Tests that verify TLB shootdown targeting must call
`state.compute_tlb_shootdown_request_plan(asid, virt)` and assert on the returned
`TlbShootdownRequestPlan` struct. Do not read `live_cpu_bitmap_for_asid` directly —
it is private and the plan helper is the intended test surface.

Required assertions:
- `plan.asid` equals the requested ASID.
- `plan.virt` equals the requested virtual address.
- `plan.target_cpu_bitmap == 0` in hosted-dev (single-CPU, no remote targets).

### N+9.2 — Zero-target assertions must be separated by ASID binding state

Two cases produce `target_cpu_bitmap == 0` for different reasons:
1. **Bound ASID, single CPU**: the only CPU running the ASID is the requester.
2. **Unbound ASID**: no CPU is running the ASID at all.

Both must be tested separately because they exercise different branches of
`live_cpu_bitmap_for_asid`.

### N+9.3 — VmPageMapProgress rollback scope must be tested directly

A test for `VmPageMapProgress` must explicitly verify the invariant:
> Rollback covers `[base_addr, mapped_end)` only, not `[base_addr, end_addr)`.

Pattern:
1. Map N pages where N < total range.
2. Simulate rollback of M < N pages by explicitly unmapping `[base_addr, base_addr + M*PAGE_SIZE)`.
3. Assert pages in `[base_addr + M*PAGE_SIZE, base_addr + N*PAGE_SIZE)` are still mapped.
4. Assert pages in `[base_addr + N*PAGE_SIZE, end_addr)` were never mapped and remain absent.

Do NOT call `rollback_anon_map` directly — it is `fn` (private). Test the rollback mechanism via
the explicit-ASID `unmap_user_page_in_asid` helper in a controlled loop.

### N+9.4 — VmPageMapProgress empty-progress must be a unit test

The initial state `VmPageMapProgress { base_addr: X, mapped_end: X, end_addr: Y }` (empty
rollback range) must have a dedicated struct unit test that verifies `mapped_end == base_addr` and
computes `end_addr - base_addr` to confirm the total range is correct. This is a fast, lock-free
test that does not need thread spawn or ASID setup.

### N+9.5 — VmBrk lazy-unmap tests must drive the syscall via TrapFrame

Tests that verify `VmBrk` shrink over lazy (never-faulted) pages must use a
real `TrapFrame` + `handle_trap(Trap::Syscall, ...)` rather than calling
`set_task_brk_bounds` and `unmap_user_page_in_current_asid` separately. The
goal is to verify the whole plan-first path: leader check → brk query → unmap
loop with Ok(None) tolerance → `set_task_brk_bounds`.

Frame layout for VM_BRK:
```rust
TrapFrame::new(
    crate::kernel::syscall::Syscall::VmBrk as usize,
    [requested_end, 0, 0, 0, 0, 0],  // arg0 = SYSCALL_ARG_CAP = requested
)
```

Required setup: task 0 must be a group leader (which Bootstrap::init() guarantees)
and must have brk bounds initialized via `state.set_task_brk_bounds(0, base, end)`.

### N+9.6 — VmBrk shrink with mixed mapped+lazy pages must be tested

A test must map only a subset of pages in the brk range (simulating partial demand paging),
then issue a VmBrk shrink over the full range. The shrink must succeed, and all pages in the
range (mapped and lazy) must be absent after the shrink.

This verifies that the unmap loop correctly handles the `Ok(None)` return from
`unmap_user_page_in_current_asid` for lazy pages without aborting the loop.

### N+9.7 — Do not add TLB live-conversion tests without x86_64 smoke

No test may assert that a TLB shootdown happens at a different lock rank order than
the current implementation (ipc rank 3 acquired after vm rank 5 and memory rank 6)
unless x86_64 SMP smoke has been approved and the rank inversion is resolved.

Tests may assert:
- The target bitmap is 0 (no shootdown needed) — always safe.
- Unmap succeeds (relies on current implementation's correctness) — always safe.
- The plan struct captures the correct fields — always safe.

Tests must NOT assert:
- That ipc(3) is acquired before vm(5) during unmap — this is false in the current implementation.
- That a single shootdown is fired for multiple pages — batch shootdown is not implemented.

---

## Rule N+10 — Two-Phase Unmap / TlbShootdownWaitPlan tests

### N+10.1 — Phase 1 must be tested in isolation

`unmap_page_phase1` is a phase-1-only helper. Tests must call it directly (not
through `handle_vm_anon_map` or `handle_vm_brk`) so that the phase boundary is
visible and verifiable at the test level.

### N+10.2 — Test the absent-page case for phase 1

Every phase-1 helper must have a test verifying it returns `Ok(None)` when the
target page is absent (lazy, never faulted). Rollback and shrink loops iterate
over sparse ranges; absent-page safety must be a tested invariant, not an
assumption.

### N+10.3 — The phys field is the deferred-reclamation frame

Tests that construct or inspect `TlbShootdownWaitPlan` must verify that the
`phys` field equals the physical address of the mapping that was removed.
This is the frame that must not be reclaimed until phase 3; the test is the
specification for that invariant.

### N+10.4 — Bitmap consistency across phase 1 and compute_tlb_shootdown_request_plan

`TlbShootdownWaitPlan.target_cpu_bitmap` is computed by `unmap_page_phase1`
via `compute_tlb_shootdown_request_plan`. Any test exercising the plan struct
must include a cross-check: the bitmap in the plan must equal the bitmap
returned by a standalone `compute_tlb_shootdown_request_plan` call for the
same ASID and virtual address.

### N+10.5 — Phase 1 is destructive at the page-table level

A test must verify that the virtual address is absent from the address space
immediately after `unmap_page_phase1` returns, even though frame reclamation
is deferred. This documents the invariant: phase 1 is not idempotent from the
address-space perspective.

### N+10.6 — Aggregate bitmap tests must use OR-of-per-page bitmaps

Tests for `VmBrkShrinkTlbPlan` and `VmAnonMapRollbackTlbPlan` must build the
`aggregate_target_bitmap` by OR-ing the per-page bitmaps from
`compute_tlb_shootdown_request_plan`, not by hard-coding a constant. This
documents the intended construction algorithm and remains correct if the
single-CPU assumption is ever relaxed.

### N+10.7 — Do not add tests asserting frame is immediately reusable after phase 1

Tests must not assert that `reclaim_memory_object_for_phys` has been called
after `unmap_page_phase1`. Phase 3 (reclamation) is explicitly deferred;
asserting premature reclamation would contradict the two-phase ordering
invariant this design is trying to enforce.

---
