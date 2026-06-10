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

## Rule N+11 — VmBrk two-phase shrink tests

### N+11.1 — Test the partial-page preservation invariant

A test must verify that a non-page-aligned `requested_end` leaves the page
containing it mapped. This is the core invariant of `round_up_page(requested)`:
only full pages strictly above `requested_end` are in the unmap range.

### N+11.2 — Test the empty-range case

A test must verify that when `round_up_page(requested) == round_up_page(current_end)`,
no pages are unmapped and the brk bounds are still updated. This guards against
the loop running when `unmap_start >= unmap_end`.

### N+11.3 — Test execute_tlb_shootdown_wait_plan directly

`execute_tlb_shootdown_wait_plan` must have at least one direct test that calls
it with a plan obtained from `unmap_page_phase1`. The test must verify:
- No error is returned.
- The page is absent from the address space after the call
  (phase 1 removed it; phase 2+3 must not re-insert it).
- In single-CPU, `target_cpu_bitmap == 0` so the fast path is taken.

### N+11.4 — Preserve Stage 5D lazy-page regression tests

The Stage 5D tests (`vm_brk_shrink_tolerates_lazy_unmapped_pages` and
`vm_brk_shrink_with_partially_mapped_lazy_region`) must continue to pass with
the Stage 5F implementation. Adding new lazy-page tests for the two-phase path
is permitted but not required if the Stage 5D tests already provide coverage.

### N+11.5 — Tests must not assert shootdown order relative to reclaim

Tests must not assert that `request_live_asid_shootdown` is called BEFORE
`reclaim_memory_object_for_phys` by inspecting internal state — this would be
an implementation detail, not an observable invariant. Instead, verify the
observable consequence: that the page is absent from the address space and that
no error occurred.

### N+11.6 — x86_64 smoke required for SMP tests

No test may assert that the two-phase ordering produces a different result on
SMP hardware without x86_64 smoke approval. Single-CPU (fast-path) tests are
always permitted.

### N+11.7 — VmAnonMap behavior must not change

Every Stage 5F test suite run must include at least one end-to-end VmAnonMap
test (map → access → unmap lifecycle). If VmAnonMap behavior changes unexpectedly,
the test must catch it. The Stage 5C/5D VmAnonMap tests serve this purpose.

---

## Rule N+12 — Stage 6 VmAnonMap live conversion gate conditions

### N+12.1 — Forward map loop must resolve ASID plan-first

Stage 6 tests must include a test verifying that `handle_vm_anon_map` can be
called with the plan-first ASID pattern: a single `task_asid(tid)` call before
the loop, followed by `map_user_page_in_asid_with_caps` per iteration. The
test must confirm that the result is identical to the old current-ASID path.

### N+12.2 — rollback_anon_map must use two-phase per-page unmap

After Stage 6 conversion, `rollback_anon_map` must call `unmap_page_phase1`
followed by `execute_tlb_shootdown_wait_plan` per page, with ASID resolved
once before the loop. A test must verify that a rollback triggered by a
mid-loop alloc failure removes all pages mapped before the failure without
error.

### N+12.3 — Rollback must tolerate absent pages

Tests must verify that `rollback_anon_map` (with two-phase path) returns
without error when called on a range that includes pages never mapped. The
`unmap_page_phase1` `Ok(None)` path must be exercised in at least one
rollback test.

### N+12.4 — Pre-existing capability-not-revoked behavior must be documented, not fixed

Stage 6 tests must NOT assert that capability slots are freed during rollback.
The pre-existing behavior (cap_refcount remains 1 after rollback; frame not
freed until task exit) must be documented in a code comment added during
Stage 6. A test that relies on the cap slot being freed would be asserting
a correctness fix that is out of scope.

### N+12.5 — Stage 5C/5D/5E/5F regression tests must all pass

Every Stage 6 commit must pass the full test suite, which includes all
VmAnonMap scaffold tests from Stage 5C, TLB plan tests from Stage 5D/5E,
and VmBrk two-phase tests from Stage 5F. These serve as the VmAnonMap
behavior-unchanged regression harness.

### N+12.6 — x86_64 -smp 1 smoke required after Stage 6 conversion

A Stage 6 VmAnonMap live conversion is not accepted until a new x86_64 -smp 1
smoke run passes with all acceptance criteria from §22.1. The VmBrk smoke
(Stage 5G) validates the two-phase pattern in isolation; VmAnonMap rollback
must be validated in its own smoke run.

## Rule N+13 — Stage 6 VmAnonMap live conversion tests

### N+13.1 — Plan-first ASID maps all pages correctly

A test must call `VmAnonMap` through `handle_trap` with a multi-page range and
verify all pages are visible via `is_user_page_mapped_in_asid(asid, ...)` after
success. This validates that the Stage 6 explicit-ASID forward map path produces
the same observable result as the old current-ASID path.

### N+13.2 — Explicit-ASID guard check fires correctly

A test must pre-map the guard page (one page below the target address), then
attempt a `PROT_WRITE` (write=true, execute=false) mapping at the target. The
syscall must fail. This validates the inlined explicit-ASID guard check in
`handle_vm_anon_map` (which replaced the `check_stack_guard` call).

### N+13.3 — Two-phase rollback removes mapped pages

A test must directly call `unmap_page_phase1` and `execute_tlb_shootdown_wait_plan`
on pre-mapped pages, simulating the rollback path, and assert all pages are absent
afterwards. This validates the Phase 1 + Phase 2 helpers as used by
`rollback_anon_map`.

### N+13.4 — Rollback tolerates absent pages (`Ok(None)` from phase1)

A test must call `unmap_page_phase1` on a page that was never mapped and assert
the result is `Ok(None)` (not an error, not `Some`). No panic must occur. This
validates the silent-skip branch in `rollback_anon_map`.

### N+13.5 — Execute-only bypass regression must be covered

A test with `PROT_EXEC` (execute=true, write=false) at a guarded address must
succeed. Tests N+13.5 and N+13.6 together confirm the guard condition
`write && !execute` is preserved exactly through the Stage 6 inline conversion.

### N+13.6 — Write+execute bypass regression must be covered

A test with `PROT_WRITE|PROT_EXEC` (both write and execute true) at a guarded
address must succeed (execute=true disarms the guard). This is the symmetric
counterpart to N+13.5.

### N+13.7 — Full suite must pass at 620+ tests

Every Stage 6 commit must pass `cargo test --lib -- --test-threads=1` with at
least 620 tests (614 from Stage 5 + 6 new Stage 6 tests). The Stage 5C/5D/5E/5F
tests serve as the behavior-unchanged regression harness.

### N+13.8 — Capability-not-revoked behavior must NOT be asserted as fixed

Stage 6 tests must not assert that capability slots are freed during rollback.
The `rollback_anon_map` comment documenting cap_refcount=1 behavior is
sufficient. Any test asserting cap-slot reclamation belongs in a later stage.

---

## Rule N+14 — Stage 7 tests: remaining current-ASID syscall.rs domain

Stage 7 converts `handle_transfer_release`, `map_shared_region_into_receiver`
rollback, and the `handle_vm_map` guard to explicit-ASID / two-phase paths,
and deletes `check_stack_guard`. The following rules govern Stage 7 tests.

### N+14.1 — `handle_transfer_release` two-phase tests must cover the success path

A test must map a page into a known ASID, call `TransferRelease` for that page,
and verify the page is no longer mapped in the address space afterward. This
confirms Phase 1 (PTE removal) and Phase 2 (shootdown + reclaim) both execute
on the success path.

### N+14.2 — `handle_transfer_release` must return `InvalidArgs` for absent pages

A test must call `TransferRelease` on a virtual address that was never mapped
and verify the syscall returns `InvalidArgs`. This preserves the
`Ok(None)` → `InvalidArgs` mapping that replaces the old
`unmap_user_page_in_current_asid` → `None` → `InvalidArgs` path.

### N+14.3 — `handle_transfer_release` fast-path bitmap test on single-CPU

A test must verify that `execute_tlb_shootdown_wait_plan` completes without
blocking (fast path) in a single-CPU simulated environment, confirming that the
bitmap fast-path in the shootdown machinery works correctly when all CPUs are
accounted for immediately.

### N+14.4 — `handle_transfer_release` multi-page test must unmap all pages

A test must map N pages into a known ASID, call `TransferRelease` covering all
N pages, and verify that every page is absent after the call. This ensures the
Phase 1/Phase 2 loop iterates correctly across a multi-page region.

### N+14.5 — `handle_vm_map` guard must use capability ASID

A test must create an address space capability, pre-map the guard page in that
capability's ASID, then call `VmMap` with `write && !execute` flags and verify
`InvalidArgs` is returned. The test must use the capability ASID explicitly
(not the current-task ASID) to confirm the ASID-consistency fix. A companion
test must confirm that an un-mapped guard page in the capability ASID does NOT
block the `VmMap` call.

### N+14.6 — `handle_vm_map` execute-only and write+execute bypass regressions

Two regression tests must confirm that the `write && !execute` guard condition
is preserved exactly:
- `execute && !write` maps must NOT trigger the guard (guard bypass test).
- `write && execute` maps must NOT trigger the guard (guard bypass test).

These correspond to N+13.5 and N+13.6 for the Stage 6 `handle_vm_anon_map`
tests and verify that deleting `check_stack_guard` and inlining the condition
did not alter the flag logic.

### N+14.7 — `map_shared_region_into_receiver` rollback two-phase test

A test must induce a partial-failure scenario in `map_shared_region_into_receiver`
(e.g., by exhausting memory after the first page is mapped) and verify that the
pages already mapped are removed by the rollback path using two-phase unmap.
This confirms the rollback loop iterates from `requested_va` up to `va` and
calls `unmap_page_phase1` + `execute_tlb_shootdown_wait_plan` for each page.

### N+14.8 — Full suite must pass at 629+ tests

Every Stage 7 commit must pass `cargo test --lib -- --test-threads=1` with at
least 629 tests (620 from Stage 6 + 9 new Stage 7 tests). All prior
Stage 5C/5D/5E/5F, Stage 6, and Stage 6A tests serve as the
behavior-unchanged regression harness.

### N+14.9 — `check_stack_guard` deletion must not require a test change

`check_stack_guard` was a private helper with no direct test. Its deletion is
covered by the Stage 7 `handle_vm_map` guard tests (N+14.5, N+14.6). No
existing test may be changed to accommodate the deletion — any break would
indicate the inlined condition differs from the deleted helper.

---

## Rule N+15 — Stage 8 tests: demand-paging explicit-ASID conversion

Stage 8 converts `try_handle_demand_page_fault` from `map_user_page_in_current_asid_with_caps`
to `map_user_page_in_asid_with_caps(asid, ...)` using the plan-first ASID
already in scope at line 98 of `fault_state.rs`. The following rules govern
Stage 8 tests.

### N+15.1 — Demand page maps into the faulting task's ASID

A test must set up a task with a known ASID, trigger a page fault in the brk
region, and verify that the mapped page appears in that exact ASID (not in any
other address space). This confirms the plan-first `asid` variable drives the
mapping, not an implicit re-read inside `map_user_page_in_current_asid_with_caps`.

### N+15.2 — Task without ASID falls through to task fault

A test must trigger a page fault for a task with no address space bound
(i.e., `task_asid(tid) = None`) and verify the task ends up `Faulted`. This
confirms the `Ok(false)` early-return path in `try_handle_demand_page_fault`
and the subsequent `fault_current_task` dispatch are both preserved.

### N+15.3 — Execute fault is not demand-mapped

A test must trigger an execute page fault in the brk region and verify that
(a) the page is NOT mapped and (b) the task is `Faulted`. The
`FaultAccess::Execute` early-return at the top of `try_handle_demand_page_fault`
must be preserved through the explicit-ASID conversion.

### N+15.4 — Already-mapped page returns without remap

A test must pre-map a page, then trigger a demand fault for the same address,
and verify that the physical address does not change. This confirms the
`already_mapped` guard (lines 108-116 of `fault_state.rs`) is preserved.

### N+15.5 — Demand-mapped page carries USER_RW flags

A test must verify that the page allocated by demand paging has exactly
`PageFlags::USER_RW` (read=true, write=true, execute=false, user=true).
Demand paging always allocates with `USER_RW`; this invariant must not change.

### N+15.6 — Stack growth window demand-maps using plan-first ASID

A test must set `tcb.user_stack_top` for a task, trigger a fault in the 8 MiB
stack growth window below `stack_top`, and verify the page is demand-mapped into
the task's ASID. This confirms `fault_addr_in_demand_backed_region` and the
plan-first ASID path cooperate correctly for the stack case.

### N+15.7 — Full suite must pass at 635+ tests

Every Stage 8 commit must pass `cargo test --lib -- --test-threads=1` with at
least 635 tests (629 from Stage 7 + 6 new Stage 8 tests). All prior tests serve
as the behavior-unchanged regression harness.

### N+15.8 — No demand-paging observable behavior change

Stage 8 tests must NOT assert any behavior that differs from the old
`map_user_page_in_current_asid_with_caps` path. The conversion is
semantics-preserving under the global lock; tests validate equivalence, not a
new behavior.

### N+15.9 — `map_user_page_in_current_asid_with_caps` test-only status

After Stage 8 the `map_user_page_in_current_asid_with_caps` helper must have
zero production callers. Its continued presence is justified only by test-module
use in `syscall.rs` and `boot/tests.rs` (equivalence comparisons). A future
stage may delete it once those tests are updated.

---

## Rule N+16 — Stage 9 test rules: current-ASID helper deletion, VmAnonMapProgressPlan wiring, rollback cap cleanup

### N+16.1 — No direct calls to deleted current-ASID helpers

`map_user_page_in_current_asid_with_caps`, `unmap_user_page_in_current_asid`,
and `is_user_page_mapped_in_current_asid` are deleted in Stage 9. All tests
must use the explicit-ASID equivalents:
- `map_user_page_in_asid_with_caps(asid, cap, virt, flags)`
- `unmap_user_page_in_asid(asid, virt)`
- `is_user_page_mapped_in_asid(asid, virt)`

When a test maps pages on behalf of a non-current task (e.g., task1), the test
must retrieve `asid` from `state.task_asid(tid)` and pass it explicitly.

### N+16.2 — VmAnonMapProgressPlan field coverage

Tests for `handle_vm_anon_map` must indirectly verify that `VmAnonMapProgressPlan`
is correctly populated:
- `base_addr` == syscall `addr` arg (no offset).
- After success, all pages in `[base_addr, end_addr)` are mapped.
- After failure, no pages from the failed syscall remain mapped.

Testing the struct fields directly is not required; observable address-space
state is sufficient.

### N+16.3 — Rollback cap cleanup correctness

After a successful VmAnonMap + simulated rollback (unmap_page_phase1 +
revoke_capability_in_cnode + execute_tlb_shootdown_wait_plan), a test must
verify that the MemoryObject count drops by exactly the number of pages rolled
back. This confirms `reclaim_memory_object_if_unreferenced` freed the frame
(both refcounts reached zero).

### N+16.4 — `find_current_task_cap_for_memory_object_phys` contract

A test must verify:
- Returns `None` for a physical address not backed by any MemoryObject in the
  current task's cnode.
- Returns `Some((cnode, cap_id))` for a physical address of a freshly mapped
  anonymous page, and that `revoke_capability_in_cnode(cnode, cap_id)` succeeds.

### N+16.5 — Full suite must pass at 640+ tests

Every Stage 9 commit must pass `cargo test --lib -- --test-threads=1` with at
least 640 tests (635 from Stage 8 + 5 new Stage 9 tests). All prior tests serve
as the behavior-unchanged regression harness.

### N+16.6 — No observable behavior change in VmAnonMap success path

Stage 9 wires `VmAnonMapProgressPlan` into the live path. Tests must confirm
that a multi-page `VmAnonMap` still maps every page in the requested range and
returns the correct address and length — identical to Stage 6/7/8 behavior.

### N+16.7 — No observable behavior change in demand paging

Stage 9 must not alter the behavior of `try_handle_demand_page_fault`. All
Stage 8 demand-page tests must continue to pass unchanged.

---

## Rule N+17 — Stage 10 test rules: MemoryObject/cap lifetime invariants + VmMap rollback

### N+17.1 — MemoryObject refcount symmetry

For every alloc/map/unmap/revoke operation, a test must verify that the
corresponding refcount changes symmetrically:

- `alloc_anonymous_memory_object` → `cap_refcount` increments from 0 to 1.
- `map_user_page_in_asid_with_caps` → `map_refcount` increments by 1 per page.
- `unmap_page_phase1` → `map_refcount` decrements by 1 per page.
- `revoke_capability_in_cnode` → `cap_refcount` decrements by 1.

### N+17.2 — Frame reclaim requires both refcounts zero

A test must verify:
- After unmap (map_refcount=0) but before cap revoke (cap_refcount=1): frame NOT freed.
- After cap revoke (cap_refcount=0) but before unmap (map_refcount=1): frame NOT freed.
- After both reach zero: frame IS freed (MemoryObject slot cleared).

### N+17.3 — VmMap plan-first ASID coverage

A test must verify that `handle_vm_map` maps into the address space identified by
`aspace_map_cap`, not the current task's address space. Specifically:
- Map into a non-current ASID via `aspace_map_cap`.
- Confirm the page appears in the target ASID.
- Confirm the page does NOT appear in the current task's ASID.

### N+17.4 — VmMap rollback cleans address space and frees frames

After a simulated partial VmMap failure (map pages, then rollback via phase-1 + cap
revoke + phase-2), a test must verify:
- All pages in the rolled-back range are unmapped.
- All MemoryObjects created for those pages are freed (slot count decrements).

### N+17.5 — Full suite must pass at 647+ tests

Every Stage 10 commit must pass `cargo test --lib -- --test-threads=1` with at least
647 tests (640 from Stage 9 + 7 new Stage 10 tests). All prior tests serve as the
behavior-unchanged regression harness.

### N+17.6 — No observable behavior change in success paths

Stage 10 wires explicit-ASID into `handle_vm_map`. Tests must confirm that a
successful VmMap still maps every page in the range and returns the correct address
and length — identical to pre-Stage-10 behavior.

### N+17.7 — Shared-region and transfer-release tests unchanged

All Stage 7 shared-region, transfer-release, and IPC recv-v2 tests must continue to
pass. Stage 10 does not modify the shared-region or IPC paths.

---

## N+18 — Stage 11 test rules: Two-phase active transfer cleanup

### N+18.1 — Two-phase unmap must be exercised by active transfer tests

Every new active-transfer-cleanup test must verify both phases of the two-phase unmap:
- Phase 1: the mapped page is gone (PTE cleared, `is_user_page_mapped_in_asid` returns false).
- Phase 2 (via reclaim): the MemoryObject slot is freed when cap_refcount, map_refcount,
  and pin_refcount all reach zero.

### N+18.2 — cap_refcount setup must match assertion

A test that asserts `memory_object_slot_by_id(mo_id).is_none()` must ensure the
MemoryObject's `cap_refcount` reaches zero after the operation under test.
If the cap was granted to a second task before the test, `cap_refcount=2` and the
slot will NOT be freed after revoking only one task's cap. Allocate while the
task-under-test is current so the cap lives only in its cspace (`cap_refcount=1`).

### N+18.3 — Absent-page tolerance

At least one test must register an active mapping for N pages but physically map
fewer than N. `purge_active_transfer_mappings_for_pid` must not panic on the unmapped
pages (silently skipped by `unmap_range_two_phase`).

### N+18.4 — Cross-pid isolation

At least one test must verify that purging PID A does not unmap pages owned by PID B.

### N+18.5 — Full suite at 652+ tests (single-threaded)

Every Stage 11 commit must pass `cargo test --lib -- --test-threads=1` with at least
652 tests (647 from Stage 10 + 5 new Stage 11 tests).

### N+18.6 — Revoke-cap path tested independently

`revoke_capability_in_cnode` → `revoke_active_transfer_mappings_for_cap` must have
its own test, independent from the purge path, confirming the same unmap-and-reclaim
behavior.

---

## Rule N+19 — Stage 12: COW/fork MemoryObject lifetime tests

### N+19.1 — Current task must own the cap before fork

Any test that asserts `cap_refcount == 2` after `fork_user_process_cow` (one for
parent, one for inherited child cap) must ensure the MemoryObject cap is minted in
the parent task's cnode. Call `yield_current_to(ThreadId(parent_tid))` after
`spawn_user_task_from_image` and before `alloc_anonymous_memory_object`. Without
this switch, the cap lands in TID 0's cnode (Bootstrap default current task);
`inherit_parent_capabilities_for_fork` finds nothing in the parent's cnode and
cap_refcount stays at 1.

### N+19.2 — Fork lifetime matrix: all four reclaim paths tested

The test suite must cover all four scenarios:
1. Both tasks alive → frame survives (cap_refcount=2, map_refcount=2).
2. Child exits first → frame survives (cap_refcount=1, map_refcount=1).
3. Parent exits first → frame survives (cap_refcount=1, map_refcount=1).
4. Both exit → frame reclaimed (cap_refcount=0, map_refcount=0).

### N+19.3 — COW split frame retention

After a COW write fault (child gets private frame), the original shared frame must
not be reclaimed while the parent still maps it. A test must confirm the shared
MemoryObject still exists after the child's COW fault, and is only reclaimed after
both tasks release all caps and mappings.

### N+19.4 — Failed-clone rollback: parent write permissions restored

A test must force `clone_user_address_space_cow` to fail (fill COW capacity) and
then verify that every parent page that was write-protected during the partial clone
has its write permission restored. Without rollback, a write to those pages causes
an unhandled fault (no COW record, not demand-paged).

### N+19.5 — Failed-clone rollback: no stale COW records

After a failed clone, the parent must have zero COW records for the pages that were
partially processed. Test by counting `cow_pages` entries for the parent asid after
the failed fork.

### N+19.6 — Read-only pages shared without COW marking

A page that is already read-only before fork must be shared in the child without
being marked as a COW page in either asid. The child gets the same physical frame
(map_refcount increments) but neither `is_cow_page(parent, virt)` nor
`is_cow_page(child, virt)` must return true.

### N+19.7 — Full suite at 663+ tests (single-threaded)

Every Stage 12 commit must pass `cargo test --lib -- --test-threads=1` with at
least 663 tests (652 from Stage 10+11 + 11 new Stage 12 tests).

## Rule N+20 — Stage 13: COW content correctness + Vec scalability tests

### N+20.1 — Clone content copy must use physical-frame keys

In hosted-dev the `UserMemoryStore` is keyed by `(asid, phys_addr)`, not
`(asid, virt_addr)`. Any test that writes to a parent page before cloning and
then reads from the child must succeed: the clone copy in
`clone_user_address_space_cow` must use `mapping.phys` as the key, not `virt`.

A test verifying this: write known bytes to `parent_asid` at a virtual address,
clone, read from `child_asid` at the same virtual address, assert the bytes match.

### N+20.2 — COW fault content copy must produce readable child data

After `try_handle_cow_fault(child_asid, virt)`, the child's new private frame must
contain a copy of the original shared frame's content. Test by:
1. Writing bytes to parent before clone.
2. Cloning → child shares the same physical frame (read-only COW).
3. Triggering `try_handle_cow_fault` on the child.
4. Reading from child at the same virtual address — must see the original bytes.
5. Reading from parent at the same virtual address — must also still see them.

### N+20.3 — Exhaustion tests use `cow_page_capacity_limit`, not array size

The two rollback-under-exhaustion tests (`clone_user_address_space_cow_cleans_child_state_on_cow_capacity_exhaustion`
and `fork_failed_clone_leaves_no_parent_cow_records`) must set
`state.with_memory_state_mut(|m| m.cow_page_capacity_limit = Some(N))` to simulate
a cap rather than relying on `super::MAX_COW_PAGES` (which no longer exists).

The limit and page count must be chosen so that failure occurs after at least one
parent+child COW pair succeeds: e.g. 3 writable pages with limit 5 causes failure at
the 6th push (child side of page 2).

### N+20.4 — Vec grows beyond old fixed-cap limit

A test must clone an address space with more than 100 writable pages (the old
`MAX_COW_PAGES` hosted-dev limit) and assert the clone succeeds without error.
Verify `cow_pages.len() == pages * 2` after the clone.

### N+20.5 — COW Vec entries cleared on ASID destroy

Destroying an ASID (via `destroy_user_address_space_by_asid`) must remove all
`cow_pages` Vec entries for that ASID. Test by:
1. Cloning → child gets COW records.
2. Destroying child ASID → child entries gone (filter by asid, count == 0).
3. Destroying parent ASID → Vec empty (`len() == 0`).

### N+20.6 — Exhaustion-test `.iter().flatten()` must become `.iter()`

With `Vec<CowPageRecord>` (not `Vec<Option<CowPageRecord>>`), any test that used
`.iter().flatten()` to scan `cow_pages` is a compile error or logic bug. All scans
must use `.iter()` directly and match on `CowPageRecord` fields, not `Option<CowPageRecord>`.

### N+20.7 — Full suite at 667+ tests (single-threaded)

Every Stage 13 commit must pass `cargo test --lib -- --test-threads=1` with at
least 667 tests (663 from Stage 12 + 4 new Stage 13 tests).
`cargo check --no-default-features` must also be clean.

## Rule N+21 — Stage 14: COW BTreeMap scalability + lifecycle stress tests

### N+21.1 — Use stable helper API, not internal field access

Tests must not access `memory.cow_pages` directly. Use the three `#[cfg(test)]`
helpers on `KernelState`:

- `state.cow_page_count()` — total records across all ASIDs
- `state.cow_page_count_for_asid(asid)` — records for one ASID
- `state.cow_asid_bucket_count()` — number of ASID keys in the BTreeMap

This keeps tests stable against future storage changes.

### N+21.2 — Empty bucket collapse is mandatory

When the last virtual-address entry is cleared for an ASID (via
`clear_cow_page` or `clear_cow_pages_for_asid`), the ASID's key must be
removed from the BTreeMap immediately. Tests must assert
`cow_asid_bucket_count()` decrements when a bucket empties.

Ghost buckets (ASID key present but `BTreeSet` empty) are a violation of
this invariant.

### N+21.3 — Lifecycle stress: fork + exit cycles leave zero records

Tests that fork and then exit (child-first or parent-first) must assert
that `cow_page_count() == 0` and `cow_asid_bucket_count() == 0` after all
tasks in the fork tree have been destroyed.

### N+21.4 — ASID isolation: same virt addr in two ASIDs must not alias

A test with two ASIDs both mapping `VirtAddr(X)` as COW must verify that
`is_cow_page(asid_a, VirtAddr(X))` and `is_cow_page(asid_b, VirtAddr(X))` are
independent — destroying or splitting one ASID's record must not affect the other.

### N+21.5 — Large-ASID cleanup is O(log A), not O(N)

A test must mark ≥ 50 virtual pages COW in one ASID and then call
`clear_cow_pages_for_asid`. Assert that after the call:

- `cow_page_count_for_asid(asid) == 0`
- `cow_asid_bucket_count()` decrements by 1 (the ASID's bucket is removed)

### N+21.6 — Duplicate insertion is idempotent (BTreeSet semantics)

Marking the same `(asid, virt)` pair COW twice must not create duplicate
records. Assert `cow_page_count_for_asid(asid) == 1` after two `mark_cow_page`
calls on the same address.

### N+21.7 — Helper tasks for cnode must use a separate ASID

When a test spawns a helper task (e.g. for cnode allocation or IPC), it must
pass `asid: Some(helper_asid)` where `helper_asid` is a dedicated address
space created for that helper. Passing `asid: None` causes
`spawn_user_task_from_image` to return `Err(UserMemoryFault)`.

If the helper task's address space would be included in a COW clone, use a
completely separate ASID so helper stack pages do not inflate the parent's
COW record count.

### N+21.8 — Full suite at 676+ tests (single-threaded)

Every Stage 14 commit must pass `cargo test --lib -- --test-threads=1` with at
least 676 tests (667 from Stage 13 + 9 new Stage 14 tests).
Both `cargo check --no-default-features` and `cargo check --features hosted-dev`
must be clean.

## N+22 — Stage 15 lifecycle test rules

### N+22.1 — IPC waiter cleanup must be tested at exit and dead

Every path that transitions a task to `Exited` or `Dead` must have at least one
test verifying that `endpoint_waiter_count`, `sender_waiter_count`, and
`notification_waiter_count` return zero for that task after the transition.

### N+22.2 — join_thread must run full mark_task_dead cleanup

A test must verify that calling `join_thread` on an already-`Exited` task
results in `task_is_dead` returning true AND that process cnode cleanup
(`YARM_PROC_CNODE_CLEANUP`) ran (i.e., `maybe_cleanup_process_cnode_for_pid`
was called).  Inline `tcb.status = Dead` without calling `mark_task_dead` is
forbidden.

### N+22.3 — Robust futex wake must not depend on current_tid ASID

Tests that call `exit_task` on a non-current task (supervisor-driven exit)
must not fail due to ASID mismatch in the futex wake path.  Use
`futex_wake_on_exit` (no ASID validation) for the robust list wake loop.

### N+22.4 — Repeated lifecycle stress tests must not exhaust the frame pool

Lifecycle stress tests that iterate more than once (fork/exit loops, futex
wait/exit cycles) must not use `spawn_user_task_from_image` unless they also
call `destroy_user_address_space_by_asid` to free user stack frames between
iterations.  Prefer `register_task` (no user stack) when the test does not
need a user address space.

### N+22.5 — futex_waiter_count relies on TCB status, not a separate list

`futex_waiter_count` counts tasks in `Blocked(WaitReason::Futex(addr))` status.
Tests must not assume a separate futex waiter list is maintained.  A task that
transitions from `Blocked(Futex)` to any other status is no longer counted.

### N+22.6 — MemoryObject refcounts must drop to zero for reclaim

A test that verifies MemoryObject reclamation must confirm both
`cap_refcount == 0` (all capability handles revoked) and `map_refcount == 0`
(all page-table entries unmapped) before checking
`memory_object_exists_for_phys` returns false.

### N+22.7 — Supervisor endpoint must receive task exit event

When a supervisor endpoint is registered, `exit_task` must enqueue a
`SUPERVISOR_OP_TASK_EXITED` message.  A test must verify the message is
retrievable from the endpoint after `exit_task` completes.

### N+22.8 — Full suite at 690+ tests (single-threaded)

Every Stage 15 commit must pass `cargo test --lib -- --test-threads=1` with at
least 690 tests (676 from Stage 14 + 14 new Stage 15 tests).
Both `cargo check --no-default-features` and `cargo check --features hosted-dev`
must be clean.

## N+23 — Stage 16 timeout/deadline/block-state test rules

### N+23.1 — IPC timeout must clear both waiter slot and TCB deadline

Every test that exercises `process_ipc_timeout_deadlines` must assert both:
- `endpoint_waiter_count` (or `sender_waiter_count`) == 0 after the timeout fires
- `ipc_deadline_count_for_tid` == 0 for the expired task

Checking only the TCB status or only the waiter slot is insufficient.

### N+23.2 — Exit before timeout must leave Exited status immune to timeout processing

When `exit_task` is called before the deadline fires, `process_ipc_timeout_deadlines`
must not set the task back to Runnable.  A test must verify that calling
`process_ipc_timeout_deadlines(deadline)` on an Exited task returns 0 expired.

### N+23.3 — Delivery before timeout must clear deadline and prevent later firing

When a receiver waiter is woken by message delivery (not timeout), the IPC deadline
in the TCB must be cleared (by `clear_ipc_timeout_for_tid` in `wake_tid_to_runnable`).
A test must verify that `process_ipc_timeout_deadlines(deadline)` is a no-op after
delivery, and `ipc_timeout_fired` must be false.

### N+23.4 — WakeTask cross-CPU must not resurrect Dead or Exited tasks

Tests for `WorkItem::WakeTask` must verify that Dead and Exited tasks are unaffected
by the cross-CPU wake item.  The fix (Stage 16) checks `Blocked(_)` before transitioning
to Runnable.  Tests must assert `task_is_dead` or `task_is_exited` remains true.

### N+23.5 — clear_ipc_waiters_for_tid must be idempotent

Calling `clear_ipc_waiters_for_tid(tid)` twice must produce the same result as
calling it once.  A test must inject waiters in all three arrays and verify that
double-clearing does not panic or corrupt state.

### N+23.6 — IPC timeout must skip non-IPC-blocked tasks

A task with `ipc_timeout_deadline` set but `Blocked(Futex(_))` or `Blocked(Join(_))`
status must not be expired by `process_ipc_timeout_deadlines`.  This guards the
`blocked_ipc` filter in the timeout path.

### N+23.7 — Test helpers must reflect live kernel state

`notification_waiter_count`, `ipc_deadline_count_for_tid`, `task_is_runnable`,
`task_is_blocked`, `task_blocked_reason` must be used instead of direct field access
in new tests.  These helpers are stable across refactors; direct field access may
silently pass after a storage layout change.

### N+23.8 — Full suite at 710+ tests (single-threaded)

Every Stage 16 commit must pass `cargo test --lib -- --test-threads=1` with at
least 710 tests (690 from Stage 15 + 20 new Stage 16 tests).
Both `cargo check --no-default-features` and `cargo check --features hosted-dev`
must be clean.

## N+24 — Stage 17 cross-CPU work queue test rules

### N+24.1 — apply_cross_cpu_wake_task must return a typed result for every status

Tests for `apply_cross_cpu_wake_task` must assert the exact `CrossCpuWakeApplyResult`
variant, not just that the call succeeds.  Each status variant (Missing, Dead, Exited,
Runnable, Running, Faulted, Blocked→Applied) must have at least one test that checks
the returned value and verifies post-call TCB state is unchanged (for Skipped variants)
or correctly transitioned (for Applied).

### N+24.2 — Missing TID must not propagate as an error

`process_cross_cpu_work_for_cpu` must return `Ok` when the work queue contains a
`WakeTask` item for an unregistered or previously-freed TID.  A test must submit such
an item and assert the call returns `Ok` and the processed count includes that item.

### N+24.3 — cross_cpu_work_count_for_cpu must track queue depth

Tests that submit items to the work queue must verify queue depth before and after
`process_cross_cpu_work_for_cpu` using `cross_cpu_work_count_for_cpu`.  Do not infer
queue state only from side-effects on TCB status.

### N+24.4 — Stale WakeTask items must not change TCB status

When a WakeTask item targets a Dead, Exited, Runnable, Running, or Faulted task, the
task's status must be identical before and after `process_cross_cpu_work_for_cpu`.
Tests must assert the exact final status, not just that the task "still exists".

### N+24.5 — Duplicate WakeTask items for the same TID are harmless

Submitting two WakeTask items for the same TID must not cause a double-enqueue,
panic, or status corruption.  The second item must be a silent no-op
(`SkippedAlreadyRunnable`).  A test must assert processed count == 2 and final
status == Runnable.

### N+24.6 — Work queue must drain to zero count after process

Every test that exercises `process_cross_cpu_work_for_cpu` must verify that
`cross_cpu_work_count_for_cpu` returns 0 after the call.  Partial drain is not
acceptable; the function must consume all pending items in one call.

### N+24.7 — Wrap-around must not corrupt queue ordering

A test that fills the queue to `MAX_CROSS_CPU_WORK`, drains it fully, and refills
it again must verify that all items are processed in FIFO order and the count returns
to 0 after each drain.  This guards against pointer-arithmetic bugs in the ring buffer.

### N+24.8 — Full suite at 728+ tests (single-threaded)

Every Stage 17 commit must pass `cargo test --lib -- --test-threads=1` with at
least 728 tests (710 from Stage 16 + 18 new Stage 17 tests).
Both `cargo check --no-default-features` and `cargo check --features hosted-dev`
must be clean.

## N+25 — Stage 18 TLB shootdown + cross-CPU VM cleanup test rules

### N+25.1 — ASID destroy must submit TlbShootdown work items before reclaiming frames

`destroy_user_address_space_by_asid` must queue `WorkItem::TlbShootdown` for every
CPU in the pending bitmap BEFORE calling `note_mapping_removed` /
`reclaim_memory_object_for_phys` for any mapping.  A test must verify this
observable ordering invariant:

1. Map N pages into an ASID.
2. Call `destroy_user_address_space_by_asid`.
3. Assert the ASID transitions to the retired state (not live).
4. Assert that no MemoryObject slots for those frames retain a non-zero
   `map_refcount` after the call (the reclaim path ran).

The ordering itself is verified by the absence of frame-in-use bugs; the
test-observable invariant is that frames are freed after, not during, the work
submission.

### N+25.2 — Retired ASID must not reappear as live after destroy

After `destroy_user_address_space_by_asid(asid)`:
- `asid_is_live_for_test(asid)` must return `false`.
- `asid_is_retired_for_test(asid)` must return `true` until all CPUs ACK the
  shootdown (or in hosted-dev, `acknowledge_shootdown` is called explicitly).

A test must assert both conditions immediately after the destroy call.

### N+25.3 — Stale TlbShootdown work items are safe to process

When a CPU processes a `WorkItem::TlbShootdown` for an ASID that has already
been fully cleaned up (retired and subsequently reused or simply absent), the
work processing must not panic or return an error.  A test must:

1. Submit a `TlbShootdown` work item for a non-existent or already-retired ASID.
2. Call `process_cross_cpu_work_for_cpu`.
3. Assert `Ok` is returned and queue depth returns to 0.

### N+25.4 — Stale WakeTask and TlbShootdown items for the same CPU are drained together

When a work queue contains a mix of `WakeTask` and `TlbShootdown` items (some
stale, some live), `process_cross_cpu_work_for_cpu` must process all of them in
a single call.  A test must submit both item types, call the processor, and
verify the final queue depth is 0 and `cross_cpu_work_count_for_cpu` returns 0.

### N+25.5 — Two-phase unmap maintains refcount symmetry after ASID destroy

For each page that was mapped before `destroy_user_address_space_by_asid`:
- `map_refcount` must be decremented by the destroy call (via `note_mapping_removed`).
- After all cap references are revoked, `cap_refcount` must reach 0 and the
  MemoryObject slot must be freed.

A test must verify that after calling `destroy_user_address_space_by_asid` and
revoking any surviving capability, `memory_object_exists_for_phys` returns `false`
for all frames that were exclusively owned by the destroyed ASID.

### N+25.6 — COW metadata cleared on ASID destroy

`destroy_user_address_space_by_asid` must call `clear_cow_pages_for_asid` (or
equivalent) so that no COW records for the destroyed ASID remain in the BTreeMap.
A test must:

1. Clone an address space to create COW records for both parent and child.
2. Destroy the child ASID.
3. Assert `cow_page_count_for_asid(child_asid) == 0`.
4. Assert `cow_asid_bucket_count()` decremented by 1.

### N+25.7 — Active-transfer cleanup does not leave stale mappings

After `purge_active_transfer_mappings_for_pid(pid)`:
- `active_transfer_count_for_pid(pid)` must return 0.
- All pages registered in active transfer mappings owned by `pid` must be absent
  from the address space (verified with `is_user_page_mapped_in_asid`).

A test must set up an active mapping, call the purge function, and assert both
conditions.

### N+25.8 — ASID destroy with zero mappings must not panic

Calling `destroy_user_address_space_by_asid` on an ASID that was created but
never had any pages mapped must succeed without error, mark the ASID retired,
and not attempt any reclaim (no MemoryObject slots involved).  This exercises
the empty-drain path.

### N+25.9 — tick_retired_shootdowns returns 0 and does not escalate

`tick_retired_shootdowns()` must return 0 in all hosted-dev scenarios, including
when retired ASID slots have non-zero pending CPU bitmaps.  The function does not
implement timeout escalation (it is a no-op placeholder).  Tests must assert the
return value is 0 and must not assert any escalation side-effects.

### N+25.10 — mapped_page_count_for_asid tracks live mappings

`mapped_page_count_for_asid(asid)` must return the number of pages currently
mapped in the given ASID's page table.  Tests must:
- Assert the count equals N immediately after mapping N pages.
- Assert the count equals 0 after `destroy_user_address_space_by_asid` completes
  (all mappings drained).

### N+25.11 — Full suite at 748+ tests (single-threaded)

Every Stage 18 commit must pass `cargo test --lib -- --test-threads=1` with at
least 748 tests (728 from Stage 17 + 20 new Stage 18 tests).
Both `cargo check --no-default-features` and `cargo check --features hosted-dev`
must be clean.

x86_64 `-smp 1` smoke is required after Stage 18 because
`destroy_user_address_space_by_asid` changed the ordering of TlbShootdown
submission relative to frame reclaim — a live ASID destroy behavior change.

---

## Rule N+26 — Stage 19: capability/cnode domain test rules

### N+26.1 — cap_refcount symmetry: mint increments, revoke decrements

Every call to `mint_capability_in_cnode` for a `CapObject::MemoryObject` or
`CapObject::DmaRegion` must be paired with exactly one call that decrements
`cap_refcount` by −1 (via `revoke_capability_in_cnode`,
`revoke_capability_direct_in_process_cnode`, or the rollback path).

A test must:
1. Create a MemoryObject and verify `cap_refcount == 1` after the initial mint.
2. Revoke the cap and verify `cap_refcount == 0` and the MemoryObject slot is
   reclaimed.

### N+26.2 — Reply caps carry no MemoryObject refcount

`CapObject::Reply` is not a `MemoryObject`.  `adjust_memory_object_cap_refcount`
must be a no-op for all cap objects that are not `MemoryObject` or `DmaRegion`.

A test must mint a Reply cap and verify that no MemoryObject's `cap_refcount`
changes.

### N+26.3 — `revoke_reply_caps_for_caller` is idempotent

Calling `revoke_reply_caps_for_caller(tid)` twice must not panic or double-decrement
any counter.  The second call must return 0.

### N+26.4 — `exit_task` clears ReplyCapRecords; `mark_task_dead` clears cnode

`exit_task` calls `revoke_reply_caps_for_caller` synchronously before returning.
A test must create a Reply cap record, call `exit_task`, and verify the record is
gone.

`mark_task_dead` additionally calls `maybe_cleanup_process_cnode_for_pid` which
revokes all caps and reclaims MemoryObjects.  Tests that verify cnode teardown
must use `mark_task_dead`, not `exit_task` (which sets `Exited`, not `Dead`).

### N+26.5 — cnode teardown cascades through delegated descendants

`revoke_capability_in_cnode` revokes the source cap and all delegation descendants
via `collect_delegated_descendants`.  Each revoke decrements `cap_refcount` by 1.
When all holders' cnodes are torn down, `cap_refcount` must reach 0 and the
MemoryObject must be reclaimed.

A test must:
1. Grant a cap from task A to task B (cap_refcount = 2).
2. Mark task A dead (triggers cnode teardown + cascade to task B's cap).
3. Verify the MemoryObject slot is reclaimed (cap_refcount = 0).

### N+26.6 — `grant_capability_task_to_task_with_rights` rollback must decrement cap_refcount

When `record_delegated_capability_link` fails (delegation link table full), the
rollback path in `grant_capability_task_to_task_with_rights` must:
1. Call `fast_revoke_reply_cap_in_cnode` to clear the cnode slot.
2. If the revoke succeeded, call `adjust_memory_object_cap_refcount(attenuated.object, -1)`
   and `reclaim_memory_object_if_unreferenced(attenuated.object)`.

Without step 2, `cap_refcount` is permanently inflated after each rollback.

A test must fill the delegation link table, call `grant_capability_task_to_task_with_rights`,
verify it returns `Err(CapabilityFull)`, and verify `cap_refcount` is unchanged from
the pre-grant value.

### N+26.7 — double revoke returns Err, not panic

Revoking a cnode slot that is already empty must return `Err(InvalidCapability)`,
not panic.  A test must revoke a cap, then attempt a second revoke of the same
`CapId` and assert it returns an error.

### N+26.8 — fork cap inheritance increments cap_refcount per inherited cap

After `fork_user_process_cow`, each inherited cap in the child's cnode corresponds
to one additional `cap_refcount` increment on the backing MemoryObject.  A test must
fork with one MemoryObject cap in the parent and verify `cap_refcount == 2` after.

### N+26.9 — Full suite at 757+ tests (single-threaded)

Every Stage 19 commit must pass `cargo test --lib -- --test-threads=1` with at
least 757 tests (748 from Stage 18 + 9 new Stage 19 tests).
Both `cargo check --no-default-features` and `cargo check --features hosted-dev`
must be clean.

x86_64 `-smp 1` smoke is NOT required after Stage 19: the changes are confined
to capability/refcount accounting in hosted-dev paths with no cross-CPU or
live-boot behavior changes.

## Rule N+27 — Stage 20: IPC cap-transfer / reply-cap / transfer-envelope test rules

### N+27.1 — recv-delivery cap is materialized once; rolled back on copy failure

Both recv-delivery paths materialize the transferred/reply cap into the receiver's
cnode (and consume the transfer envelope) before the metadata/payload copy that may
fault. A test must verify that on copy failure the materialized cap is rolled back:

- Transfer cap: `rollback_materialized_recv_cap(receiver, cap, false)` clears the
  receiver cnode slot AND decrements `cap_refcount` to the pre-materialization value.
- Reply cap: `rollback_materialized_recv_cap(receiver, cap, true)` fast-revokes the
  slot AND clears the global `waiter_cap_id`, leaving the `ReplyCapRecord` live.

### N+27.2 — successful transfer materialization sets cap_refcount = 2

A test must stash an envelope, take it, grant the source cap into the receiver, and
verify the backing MemoryObject `cap_refcount == 2` (sender + receiver) and the
envelope is consumed exactly once (a second take returns `None`).

### N+27.3 — reply cap is one-shot; double revoke is a stable error

A test must `ipc_reply` once (consuming the global `ReplyCapRecord`), then verify a
second `ipc_reply` on the same `CapId` returns a stable error
(`InvalidCapability`), never panics or underflows.

### N+27.4 — transfer-envelope / mapping cleanup is idempotent

A test must verify that a second `take_transfer_envelope` returns `None` without
double-decrementing `pin_refcount`, that a second `rollback_materialized_recv_cap`
returns `false` without underflowing `cap_refcount`, and that
`remove_active_transfer_mapping` returns `false` for an already-removed mapping.

### N+27.5 — `clear_reply_cap_waiter_cap` is generation-guarded

A test must verify that clearing `waiter_cap_id` with a stale generation is ignored
and with the matching generation succeeds.

### N+27.6 — mapping registration does not change cap_refcount

A test must verify `register_active_transfer_mapping` /
`remove_active_transfer_mapping` leave the backing MemoryObject `cap_refcount`
governed solely by cap mint/revoke (Invariant 11).

### N+27.7 — Full suite at 767+ tests (single-threaded)

Every Stage 20 commit must pass `cargo test --lib --features hosted-dev --
--test-threads=1` with at least 767 tests (757 from Stage 19 + 10 new Stage 20
tests). Both `cargo check --no-default-features` and `cargo check --features
hosted-dev` must be clean, and `git diff --check` must be clean.

x86_64 `-smp 1` smoke is NOT required after Stage 20: the only behavioral change is
on the IPC recv copy-fault error path (a previously-leaking cnode slot is now
revoked); the success path, syscall ABI, `SYSCALL_COUNT`, and SpawnV5 semantics are
unchanged, and no cross-CPU, SMP, trap/timer/bootstrap, or live boot path is touched.

## Rule N+28 — Stage 21: notification / IRQ-route lifetime test rules

Stage 21 hardens the notification wake path and adds the `destroy_notification`
teardown primitive. Tests must cover both the wake-safety guard and the
route/object/generation teardown ordering.

### N+28.1 — Signal must not resurrect a non-Blocked waiter

A test must assert that `signal_notification` (driven via `route_external_irq`)
woken against a `Dead` or `Exited` waiter TID lingering in `notification_waiters`
leaves the task in its terminal state. Only a `Blocked(_)` waiter may transition
to `Runnable`. See `stage21_signal_skips_dead_waiter_safely`,
`stage21_signal_skips_exited_waiter_safely`,
`stage21_signal_wakes_waiting_task_exactly_once`.

### N+28.2 — Exit/death clears the notification waiter

Both `exit_task` and `mark_task_dead` must clear the `notification_waiters` slot
for the dying TID (via `clear_ipc_waiters_for_tid`). See
`stage21_exit_task_clears_notification_waiter`,
`stage21_mark_task_dead_clears_notification_waiter`.

### N+28.3 — Destroy ordering and generation bump

A test must assert `destroy_notification` (a) tears down every `irq_routes` entry
targeting the slot, (b) clears the waiter and returns its snapshot, (c) removes
the object and bumps the generation so the cap is non-live via
`capability_object_live`. See
`stage21_destroy_notification_clears_waiter_and_invalidates_caps`,
`stage21_irq_route_registration_and_teardown`.

### N+28.4 — Pending vs waiter ordering

Tests must cover signal-before-wait (pending consumed by later recv),
wait-before-signal (waiter registered then woken), and repeated signal (FIFO
accumulation drained by recv). See
`stage21_signal_before_wait_leaves_pending_for_later_recv`,
`stage21_wait_before_signal_registers_then_wakes`,
`stage21_repeated_signal_accumulates_pending`.

### N+28.5 — IRQ to a destroyed target is a benign no-op

A test must assert a hardware IRQ routed at a destroyed notification (including a
forced stale route surviving the destroy) returns `Ok(())`, not an error. See
`stage21_irq_delivery_to_destroyed_notification_is_safe_noop`.

### N+28.6 — Full suite at 778+ tests (single-threaded)

Every Stage 21 commit must pass `cargo test --lib --features hosted-dev --
--test-threads=1` with at least 778 tests (767 from Stage 20 + 11 new Stage 21
tests). Both `cargo check --no-default-features` and `cargo check --features
hosted-dev` must be clean, and `git diff --check` must be clean.

x86_64 `-smp 1` smoke is NOT required after Stage 21: the only behavioral change is
on the notification wake/destroy paths — the wake now refuses a non-Blocked waiter
(strictly safer), and `destroy_notification` is a new teardown primitive not yet
wired into a live boot path. Syscall ABI, `SYSCALL_COUNT`, SpawnV5,
trap/timer/bootstrap, SMP, and the live IRQ-delivery success path are unchanged.

## Rule N+29 — Stage 22: `destroy_notification` revoke-wiring test rules

Stage 22 wires `destroy_notification` into the capability-revoke / cnode-teardown
/ task-exit cleanup paths. Notification caps are single-owner per object (no
refcount, never granted cross-process), so revoking any Notification cap destroys
the object. Tests must cover the revoke teardown, its idempotence, the lock-rank
separation outcomes, and stale-cap stability.

### N+29.1 — Cap revoke destroys the object and bumps generation

A test must assert that revoking a Notification cap via `revoke_capability_in_cnode`
frees `notifications[idx]`, bumps `notification_generations[idx]`, and renders the
pre-revoke generation non-live via `capability_object_live`. See
`stage22_notification_cap_revoke_destroys_object`.

### N+29.2 — Cap revoke tears down IRQ routes and clears+wakes the waiter

A test must assert revoke clears every `irq_routes` entry targeting the slot, and
that a parked `Blocked(_)` waiter is cleared and woken to `Runnable` (reusing the
Stage 21 Blocked-only gate). See `stage22_notification_cap_revoke_tears_down_irq_route`,
`stage22_notification_cap_revoke_clears_waiter`.

### N+29.3 — Cnode teardown / task exit inherit the destroy

A test must assert that `mark_task_dead` followed by
`maybe_cleanup_process_cnode_for_pid` frees a notification object held in the
dying process's cnode (the teardown loop revokes every live cap). See
`stage22_cnode_teardown_destroys_notification_object`.

### N+29.4 — Idempotence: double-revoke is safe, no double-destroy

A test must assert a second revoke of the now-empty cnode slot returns an error
(`InvalidCapability`) and does NOT bump the generation a second time —
`destroy_notification`'s `WrongObject` is swallowed. See
`stage22_double_revoke_notification_cap_is_safe`.

### N+29.5 — Stale-cap stability after revoke

Tests must assert post-revoke that: an external IRQ on the old line is a stable,
repeatable no-op; a recv via the stale RECEIVE cap returns a stable, repeatable
error (never a panic); a forced stale route at the freed slot is a benign no-op.
See `stage22_signal_after_revoke_is_stable_noop`,
`stage22_wait_after_revoke_is_stable_error`,
`stage22_route_after_revoke_is_benign_noop`.

### N+29.6 — Slot reuse and isolation

Tests must assert a fresh `create_notification` reusing a freed slot starts clean
(fresh generation, no inherited route, no stale waiter, live cap), and that
revoking one notification leaves an unrelated notification fully intact and still
delivering. See `stage22_create_notification_after_revoke_reuses_slot_cleanly`,
`stage22_notification_cap_revoke_does_not_affect_unrelated_notification`.

### N+29.7 — Full suite at 788+ tests (single-threaded)

Every Stage 22 commit must pass `cargo test --lib --features hosted-dev --
--test-threads=1` with at least 788 tests (778 from Stage 21 + 10 new Stage 22
tests). Both `cargo check --no-default-features` and `cargo check --features
hosted-dev` must be clean (the latter no longer needs a `dead_code` allow on
`destroy_notification` — it is now reachable on the non-test revoke path), and
`git diff --check` must be clean.

x86_64 `-smp 1` smoke is NOT required after Stage 22: `destroy_notification` is
reached only from the capability-revoke / cnode-teardown / task-exit cleanup
paths, and no live boot service revokes a Notification cap or tears down a cnode
holding one (`create_notification` is not invoked from any syscall handler). The
waiter wake is gated to `Blocked(_)` only (strictly safer). Syscall ABI,
`SYSCALL_COUNT`, SpawnV5, trap/timer/bootstrap, SMP/`smp.rs`, VFS/syscall27, and
the live IRQ-delivery success path are all unchanged.

## Rule N+30 — Stage 23: notification live revoke/release surface audit test rules

Stage 23 is an audit stage that proves whether a user-facing
capability-release / capability-revoke syscall can reach the Stage 22
Notification teardown. Audit result (KERNEL_LOCKING.md §41): **no generic
cap-release syscall exists**; the only release-shaped syscall is `TransferRelease`
(NR = 4), which is MemoryObject/transfer-scoped and cannot target a Notification
cap; and `create_notification` has no syscall caller. Teardown is reachable only
via the direct revoke helpers and the task/process-exit cnode-teardown path.
Tests must not fake a live cap-release syscall.

### N+30.1 — Reachable revoke surface destroys object + routes

A test must assert that the direct revoke helper (`revoke_capability_in_cnode`),
the only reachable teardown surface, frees `notifications[idx]` AND tears down the
IRQ route in one pass. See `stage23_revoke_notification_cap_destroys_object_and_routes`.

### N+30.2 — Task-exit cnode cleanup is the closest live-path equivalent

A test must assert that `mark_task_dead` + `maybe_cleanup_process_cnode_for_pid`
(the task/process-exit teardown) frees a notification object held in the dying
task's cnode and tears down its route. See
`stage23_cnode_cleanup_on_task_exit_destroys_notification`.

### N+30.3 — Idempotent double-revoke

A test must assert a second revoke of the now-empty slot is a safe error with no
double-destroy and no second generation bump. See
`stage23_notification_cleanup_idempotent_double_revoke`.

### N+30.4 — `TransferRelease` cannot target a Notification cap

A test must assert the only user release syscall's gate fails for a Notification
cap: `active_transfer_mapping_for(owner, notif_cap)` is `None`, so
`handle_transfer_release` errors before `revoke_capability_in_cnode`, leaving the
notification object alive. Must also assert `SYSCALL_COUNT == 30` (no cap-release
syscall number added). See
`stage23_transfer_release_syscall_cannot_target_notification_cap`.

### N+30.5 — Stale cap after revoke cannot signal or recv

A test must assert post-revoke that an external IRQ on the old line is a benign
repeatable no-op and a recv via the stale RECEIVE cap is a stable repeatable
error. See `stage23_stale_notification_cap_after_revoke_cannot_signal_or_recv`.

### N+30.6 — Documented missing live surface

A `#[test]` must document (with `assert_eq!(SYSCALL_COUNT, 30)` and an
`assert!(true, "...")`) that no live user-facing notification release/revoke
syscall exists and that adding one is deferred (would need a new syscall number,
out of scope by the ABI invariant). See
`stage23_no_live_notification_release_syscall_documented`.

### N+30.7 — No production change; no smoke

Stage 23 changes no production code (audit + tests + docs only), so x86_64
`-smp 1` smoke is NOT required. `cargo check --no-default-features` and `cargo
check --features hosted-dev` must be clean, the Stage 23 + Stage 22 tests must
pass single-threaded (`RUST_MIN_STACK=8388608`), and `git diff --check` must be
clean. Syscall ABI / `SYSCALL_COUNT` (30), SpawnV5, IPC recv-v2/reply-cap/
transfer-envelope, trap/timer/bootstrap, SMP/`smp.rs`, VFS/syscall27, and the
live IRQ-delivery success path are all unchanged.

## Rule N+31 — Stage 24: VFS ELF staging take-once + endpoint/reply-cap revoke audit test rules

Stage 24 has two parts. Part A replaces the raw `static mut VFS_ELF_STAGING`
with the typed `TakeOnceStagingBuffer`, encoding exclusive (mutual-exclusion)
access in the type system. Part B is an audit of endpoint/reply-cap revoke and
cnode-teardown surfaces (KERNEL_LOCKING.md §42). No production code changed in
Part B; the only production change is Part A's soundness wrapper, which is
behavior-preserving for the legitimate single-in-flight spawn path.

### N+31.1 — Take-once buffer: first claim succeeds, second fails

Tests must assert that `TakeOnceStagingBuffer::try_take` returns `Some` on the
first call and `None` while a claim is outstanding (exclusive access enforced by
the atomic flag). Use a local `static` instance for determinism — do **not**
depend on the live `VFS_ELF_STAGING` static (its claim state is shared with the
real handlers). See `stage24_vfs_elf_staging_first_claim_succeeds`,
`stage24_vfs_elf_staging_second_claim_fails`.

### N+31.2 — Take-once buffer: reusable after drop

A test must assert the RAII guard releases the claim on `Drop` so the shared
buffer is reusable by the next spawn syscall (the buffer is NOT a permanent
one-shot — both spawn handlers reuse it across many process spawns). See
`stage24_vfs_elf_staging_claim_reusable_after_drop`. A test must also confirm
`as_mut_slice` exposes exactly `N` bytes
(`stage24_vfs_elf_staging_as_mut_slice_has_full_length`).

### N+31.3 — Endpoint waiter teardown leaves no stranded receiver/sender

Tests must assert that after `mark_task_dead`, a task that was the endpoint
receiver waiter (`endpoint_waiters`) or an endpoint sender waiter
(`endpoint_sender_waiters`) is removed (`clear_ipc_waiters_for_tid` runs in the
exit path). See
`stage24_cnode_teardown_with_endpoint_cap_does_not_leave_receiver_waiter`,
`stage24_cnode_teardown_with_endpoint_cap_does_not_leave_sender_waiter`.

### N+31.4 — Reply-cap global record cleared at caller teardown; stale cap is a stable error

A test must assert that `mark_task_dead(caller)` clears the global
`reply_caps[idx]` record (via `revoke_reply_caps_for_caller`), and a separate
test must assert that a Reply cap left in the replier's cnode after the caller is
torn down resolves to a stable `StaleCapability` error from `ipc_reply` (never a
panic or a stale delivery). See `stage24_reply_cap_cnode_teardown_clears_global_record`,
`stage24_stale_reply_cap_cannot_be_reused_after_cnode_teardown`.

### N+31.5 — Endpoint cap revoke is isolated (multi-owner object survives)

A test must assert that revoking one endpoint cap clears only that cnode slot and
does NOT destroy the endpoint object (endpoints are multi-owner / delegated
cross-process), nor bump its generation, nor disturb an unrelated endpoint. See
`stage24_endpoint_cap_revoke_does_not_affect_unrelated_endpoint`.

### N+31.6 — Behavior-preserving production change; no smoke

Part A is behavior-preserving for the single-in-flight spawn path; Part B changed
no production code. No live boot/runtime behavior change → x86_64 `-smp 1` smoke
is NOT required. `cargo check --no-default-features` and `cargo check --features
hosted-dev` must be clean; the full hosted-dev suite must pass single-threaded
(`RUST_MIN_STACK=8388608`); `git diff --check` must be clean. Syscall ABI /
`SYSCALL_COUNT` (30), SpawnV5, PM/init/service boot, IPC recv-v2/reply-cap/
transfer-envelope, trap/timer/bootstrap/BT2, SMP/`smp.rs`, entering/exiting TID
trap logic, `handle_trap_with_cpu`, VFS_READ_SHARED_REPLY_ENABLED/syscall27,
Phase 3B MemoryObject zero-copy spawn, and RAMFS/FAT runtime spawning are all
unchanged.

## Rule N+32 — Stage 25: replier-exit reply-cap cleanup + endpoint permanence certification test rules

Stage 25 adds `revoke_reply_caps_for_replier(tid)` (mirror of
`revoke_reply_caps_for_caller`, keyed on `ReplyCapRecord.responder_tid`) and wires
it into `exit_task` and `mark_task_dead`, so reply records are cleared proactively
from the replier side, not only when the (possibly long-lived) caller exits. It
also certifies endpoints as permanent-once-created multi-owner objects
(KERNEL_LOCKING.md §43). The only production change is the new replier-side revoke
helper and its two call sites in `restart_state.rs`.

### N+32.1 — Replier teardown clears the global reply record

A test must assert that tearing down the replier (`mark_task_dead(replier)`),
while the caller is still alive, clears the global `reply_caps[idx]` record whose
`responder_tid == replier`. See
`stage25c_replier_exit_clears_global_reply_record`. A regression test must confirm
the existing caller-side clearing still works
(`stage25c_caller_exit_still_clears_global_reply_record`).

### N+32.2 — Both teardown directions are idempotent (no leak, no underflow)

Tests must cover all interleavings: caller-first-then-replier and
replier-first-then-caller must each be a no-op past the first clear (0 revoked on
the second call); a single teardown that runs both helpers must not leak or
underflow. See `stage25c_caller_exits_first_then_replier_exit_is_idempotent`,
`stage25c_replier_exits_first_then_caller_cleanup_is_idempotent`,
`stage25c_both_exit_no_leak_no_underflow`.

### N+32.3 — Stale reply cap after replier teardown is a stable error

A test must assert that the materialized `waiter_cap_id` does not outlive the
replier (record gone after teardown) and that a Reply cap whose record was cleared
by replier teardown resolves to a stable `StaleCapability` error from `ipc_reply`
(never a panic or stale delivery). See
`stage25c_stale_waiter_cap_id_cleared_on_replier_teardown`,
`stage25c_reply_cap_cannot_be_reused_after_replier_teardown`.

### N+32.4 — Replier revoke is selective; full teardown remains safe

A test must assert `revoke_reply_caps_for_replier` clears only records whose
`responder_tid` matches, leaving unrelated records intact
(`stage25c_unrelated_reply_record_unaffected`), and that the full
`mark_task_dead` path (including cnode teardown) with a held reply cap remains
safe (`stage25c_cnode_teardown_with_reply_cap_remains_safe`).

### N+32.5 — Endpoint permanence: cap revoke and one-owner teardown do not destroy the object

Tests must assert that a single endpoint cap revoke clears only the cnode slot,
leaving the multi-owner endpoint object and its generation intact
(`stage25d_endpoint_cap_revoke_does_not_destroy_shared_endpoint`); that tearing
down one owner's cnode leaves the shared endpoint present
(`stage25d_endpoint_remains_after_one_owner_cnode_teardown`); and that revoking a
cap of one endpoint does not disturb an unrelated endpoint
(`stage25d_unrelated_endpoint_unaffected_by_other_endpoint_revoke`).
`destroy_endpoint` must NOT be wired into cnode teardown (multi-owner by design).

### N+32.6 — Endpoint waiter cleanup on exit is complete and idempotent

Tests must assert that `exit_task` clears a task's endpoint receiver waiter and
sender waiter (`stage25d_task_exit_clears_endpoint_receiver_waiter`,
`stage25d_task_exit_clears_endpoint_sender_waiter`) and that repeated
`clear_ipc_waiters_for_tid` for an already-cleared tid is a stable no-op
(`stage25d_repeated_waiter_cleanup_idempotent`).

### N+32.7 — Lock rank, ABI, and no-smoke

`revoke_reply_caps_for_replier` runs under `ipc_state_lock` (rank 3), identical to
the caller-side revoke; it only sets bounded global array slots to `None` (no
wake, no scheduler mutation, no new lock acquisition). It is a proactive-cleanup
superset of previously-deferred behavior, so observable live-boot behavior is
unchanged → x86_64 `-smp 1` smoke is NOT required. `cargo check
--no-default-features` and `cargo check --features hosted-dev` must be clean; the
full hosted-dev suite must pass single-threaded (`RUST_MIN_STACK=8388608`);
`git diff --check` must be clean. Syscall ABI / `SYSCALL_COUNT` (30), SpawnV5,
PM/init/service boot, IPC recv-v2/reply-cap/transfer-envelope, trap/timer/
bootstrap/BT2, SMP/`smp.rs`, entering/exiting TID trap logic, `handle_trap_with_cpu`,
VFS_READ_SHARED_REPLY_ENABLED/syscall27, Phase 3B MemoryObject zero-copy spawn,
notification single-owner semantics, endpoint multi-owner semantics, and RAMFS/FAT
runtime spawning are all unchanged.

## Rule N+33 — Stage 26: global-lock callsite audit + domain-lock-only extraction test rules

### N+33.1 — Every new split-read helper must match its global-locked equivalent

For each path extracted out of the global lock onto a single domain lock, a test
must prove the split-read helper returns the **same value** as the globally-locked
accessor, both before and after the relevant state is created/mutated. The two
Stage 26 extractions are covered by
`stage26_notification_waiter_count_split_read_matches_global` (ipc, rank 3) and
`stage26_cnode_registered_split_read_matches_global` (capability, rank 4). Each
test also asserts an adjacent-path no-regression case (an unrelated, empty slot or
unrelated pid still reads the default).

### N+33.2 — Extractions must be single-domain and read-only

A Stage-26-style extraction may acquire **exactly one** domain lock (no rank
inversion possible from a single lock) and must not mutate state, wake a task, or
touch the scheduler. The helper doc must name the domain rank and the forbidden
caller-held lock ranks (≤ its own rank). Soundness for ipc array-slot reads relies
on Stage 25 endpoint/notification permanence (slot storage stable for the kernel
lifetime).

### N+33.3 — ABI guard

A dedicated test (`stage26_global_lock_audit_syscall_count_unchanged`) must assert
`SYSCALL_COUNT == 30`. The audit + extractions are pure refactoring: no syscall
opcode is added or removed.

### N+33.4 — Lock rank, no global-lock-removal claim, and no-smoke

The two new helpers each take a single domain lock (ipc rank 3 / capability rank 4)
via `*_from_raw` + `addr_of!`, never the outer `SpinLock<KernelState>` global lock.
**No full global-lock-removal is claimed** — every mutation, trap, Spawn/fork/exec,
and SMP path still serializes on the global lock. The helpers are additive and (this
stage) exercised only by the new unit tests, so no live boot/runtime/trap path
changes → x86_64 `-smp 1` smoke is NOT required. `cargo check --no-default-features`
and `cargo check --features hosted-dev` must be clean; the full hosted-dev suite must
pass single-threaded (`RUST_MIN_STACK=8388608`); `git diff --check` must be clean.
Syscall ABI / `SYSCALL_COUNT` (30), SpawnV5, PM/init/service boot, IPC
recv-v2/reply-cap/transfer-envelope, trap/timer/bootstrap/BT2, SMP/`smp.rs`,
entering/exiting TID trap logic, `handle_trap_with_cpu`,
VFS_READ_SHARED_REPLY_ENABLED/syscall27, Phase 3B MemoryObject zero-copy spawn,
notification single-owner / endpoint multi-owner semantics, and RAMFS/FAT runtime
spawning are all unchanged.

## Rule N+34 — Stage 27: first mutating global-lock extraction (`control_plane_set_process_cnode_slots`) test rules

### N+34.1 — A split-mutation helper must match global-locked behavior

The first mutating extraction (`control_plane_set_process_cnode_slots_split_mut`)
must produce the **same final state and the same errors** as the global-locked
`control_plane_set_process_cnode_slots` / `_planned` path. Covered by
`stage27_split_mut_helper_matches_global_lock_behavior_for_success` (a
system-server resize ends at the requested capacity) and
`stage27_split_mut_duplicate_update_preserves_existing_behavior` (re-apply is a
stable success). The two preserved error returns are asserted by
`stage27_split_mut_missing_process_returns_stable_error` (`TaskMissing` for an
absent requester TID) and `stage27_split_mut_missing_cnode_returns_stable_error`
(`MissingRight` for an App resizing another process's cnode).

### N+34.2 — Two-phase task(read)→capability(mutate), no rank inversion

The mutating helper must take the task lock (rank 2) only to **snapshot**, release
it, then take the capability lock (rank 4) to **apply** — never holding the
capability lock while acquiring the task lock, and never acquiring the outer global
`SpinLock<KernelState>` (no `with` / `with_cpu`). It must touch **only** the task
(read) and capability (mutate) domains plus a boot-config limits snapshot.
`stage27_split_mut_no_scheduler_wake_side_effect` asserts scheduler runnable count
and dispatch telemetry are unchanged; `stage27_split_mut_no_ipc_side_effect`
asserts a planted IPC notification waiter is undisturbed;
`stage27_split_mut_two_processes_isolated` asserts resizing one process's cnode
leaves another's untouched.

### N+34.3 — Helper-only extraction must document its live-wiring blocker

When a mutating extraction is proven by direct tests but **not** rewired at the live
callsite, the blocker must be documented (here: the `…_via_syscall` → `handle_trap`
trap-dispatch seam is class F+I and must keep entering the global lock). No full
global-lock-removal is claimed.

### N+34.4 — ABI guard, no-smoke, invariants

`SYSCALL_COUNT == 30` is unchanged (no opcode added/removed; the extraction is a
pure refactor plus an additive helper). No live boot/runtime/trap path is rewired,
so x86_64 `-smp 1` smoke is NOT required. `cargo check --no-default-features` and
`cargo check --features hosted-dev` must be clean; the Stage 27 tests must pass
single-threaded (`RUST_MIN_STACK=8388608`); `git diff --check` must be clean.
Syscall ABI / `SYSCALL_COUNT` (30), SpawnV5, PM/init/service boot, IPC
recv-v2/reply-cap/transfer-envelope, trap/timer/bootstrap/BT2, SMP/`smp.rs`,
entering/exiting TID trap logic, `handle_trap_with_cpu`,
VFS_READ_SHARED_REPLY_ENABLED/syscall27, Phase 3B MemoryObject zero-copy spawn,
notification single-owner / endpoint multi-owner semantics, and RAMFS/FAT runtime
spawning are all unchanged.

## Rule N+35 — Stage 28: trap/syscall dispatch seam audit + split-dispatch bridge scaffold test rules

### N+35.1 — The split-dispatch whitelist must be default-deny

`try_split_dispatch` / `classify_split_eligible` (`src/kernel/syscall_split.rs`)
must return `Some(..)` ONLY for explicitly whitelisted syscalls and `None` for
every other syscall (`_ => None` default arm). The acceptance proof must exhaust
the decodable syscall space: `stage28_split_dispatch_fallback_preserved_for_unwhitelisted`
walks every `Syscall::decode(nr)` for `nr < SYSCALL_COUNT` and asserts `None`, and
the targeted-reject tests assert `None` for representative dangerous classes —
`stage28_split_dispatch_whitelist_rejects_ipc_send`, `…_rejects_ipc_recv`,
`…_rejects_spawnv5`, `…_rejects_vm_map`. A non-whitelisted syscall returning
`Some` is a test failure.

### N+35.2 — A whitelisted candidate must service via its proven split helper

The sole whitelisted candidate (`ControlPlaneSetCnodeSlots`) must dispatch through
the Stage 27 `control_plane_set_process_cnode_slots_split_mut` helper and produce
the same final state as the global-locked path.
`stage28_split_dispatch_whitelist_accepts_cnode_slots_syscall` asserts it is
classified eligible and that the split dispatch resizes the target cnode;
`stage28_stage27_split_mut_helper_still_works` is the regression guard that the
delegated Stage 27 helper still resizes and still returns `TaskMissing` for an
absent requester.

### N+35.3 — Helper-only bridge must document the exact arch blocker

A trap/syscall split-dispatch bridge that is NOT live-wired must record the exact
missing arch abstraction (here: a pre-global-lock trapframe result-writeback seam
that owns `frame.set_ok(slots, pid, 0)`, plus preservation of the x86_64
`entering_tid`/`exiting_tid`/`task_switched` snapshots). The bridge must not touch
the `TrapFrame`, must not block/yield/schedule, and must not copy user memory.

### N+35.4 — ABI guard, no-smoke, invariants

`SYSCALL_COUNT == 30` is unchanged (the bridge is additive; no opcode
added/removed). `stage28_syscall_count_unchanged` is the explicit guard. No live
trap/syscall dispatch path is rewired (default-deny fallback keeps every live
syscall on the global-lock path), so x86_64 `-smp 1` smoke is NOT required and the
full suite is intentionally not run. `cargo check --no-default-features` and
`cargo check --features hosted-dev` must be clean; the Stage 28 and Stage 27 tests
must pass single-threaded (`RUST_MIN_STACK=8388608`); `git diff --check` must be
clean. x86_64 entering/exiting TID logic, `handle_trap_with_cpu`,
trap/timer/bootstrap/BT2, SMP/`smp.rs`, SpawnV5, PM/init/service boot,
VFS_READ_SHARED_REPLY_ENABLED/syscall27, Phase 3B MemoryObject zero-copy spawn, and
RAMFS/FAT runtime spawning are all unchanged.

## Rule N+36 — Stage 29: live-wire ControlPlaneSetCnodeSlots split dispatch (NR 8) test rules

Stage 29 live-wires exactly one whitelisted syscall — `ControlPlaneSetCnodeSlots` /
NR 8 — through the Stage 28 split-dispatch bridge via the new pre-global-lock
result-writeback seam `try_split_dispatch_into_frame` in
`src/kernel/syscall_split.rs`, called from `handle_trap_entry_shared`
(`src/arch/trap_entry.rs`) before `with_cpu`.

### N+36.1 — Behavior equivalence (≥8)

Tests must prove the split seam produces the SAME observable result as the old
global-lock handler for NR 8: on success `set_ok(slots, target_pid, 0)` (ret0 ==
slots, ret1 == target_pid, ret2 == 0, no error) AND the capability domain is
actually resized; on a domain error the seam returns
`TrapHandleError::Syscall(SyscallError::from(kernel_err))` and writes NO success
payload. Cover missing-task, bad-requester-class (MissingRight), missing/created
cnode, duplicate update, capacity resize, error-code preservation, and
no-scheduler / no-IPC side-effects.

### N+36.2 — Fallback safety (≥6)

Every non-whitelisted syscall must return `None` from
`try_split_dispatch_into_frame` (IPC send/recv, SpawnV5, VM map, futex), and an
exhaustive 0..SYSCALL_COUNT walk must show ONLY NR 8 is split-eligible
(`classify_split_eligible_nr_only`). `SYSCALL_COUNT == 30` must be guarded
explicitly (`stage29_syscall_count_still_30`). A whitelisted syscall with a failed
precondition or absent requester TID must also fall back (`None`).

### N+36.3 — Result-writeback equivalence (≥4)

Tests must compare the seam's success lanes against a reference
`frame.set_ok(slots, pid, 0)`; compare the seam's error against the underlying
split-mut helper's `Result` (no divergence); prove `entering_tid == exiting_tid`
across the seam (`task_switched == false`); and prove the `None` fallback path still
reaches the global-lock dispatch.

### N+36.4 — Live-wire constraints

The seam must call `set_ok` / `set_err` directly (they are architecture-neutral pure
register writes; no new contract type). It must read the requester TID via
`current_tid_split_read(cpu)` (value-equivalent to the old `current_tid`). It must
NOT block, yield, schedule, switch tasks, or copy user memory. It must be gated on
`TrapEvent::Syscall`. A `None` return must leave the existing global-lock path
byte-for-byte UNCHANGED. The whitelist must not be broadened beyond NR 8.

### N+36.5 — Smoke, suite, invariants

Because NR 8 is live-wired, x86_64 `-smp 1` smoke IS required; if QEMU is
unavailable it is deferred to CI/manual with the §47.9 markers documented. The live
`kernel_boot` binary must build clean for `targets/x86_64-yarm-none.json`. The full
test suite must pass single-threaded (`--test-threads=1`; the multi-threaded run is
unsupported because tests share global static state). x86_64 entering/exiting TID
logic, `handle_trap_with_cpu`, the trap boundary, SMP/`smp.rs`, SpawnV5, PM/init/
service boot, VFS_READ_SHARED_REPLY_ENABLED/syscall27, Phase 3B MemoryObject
zero-copy spawn, and RAMFS/FAT runtime spawning must all be unchanged. `git diff
--check` must be clean.

### Rule N+37 (Stage 30)

Every debug-only safety guard added to arch/runtime code (C1 `borrow_kernel_for_boot`
raw-borrow window, arch timer/trap `debug_assert!`) must have a corresponding test
that: (a) sets the guard state, (b) asserts the detection function returns the
expected value, and (c) clears state so subsequent tests are not contaminated. If
the guard fires a `debug_assert!`/panic in double-set scenarios, a `#[should_panic]`
test must prove it. All guard tests must pass with `--test-threads=1` (the
`AtomicBool` guard is process-global and not thread-isolated). Validation-status
labels on split helpers must be accurate: `TRAP_FORBIDDEN` helpers must not be
called from the pre-global-lock trap seam; `LIVE_TRAP_SMOKE_X86_64` helpers must
have a smoke-validated call path. The boot-guard `BOOT_RAW_BORROW_ACTIVE` static,
begin/end helpers, and `BootRawKernelBorrowGuard` must all be
`#[cfg(any(debug_assertions, test))]` with zero release-build cost. `SYSCALL_COUNT
== 30` must be confirmed by a stage30_ test.

### Rule N+38 (Stage 31)

The first IPC live fast-path split candidate — `IpcRecv` of a plain message
already queued on a buffered endpoint — must be **default-deny outside the single
proven case** and, when not provably split-safe on the live trap seam, must be
**helper-only** (NOT wired into `try_split_dispatch_into_frame`). A Stage 31 split
recv test suite (`stage31_` prefix) must prove all of:

(a) **Behavior:** a queued plain message to a kernel-task (no user ASID) receiver
is dequeued and the frame return lanes are written byte-for-byte identical to the
unchanged `syscall::dispatch` recv path — ret0/ret1/ret2 + error lane + both inline
payload words (`stage31_split_recv_return_lanes_match_old_path`). Exactly one
message is dequeued per call (`stage31_split_recv_dequeues_exactly_one_message`).

(b) **Fallback (return None, no error written):** empty endpoint
(WouldBlock-class), recv-v2 metadata request, cap-transfer / reply-cap message,
sender-waiter refill, and any non-IpcRecv IPC syscall (send/call/reply) must all
fall back. A fallback must NOT write an error lane.

(c) **Error equivalence:** an invalid recv cap must return the SAME error the old
global-lock recv path returns (`Some(Err(TrapHandleError::Syscall(..)))`), proven
against a freshly-built reference state, not a silent fallback.

(d) **No scheduler/waiter side effects:** `task_switched` stays `false` (current
TID unchanged across the call); no orphaned receiver/sender waiter is left on the
endpoint; no sender-wake plan is produced (the refill case is rejected).

(e) **Live-seam contract guard:** a test must assert that IpcRecv remains
default-deny in `try_split_dispatch_into_frame` (helper-only), and the Stage 29
NR-8 live split-dispatch must still work (`stage31_nr8_split_still_works`).

(f) **ABI / invariants:** `SYSCALL_COUNT == 30` confirmed by a stage31_ test; no
syscall numbers added; Stages 12–30 behavior preserved. If helper-only, the exact
live-wiring blocker (user-copy + capability-domain resolution not split-extracted)
must be documented in `doc/KERNEL_LOCKING.md` §49. No full global-lock-removal
claim. x86_64 smoke is required only if the path is live-wired; a helper-only
Stage 31 defers smoke and must keep the `YARM_LOCK_SPLIT_DISPATCH nr=8 result=ok`
marker as the live split indicator. All stage31_ tests must pass with
`--test-threads=1`.

### Rule N+39 (Stage 32)

The endpoint-cap resolution split for the helper-only IPC queued-plain recv path
must be **phase-separated and non-mutating**, and the user-ASID writeback must stay
**default-deny** until its copy-failure semantics are proven. A Stage 32 test suite
(`stage32_` prefix) must prove all of:

(a) **Cap resolution lock discipline:** `resolve_endpoint_recv_cap_split_read`
must resolve a valid endpoint RECEIVE cap into an `EndpointRecvCapSnapshot` carrying
the resolved endpoint object + rights + requester identity
(`stage32_cap_resolution_valid_endpoint_recv_right`); it must NOT touch the IPC
domain (a message queued before resolution is still deliverable after —
`stage32_cap_resolution_no_ipc_lock_required`); it must NOT mutate cap state
(two resolutions yield identical snapshots — `stage32_cap_resolution_no_cap_mutation`);
and it must respect per-process cnode isolation
(`stage32_cap_resolution_two_processes_isolated`). The helper must never hold the
task lock and capability lock simultaneously and never acquire `ipc_state_lock`
(documented + enforced by the `_from_raw` phase split).

(b) **Cap resolution error equivalence:** missing cap / out-of-range cap / unknown
requester → `KernelError::InvalidCapability`; non-endpoint object →
`WrongObject`; endpoint without RECEIVE → `MissingRight`
(`stage32_cap_resolution_missing_cap_error`, `_invalid_cap_id_error`,
`_unknown_requester_error`, `_wrong_object_error`, `_missing_recv_right_error`).

(c) **Integration equivalence + fallback:** the integrated split path
(`try_split_ipc_recv_queued_plain_into_frame` using the snapshot) must succeed for a
kernel-task queued plain recv with lanes matching the old path
(`stage32_integrated_queued_recv_valid_cap_succeeds`,
`stage32_integrated_lanes_match_old_path`); preserve the old error code for invalid
cap and wrong object (`stage32_integrated_invalid_cap_matches_old_path_error`,
`_wrong_object_matches_old_path`); and fall back (return `None`) for empty endpoint,
user-ASID receiver, cap-transfer message, and recv-v2 request
(`stage32_integrated_empty_endpoint_fallback`, `_user_asid_receiver_fallback`,
`_cap_transfer_fallback`, `_recv_v2_fallback`).

(d) **Writeback plan scaffold:** the `IpcRecvQueuedPlainWritebackPlan` must capture
payload bytes + return metadata (`stage32_writeback_plan_stores_payload`); reject a
payload larger than `MAX_PLAIN_PAYLOAD` (`stage32_writeback_plan_bounds_payload_len`);
produce kernel-task lanes equal to the integrated split path
(`stage32_writeback_plan_kernel_task_writeback`); and keep the user-ASID branch
DISABLED (kernel-task-only constructor — `stage32_writeback_plan_user_asid_disabled`).
The user-ASID receiver case must remain fallback-only until the
message-consumed-on-copy-fail semantics across a post-dequeue
`copy_to_current_user` (outside `ipc_state_lock`) are matched and documented in
`doc/KERNEL_LOCKING.md` §50.

(e) **Live-seam contract + regression:** `IpcRecv` must remain default-deny in
`try_split_dispatch_into_frame` (`stage32_ipc_recv_not_wired_into_live_seam`); the
Stage 29 NR-8 live split-dispatch must still work (`stage32_nr8_split_still_works`);
`SYSCALL_COUNT == 30` confirmed (`stage32_syscall_count_still_30`); no syscall
numbers added; Stages 12–31 behavior preserved. x86_64 smoke is deferred (the path
stays helper-only); the `YARM_LOCK_SPLIT_DISPATCH nr=8 result=ok` marker remains the
live split indicator. All stage32_ tests must pass with `--test-threads=1`.

### Rule N+40 (Stage 32B)

When the kernel-task IpcRecv queued-plain split is **live-wired** into
`try_split_dispatch_into_frame`, a `stage32b_` test suite must prove the live-wire
is safe AND default-deny-preserving:

(a) **Live kernel-task path:** a kernel-task receiver of a queued plain message,
routed through `try_split_dispatch_into_frame` (NOT just the helper), must return
`Some(Ok(()))` and write the canonical kernel-task lanes
(`set_ok(sender, raw_len, NO_TRANSFER_CAP)`) —
`stage32b_ipc_recv_live_kernel_task_queued_plain`,
`stage32b_ipc_recv_live_wired_into_seam_kernel_task`.

(b) **Fallback preservation through the LIVE seam:** every non-serviceable case must
return `None` from `try_split_dispatch_into_frame` (NOT a fabricated `Some(Err)`),
so the global-lock path handles it unchanged — user-ASID receiver
(`stage32b_ipc_recv_user_asid_still_fallback`), empty queue
(`stage32b_ipc_recv_empty_queue_still_fallback`), recv-v2
(`stage32b_ipc_recv_v2_fallback`). An invalid cap is NOT a fallback: it must return
the SAME `Some(Err)` the global-lock path produces
(`stage32b_ipc_recv_invalid_cap_matches_old_path_error`).

(c) **Classification:** NR 2 must pass the NR-only split gate and classify as
`IpcRecvKernelTask` (`stage32b_ipc_recv_classify_nr2_eligible`); the arg-only
`try_split_dispatch` must still defer IpcRecv to `None`
(`stage32b_arg_only_dispatch_defers_ipc_recv`); IpcRecvTimeout (NR 5) must NOT be
split-eligible (`stage32b_ipc_recv_timeout_nr_not_in_whitelist`,
`stage32b_ipc_recv_timeout_nr_not_split_eligible`); IpcSend/IpcCall/IpcReply stay
default-deny (`stage32b_ipc_send_call_reply_not_split_eligible`).

(d) **ABI + NR-8 regression:** NR-8 split dispatch must still succeed
(`stage32b_nr8_regression`); `SYSCALL_COUNT == 30`
(`stage32b_syscall_count_30`); no syscall numbers added; Stages 12–32 behavior
preserved. Telemetry markers must be low-noise (LIVE path only):
`YARM_LOCK_SPLIT_IPC_RECV nr=2 phase=cap_plan|writeback result=ok`. All
`stage32b_` tests must pass with `--test-threads=1`.

### Rule N+41 (Stage 33+34)

When the **canonical internal receive engine** (`recv_core`) and the
**recv_shared_v3 scaffold** are landed, a `stage33_` test suite must prove:

**(A) Request model and adapter correctness:**

- `from_legacy_ipc_recv` with a user-ASID `is_kernel_task=false` must produce a
  request with `RecvPayloadTarget::UserMemory` and `RecvRequestKind::LegacyRecv`
  (`stage33_legacy_adapter_user_asid_plan_fallback`).
- `from_legacy_ipc_recv` with `is_kernel_task=true` must produce
  `RecvPayloadTarget::KernelRegister` and plan as `KernelPlainEligible`
  (`stage33_legacy_adapter_kernel_task_plan_eligible`).
- `from_recv_v2` with a non-zero meta_ptr ≥ `META_V2_MIN_LEN` must set
  `RecvMetaTarget::V2` (`stage33_recv_v2_adapter_meta_target_populated`,
  `stage33_legacy_adapter_v2_meta_detected`).
- `from_recv_v2` with a meta_len below `META_V2_MIN_LEN` must set
  `RecvMetaTarget::None` (`stage33_recv_v2_adapter_small_meta_len_yields_no_meta`).
- `from_ipc_recv_timeout` with `ticks > 0` must yield `RecvBlockingPolicy::Timed`
  (`stage33_timeout_adapter_nonzero_ticks_is_timed_recv`).
- `from_ipc_recv_timeout` with `ticks == 0` must yield `RecvBlockingPolicy::NonBlocking`
  (`stage33_timeout_adapter_zero_ticks_is_nonblocking_probe`).

**(B) Planning/eligibility:**

- User-ASID recv plan must return `FallbackRequired(UserAsidCopySemantics)`
  (`stage33_canonical_core_fallback_for_user_asid`,
  `stage33_legacy_adapter_user_asid_plan_fallback`).
- V2-meta recv plan must return `FallbackRequired(RecvV2MetaUserCopy)`
  (`stage33_canonical_core_fallback_for_recv_v2`,
  `stage33_legacy_adapter_v2_meta_kernel_task_fallback_on_meta`).
- Kernel-task plain recv plan must return `KernelPlainEligible`
  (`stage33_legacy_adapter_kernel_task_plan_eligible`).
- `future_shared_v3` plan must return `FallbackRequired(SharedV3HelperOnly)`
  (`stage33_recv_v3_future_adapter_is_helper_only`).
- `META_V2_MIN_LEN == 40` constant confirmed
  (`stage33_recv_core_metadata_v2_min_len_constant`).

**(C) Canonical core execution (live path):**

- `try_recv_core_kernel_plain` must deliver a queued plain message to a
  kernel-task receiver, returning `RecvOutcome::Delivered` with
  `RecvWritebackPlan::KernelRegister` (`stage33_kernel_task_queued_plain_recv_through_canonical_core`).
- Empty queue must yield `RecvOutcome::WouldBlock` (`stage33_empty_queue_fallback`).
- Invalid cap must yield `RecvOutcome::Error` (`stage33_legacy_adapter_invalid_cap_matches_old_path_error`).
- Telemetry markers `YARM_RECV_CORE_ADAPTER kind=legacy` and
  `YARM_RECV_CORE_LIVE kind=kernel_plain` must be emitted on the live path
  (`stage33_cap_plan_markers_through_canonical_core`).

**(D) Copy-failure semantics (documentation freeze):**

- User-ASID recv on the split path must return `None` (fallback) WITHOUT dequeuing
  (`stage33_copy_failure_user_asid_recv_falls_back_not_dequeues`).
- The documented fallback reason must be `UserAsidCopySemantics`
  (`stage33_copy_failure_plan_fallback_reason_is_copy_semantics`).

**(E) recv_shared_v3 scaffold:**

- `SYSCALL_COUNT == 30` — no new public syscall added (`stage33_recv_v3_no_syscall_added`).
- `future_shared_v3` adapter is `#[cfg(test)]` only; it always falls back with
  `SharedV3HelperOnly` (`stage33_recv_v3_future_adapter_is_helper_only`).
- `V3_VERSION == 3` validated in request header; version 0 and mismatched versions rejected
  (`stage33_recv_v3_request_validates_version`).
- Short record (< `V3_MIN_REQUEST_LEN`) rejected
  (`stage33_recv_v3_request_rejects_short_record`).
- Nonzero reserved fields rejected (`stage33_recv_v3_request_rejects_nonzero_reserved`).
- `MAP_READ` / `MAP_WRITE` map_intent bits round-trip correctly
  (`stage33_recv_v3_request_parses_read_only_intent`,
  `stage33_recv_v3_request_parses_read_write_intent`).
- Unknown map_intent bits (outside `MAP_READ | MAP_WRITE`) rejected
  (`stage33_recv_v3_request_rejects_map_intent_without_metadata_buffer` covers this
  via the validate function).
- Output record version checked; short output record rejected
  (`stage33_recv_v3_output_record_version_checked`).

**(F) Regression:**

- Stage 32B kernel-plain live path still works through the new canonical core
  (`stage33_stage32b_kernel_plain_path_still_live`).
- NR-8 split dispatch still succeeds (`stage33_nr8_split_still_live`).
- Full IPC round-trip (send + queued recv) still delivers the correct message
  (`stage33_full_ipc_round_trip_still_works`).
- `from_legacy_ipc_recv` and timeout adapter behavior is identical to the old
  direct path (`stage33_timeout_adapter_full_path_behavior_unchanged`).
- User-ASID fallback reason is explicitly confirmed via `plan_recv_core`
  (`stage33_user_asid_fallback_reason_explicit`).
- `SYSCALL_COUNT == 30` (`stage33_syscall_count_still_30`).

**Run command:** `cargo test --lib stage33 -- --test-threads=1`

All `stage33_` tests must pass. The full lib test (`cargo test --lib --
--test-threads=1`) must pass with 0 failures. Multi-threaded runs (> 1 thread) may
abort due to the pre-existing large-KernelState stack/allocator interaction;
single-threaded runs are authoritative.

### Rule N+42 (Stage 35)

When receive ABI adapters are integrated into the full-path handlers, a `stage35_` test suite must prove:

**(A) Adapter request shape:**
- `from_legacy_ipc_recv` with `is_kernel_task=true` → `KernelRegister` payload target + `KernelPlainEligible` plan (`stage35_ipc_recv_adapter_kernel_task_has_register_target`)
- `from_legacy_ipc_recv` with `is_kernel_task=false` → `UserMemory` payload target + `UserAsidCopySemantics` fallback (`stage35_ipc_recv_adapter_user_asid_has_user_memory_target`)
- `from_legacy_ipc_recv` with `meta_ptr ≠ 0, meta_len ≥ 40` → `RecvMetaTarget::V2` + `RecvV2MetaUserCopy` fallback (`stage35_ipc_recv_adapter_v2_meta_detected_when_len_sufficient`)
- `meta_len < 40` → `RecvMetaTarget::None` (`stage35_ipc_recv_adapter_no_meta_when_len_too_small`)
- `meta_ptr == 0` → `RecvMetaTarget::None` regardless of meta_len (`stage35_ipc_recv_adapter_no_meta_when_ptr_zero`)

**(B) Timeout adapter shape:**
- `timeout_ticks == 0` → `NonblockingProbe` + `NoWait` (`stage35_timeout_adapter_zero_ticks_is_nonblocking`)
- `timeout_ticks > 0` → `TimedRecv` + `Deadline` (`stage35_timeout_adapter_nonzero_ticks_is_deadline`)
- `preread_deadline = Some(abs)` → `Deadline(abs)` (`stage35_timeout_adapter_preread_deadline_captured`)

**(C) Equivalence:**
- Canonical `meta_target` v2 detection must agree with the old inline `frame.arg(INLINE0) != 0 && frame.arg(INLINE1) >= 40` for all representative (ptr, len) pairs (`stage35_v2_detection_canonical_matches_inline`)

**(D) Regression:**
- Stage 32B kernel-plain live split still works (`stage35_kernel_plain_path_still_live`)
- User-ASID falls back before dequeue; message preserved in queue (`stage35_user_asid_still_fallback_before_dequeue`)
- `timeout_ticks == 0` on empty queue returns None from `try_ipc_recv` (`stage35_ipc_recv_timeout_zero_ticks_empty_queue_returns_none`)
- `recv_shared_v3` adapter still helper-only (`stage35_recv_v3_still_helper_only`)
- `SYSCALL_COUNT == 30` (`stage35_syscall_count_still_30`)

**Run command:** `cargo test --lib stage35 -- --test-threads=1`

All 15 `stage35_` tests must pass.

---

## Rule N+43: Stage 36 — user-ASID plain recv live-enable

When user-ASID receive writeback semantics are formalized and the narrow plain recv path is live-enabled, a `stage36_` test suite must prove:

**(A) Plan shape: user-ASID cases:**
- Plain user-ASID (no meta, no map_intent) → `UserPlainEligible` (`stage36_plan_user_asid_plain_no_meta_is_eligible`)
- user-ASID + V2 meta → `UserPlainV2Eligible` (`stage36_plan_user_asid_with_v2_meta_is_v2_eligible`) [updated Stage 37: user-ASID+V2+no-map now promoted]
- Mapped recv (map_intent != None) → `UserAsidCopySemantics` (`stage36_plan_user_asid_mapped_recv_falls_back_copy_semantics`)
- NoWait user-ASID plain → `UserPlainEligible` (`stage36_plan_user_asid_nowait_no_meta_is_eligible`)
- Kernel-task still `KernelPlainEligible` (`stage36_plan_kernel_task_still_kernel_plain_eligible`)

**(B) Semantics equivalence:**
- Undersized buffer → `Err(InvalidArgs)` matching full-path (`stage36_undersized_buffer_returns_invalid_args`)
- Message consumed after undersized error (`stage36_undersized_buffer_consumes_message`)
- Empty queue → split returns None (`stage36_empty_queue_user_asid_falls_back`)

**(C) Live split path:**
- Zero-byte message always fits → split delivers (`stage36_user_asid_split_delivers_empty_payload`)
- Sufficient buffer → split delivers (`stage36_user_asid_split_delivers_with_sufficient_buffer`)

**(D) Writeback outcomes:**
- UndersizedBuffer when user_buf_len < payload_len (`stage36_writeback_outcome_undersized_buffer`)
- Ok or CopyFault for 0-byte payload without ASID (`stage36_writeback_outcome_ok_for_empty_payload`)
- Empty queue → WouldBlock (`stage36_try_recv_core_user_plain_empty_queue_is_would_block`)

**(E) Regressions:**
- Kernel-plain split path still live (`stage36_kernel_plain_path_still_live`)
- recv-v2 still falls back (`stage36_recv_v2_still_falls_back`)
- Mapped recv still falls back (`stage36_mapped_recv_still_falls_back`)
- Cap-transfer user-ASID falls back at dequeue (`stage36_user_asid_cap_transfer_message_falls_back`)

**(F) Invariants:**
- `SYSCALL_COUNT == 30` (`stage36_syscall_count_still_30`)
- recv-v3 still helper-only (`stage36_recv_v3_still_helper_only`)

**Run command:** `cargo test --lib stage36 -- --test-threads=1`

All 19 `stage36_` tests must pass.

---

## Rule N+44: Stage 37 — recv-v2 metadata writeback semantics audit and live-enable

When recv-v2 metadata writeback semantics for plain queued messages are formalized and
the narrow user-ASID + V2 meta path is live-enabled, a `stage37_` test suite must prove:

**(A) Plan shape:**
- user-ASID + V2 meta + no map_intent → `UserPlainV2Eligible` (`stage37_plan_user_asid_v2_meta_no_map_is_v2_eligible`)
- user-ASID + V2 meta + map_intent → `UserAsidCopySemantics` (`stage37_plan_user_asid_v2_meta_with_map_falls_back_copy_semantics`)
- kernel-task + V2 meta still → `RecvV2MetaUserCopy` (`stage37_plan_kernel_task_v2_meta_still_fallback`)
- user-ASID + no meta still → `UserPlainEligible` (`stage37_plan_user_asid_no_meta_still_plain_eligible`)
- kernel-task + no meta still → `KernelPlainEligible` (`stage37_plan_kernel_task_no_meta_still_kernel_plain_eligible`)

**(B) `try_recv_core_user_plain_v2` shape:**
- Delivers queued plain message with `UserMemoryV2` writeback plan (`stage37_try_recv_core_user_plain_v2_delivers_message`)
- Empty queue returns `WouldBlock` (`stage37_try_recv_core_user_plain_v2_empty_queue_is_would_block`)

**(C) Writeback outcomes:**
- Meta copy fault when no ASID → `MetaCopyFault` (`stage37_writeback_v2_meta_fault_when_no_asid`)
- Payload undersized after meta success → `PayloadUndersized` (`stage37_writeback_v2_payload_undersized_after_meta_success`)
- Zero-byte payload with ASID → `Ok` (`stage37_writeback_v2_ok_for_empty_payload_with_asid`)

**(D) Meta struct fields:**
- Bytes [0..8] = sender_tid (`stage37_meta_struct_sender_field_is_sender_tid`)
- Bytes [12..16] = payload_len as u32 (`stage37_meta_struct_payload_len_field`)
- Bytes [16..24] = `Message::NO_TRANSFER_CAP` = `u64::MAX` (`stage37_meta_struct_transfer_cap_field_is_no_transfer_cap`)
- Bytes [24..32] = 0 (recv_meta_flags = 0 for plain) (`stage37_meta_struct_flags_field_is_zero_for_plain`)

**(E) Integration (split path end-to-end):**
- `ret0 == 0` on success in recv-v2 mode (`stage37_user_asid_v2_split_ret0_is_zero_on_success`)
- Zero-byte payload delivers via split (`stage37_user_asid_v2_split_delivers_empty_payload`)
- Meta fault → `Err(PageFault)` returned (`stage37_user_asid_v2_meta_fault_returns_page_fault`)
- Meta fault consumes message (dequeue already happened) (`stage37_user_asid_v2_meta_fault_consumes_message`)
- Payload undersized → `Err(InvalidArgs)` returned (`stage37_user_asid_v2_payload_undersized_returns_invalid_args`)
- Empty queue → split returns None → falls back (`stage37_empty_queue_v2_user_asid_falls_back`)

**(F) Regressions:**
- Kernel-plain split path still live (`stage37_kernel_plain_path_still_live`)
- User-plain (no meta) split path still live (`stage37_user_plain_path_still_live`)
- Mapped recv still falls back (`stage37_mapped_recv_still_falls_back`)
- recv-v3 still helper-only (`stage37_recv_v3_still_helper_only`)

**(G) Invariants:**
- `SYSCALL_COUNT == 30` (`stage37_syscall_count_still_30`)

**Run command:** `cargo test --lib stage37 -- --test-threads=1`

All 25 `stage37_` tests must pass.

---

## Rule N+45: Stage 38+39 — transfer/reply/shared audit + sender-waiter fix

When the recv-core transfer/reply/shared semantics are audited and plain sender-waiter
refill is live-enabled, a `stage38_` test suite must prove:

**(A) Cap-transfer/reply-cap blocked before dequeue:**
- `FLAG_CAP_TRANSFER` at head → `FallbackRequired(CapTransfer)`, message not dequeued (`stage38_cap_transfer_flag_at_head_returns_fallback_before_dequeue`)
- `FLAG_REPLY_CAP` at head → `FallbackRequired(CapTransfer)`, message not dequeued (`stage38_reply_cap_flag_at_head_returns_fallback_before_dequeue`)
- `FLAG_CAP_TRANSFER_PLAIN` at head → `FallbackRequired(CapTransfer)` (`stage38_cap_transfer_plain_flag_at_head_returns_fallback`)
- Split dispatch returns None when cap-transfer at head (`stage38_cap_transfer_split_dispatch_returns_none`)

**(B) Sender-waiter with cap-transfer still falls back:**
- Cap-transfer sender-waiter → `FallbackRequired(SenderWaiterWake)`, plain head not dequeued (`stage38_sender_waiter_cap_transfer_message_still_falls_back`)

**(C) Sender-waiter with plain message now live:**
- `try_recv_core_kernel_plain` + plain sender-waiter → `Delivered + WakeSender` (`stage38_try_recv_core_kernel_plain_sender_waiter_returns_delivered_with_wake`)
- Integration: split dispatch delivers 'first' + wakes sender (`stage38_sender_waiter_plain_kernel_split_dispatch_delivers_first_wakes_sender`)
- 'second' in queue after delivery (`stage38_sender_waiter_plain_kernel_second_in_queue_after_delivery`)
- `try_recv_core_user_plain` + plain sender-waiter → `Delivered + WakeSender` (`stage38_try_recv_core_user_plain_sender_waiter_returns_delivered_with_wake`)

**(D) Plan model:**
- Mapped recv still → `UserAsidCopySemantics` (`stage38_plan_user_asid_mapped_recv_still_falls_back_copy_semantics`)
- v3 shared still → `SharedV3HelperOnly` (`stage38_plan_shared_v3_still_helper_only`)
- v3 with user meta still → `SharedV3HelperOnly` (`stage38_plan_user_asid_v3_meta_falls_back_meta_copy`)

**(E) Regression:**
- kernel-plain path still live (`stage38_kernel_plain_path_still_live`)
- user-plain path still live (`stage38_user_plain_path_still_live`)
- user-v2 plain path still live (`stage38_user_v2_plain_path_still_live`)
- recv_shared_v3 still helper-only (`stage38_recv_shared_v3_remains_helper_only`)
- `SYSCALL_COUNT == 30` (`stage38_syscall_count_still_30`)

**Run command:** `cargo test --lib stage38 -- --test-threads=1`

All `stage38_` tests must pass.

---

## Rule N+46: Stage 40+41 — recv_shared_v3 ABI contract + disabled dispatch scaffold

**Scope:** `mod stage40` in `src/kernel/boot/tests.rs` and `#[cfg(test)] mod tests` in
`crates/yarm-ipc-abi/src/recv_shared_v3_abi.rs` and
`crates/yarm-user-rt/src/recv_v3_draft.rs`.

All tests must be deterministic at `--test-threads=1` and must NOT add any live
syscall dispatch.  `SYSCALL_COUNT` must remain 30.

**(A) ABI record validation (kernel `recv_shared_v3` module):**
- Valid request accepted (`stage40_abi_valid_request_accepted`)
- Bad version rejected (`stage40_abi_bad_version_rejected`)
- Short record rejected (`stage40_abi_short_record_rejected`)
- Nonzero reserved rejected (`stage40_abi_nonzero_reserved_rejected`)
- Nonzero flags rejected (`stage40_abi_nonzero_flags_rejected`)
- map_intent without metadata rejected (`stage40_abi_map_intent_without_metadata_rejected`)
- Unknown map intent bits rejected (`stage40_abi_unknown_map_intent_bits_rejected`)
- Read-only map intent accepted (`stage40_abi_read_only_map_intent_accepted`)
- Read-write map intent accepted (`stage40_abi_read_write_map_intent_accepted`)
- Valid output accepted (`stage40_abi_valid_output_accepted`)
- Output bad version rejected (`stage40_abi_output_bad_version_rejected`)
- Output short record rejected (`stage40_abi_output_short_record_rejected`)

**(B) from_v3_abi_request kernel adapter:**
- Valid request → SharedV3Future kind + correct tid/cap (`stage40_adapter_valid_request_yields_shared_v3_future`)
- Bad version → Err (`stage40_adapter_bad_version_returns_error`)
- timeout=MAX → WaitForever (`stage40_adapter_no_timeout_is_wait_forever`)
- timeout=0 → NoWait (`stage40_adapter_zero_timeout_is_no_wait`)
- timeout=N → Deadline(N) (`stage40_adapter_nonzero_timeout_is_deadline`)
- No map_intent → RecvMapIntent::None (`stage40_adapter_no_map_intent_is_none`)
- MAP_READ → ReadOnly (`stage40_adapter_read_only_map_intent`)
- MAP_READ|MAP_WRITE → ReadWrite (`stage40_adapter_read_write_map_intent`)
- No metadata → None meta target (`stage40_adapter_without_metadata_sets_none_meta_target`)
- With metadata (≥80 bytes) → V3Future meta target (`stage40_adapter_with_metadata_sets_v3future_meta_target`)
- Short metadata len → None meta target (`stage40_adapter_short_metadata_len_gives_none_meta_target`)
- Payload target is UserMemory (`stage40_adapter_payload_target_is_user_memory`)

**(C) plan always returns SharedV3HelperOnly:**
- Valid adapter request (`stage40_plan_valid_adapter_request_returns_helper_only`)
- With metadata (`stage40_plan_v3_with_metadata_still_helper_only`)
- With read map intent (`stage40_plan_v3_with_read_map_intent_still_helper_only`)

**(D) Invariants:**
- `SYSCALL_COUNT == 30` (`stage40_syscall_count_still_30`)
- No public v3 dispatch reachable (`stage40_no_public_v3_dispatch_reachable`)

**(E) Regression:**
- Kernel-plain path still live (`stage40_kernel_plain_path_still_live`)
- User-plain path still live (`stage40_user_plain_path_still_live`)

**(F) yarm-ipc-abi recv_shared_v3_abi tests:**
- All 15 `abi_*` and `recv_shared_v3_abi::tests::*` tests in the ABI crate must pass.
- Run: `cargo test -p yarm-ipc-abi`

**(G) yarm-user-rt draft module tests:**
- All `recv_v3_draft::tests::*` tests must pass.
- Run: `cargo test -p yarm-user-rt` (when yarm-user-rt has a test harness).

**Run command:** `cargo test --lib stage40 -- --test-threads=1`

All 31 `stage40_` tests must pass.


## Rule N+47: Stage 42+43 — cap-transfer split path + live recv_shared_v3 dispatch (NR 30)

Stage 42+43 proves cap-transfer materialization on the split path and wires syscall NR 30
(`recv_shared_v3`). Tests must cover: syscall number allocation, cap-transfer dequeue
behavior, plan and delivery, and the live non-blocking handler.

**(A) Syscall ABI invariants:**
- `SYSCALL_COUNT == 31` (`stage42_syscall_count_is_31`)
- `SYSCALL_RECV_SHARED_V3_NR == 30` (`stage42_recv_shared_v3_nr_is_30`)
- `RecvSharedV3` decodes from NR 30 (`stage42_recv_shared_v3_decodes_from_30`)
- `VARIANT_COUNT == 23` (`stage42_variant_count_is_23`)
- NR 31 slot is unoccupied (`stage42_no_unused_syscall_number_31`)

**(B) Cap-transfer dequeue behavior:**
- Cap-transfer message dequeued (not fallback) (`stage42_cap_transfer_message_dequeued_not_fallback`)
- Reply-cap message dequeued (not fallback) (`stage42_reply_cap_message_dequeued_not_fallback`)
- `RecvCapTransferPlan` populated for `FLAG_CAP_TRANSFER` (`stage42_cap_transfer_plan_populated_for_flag_cap_transfer`)
- `RecvCapTransferPlan.is_reply_cap` for `FLAG_REPLY_CAP` (`stage42_cap_transfer_plan_reply_cap_flag`)
- Plain message has no plan (`stage42_plain_message_has_no_cap_transfer_plan`)

**(C) Regression: existing paths unchanged:**
- Kernel-plain path still live (`stage42_kernel_plain_path_still_live`)
- User-plain path still live (`stage42_user_plain_path_still_live`)

**(D) Lock-order proof (see §58):**
- ipc_state_lock (rank 3) always released before capability_lock (rank 4).
- No lock held across user-memory copy.
- Rollback only on meta fault or undersized buffer; no rollback on payload copy fault.

**Run command:** `cargo test --lib stage42 -- --test-threads=1`

All 12 `stage42_` tests must pass.


## Rule N+48: Stage 44 — SYSCALL_RECV_SHARED_V3_NR off-by-one fix + user-rt wrapper + dispatch tests

Stage 44 corrects the off-by-one (`SYSCALL_RECV_SHARED_V3_NR` was 31, valid range is
`0..SYSCALL_COUNT-1 = 0..30`) and adds the userspace runtime wrapper plus kernel dispatch tests.

**(A) Off-by-one fix:**
- `SYSCALL_RECV_SHARED_V3_NR == 30` (compile-time assert added)
- `SYSCALL_COUNT == 31` unchanged
- NR 31 unallocated (`stage44_nr31_is_out_of_range`)

**(B) Kernel dispatch tests (9 in `mod stage44`):**
- NR 30 routes to RecvSharedV3 (`stage44_syscall_nr30_is_recv_shared_v3`)
- `SYSCALL_COUNT` unchanged at 31 (`stage44_syscall_count_still_31`)
- req_len below minimum → InvalidArgs (`stage44_req_len_below_minimum_returns_invalid_args`)
- Empty endpoint → WouldBlock (`stage44_empty_endpoint_returns_would_block`)
- Queued plain message delivers, correct payload length (`stage44_queued_plain_message_delivers`)
- Nonzero timeout → WouldBlock (`stage44_timeout_nonzero_returns_would_block`)
- Nonzero map_intent → InvalidArgs (`stage44_map_intent_nonzero_returns_invalid_args`)
- IpcRecv NR 2 still dispatches (`stage44_ipc_recv_nr2_still_dispatches`)

**(C) user-rt wrapper (`crates/yarm-user-rt/src/syscall/recv_v3.rs`):**
- `ipc_recv_shared_v3_nonblocking()` — non-blocking, no map_intent
- `RecvSharedV3Delivery` with `has_transfer_cap()`, `status()` accessors
- `STATUS_SENTINEL_UNWRITTEN = 0xFF_FF_FF_FF` for aarch64/riscv64 disambiguation
- 13 unit tests covering NR constant, encoding, decoding, struct sizes, sentinel distinctness

**Run commands:**
- `cargo test --lib stage44 -- --test-threads=1` (9 kernel tests)
- `cargo test -p yarm-user-rt --lib recv_v3` (13 user-rt tests)

All tests must pass. Production services not migrated; SYSCALL_COUNT unchanged.


## Rule N+49: Stage 45 — first userspace proof (output metadata + decoder)

Stage 45 proves the recv_shared_v3 output metadata contract end-to-end: the kernel writes
correct authoritative fields to the user-supplied metadata buffer, and the user-rt decoder
(`RecvSharedV3Delivery::from_output()`) correctly reads them.

**(A) Kernel output metadata field proof (kernel `mod stage45`):**
- Plain message dispatch with `metadata_ptr` set: all 8 authoritative fields verified:
  `version`, `record_len`, `abi_version`, `result_status`, `sender_tid`, `message_len`,
  `message_flags`, `transferred_cap` (`stage45_plain_receive_output_metadata_all_fields`)
- Wire-format decode contract: 40-byte output parses to correct delivery fields
  (`stage45_output_wire_bytes_decode_to_delivery_fields`)

**(B) user-rt decoder proof (6 new tests in `recv_v3.rs`):**
- `from_output()` on STATUS_OK, no cap → Some(delivery) with correct fields
- `from_output()` on STATUS_OK with cap → Some(delivery), `has_transfer_cap() == true`
- `from_output()` on STATUS_WOULD_BLOCK → None
- `from_output()` on other error statuses → None
- 80-byte wire format parses correctly to delivery field values
- `from_output()` and manual decode agree on plain message

**(C) Cap-transfer through dispatch() — deferred blocker:**
- Cap-transfer via `dispatch()` requires `stash_transfer_envelope` setup not exposed
  by the boot-level `ipc_send` helper; deferred to a future stage.
- Cap-transfer decoding is proven in user-rt unit tests (tests B above).

**(D) Production invariants:**
- No production service loops migrated to recv_shared_v3.
- `SYSCALL_COUNT == 31` unchanged.
- No new syscall numbers.

**Run commands:**
- `cargo test --lib stage45 -- --test-threads=1` (2 kernel tests)
- `cargo test -p yarm-user-rt --lib recv_v3` (19 user-rt tests total)

All tests must pass.


## Rule N+50: Stage 46 — cap-transfer recv_shared_v3 proof via real handle_ipc_send path

Stage 46 closes the cap-transfer blocker documented in Rule N+49(C): uses
`dispatch(IpcSend)` (which calls `handle_ipc_send` → `stash_transfer_handle` →
`stash_transfer_envelope`) to send a cap-transfer message, then proves
`dispatch(RecvSharedV3)` materializes the cap and writes a non-sentinel
`transferred_cap` to the output metadata buffer.

**(A) Kernel dispatch tests (2 in `mod stage46`):**

- `stage46_cap_transfer_through_real_send_path` — Full chain proof:
  - Sends via `dispatch(IpcSend)` with `arg5 = mem_cap` (inline payload, no user ASID).
  - Asserts `cap_transfer_stage4e_enqueued` incremented by 1 (proves `stash_transfer_envelope` was called).
  - Sets up user ASID + mapped page at VA 0x1_0000.
  - Receives via `dispatch(RecvSharedV3)`.
  - Asserts: `result_status == OK`, `transferred_cap != u64::MAX`, `message_flags & FLAG_CAP_TRANSFER != 0`.
  - Asserts `frame.ret2() == transferred_cap` (register and metadata agree).
  - Resolves materialized cap: must be `CapObject::MemoryObject`.

- `stage46_direct_enqueue_phony_cap_transfer_fails_materialization` — Negative / blocker documentation:
  - Uses boot-level `state.ipc_send()` with a phony `FLAG_CAP_TRANSFER` handle (not stashed).
  - `recv_shared_v3` dispatch must return `Err(SyscallError::InvalidCapability)` because
    `take_transfer_envelope` returns `None` for the un-stashed handle.
  - Proves `stash_transfer_envelope` is required for cap transfer to succeed.

**(B) user-rt test (`crates/yarm-user-rt/src/syscall/recv_v3.rs`):**

- `from_output_stage46_cap_transfer_output_roundtrip` — Proves `from_output()` decodes
  a kernel-style output with `transferred_cap = 42` (non-sentinel) into
  `RecvSharedV3Delivery { has_transfer_cap() == true, transferred_cap == Some(42) }`.

**(C) Cap-transfer object identity proof — deferred:**
- `object_kind`, `object_generation`, `effective_rights`, `exact_object_size` remain 0
  (FUTURE fields; object introspection not yet implemented).
- Existing ipc_recv / recv-v2 cap-transfer behavior unchanged (covered by prior tests).
- No production services migrated. `SYSCALL_COUNT == 31` unchanged.

**Run commands:**
- `cargo test --lib stage46 -- --test-threads=1` (2 kernel tests)
- `cargo test -p yarm-user-rt --lib recv_v3` (27 user-rt tests total)
- `cargo test --features hosted-dev --lib -- --test-threads=1` (1128 total)

All tests must pass.

## Rule N+51: Stage 47+48 — object metadata for transferred caps in recv_shared_v3

Stage 47+48 implements the three object-introspection fields in the
`RecvSharedV3Output` buffer that were previously FUTURE/zero:
`object_kind`, `object_generation`, and `effective_rights`. `exact_object_size`
remains 0 (FUTURE; Stage 49).

**ABI constraints (frozen):**
- `object_kind` @40 (u32): `RecvSharedV3ObjectKind` discriminant (0=Unknown, 1=MemoryObject, 2=Endpoint, 3=ReplyCap, 4=Notification, 0xFF=Other).
- C-layout padding @44-47: always zero.
- `object_generation` @48 (u64): generation from Endpoint/Notification/Reply caps; 0 for MemoryObject (no generation field).
- `effective_rights` @56 (u32): `CapRights::bits() as u32` on the receiver-local materialized cap; 0 if no cap.
- C-layout padding @60-63: always zero.
- `exact_object_size` @64 (u64): always 0 (FUTURE).

**Kernel implementation (`src/kernel/syscall.rs`):**
- `write_v3_output_to_user` extended with `object_kind: u32`, `object_generation: u64`, `effective_rights: u32` params; fills bytes [40..60].
- `recv_v3_object_kind(obj: CapObject) -> u32` maps variant to discriminant.
- `recv_v3_object_generation(obj: CapObject) -> u64` returns generation or 0.
- WouldBlock call site passes `0, 0, 0` for the three new params.
- OK/Delivered call site computes metadata via `kernel.capability_service().resolve_current_task_capability(CapId(cap_id_raw))` on the materialized cap.

**(A) Kernel dispatch tests (2 in `mod stage47`):**

- `stage47_object_kind_and_rights_for_transferred_memory_object` — Full-path proof:
  - Sends a MemoryObject cap via `dispatch(IpcSend)`.
  - Receives via `dispatch(RecvSharedV3)`.
  - Reads the full 80-byte output and asserts:
    - `object_kind @40 == 1` (MemoryObject)
    - `padding @44 == 0` (C-layout gap)
    - `object_generation @48 == 0` (MemoryObject has no generation)
    - `effective_rights @56 == 0x07` (READ|WRITE|MAP for anonymous MemoryObject)
    - `padding @60 == 0` (C-layout gap)
    - `exact_object_size @64 == 0` (FUTURE)

- `stage47_plain_message_object_metadata_is_zero` — Proves that when no cap is
  transferred all object introspection fields are zero in the output buffer.

**(B) user-rt tests (`crates/yarm-user-rt/src/syscall/recv_v3.rs`, 5 new tests):**

- `object_kind_accessor_returns_field` — accessor passes through the field.
- `object_generation_accessor_returns_field` — accessor passes through the field.
- `effective_rights_accessor_returns_field` — accessor passes through the field.
- `from_output_decodes_object_kind_memory_object` — `from_output()` reads `object_kind=1, object_generation=0, effective_rights=0x07` from the output struct.
- `from_output_object_metadata_zero_when_no_cap` — `from_output()` decodes zeros for all object fields when `transferred_cap == NO_TRANSFER_CAP`.
- `from_output_endpoint_kind_and_generation` — decodes `object_kind=2` (Endpoint), non-zero generation, and `effective_rights=0x08` (SEND).

**(C) ABI crate tests (`crates/yarm-ipc-abi/src/recv_shared_v3_abi.rs`, 4 new tests):**
- `abi_cap_rights_constants_match_cap_rights_bits` — `RECV_V3_CAP_RIGHTS_*` constants match `CapRights::bits()` values.
- `abi_object_kind_values_are_stable` — enum discriminants are frozen.
- `abi_object_kind_anonymous_memory_object_is_one` — `MemoryObject == 1`.
- `abi_effective_rights_read_write_map_combo` — `READ | WRITE | MAP == 0x07`.

**Run commands:**
- `cargo test --lib stage47 -- --test-threads=1` (2 kernel tests; exact_object_size assertion updated to PAGE_SIZE in Stage 49)
- `cargo test -p yarm-user-rt --lib recv_v3` (33 user-rt tests total)
- `cargo test -p yarm-ipc-abi --lib recv_shared_v3` (ABI crate tests)
- `cargo test --features hosted-dev --lib -- --test-threads=1` (all kernel tests)

All tests must pass.

## Rule N+52: Stage 49 — exact_object_size for MemoryObject transfers in recv_shared_v3

Stage 49 fills `exact_object_size @64` in the `RecvSharedV3Output` buffer with the
page-aligned byte length of a transferred MemoryObject. All other cap kinds and plain
messages receive 0.

**Authoritative source:**
`MemoryObject.len: usize` from `MemorySubsystem.memory_objects` (kernel registry).
Accessed via `kernel.with_memory_state(|m| m.memory_objects.iter().flatten().find(...).map(|e| e.len as u64))`.

**ABI semantics (frozen):**
- `exact_object_size > 0` and `PAGE_SIZE`-aligned when `object_kind == MemoryObject`.
- 0 for all other cap kinds (not fabricated — genuinely unavailable).
- 0 for plain messages (no cap transferred).
- `exact_region_len` (DmaRegion sub-range) remains 0 (FUTURE).

**Kernel changes:**
- `recv_v3_exact_object_size(kernel: &KernelState, obj: CapObject) -> u64` helper.
- `write_v3_output_to_user` gains `exact_object_size: u64` param, writes `out[64..72]`.
- `handle_recv_shared_v3` OK path refactored: capability resolved first (borrow released),
  then `recv_v3_exact_object_size` called separately.
- Stage 47 test updated: `exact_object_size @64 == PAGE_SIZE` (was 0/FUTURE).

**(A) Kernel dispatch tests (2 in `mod stage49`):**

- `stage49_exact_object_size_for_transferred_memory_object` — proves `exact_object_size == PAGE_SIZE`
  for a 1-page anonymous MemoryObject, and `region_offset == 0` (still FUTURE).
- `stage49_plain_message_exact_object_size_is_zero` — proves `exact_object_size == 0` when no cap.

**(B) user-rt tests (6 new in `recv_v3.rs`):**
- `exact_object_size_accessor_returns_field`
- `has_exact_object_size_true_when_nonzero`
- `has_exact_object_size_false_when_zero`
- `from_output_decodes_exact_object_size_for_memory_object`
- `from_output_exact_object_size_zero_for_no_cap`
- `from_output_exact_object_size_zero_for_endpoint`

**(C) ABI crate tests (2 new):**
- `abi_exact_object_size_zero_when_no_cap`
- `abi_exact_object_size_field_at_offset_64`

**Run commands:**
- `cargo test --lib stage49 -- --test-threads=1` (2 kernel tests)
- `cargo test -p yarm-user-rt --lib recv_v3` (39 user-rt tests total)
- `cargo test -p yarm-ipc-abi --lib recv_shared_v3` (22 ABI crate tests)
- `cargo test --features hosted-dev --lib -- --test-threads=1` (1132 total)

All tests must pass.

## Rule N+53: Stage 50+51 — exact_region_len for DmaRegion transfers + map_intent audit

**Summary:**  `exact_region_len @80..88` is filled with the authoritative DmaRegion
sub-region byte length when the caller provides at least 88 bytes for `metadata_len`.
`DmaRegion.len` is embedded in `CapObject::DmaRegion { id, offset, len }` — no registry
lookup is needed.  `map_intent` remains gated and always returns `InvalidArgs`.

**Constraints (verbatim):**
- `SYSCALL_COUNT == 31` unchanged.
- Syscall numbers unchanged.
- Public ABI field offsets unchanged; only `exact_region_len @80..88` newly written.
- `map_intent != 0 → InvalidArgs` gate unchanged.
- No VM mapping performed.
- No fabricated region length.

**Kernel changes (`src/kernel/syscall.rs`):**
- `recv_v3_exact_region_len(obj: CapObject) -> u64` helper: DmaRegion → len, else 0.
- `write_v3_output_to_user` gains `exact_region_len: u64` param; buffer extended to 88 bytes;
  writes `min(out_len, 88)` bytes so existing 80-byte callers are unaffected.
- OK-path metadata tuple extended to 5 elements: `(obj_kind, obj_gen, eff_rights, exact_obj_size, exact_reg_len)`.

**ABI crate changes (`crates/yarm-ipc-abi/src/recv_shared_v3_abi.rs`):**
- `RECV_V3_EXTENDED_OUTPUT_LEN: u32 = 88` constant added.
- `exact_region_len` field doc updated (authoritative for DmaRegion).
- Layout assertion: `offset_of!(RecvSharedV3Output, exact_region_len) == 80`.

**user-rt changes (`crates/yarm-user-rt/src/syscall/recv_v3.rs`):**
- `exact_region_len: u64` field added to `RecvSharedV3Delivery`.
- `exact_region_len()` and `has_exact_region_len()` accessors added.
- `from_output()` and `ipc_recv_shared_v3_nonblocking` inline decode updated.
- `from_output_and_manual_decode_agree_on_plain_message` updated.

**(A) Kernel dispatch tests (5 in `mod stage50`):**

- `stage50_exact_region_len_for_dma_region_transfer` — proves `exact_region_len == PAGE_SIZE`
  for a 1-page DmaRegion (via `mint_dma_region_cap`), and `object_kind == 0xFF` (Other).
- `stage50_memory_object_transfer_exact_region_len_is_zero` — MemoryObject has `exact_object_size`;
  `exact_region_len` must be 0.
- `stage50_plain_message_exact_region_len_is_zero` — no cap → 0.
- `stage50_map_intent_read_only_remains_invalid_args` — map_intent=RECV_V3_MAP_READ → InvalidArgs.
- `stage50_map_intent_read_write_remains_invalid_args` — map_intent=READ|WRITE → InvalidArgs.

**(B) user-rt tests (6 new in `recv_v3.rs`):**
- `exact_region_len_accessor_returns_field`
- `has_exact_region_len_true_when_nonzero`
- `has_exact_region_len_false_when_zero`
- `from_output_decodes_exact_region_len_for_dma_region`
- `from_output_exact_region_len_zero_for_no_cap`
- `from_output_exact_region_len_zero_for_memory_object`

**(C) ABI crate tests (3 new):**
- `abi_exact_region_len_zero_when_no_cap`
- `abi_exact_region_len_field_at_offset_80`
- `abi_extended_output_len_is_88`

**Run commands:**
- `cargo test --lib stage50 -- --test-threads=1` (5 kernel tests)
- `cargo test -p yarm-user-rt --lib recv_v3` (45 user-rt tests total)
- `cargo test -p yarm-ipc-abi --lib recv_shared_v3` (25 ABI crate tests)
- `cargo test --features hosted-dev --lib -- --test-threads=1` (1137 total)

All tests must pass.

## Rule N+54: Stage 52+53 — DmaRegion first-class object kind + cleanup-token scaffold

**Summary:**  `CapObject::DmaRegion` is now dispatched to `RecvSharedV3ObjectKind::DmaRegion`
(discriminant 5) instead of falling through to `Other (0xFF)`.  A helper-only
`RecvSharedV3CleanupIdentity` scaffold struct is added to `yarm-ipc-abi` with no live
allocation; `cleanup_token @112` is never written (beyond the 88-byte kernel write window).
`map_intent` gate unchanged.  `SYSCALL_COUNT == 31` unchanged.

**Constraints (verbatim):**
- `SYSCALL_COUNT == 31` unchanged.
- Syscall numbers unchanged.
- Public ABI field offsets unchanged.
- `map_intent != 0 → InvalidArgs` gate unchanged.
- No live cleanup token allocation.
- No Drop-based cleanup, no release syscall behavior.
- `cleanup_token @112` never written by kernel.

**Kernel changes (`src/kernel/syscall.rs`):**
- `recv_v3_object_kind`: added `CapObject::DmaRegion { .. } => 5` arm before catch-all.
- Stage50 test updated: `assert_eq!(object_kind, 5, ...)` (was `0xFF`).

**ABI crate changes (`crates/yarm-ipc-abi/src/recv_shared_v3_abi.rs`):**
- `RecvSharedV3ObjectKind::DmaRegion = 5` variant added.
- `RECV_V3_CLEANUP_TOKEN_NONE: u64 = 0` constant added.
- `RecvSharedV3CleanupIdentity` scaffold struct added (helper-only, never kernel-allocated).

**user-rt changes (`crates/yarm-user-rt/src/syscall/recv_v3.rs`):**
- `cleanup_token: u64` field added to `RecvSharedV3Delivery` (decoded but always 0).
- `is_dma_region()`, `cleanup_token()`, `has_cleanup_token()` accessors added.
- `from_output()` updated to decode `cleanup_token` from output struct.

**(A) Kernel dispatch tests (5 in `mod stage52`, `src/kernel/boot/tests.rs`):**

- `stage52_dma_region_object_kind_is_five` — DmaRegion cap transfer → object_kind @40 == 5.
- `stage52_dma_region_full_metadata_output` — all metadata fields for DmaRegion are correct.
- `stage52_memory_object_still_kind_one_with_exact_object_size` — MemoryObject unaffected.
- `stage52_recv_writes_exactly_88_bytes_for_dma_region` — write window is exactly 88 bytes.
- `stage52_recv_writes_exactly_88_bytes_for_plain_message` — plain message: same boundary.

**(B) user-rt tests (7 new, `crates/yarm-user-rt/src/syscall/recv_v3.rs`):**
- `is_dma_region_true_for_kind_five`
- `is_dma_region_false_for_memory_object`
- `is_dma_region_false_when_no_cap`
- `from_output_dma_region_kind_five_decoded`
- `cleanup_token_accessor_returns_zero_always`
- `from_output_cleanup_token_zero_for_dma_region`
- `from_output_and_manual_decode_agree_on_dma_region`

**(C) ABI crate tests (10 new, `crates/yarm-ipc-abi/src/recv_shared_v3_abi.rs`):**
- `abi_dma_region_kind_discriminant_is_five`
- `abi_object_kind_from_raw_dma_region`
- `abi_object_kind_values_include_dma_region`
- `abi_cleanup_token_none_sentinel_is_zero`
- `abi_cleanup_token_zero_in_zeroed_output`
- `abi_cleanup_identity_none_is_not_active`
- `abi_cleanup_identity_none_is_not_structurally_valid`
- `abi_cleanup_identity_structurally_valid_requires_page_aligned_len`
- `abi_cleanup_identity_requires_non_sentinel_cap`
- `abi_cleanup_token_field_at_offset_112`

**Run commands:**
- `cargo test --lib stage52 -- --test-threads=1` (5 kernel tests)
- `cargo test -p yarm-user-rt --lib recv_v3` (52 user-rt tests total)
- `cargo test -p yarm-ipc-abi --lib recv_shared_v3` (35 ABI crate tests total)
- `cargo test --features hosted-dev --lib -- --test-threads=1` (1142 total)

All tests must pass.

---

## Rule N+55: Stage 54+55 — recv_shared_v3 map_intent/shared mapping audit + Option B helper

**Constraints (verbatim):**
- `SYSCALL_COUNT == 31` unchanged.
- Syscall numbers unchanged.
- Public ABI field offsets unchanged.
- `map_intent != 0 → InvalidArgs` gate unchanged.
- No live mapping (Option C not implemented).
- No VM mutation in `compute_recv_v3_mapping_plan`.
- `mapped_base @88`, `page_rounded_mapped_len @96`, `actual_mapping_perm @104` never written by kernel.

**Kernel changes (`src/kernel/recv_core.rs`, `mod recv_shared_v3`):**
- `OPCODE_SHARED_MEM_VALUE: u16 = 1` — const matching `OPCODE_SHARED_MEM`.
- `CAP_RIGHT_WRITE: u8 = 0x02`, `CAP_RIGHT_MAP: u8 = 0x04` — cap rights masks.
- `RecvV3MappingPlan` enum (Skip / Map / InsufficientRights / InvalidRegion).
- `compute_recv_v3_mapping_plan()` — pure planning function, no VM mutation.

**ABI crate changes (`crates/yarm-ipc-abi/src/recv_shared_v3_abi.rs`):**
- `RECV_V3_MAPPED_OUTPUT_LEN: u32 = 108` constant added.

**user-rt changes (`crates/yarm-user-rt/src/syscall/recv_v3.rs`):**
- `mapped_base: u64`, `page_rounded_mapped_len: u64`, `actual_mapping_perm: u32` fields
  added to `RecvSharedV3Delivery`.
- `mapped_base()`, `page_rounded_mapped_len()`, `actual_mapping_perm()`, `has_mapping()`
  accessors added.
- `from_output()` updated to decode mapping fields.

**(A) Kernel dispatch tests (14 in `mod stage54`, `src/kernel/boot/tests.rs`):**

- `stage54_map_intent_read_only_still_invalid_args`
- `stage54_map_intent_read_write_still_invalid_args`
- `stage54_syscall_count_is_31_and_nr_is_30`
- `stage54_plain_receive_unchanged`
- `stage54_mapping_plan_skip_when_map_intent_zero`
- `stage54_mapping_plan_skip_when_opcode_not_shared_mem`
- `stage54_mapping_plan_read_only_for_map_read_intent`
- `stage54_mapping_plan_read_write_for_map_readwrite_intent`
- `stage54_mapping_plan_region_len_rounds_up_to_page`
- `stage54_mapping_plan_insufficient_rights_when_map_bit_missing`
- `stage54_mapping_plan_insufficient_rights_when_write_requested_but_cap_read_only`
- `stage54_mapping_plan_invalid_region_when_payload_ptr_zero`
- `stage54_mapping_plan_invalid_region_when_region_len_zero`
- `stage54_mapping_plan_invalid_region_when_payload_buf_too_small`

**(B) user-rt tests (7 new, total 59, `crates/yarm-user-rt/src/syscall/recv_v3.rs`):**
- `mapped_base_accessor_returns_field`
- `has_mapping_true_when_mapped_base_nonzero`
- `has_mapping_false_when_mapped_base_zero`
- `page_rounded_mapped_len_accessor_returns_field`
- `actual_mapping_perm_accessor_returns_field`
- `from_output_decodes_mapping_fields`
- `from_output_mapping_fields_zero_when_output_zeroed`

**(C) ABI crate tests (5 new, total 40, `crates/yarm-ipc-abi/src/recv_shared_v3_abi.rs`):**
- `abi_mapped_output_len_is_108`
- `abi_mapped_base_at_offset_88`
- `abi_page_rounded_mapped_len_at_offset_96`
- `abi_actual_mapping_perm_at_offset_104`
- `abi_mapped_output_len_covers_actual_mapping_perm`

**Run commands:**
- `cargo test --lib stage54 -- --test-threads=1` (14 kernel tests)
- `cargo test -p yarm-user-rt --lib recv_v3` (59 user-rt tests total)
- `cargo test -p yarm-ipc-abi --lib recv_shared_v3` (40 ABI crate tests total)
- `cargo test --features hosted-dev --lib -- --test-threads=1` (1156 total)

All tests must pass.

---

## Rule N+56: Stage 56+57 — cleanup-token lifecycle design + helper-only registry

**Constraints (verbatim):**
- `SYSCALL_COUNT == 31` unchanged.
- Syscall numbers unchanged.
- Public ABI field offsets unchanged.
- `map_intent != 0 → InvalidArgs` gate unchanged.
- No live mapping, no VM mutation.
- No live syscall uses `RecvV3CleanupRegistry`.
- `cleanup_token @112` never written by kernel.
- No Drop-based cleanup in user-rt.
- No release syscall API.

**Kernel changes (`src/kernel/recv_core.rs`, `mod recv_shared_v3`):**
- `RECV_V3_CLEANUP_REGISTRY_CAPACITY: usize = 16`.
- `RecvV3CleanupToken` — opaque `u64`; encoding `(slot+1) | (gen << 16)`; NONE = 0.
- `RecvV3CleanupIdentity` — 10-field kernel-internal identity; `zeroed()`, `is_mapped()`.
- `RecvV3CleanupReleaseResult` — Released / AlreadyReleased / InvalidToken / StaleGeneration.
- `RecvV3CleanupRegistry` — fixed-capacity no-heap registry; `new()`, `allocate()`,
  `release()`, `lookup()`, `count_occupied()`.

**ABI crate changes (`crates/yarm-ipc-abi/src/recv_shared_v3_abi.rs`):**
- `RecvSharedV3CleanupIdentity` expanded with `mapped_base`, `mapped_len`,
  `actual_mapping_perm`, `map_intent`.
- `none()` updated to zero all new fields.
- `is_mapped()` method added.

**user-rt changes:** none.

**(A) Kernel tests — 14 in `mod stage56`, `src/kernel/boot/tests.rs`:**
- `stage56_token_none_is_invalid`
- `stage56_allocate_gives_nonzero_token`
- `stage56_token_encodes_slot_and_generation`
- `stage56_release_valid_token_gives_released`
- `stage56_duplicate_release_gives_already_released`
- `stage56_stale_token_after_realloc_gives_stale_generation`
- `stage56_none_token_gives_invalid_token`
- `stage56_registry_full_returns_none`
- `stage56_fill_release_refill`
- `stage56_lookup_returns_correct_identity`
- `stage56_lookup_after_release_returns_none`
- `stage56_two_slots_are_independent`
- `stage56_integration_map_plan_to_identity_and_token`
- `stage56_rw_plan_to_identity_is_not_read_only`

**(B) Kernel tests — 8 in `mod stage57`, `src/kernel/boot/tests.rs`:**
- `stage57_syscall_count_still_31_and_nr_still_30`
- `stage57_map_intent_read_only_gate_still_disabled`
- `stage57_map_intent_read_write_gate_still_disabled`
- `stage57_plain_receive_write_window_is_88_no_mapping_fields`
- `stage57_memory_object_transfer_no_mapping_output`
- `stage57_dma_region_transfer_no_mapping_output`
- `stage57_cleanup_token_none_matches_abi_sentinel`
- `stage57_new_registry_has_zero_occupied_slots`

**(C) ABI crate tests — 5 new, total 190, `crates/yarm-ipc-abi/src/recv_shared_v3_abi.rs`:**
- `abi_cleanup_identity_none_has_all_new_fields_zero`
- `abi_cleanup_identity_is_mapped_false_in_none`
- `abi_cleanup_identity_is_mapped_requires_both_base_and_len`
- `abi_cleanup_identity_is_active_and_is_mapped_are_independent`
- `abi_cleanup_identity_full_round_trip`

**Run commands:**
- `cargo test --lib stage56 -- --test-threads=1` (14 kernel tests)
- `cargo test --lib stage57 -- --test-threads=1` (8 kernel tests)
- `cargo test -p yarm-ipc-abi --lib -- --test-threads=1` (190 ABI tests total)
- `cargo test --lib ipc_recv -- --test-threads=1` (regression)
- `cargo test --lib recv_v2 -- --test-threads=1` (regression)
- `cargo test --features hosted-dev --lib -- --test-threads=1` (1178 total)

All tests must pass.

## Rule N+57: Stage 58+59 — recv_shared_v3 live map_intent + DmaRegion RO mapping

### Scope

Stage 58+59 enables live `map_intent` mapping in `recv_shared_v3` (NR 30).
DmaRegion read-only is the primary candidate.  SYSCALL_COUNT=31 is unchanged.

### Hard invariants

1. SYSCALL_COUNT == 31; NR 30 == RecvSharedV3.
2. `map_intent != 0` with `metadata_len < 120` returns `InvalidArgs`.
3. Mapping is only performed for `OPCODE_SHARED_MEM` messages carrying a
   `DmaRegion` or `MemoryObject` cap; all other messages with `map_intent != 0`
   return `InvalidArgs`.
4. `cleanup_token` is always written when a live mapping succeeds; it equals the
   receiver-local cap ID (opaque token).
5. `execute_user_asid_plain_writeback` is skipped when mapping succeeds
   (`skip_payload = true`); `frame.ret1` (payload_len_copied) is 0.
6. `register_active_transfer_mapping` is called after all pages are mapped;
   the count is observable via `active_transfer_count_for_pid`.
7. VFS_SHARED_IO remains disabled.
8. Plain receives (no `map_intent`) and DmaRegion transfers without `map_intent`
   behave identically to Stage 56+57.
9. Legacy `ipc_recv` (NR 2) is unaffected.

### Test-ordering constraint

`IpcSend` calls in stage58/stage59 tests must happen **before** any
`setup_receiver`/`setup_recv_asid` call that binds a user ASID to task 0.
After ASID binding, `IpcSend` from task 0 takes the user-ASID path and
calls `copy_from_current_user(VA=0, len)`, which faults silently (message
not queued, no `Err` returned).  The send must use the kernel-task path.

### Tests

**(A) Kernel tests — 12 in `mod stage58`, `src/kernel/boot/tests.rs`:**
- `stage58_map_intent_requires_metadata_len_120`
- `stage58_map_intent_read_write_also_requires_metadata_len_120`
- `stage58_dma_region_ro_mapping_result_status_ok`
- `stage58_dma_region_ro_mapped_base_equals_payload_ptr`
- `stage58_dma_region_ro_mapped_len_equals_page_size`
- `stage58_dma_region_ro_actual_perm_is_1`
- `stage58_dma_region_ro_cleanup_token_nonzero`
- `stage58_dma_region_ro_cleanup_token_equals_transferred_cap`
- `stage58_active_transfer_count_increments_after_mapping`
- `stage58_mapping_skips_payload_copy_frame_payload_len_is_zero`
- `stage58_map_intent_without_cap_message_rejected`
- `stage58_map_intent_with_non_shared_mem_message_rejected`

**(B) Kernel tests — 6 in `mod stage59`, `src/kernel/boot/tests.rs`:**
- `stage59_syscall_count_still_31_and_nr_still_30`
- `stage59_plain_receive_write_window_still_88_bytes`
- `stage59_dma_region_transfer_without_map_intent_unchanged`
- `stage59_map_intent_small_buffer_still_invalid_args`
- `stage59_vfs_shared_io_disabled`
- `stage59_legacy_ipc_recv_unaffected_by_mapping`

**Run commands:**
- `cargo test --lib stage58 -- --test-threads=1` (12 kernel tests)
- `cargo test --lib stage59 -- --test-threads=1` (6 kernel tests)
- `cargo test --lib stage56 -- --test-threads=1` (14 regression)
- `cargo test --lib ipc_recv -- --test-threads=1` (regression)
- `cargo test --lib recv_v2 -- --test-threads=1` (regression)
- `cargo test -p yarm-ipc-abi --lib -- recv_shared_v3` (ABI regression)
- `cargo test -p yarm-user-rt --lib -- recv_v3` (user-rt regression)

All tests must pass.

---

## Rule N+58: Stage 60 — recv_shared_v3 cleanup-token hardening + rollback

**Purpose:** Verify cleanup-token generation safety, duplicate-release rejection,
stale-token rejection, writeback-failure rollback, and RW gate.

**Hard invariants:**
- SYSCALL_COUNT = 31, SYSCALL_RECV_SHARED_V3_NR = 30, SYSCALL_TRANSFER_RELEASE_NR = 4.
- `write_v3_output_to_user` returns `bool`; `false` → rollback in skip_payload branch.
- `cleanup_token = CapId.0` encodes generation in bits[63:16]; stale tokens auto-rejected.
- `map_intent & WRITE != 0` → `InvalidArgs` before mapping code runs.
- All send operations must occur before ASID binding (test-ordering constraint from Rule N+57).

**Test list (mod stage60, 10 tests):**

- `stage60_cleanup_token_encodes_generation`
- `stage60_duplicate_release_rejected`
- `stage60_stale_token_release_rejected`
- `stage60_output_writeback_fail_rolls_back_mapping`
- `stage60_transfer_release_removes_active_mapping`
- `stage60_map_intent_rw_rejected`
- `stage60_map_intent_write_only_rejected`
- `stage60_syscall_count_still_31`
- `stage60_vfs_shared_io_disabled`
- `stage60_legacy_ipc_recv_unaffected`

**Run commands:**
- `cargo test --features hosted-dev "stage60::" -- --test-threads=4` (10 tests)
- `cargo test --features hosted-dev "stage58::" -- --test-threads=4` (12 regression)
- `cargo test --features hosted-dev "stage59::" -- --test-threads=4` (6 regression)
- `cargo test --features hosted-dev ipc_recv -- --test-threads=4` (regression)
- `cargo test --features hosted-dev recv_v2 -- --test-threads=4` (regression)

All tests must pass.

---

### Rule N+59 — Stage 61+62: recv_shared_v3 read-only mapped receive proof

**Coverage:** 14 kernel dispatch proof tests in `mod stage61_62`; 17 user-rt tests
in `yarm-user-rt` (14 in `recv_v3` + 3 in `shared_transfer`).

**Kernel tests** (`src/kernel/boot/tests.rs`, `mod stage61_62`):
- `stage61_kernel_dispatch_map_intent_one_populates_mapped_base`
- `stage61_kernel_dispatch_map_intent_one_populates_mapped_len`
- `stage61_kernel_dispatch_map_intent_one_actual_perm_read_only`
- `stage61_kernel_dispatch_map_intent_one_cleanup_token_nonzero`
- `stage61_kernel_dispatch_map_intent_one_result_status_ok`
- `stage61_kernel_dispatch_map_intent_one_registers_active_mapping`
- `stage61_v3_output_struct_size_is_128`
- `stage61_v3_output_parses_via_abi_struct`
- `stage61_cleanup_token_generation_in_bits_63_16`
- `stage62_release_via_cleanup_token_removes_active_mapping`
- `stage62_duplicate_release_rejected_via_v3_path`
- `stage61_syscall_count_still_31`
- `stage61_vfs_shared_io_disabled`
- `stage61_legacy_ipc_recv_unaffected`

**Run commands:**
- `cargo test --features hosted-dev stage61 --lib` (14 tests)
- `cargo test --features hosted-dev stage62 --lib` (regression subset)
- `cargo test -p yarm-user-rt --lib` (109 tests, includes stage61/62 user-rt)
- `cargo test --features hosted-dev stage60 --lib` (10 regression)

All tests must pass.

---

### Rule N+60 — Stage 63+64: plain-receive adoption proof and VFS readiness

**Coverage:** 8 kernel dispatch proof tests in `mod stage63`; 4 user-rt adoption tests in
`yarm-user-rt`.

**Kernel tests** (`src/kernel/boot/tests.rs`, `mod stage63`):
- `stage63_plain_recv_result_status_ok`
- `stage63_plain_recv_transferred_cap_present`
- `stage63_plain_recv_mapped_base_zero`
- `stage63_plain_recv_cleanup_token_zero`
- `stage63_plain_recv_no_active_mapping_registered`
- `stage63_plain_recv_from_output_via_abi_struct`
- `stage63_syscall_count_still_31`
- `stage63_vfs_shared_io_disabled`

**User-rt adoption tests** (`crates/yarm-user-rt/src/syscall/recv_v3.rs`):
- `stage63_adoption_encode_plain_recv_map_intent_zero`
- `stage63_adoption_from_output_plain_recv_no_mapping`
- `stage63_adoption_mapped_recv_delivery_differs_from_plain`
- `stage63_adoption_encode_mapped_vs_plain_map_intent_differ`

**VFS readiness:** Documented in `doc/VFS_SHARED_IO_CONTRACT.md` §Stage 64.
No new Rust code in `yarm-fs-servers` (existing 207 tests unchanged).

**Run commands:**
- `cargo test --features hosted-dev "stage63::" --lib` (8 tests)
- `cargo test -p yarm-user-rt --lib stage63` (4 tests)
- `cargo test -p yarm-fs-servers --lib` (207 regression)
- `cargo test --features hosted-dev "stage61_62::" --lib` (14 regression)
- `cargo test --features hosted-dev "stage60::" --lib` (10 regression)

All tests must pass.

---

### Rule N+61 — Stage 65: VfsWriteSharedBinding contract and WRITE_SHARED_REQUEST helper bridge

**Crate:** `yarm-fs-servers`
**File:** `crates/yarm-fs-servers/src/fs/common/shared_io_adapter.rs`

**Coverage:** 21 new `stage65_*` tests proving the full `VfsWriteSharedBinding` lifecycle.

**Rejection tests (one per error variant):**
- `stage65_missing_cleanup_token_rejected` — `MissingCleanupToken`
- `stage65_missing_transfer_cap_rejected` — `NoTransferCap`
- `stage65_non_readonly_mapping_rejected` — `MappingNotReadOnly`
- `stage65_zero_mapped_base_rejected` — `MappingNotEstablished`
- `stage65_non_dma_object_kind_rejected` — `UnsupportedObjectKind`
- `stage65_wrong_access_flag_rejected` — `WrongDescriptorAccess`
- `stage65_descriptor_handle_mismatch_rejected` — `DescriptorHandleMismatch`
- `stage65_descriptor_generation_mismatch_rejected` — `DescriptorGenerationMismatch`
- `stage65_mapping_range_too_short_rejected` — `MappingRangeTooShort`
- `stage65_zero_exact_region_len_rejected` — `ExactRegionLenInsufficient`
- `stage65_zero_request_id_rejected` — `ZeroRequestId`

**Acceptance and lifecycle tests:**
- `stage65_valid_write_shared_binding_accepted` — all 11 checks pass, fields preserved
- `stage65_cleanup_token_parts_decompose_correctly` — `cleanup_token_parts()` decomposes correctly
- `stage65_ramfs_consumes_immutable_bytes_via_binding_and_mapper` — full RAMFS write roundtrip
- `stage65_mapper_rejects_write_access_to_write_request_buffer` — direction safety enforced
- `stage65_cleanup_idempotent_after_success` — lifecycle cleanup is idempotent
- `stage65_cleanup_before_fallback_required_for_write_request` — fallback requires prior cleanup
- `stage65_production_mapper_rejects_write_shared_request` — `UnsupportedSharedIoMapper` rejects
- `stage65_read_shared_reply_still_unsupported_by_production_mapper` — READ_SHARED_REPLY blocked
- `stage65_vfs_shared_io_enabled_remains_disabled` — VFS_SHARED_IO_ENABLED still false
- `stage65_descriptor_accessor_returns_correct_fields` — `descriptor()` accessor correct

**Binding contract under test:**
- `descriptor.object_handle == cleanup_token` (full u64 CapId)
- `descriptor.object_generation == cleanup_token >> 16` (generation field)
- `actual_mapping_perm == MAP_PERM_READ_ONLY (1)` for WRITE_SHARED_REQUEST
- `object_kind == OBJECT_KIND_DMA_REGION`

**Run commands:**
- `cargo test -p yarm-fs-servers --lib stage65` (21 tests)
- `cargo test -p yarm-fs-servers --lib` (228 total, full regression)
- `cargo test --features hosted-dev "stage63::" --lib` (8 kernel regression)
- `cargo test -p yarm-user-rt --lib stage63` (4 user-rt regression)

All tests must pass. `VFS_SHARED_IO_ENABLED` must remain disabled after this stage.

---

### Rule N+62 — Stage 66+67+68: gated WRITE_SHARED_REQUEST live route in VfsService

**Crate:** `yarm-fs-servers`
**Files:** `crates/yarm-fs-servers/src/fs/common/vfs_service.rs` (17 tests in `mod stage66_68_tests`);
`crates/yarm-fs-servers/src/fs/common/shared_io_adapter.rs` (3 new constants);
`crates/yarm-fs-servers/src/fs/ramfs/tree.rs` (`write_shared_bytes` override);
`crates/yarm-srv-common/src/vfs_core.rs` (`write_shared_bytes` default method).

**Coverage:** gated live dispatch route, RAMFS roundtrip proof, rejection coverage, invariant gates.

**Live route tests (`mod stage66_68_tests`):**
- `stage66_default_dispatch_still_rejects_write_shared_opcode`
- `stage66_gated_dispatch_ramfs_write_shared_succeeds`
- `stage66_gated_dispatch_bytes_written_match_file_contents`
- `stage66_gated_dispatch_cleanup_performed_exactly_once`
- `stage66_gated_dispatch_op_sequence_advances_on_success`
- `stage66_gated_dispatch_missing_cleanup_token_rejected`
- `stage66_gated_dispatch_stale_generation_rejected`
- `stage66_gated_dispatch_wrong_object_handle_rejected`
- `stage66_gated_dispatch_non_readonly_mapping_rejected`
- `stage66_gated_dispatch_range_too_short_rejected`
- `stage66_gated_dispatch_unsupported_production_mapper_rejected`
- `stage66_gated_dispatch_cleanup_called_even_on_failed_write`
- `stage67_read_shared_reply_still_unsupported_by_parse_request`
- `stage68_write_shared_request_gate_disabled_by_default`
- `stage68_read_shared_reply_gate_disabled_by_default`
- `stage68_global_vfs_shared_io_disabled_by_default`
- `stage68_global_gate_false_unless_both_direction_gates_true`

**Gate constants:**
- `VFS_WRITE_SHARED_REQUEST_ENABLED = false` (WRITE direction only)
- `VFS_READ_SHARED_REPLY_ENABLED = false` (blocked by MAP_WRITE)
- `VFS_SHARED_IO_ENABLED = false` (aggregate = WRITE && READ)

**Run commands:**
- `cargo test -p yarm-fs-servers --lib stage66` (12 tests)
- `cargo test -p yarm-fs-servers --lib stage6` (38 stage65+66+67+68 tests)
- `cargo test -p yarm-fs-servers --lib` (245 total, full regression)
- `cargo check -p yarm-fs-servers` (no errors)

All tests must pass. `VFS_SHARED_IO_ENABLED` must remain `false`. `VFS_READ_SHARED_REPLY_ENABLED`
must remain `false`. `handle_request` must continue to reject `VFS_OP_WRITE_SHARED_REQUEST`.

---

## Rule N+63 — Stage 69+70 MAP_WRITE audit + READ_SHARED_REPLY helper/gated path

**Stage:** 69+70
**Crate:** `yarm-fs-servers`
**Files:** `crates/yarm-fs-servers/src/fs/common/vfs_service.rs` (16 tests in `mod stage69_70_tests`);
`crates/yarm-fs-servers/src/fs/common/shared_io_adapter.rs` (`VfsReadSharedBinding` + 12-constraint validate);
`crates/yarm-fs-servers/src/fs/ramfs/tree.rs` (`read_shared_bytes` override);
`crates/yarm-srv-common/src/vfs_core.rs` (`read_shared_bytes` default method).

**Coverage:** MAP_WRITE audit invariant, regression for WRITE_SHARED_REQUEST, gate constant
assertions, RAMFS roundtrip proof, rejection coverage for all 12 binding constraints,
cleanup-on-error invariant, release-exactly-once invariant.

**Tests (`mod stage69_70_tests`):**
- `stage69_audit_map_write_gate_remains_blocking`
- `stage69_write_shared_request_still_works_after_read_shared_added`
- `stage69_read_shared_reply_default_dispatch_still_unsupported`
- `stage69_gate_values_all_false`
- `stage70_read_shared_reply_ramfs_writes_bytes_into_buffer`
- `stage70_read_shared_reply_short_eof_bytes_read_le_requested`
- `stage70_read_shared_reply_wrong_direction_rejected`
- `stage70_read_shared_reply_readonly_mapping_rejected`
- `stage70_read_shared_reply_stale_generation_rejected`
- `stage70_read_shared_reply_range_too_short_rejected`
- `stage70_read_shared_reply_cleanup_called_on_backend_error`
- `stage70_read_shared_reply_unsupported_production_mapper_rejects_safely`
- `stage70_read_shared_reply_op_sequence_advances_on_success`
- `stage70_read_shared_reply_cleanup_exactly_once`
- `stage70_global_vfs_shared_io_still_false`
- `stage70_write_shared_request_still_unsupported_in_handle_request`

**Hard invariants that must never be violated by future changes:**
- `VFS_READ_SHARED_REPLY_ENABLED` must remain `false` until kernel process-exit cleanup is confirmed safe.
- `VFS_SHARED_IO_ENABLED` must remain `false` unless both direction gates are `true`.
- `handle_request` must return `Unsupported` for `VFS_OP_READ_SHARED_REPLY`.
- `dispatch_read_shared_reply` must call `mapper.release(descriptor)` unconditionally.
- `VfsReadSharedBinding::validate` must reject `actual_mapping_perm & 0x2 == 0` with `MappingNotWritable`.
- `VfsReadSharedBinding::validate` must reject `actual_mapping_perm & 0x4 != 0` with `ExecutableMapping`.
- Stage 60 kernel MAP_WRITE gate (`syscall.rs`) must not be removed until process-exit cleanup is confirmed.

**Run commands:**
- `cargo test -p yarm-fs-servers --lib stage69` (4 tests)
- `cargo test -p yarm-fs-servers --lib stage70` (12 tests)
- `cargo test -p yarm-fs-servers --lib` (261 total, full regression)
- `cargo check -p yarm-fs-servers` (no errors)

---

## Rule N+64 — Stage 71 active recv_shared_v3 mapping cleanup audit

**Stage:** 71
**Crate:** `yarm` (kernel)
**Files:** `src/kernel/boot/tests.rs` (9 tests in `mod stage71`).

**Coverage:** process-exit cleanup via `mark_task_dead`, `TransferRelease` after exit purge
returns `InvalidArgs`, idempotent double-purge, explicit release before exit leaves no entry,
writeback rollback regression, `timeout_ticks != 0` creates no mapping (`WouldBlock`),
MAP_WRITE gate regression, syscall-number and VFS feature-gate invariants.

**Tests (`mod stage71`):**
- `stage71_mark_task_dead_cleans_active_recv_v3_mapping`
- `stage71_transfer_release_after_exit_cleanup_returns_invalid_args`
- `stage71_duplicate_process_exit_cleanup_is_idempotent`
- `stage71_explicit_transfer_release_before_exit_leaves_no_active_mapping`
- `stage71_writeback_rollback_removes_active_mapping_regression`
- `stage71_nonzero_timeout_returns_would_block_no_mapping_created`
- `stage71_map_write_still_rejected_after_exit_path_added`
- `stage71_syscall_count_and_nrs_unchanged`
- `stage71_vfs_shared_io_still_disabled`

**Hard invariants that must never be violated by future changes:**
- `purge_active_transfer_mappings_for_pid` must be called from `maybe_cleanup_process_cnode_for_pid`.
- `maybe_cleanup_process_cnode_for_pid` must be called from `mark_task_dead`.
- `unmap_range_two_phase` must be used (not `unmap_user_page_in_asid`) for tolerating absent pages/ASIDs.
- `timeout_ticks != 0` must return `WouldBlock` before any endpoint or mapping work.
- Stage 60 MAP_WRITE gate removed by Stage 72; do not re-add without re-assessing the full policy.
- SYSCALL_COUNT must remain 31; `RecvSharedV3` must remain NR 30; `TransferRelease` must remain NR 4.

**Run commands:**
- `cargo test --lib stage71 -- --test-threads=1` (9 tests)
- `cargo test --lib stage60 -- --test-threads=1` (regression)
- `cargo test --lib ipc_recv -- --test-threads=1` (regression)
- `cargo check --no-default-features` (no errors)
- `cargo check -p yarm-fs-servers` (no errors)

---

### Rule N+65 — Stage 72: narrow recv_shared_v3 MAP_WRITE enablement

**Purpose:** Prove that removing the Stage 60 blanket WRITE gate delivers the correct
narrowly-scoped MAP_WRITE behaviour: perm=3 only for caps with write rights, identical
cleanup/rollback paths, WRITE-only rejected, MAP_READ regression unaffected.

**Module:** `mod stage72` in `src/kernel/boot/tests.rs`

**Tests (9 total):**
- `stage72_map_read_write_delivers_rw_mapping` — actual_perm=3, cleanup_token≠0
- `stage72_map_write_without_write_rights_rejected` — restricted cap (MAP|READ only) → InvalidArgs
- `stage72_rw_mapping_writeback_rollback_cleans_up` — bad metadata_ptr + map_intent=3 → rollback, count=0
- `stage72_transfer_release_removes_rw_mapping` — TransferRelease on RW mapping → count=0
- `stage72_process_exit_cleans_rw_mapping` — mark_task_dead → count=0 (identical path to RO)
- `stage72_timeout_blocked_before_map_write_check` — timeout_ticks=1000 + map_intent=3 → WouldBlock
- `stage72_map_read_only_regression` — map_intent=1 → actual_perm=MAP_PERM_READ_ONLY=1
- `stage72_syscall_count_and_nrs_unchanged` — SYSCALL_COUNT=31, NR30, NR4
- `stage72_vfs_shared_io_still_disabled` — `!cfg!(feature = "vfs-shared-io")`

**Hard invariants:**
- Stage 72 removes only the blanket WRITE gate; all other validation is preserved.
- WRITE-only (map_intent=0x2) must remain invalid (rejected by `validate_v3_request`).
- Rights enforcement must remain in `compute_recv_v3_mapping_plan`, not in syscall.rs.
- `ActiveTransferMapping` must carry no permission field; cleanup path must be perm-agnostic.
- `VFS_READ_SHARED_REPLY_ENABLED` enabled to `true` by Stage 73; `VFS_SHARED_IO_ENABLED` must remain `false`.
- SYSCALL_COUNT=31, NR30, NR4 must not change.

**Run commands:**
- `cargo test --lib stage72 -- --test-threads=1` (9 tests)
- `cargo test --lib stage71 -- --test-threads=1` (regression)
- `cargo test --lib stage60 -- --test-threads=1` (regression)
- `cargo test --lib ipc_recv -- --test-threads=1` (regression)
- `cargo test --lib recv_v2 -- --test-threads=1` (regression)
- `cargo check --no-default-features` (no errors)
- `cargo check -p yarm-fs-servers` (no errors)
- `cargo test -p yarm-fs-servers shared_io --lib` (regression)

---

## Rule N+66 — Stage 73+74: RequesterExit helper model + VFS_READ_SHARED_REPLY_ENABLED

**Purpose:** Prove the VFS-side `deliver_requester_exit` lifecycle invariants and confirm
that `VFS_READ_SHARED_REPLY_ENABLED = true` is safe to enable: dispatch handles RW-perm
buffers, rejects RO-perm, delivers short-EOF, and calls cleanup exactly once.

**Modules:**
- `mod stage73` in `crates/yarm-fs-servers/src/fs/common/shared_io_lifecycle.rs` (7 lifecycle tests)
- `mod stage73_74_tests` in `crates/yarm-fs-servers/src/fs/common/vfs_service.rs` (9 dispatch tests)

**Lifecycle tests (`mod stage73`, 7 total):**
- `stage73_requester_exit_before_completion_wins` — RequesterExit during Active → `Cleaned`
- `stage73_duplicate_requester_exit_is_idempotent` — double exit → `AlreadyCleaned`
- `stage73_success_cleanup_beats_requester_exit` — success cleanup blocks later RequesterExit
- `stage73_backend_error_beats_requester_exit` — error cleanup blocks later RequesterExit
- `stage73_requester_exit_blocks_inline_fallback` — exit prevents `begin_inline_fallback`
- `stage73_requester_exit_from_reserved_state` — Reserved-state exit is safe no-op
- `stage73_handle_generation_advances_after_requester_exit` — generation counter advances

**Dispatch tests (`mod stage73_74_tests`, 9 total):**
- `stage74_vfs_read_shared_reply_enabled` — `VFS_READ_SHARED_REPLY_ENABLED == true`
- `stage74_vfs_shared_io_still_disabled` — `VFS_SHARED_IO_ENABLED == false`
- `stage74_write_shared_request_still_disabled` — `VFS_WRITE_SHARED_REQUEST_ENABLED == false`
- `stage74_handle_request_rejects_read_shared_opcode` — `handle_request` → `Unsupported`
- `stage74_read_shared_reply_with_kernel_rw_perm_delivers_bytes` — perm=3, RW buffer → bytes
- `stage74_read_shared_reply_short_eof` — bytes_read ≤ requested (EOF case)
- `stage74_read_shared_reply_cleanup_exactly_once` — release_count=1 after success
- `stage74_read_shared_reply_readonly_perm_rejected` — perm=1 → `PermissionDenied`
- `stage74_write_shared_request_regression` — `VFS_OP_WRITE_SHARED_REQUEST` still `Unsupported`

**Hard invariants:**
- `deliver_requester_exit` must be a thin wrapper over `cleanup(handles, RequesterExit)`.
- Double RequesterExit must return `AlreadyCleaned` (idempotent — no panic, no double-free).
- `VFS_READ_SHARED_REPLY_ENABLED = true` (Stage 73); do not revert without re-auditing prerequisites.
- `VFS_SHARED_IO_ENABLED = false`; do not enable until live `SUPERVISOR_OP_TASK_EXITED` wiring is proven.
- `VFS_WRITE_SHARED_REQUEST_ENABLED = false`; do not enable without separate stage.
- `handle_request` must continue to reject `VFS_OP_READ_SHARED_REPLY` with `Unsupported`.
- No Drop-based cleanup may be added.
- SYSCALL_COUNT=31, NR30, NR4 must not change.

**Run commands:**
- `cargo test -p yarm-fs-servers --lib stage73 -- --test-threads=1` (7 lifecycle tests)
- `cargo test -p yarm-fs-servers --lib stage73_74 -- --test-threads=1` (9 dispatch tests)
- `cargo test -p yarm-fs-servers --lib -- --test-threads=1` (full 277-test regression)
- `cargo test --lib stage72 -- --test-threads=1` (kernel regression)
- `cargo test --lib stage60 -- --test-threads=1` (kernel regression)
- `cargo check --no-default-features` (no errors)
- `cargo check -p yarm-fs-servers` (no errors)

---

## Rule N+67 — Stage 75: TID-matched RequesterExit identity model

**Purpose:** Prove VFS-side identity model for RequesterExit delivery: `requester_tid` field
in `VfsSharedIoLifecycle`, TID-matched delivery with `deliver_requester_exit_if_tid_matches`,
safe no-op on mismatch, idempotency, and gate checks.  Documents exact missing infrastructure
before `VFS_SUPERVISOR_TASK_EXIT_NOTIFICATION_ENABLED` can be `true`.

**Modules:**
- `mod stage75` in `crates/yarm-fs-servers/src/fs/common/shared_io_lifecycle.rs` (10 tests)
- `mod stage75_tests` in `crates/yarm-fs-servers/src/fs/common/vfs_service.rs` (8 tests)

**Lifecycle tests (`mod stage75`, 10 total):**
- `stage75_lifecycle_requester_tid_stored` — `requester_tid()` returns stored TID
- `stage75_matched_tid_delivers_requester_exit` — TID match → `Matched(Won(RequesterExit))`
- `stage75_unmatched_tid_is_safe_noop` — different TID → `NotMatched`, state unchanged
- `stage75_duplicate_matched_tid_is_idempotent` — second call → `Matched(AlreadyCleaned)`
- `stage75_explicit_cleanup_before_matched_tid_is_noop` — success cleanup before TID match → `Matched(AlreadyCleaned(Success))`
- `stage75_zero_tid_lifecycle_matches_only_zero_tid` — TID=0 only matches TID=0
- `stage75_generation_advances_after_tid_matched_exit` — generation increments after TID match
- `stage75_read_reply_lifecycle_observes_tid_matched_exit` — ReadReply direction cleaned by TID exit
- `stage75_write_request_lifecycle_unaffected_by_unmatched_tid` — WriteRequest not affected by wrong TID
- `stage75_multiple_lifecycles_only_matched_tid_cleaned` — 2 lifecycles, 1 TID match, only that one cleaned

**Dispatch tests (`mod stage75_tests`, 8 total):**
- `stage75_supervisor_task_exit_notification_not_yet_wired` — `VFS_SUPERVISOR_TASK_EXIT_NOTIFICATION_ENABLED == false`
- `stage75_vfs_shared_io_still_disabled` — `VFS_SHARED_IO_ENABLED == false`
- `stage75_vfs_read_shared_reply_still_enabled` — `VFS_READ_SHARED_REPLY_ENABLED == true`
- `stage75_write_shared_request_still_disabled` — `VFS_WRITE_SHARED_REQUEST_ENABLED == false`
- `stage75_tid_matched_exit_cleans_lifecycle_in_vfs_context` — TID match in VfsService context
- `stage75_unrelated_task_exit_does_not_affect_active_request` — wrong TID → NotMatched
- `stage75_handle_request_unchanged_for_read_shared_opcode` — `handle_request` → `Unsupported`
- `stage75_old_vfs_parse_request_accepts_standard_ops` — standard ops still parse correctly

**Hard invariants:**
- `VfsSharedIoLifecycle::requester_tid` must be set at `reserve()` time and never change.
- `deliver_requester_exit_if_tid_matches(wrong_tid, handles)` must return `NotMatched` without touching state.
- `deliver_requester_exit_if_tid_matches(matching_tid, handles)` must be idempotent.
- `VFS_SUPERVISOR_TASK_EXIT_NOTIFICATION_ENABLED = false`; do not enable without both missing pieces.
- `VFS_SHARED_IO_ENABLED = false`; unchanged.
- `VFS_READ_SHARED_REPLY_ENABLED = true`; unchanged.
- No startup slots changed; no Drop-based cleanup added; SYSCALL_COUNT=31 unchanged.

**Run commands:**
- `cargo test -p yarm-fs-servers --lib stage75 -- --test-threads=1` (10 lifecycle tests)
- `cargo test -p yarm-fs-servers --lib stage75_tests -- --test-threads=1` (8 dispatch tests)
- `cargo test -p yarm-fs-servers --lib -- --test-threads=1` (full 295-test regression)

---

## Rule N+68 — Stage 76: PM-owned TaskExited/ProcessExited notification ABI

**Purpose:** Define the PM-owned lifecycle notification contract. Prove ABI codec
correctness for `PROC_OP_TASK_EXITED`/`PROC_OP_PROCESS_EXITED` message types, VFS handler
dispatch via `handle_pm_task_exited`, idempotency, safe no-op on unmatched TID, and gate
status. Documents the two infrastructure blockers before live wiring is possible.

**Modules:**
- `mod stage76_tests` in `crates/yarm-fs-servers/src/fs/common/vfs_service.rs` (18 tests)

**Gate and opcode tests (7 total):**
- `stage76_pm_task_exit_notification_gate_disabled` — `VFS_PM_TASK_EXIT_NOTIFICATION_ENABLED == false`
- `stage76_supervisor_task_exit_notification_still_disabled` — supervisor gate unchanged
- `stage76_vfs_shared_io_umbrella_still_disabled` — `VFS_SHARED_IO_ENABLED == false`
- `stage76_read_shared_reply_still_enabled` — `VFS_READ_SHARED_REPLY_ENABLED == true`
- `stage76_write_shared_request_still_disabled` — `VFS_WRITE_SHARED_REQUEST_ENABLED == false`
- `stage76_proc_op_task_exited_is_13` — `PROC_OP_TASK_EXITED == 13u16`
- `stage76_proc_op_process_exited_is_14` — `PROC_OP_PROCESS_EXITED == 14u16`

**Codec tests (5 total):**
- `stage76_pm_task_exited_event_encode_decode_roundtrip` — encode→decode yields same fields
- `stage76_pm_task_exited_event_decode_short_payload_rejected` — <16-byte payload → error
- `stage76_pm_process_exited_event_encode_decode_roundtrip` — encode→decode yields same fields
- `stage76_pm_process_exited_event_decode_short_payload_rejected` — <16-byte payload → error
- `stage76_pm_task_exited_event_le_byte_order` — LE byte ordering verified

**Handler dispatch tests (6 total):**
- `stage76_pm_task_exited_matched_tid_delivers_requester_exit` — TID match → `Matched(Won(RequesterExit))`
- `stage76_pm_task_exited_matched_lifecycle_write_direction` — WriteRequest direction also works
- `stage76_pm_task_exited_unmatched_tid_is_safe_noop` — wrong TID → `NotMatched`, then second call succeeds
- `stage76_pm_task_exited_duplicate_matched_tid_is_idempotent` — second call → `Matched(AlreadyCleaned)`
- `stage76_pm_task_exited_uses_separate_dispatch_not_handle_request` — helper route works end-to-end
- `stage76_pm_and_vfs_opcodes_are_in_separate_endpoint_namespaces` — opcode isolation documented

**Hard invariants:**
- `VFS_PM_TASK_EXIT_NOTIFICATION_ENABLED = true` (Stage 77+78: both blockers resolved).
- `PROC_OP_TASK_EXITED = 13`, `PROC_OP_PROCESS_EXITED = 14`; do not change these opcode values.
- `PmTaskExitedEvent` and `PmProcessExitedEvent` wire layouts are 16 bytes LE; do not change.
- `handle_pm_task_exited` must delegate to `deliver_requester_exit_if_tid_matches`; no state mutation on mismatch.
- `VFS_SUPERVISOR_TASK_EXIT_NOTIFICATION_ENABLED = false`; unchanged from Stage 75.
- `VFS_SHARED_IO_ENABLED = false`; unchanged.
- `VFS_READ_SHARED_REPLY_ENABLED = true`; unchanged.
- No startup slots changed; SYSCALL_COUNT = 31 unchanged; SpawnV5 ABI unchanged.

**Run commands:**
- `cargo test -p yarm-fs-servers --lib stage76_tests -- --test-threads=1` (18 tests)
- `cargo test -p yarm-ipc-abi -- --test-threads=1` (190 process_abi + vfs_abi tests)
- `cargo test -p yarm-fs-servers --lib -- --test-threads=1` (full 313-test regression; 325 after Stage 77+78)
- `cargo check -p yarm-fs-servers` (no errors)

---

## Rule N+69 — Stage 77+78: Kernel→PM TaskExited + VFS dispatch wiring

**Purpose:** Resolve both blockers from Stage 76. Add kernel-side `pm_task_exit_endpoint`,
`report_task_exit_to_pm()`, and VFS-side `dispatch_pm_task_exited_push()`. Enable
`VFS_PM_TASK_EXIT_NOTIFICATION_ENABLED = true`. Prove end-to-end data pipeline.

**Modules:**
- `mod stage77` in `src/kernel/boot/tests.rs` (15 kernel tests)
- `mod stage77_vfs_tests` in `crates/yarm-fs-servers/src/fs/common/vfs_service.rs` (12 VFS tests)

**Kernel tests (15 total):**
- `stage77_set_pm_task_exit_endpoint_requires_receive_cap` — SEND cap rejected; RECEIVE cap accepted
- `stage77_set_pm_task_exit_endpoint_requires_existing_task` — unregistered TID rejected
- `stage77_report_task_exit_to_pm_delivers_correct_opcode_and_tid` — `KERNEL_OP_PM_TASK_EXITED` + correct payload
- `stage77_report_task_exit_to_pm_noop_when_unregistered` — silent success when endpoint is `None`
- `stage77_exit_task_fires_pm_task_exit_endpoint` — `exit_task()` delivers message to PM endpoint
- `stage77_exit_task_fires_supervisor_and_pm_endpoints_independently` — both endpoints receive separate messages
- `stage77_exit_task_noop_pm_when_endpoint_not_registered` — no PM endpoint → `exit_task` still succeeds
- `stage77_kernel_pm_task_exited_payload_encode_decode_roundtrip` — codec roundtrip
- `stage77_kernel_pm_task_exited_payload_le_byte_order` — LE byte ordering verified
- `stage77_kernel_pm_task_exited_payload_decode_rejects_short` — <16-byte rejected
- `stage77_kernel_op_pm_task_exited_is_0xdc` — opcode value assertion
- `stage77_kernel_op_pm_task_exited_distinct_from_supervisor_op` — no collision with `0xEE`
- `stage77_kernel_pm_task_exited_payload_encoded_len_is_16` — payload size contract
- `stage77_syscall_count_unchanged` — SYSCALL_COUNT = 31
- `stage77_proc_op_task_exited_opcode_unchanged` — PROC_OP_TASK_EXITED = 13

**VFS tests (12 total):**
- `stage77_vfs_pm_task_exit_notification_now_enabled` — gate = `true`
- `stage77_vfs_shared_io_enabled_stage78` — umbrella gate enabled in Stage 78
- `stage77_dispatch_pm_task_exited_push_matched_tid_delivers_requester_exit` — TID match → `Matched(Won(RequesterExit))`
- `stage77_dispatch_pm_task_exited_push_unmatched_tid_is_safe_noop` — wrong TID → `NotMatched`
- `stage77_dispatch_pm_task_exited_push_rejects_wrong_opcode` — `WrongOpcode` error
- `stage77_dispatch_pm_task_exited_push_rejects_short_payload` — `Malformed` error
- `stage77_decode_kernel_pm_task_exited_correct_opcode_and_payload` — decodes (tid, code) correctly
- `stage77_decode_kernel_pm_task_exited_rejects_wrong_opcode` — `WrongOpcode` error
- `stage77_decode_kernel_pm_task_exited_rejects_short_payload` — `Malformed` error
- `stage77_kernel_pm_vfs_full_data_pipeline_tid_matches` — end-to-end: kernel payload → PM decode → VFS dispatch → cleanup
- `stage77_kernel_op_pm_task_exited_distinct_from_proc_op_task_exited` — `0xDC` ≠ `13`
- `stage77_handle_pm_task_exited_direct_still_works` — Stage 76 helper not broken

**Hard invariants:**
- `KERNEL_OP_PM_TASK_EXITED = 0xDC`; do not change.
- `KernelPmTaskExitedPayload::ENCODED_LEN = 16`; do not change.
- `FaultSubsystem::pm_task_exit_endpoint` initialized to `None` in bootstrap.
- `report_task_exit_to_pm` called from `exit_task()` after `report_task_exit_to_supervisor()`.
- No new syscalls; SYSCALL_COUNT = 31 unchanged.
- STARTUP_SLOT_COUNT = 18 unchanged.
- `VFS_SHARED_IO_ENABLED = true` (Stage 78 enables; write prerequisites proven).
- `VFS_SUPERVISOR_TASK_EXIT_NOTIFICATION_ENABLED = false`; unchanged.

**Run commands:**
- `cargo test -p yarm --lib --features hosted-dev -- stage77` (15 kernel tests)
- `cargo test -p yarm-fs-servers --features hosted-dev -- stage77` (12 VFS tests)
- `cargo test -p yarm-fs-servers --features hosted-dev` (full 340-test regression)

---

## Rule N+70 — Stage 78: Final VFS shared-I/O readiness audit + global enable

**Purpose:** Audit all VFS shared-I/O gates. Enable `VFS_WRITE_SHARED_REQUEST_ENABLED = true`
and `VFS_SHARED_IO_ENABLED = true` after confirming all prerequisites. Prove RequesterExit for
WRITE direction via PM push. Confirm `handle_request` still rejects shared opcodes.

**Gate matrix (all pass):**

| Gate | Value | Stage achieved |
|------|-------|----------------|
| `VFS_WRITE_SHARED_REQUEST_ENABLED` | `true` | Stage 78 |
| `VFS_READ_SHARED_REPLY_ENABLED` | `true` | Stage 73 |
| `VFS_PM_TASK_EXIT_NOTIFICATION_ENABLED` | `true` | Stage 77+78 |
| `VFS_SHARED_IO_ENABLED` | `true` | Stage 78 |
| `VFS_SUPERVISOR_TASK_EXIT_NOTIFICATION_ENABLED` | `false` | Stage 75 (PM model replaces) |

**Production policy:** `handle_request` still returns `VfsError::Unsupported` for
`VFS_OP_WRITE_SHARED_REQUEST` and `VFS_OP_READ_SHARED_REPLY`. `UnsupportedSharedIoMapper`
remains the production default. Gate `true` means helper prerequisites are proven, not that
live routing is active.

**VFS tests (15 total, mod stage78_tests):**
- `stage78_write_shared_request_gate_now_enabled` — `VFS_WRITE_SHARED_REQUEST_ENABLED = true`
- `stage78_read_shared_reply_gate_still_enabled` — `VFS_READ_SHARED_REPLY_ENABLED = true`
- `stage78_pm_task_exit_notification_still_enabled` — `VFS_PM_TASK_EXIT_NOTIFICATION_ENABLED = true`
- `stage78_global_vfs_shared_io_now_enabled` — `VFS_SHARED_IO_ENABLED = true`
- `stage78_global_gate_requires_all_three_prerequisites` — conjunction invariant
- `stage78_supervisor_exit_notification_still_disabled` — supervisor path unchanged
- `stage78_handle_request_still_rejects_write_shared_despite_gate_true` — Unsupported
- `stage78_handle_request_still_rejects_read_shared_despite_gate_true` — Unsupported
- `stage78_write_direction_requester_exit_via_pm_push_cleans_lifecycle` — WRITE lifecycle cleaned
- `stage78_write_direction_duplicate_requester_exit_idempotent` — AlreadyCleaned on second exit
- `stage78_write_requester_exit_unmatched_tid_is_safe_noop` — NotMatched
- `stage78_legacy_vfs_service_constructible` — service construction unchanged
- `stage78_legacy_vfs_ramfs_read_write_unchanged` — RAMFS backend unchanged
- `stage78_production_mapper_still_rejects_write_direction` — UnsupportedMapping
- `stage78_production_mapper_still_rejects_read_direction` — UnsupportedMapping

**Hard invariants:**
- `VFS_WRITE_SHARED_REQUEST_ENABLED = true`; do not revert without re-auditing prerequisites.
- `VFS_SHARED_IO_ENABLED = WRITE && READ && PM`; definition must include all three gates.
- `handle_request` must NOT route shared opcodes until a real `VfsSharedIoMapper` is added.
- `VFS_SUPERVISOR_TASK_EXIT_NOTIFICATION_ENABLED = false`; unchanged.
- SYSCALL_COUNT = 31; STARTUP_SLOT_COUNT = 18; both unchanged.
- FAT/ext4/blkcache production behavior unchanged.

**Run commands:**
- `cargo test -p yarm-fs-servers --features hosted-dev -- stage78` (15 VFS tests)
- `cargo test -p yarm-fs-servers --features hosted-dev` (full 340-test regression)
- `cargo check -p yarm-fs-servers` (no errors)

---

## Rule N+71 — Stage 79: RecvV3SharedIoMapper tests must never reach `from_raw_parts`

**Context:** `RecvV3SharedIoMapper` holds `mapped_base: u64` from a `RecvSharedV3Delivery`. In
hosted-dev builds, this value is synthetic (not a live kernel-mapped VA). Calling
`core::slice::from_raw_parts` or `core::slice::from_raw_parts_mut` on it is UB and will crash
the test process.

**Rule:** Every `RecvV3SharedIoMapper` test must target an error path that returns before the
unsafe slice construction. The six ordered validation layers provide six safe exit points:

1. `released` flag → `AccessAfterCleanup`
2. Direction mismatch → `WrongDirection`
3. Handle/generation cross-reference → `StaleHandle`
4. Permission bits → `MissingRights`
5. Range arithmetic → `BadRange`
6. `from_raw_parts` — only reached with a real kernel-mapped VA (out of scope in hosted-dev)

**Rule:** RAMFS byte-content proofs for Stage 79 must use `BorrowedSharedIoTestMapper`, not
`RecvV3SharedIoMapper`. The `BorrowedSharedIoTestMapper` has an in-process `Vec<u8>` backing
store and is safe to call from any hosted-dev test.

**Rule:** The `release` method may return `Err(VfsSharedIoAdapterError::ReleaseFailure)` in
hosted-dev (Linux `write(fd=cleanup_token)` → EBADF). Tests that call `release` must accept
both `Ok(())` and `Err(ReleaseFailure)` for the first call. The `released` flag must be `true`
after either outcome. The second call must return `Ok(())`.

**Stage 79 test inventory:**

*`shared_io_adapter.rs` — `mod tests`:*
- `stage79_recv_v3_mapper_from_delivery_constructs_with_all_fields`
- `stage79_recv_v3_mapper_from_fields_is_not_released`
- `stage79_write_request_wrong_direction_rejected`
- `stage79_write_request_stale_handle_rejected`
- `stage79_write_request_stale_generation_rejected`
- `stage79_write_request_rw_perm_rejected`
- `stage79_write_request_bad_range_rejected`
- `stage79_read_reply_wrong_direction_rejected`
- `stage79_read_reply_readonly_perm_rejected`
- `stage79_release_stale_handle_rejected`
- `stage79_release_marks_released_and_blocks_subsequent_access`
- `stage79_release_idempotent_second_call_returns_ok`

*`vfs_service.rs` — `mod stage79_tests`:*
- `stage79_recv_v3_mapper_implements_vfs_shared_io_mapper_trait`
- `stage79_dispatch_write_shared_request_with_recv_v3_mapper_rw_perm_rejected`
- `stage79_dispatch_read_shared_reply_with_recv_v3_mapper_ro_perm_rejected`
- `stage79_byte_access_blocker_documented_and_gates_unchanged`
- `stage79_dispatch_write_shared_request_ramfs_regression`
- `stage79_dispatch_read_shared_reply_ramfs_regression`
- `stage79_handle_request_still_rejects_shared_opcodes`
- `stage79_gate_values_unchanged_from_stage78`

**Hard invariants:**
- `RecvV3SharedIoMapper::from_delivery` copies all four delivery fields unchanged.
- `released` flag set BEFORE `release_v3_cleanup_token` call (panic-safe at-most-once).
- All six validation layers preserved in order; none skipped or reordered.
- `handle_request` must NOT route shared opcodes (unchanged from Stage 78).
- All three direction/PM gate flags remain `true`; supervisor flag remains `false`.
- SYSCALL_COUNT = 31; recv_shared_v3 ABI offsets unchanged; SpawnV5/Phase2B/Phase3B unchanged.
- FAT/ext4/blkcache production behavior unchanged; shared I/O not enabled for those backends.

**Run commands:**
- `cargo test -p yarm-fs-servers --features hosted-dev -- stage79` (20 Stage 79 tests)
- `cargo test -p yarm-fs-servers --features hosted-dev` (full 360-test regression)

---

## Rule N+72 — Stage 80: ramfs_srv/fat_srv/ext4_srv CPIO staging, spawn wiring, and conservative mount policy

**Context:** Stage 80 adds `ramfs_srv`, `fat_srv`, and `ext4_srv` to the CPIO initramfs archive,
wires their spawn via the PM image-ID table, and documents conservative VFS mount policy.
`ext4_srv` VFS registration is deliberately deferred: `ext4/service.rs::run()` is a demo smoke that
returns without entering a kernel `ipc_recv` loop; VFS cannot route requests to a non-listening service.

### N+72.1 — CPIO ELF alignment: all three FS server ELFs must be 4096-byte aligned

All three ELFs (`sbin/ramfs_srv`, `sbin/fat_srv`, `sbin/ext4_srv`) must be packed with their data
payloads at offsets divisible by `PAGE_ALIGN = 4096`. The packer must emit an `ALIGN_PROOF` marker
to stderr for each, confirming `alignment_mod=0 aligned=true`.

**Test:** `test_stage80_ramfs_fat_ext4_elfs_are_aligned_and_emit_proof` in
`scripts/test_pack_initramfs_aligned.py`. Each ELF is synthetic (`b"\x7fELF" + name.encode() + b"\x00"*8`);
`insert_alignment_pad` is exercised exactly as in production packing. The test verifies:
- `sbin/ramfs_srv`, `sbin/fat_srv`, `sbin/ext4_srv` all present in CPIO offsets table.
- `offsets[name] % PAGE_ALIGN == 0` for each.
- `ALIGN_PROOF path=/<name> data_offset=<N> alignment_mod=0 aligned=true` in stderr.

**Prohibited:** Packing the ELFs without calling `insert_alignment_pad`. Packing without the ALIGN_PROOF
emit. Omitting any of the three from the CPIO archive.

### N+72.2 — PM image-ID table: ext4_srv must be image_id=12; range max must be 12

`VFS_SERVICE_IMAGE_ID_MAX` must be 12. `pm_vfs_spawn_inline` must map `12 => b"/initramfs/sbin/ext4_srv"`.
`pm_image_cpio_name` must map `12 => Some(b"sbin/ext4_srv")`. Image IDs 10 (fat_srv) and 11 (ramfs_srv)
must also be covered; all three must fall within `[VFS_SERVICE_IMAGE_ID_MIN, VFS_SERVICE_IMAGE_ID_MAX]`.

**Test (source inspection):** `stage80_pm_image_id_range_covers_fs_servers` in
`crates/yarm-control-plane-servers/src/control_plane/mod.rs` — asserts `const VFS_SERVICE_IMAGE_ID_MAX: u64 = 12;`
in PM service source. `stage80_pm_ext4_cpio_path_registered` asserts both the `pm_vfs_spawn_inline`
and `pm_image_cpio_name` match arms exist.

**Prohibited:** Setting `VFS_SERVICE_IMAGE_ID_MAX` above 12 (no new image IDs added in Stage 80).
Setting it below 12 (blocks fat/ramfs/ext4 spawn). Adding new syscalls (`SYSCALL_COUNT` must remain 31).

### N+72.3 — init spawn wiring: ext4_srv spawned with image_id=12; mount deferred

`init/service.rs::run()` must call `spawn_v5_cap(pm_send, pm_recv, 12, [0, 0, 0, 0], 1)` and log:
- `INIT_EXT4_SPAWN_BEGIN` before the spawn call.
- `INIT_EXT4_SPAWN_OK child_tid=<N> mount_deferred=true reason=no-ipc-loop` on success.
- `EXT4_SRV_READY child_tid=<N>` on success.

The `mount_deferred=true` and `reason=no-ipc-loop` tokens document the blocker: ext4's service loop
is a demo smoke; it does not enter a kernel IPC receive loop. VFS registration cannot be wired until
a real recv loop exists. Do NOT call `register_ext4_mount_with_vfs()` or any VFS mount registration
path for ext4 in Stage 80.

**Tests (source inspection):** `stage80_init_spawns_ext4_srv_with_image_id_12` and
`stage80_init_ext4_vfs_mount_deferred_blocker_documented` in `control_plane/mod.rs`.

**Prohibited:** Calling `register_ext4_mount_with_vfs()` in Stage 80. Omitting the blocker
documentation in the init log. Placing stage80 tests inside `init/service.rs` — the `pub mod init`
is `#[cfg(any(not(test), feature = "legacy-tests"))]` and those tests would never compile in normal
test mode. All gate tests for init/PM behavior must use `include_str!()` source-inspection in
`control_plane/mod.rs`.

### N+72.4 — FS backend behavior: ext4 writes must remain Unsupported; FAT must guard no-block-backend

`Ext4Backend::write(fd, len)` must return `Err(VfsError::Unsupported)` for all write lengths.
`service_from_startup_config(FatStartupConfig::production(None, Some(1), 1))` must return
`Err(FatServiceStartup::NoBlockBackend)`.
`run_with_config(RamFsStartupConfig::default_compat())` must return `RamFsServiceStartup::Mounted { .. }`.

**Tests:** `stage80_ext4_write_path_remains_unsupported`, `stage80_ext4_backend_rejects_writes_of_all_sizes`
(sizes `[1u64, 512, 4096, 65536, 16*1024*1024+1]`), `stage80_fat_write_mode_guard_requires_block_backend`,
`stage80_ramfs_run_with_config_smoke_unchanged` — all in `crates/yarm-fs-servers/src/lib.rs`
`mod stage80_tests`.

**Prohibited:** Enabling ext4 writes. Allowing FAT production writes without a block backend.
Changing `VFS_SHARED_IO_ENABLED` (must remain true from Stage 78).

### N+72.5 — Binary entry markers: all three FS server bins must have ENTRY and READY markers

`sbin/ramfs_srv.rs` must contain `RAMFS_BIN_ENTRY_START`. `sbin/fat_srv.rs` must contain `FAT_BIN_ENTRY_START`.
`sbin/ext4_srv.rs` must contain `EXT4_BIN_ENTRY_START`, `EXT4_SRV_ENTRY`, and `EXT4_MOUNT_READY`.
`ext4/service.rs` must NOT contain `ipc_recv(` — the recv loop blocker must remain in place.

**Tests:** `stage80_ext4_srv_bin_has_entry_and_ready_markers`, `stage80_all_three_fs_server_bins_have_entry_markers`,
`stage80_ext4_vfs_registration_deferred_blocker_no_ipc_loop` in `mod stage80_tests`.

### N+72.6 — VFS shared-I/O gate values must be unchanged from Stage 78

`VFS_WRITE_SHARED_REQUEST_ENABLED`, `VFS_READ_SHARED_REPLY_ENABLED`, and `VFS_SHARED_IO_ENABLED` must
all remain `true`. These were set in Stage 78 and must not be altered in Stage 80.

**Test:** `stage80_vfs_shared_io_enabled_consistent_with_stage78` in `mod stage80_tests`.

**Stage 80 test inventory:**

*`scripts/test_pack_initramfs_aligned.py`:*
- `test_stage80_ramfs_fat_ext4_elfs_are_aligned_and_emit_proof`

*`crates/yarm-control-plane-servers/src/control_plane/mod.rs` — `mod tests`:*
- `stage80_pm_image_id_range_covers_fs_servers`
- `stage80_init_spawns_ext4_srv_with_image_id_12`
- `stage80_init_ext4_vfs_mount_deferred_blocker_documented`
- `stage80_pm_ext4_cpio_path_registered`
- `stage80_syscall_count_unchanged`

*`crates/yarm-fs-servers/src/lib.rs` — `mod stage80_tests`:*
- `stage80_ext4_write_path_remains_unsupported`
- `stage80_ext4_backend_rejects_writes_of_all_sizes`
- `stage80_fat_write_mode_guard_requires_block_backend`
- `stage80_vfs_shared_io_enabled_consistent_with_stage78`
- `stage80_ramfs_run_with_config_smoke_unchanged`
- `stage80_ext4_srv_bin_has_entry_and_ready_markers`
- `stage80_all_three_fs_server_bins_have_entry_markers`
- `stage80_ext4_vfs_registration_deferred_blocker_no_ipc_loop`

**Hard invariants:**
- `VFS_SERVICE_IMAGE_ID_MAX = 12`; `SYSCALL_COUNT = 31` — neither may change in Stage 80.
- `ext4/service.rs` must NOT contain `ipc_recv(` until the recv-loop blocker is lifted.
- `VFS_SHARED_IO_ENABLED` must remain `true` (unchanged from Stage 78).
- CPIO archive must contain `sbin/ramfs_srv`, `sbin/fat_srv`, `sbin/ext4_srv` all at 4096-aligned offsets.
- No kernel syscall/IPC/VM/cap internals changed by Stage 80.

**Run commands:**
- `python3 scripts/test_pack_initramfs_aligned.py` (4 CPIO alignment tests)
- `cargo test -p yarm-control-plane-servers --features hosted-dev -- stage80` (5 gate tests)
- `cargo test -p yarm-fs-servers --features hosted-dev -- stage80` (8 FS backend tests)

---

## Rule N+73 — Stage 80R/81: Profile-gate optional FS live spawns; never block core-service startup on unresolved kernel spawn-path entries

### Background: regression introduced by Stage 80

Stage 80 wired `init/service.rs` to spawn `ramfs_srv` (image_id=11) between the VFS spawn and
the `driver_manager` spawn, and to spawn `fat_srv` (image_id=10) and `ext4_srv` (image_id=12)
after `driver_manager`. On the next boot smoke run, these spawns caused the following regression:

- `BLKCACHE_SRV_READY`, `VIRTIO_BLK_SRV_READY`, `DRIVER_MANAGER_READY`, `PM_ELF_ZC_DONE 7/8/9`
  all became `=0` on x86_64 QEMU.
- On AArch64 QEMU the kernel halted entirely after the first optional-FS spawn attempt.

### Root cause: `spawn_image_path_for_image_id` kernel gap

`src/kernel/syscall.rs::spawn_image_path_for_image_id()` maps image IDs to kernel-side ELF paths.
It covers IDs 0–9 only. IDs 10, 11, and 12 return `None`, which propagates as
`SyscallError::InvalidArgs` through both Phase 3A (`spawn_from_memory_object`, syscall nr=29) and
Phase 2B (`handle_spawn_v4_cap_process`). On AArch64, `InvalidArgs` from a syscall reaches the
AArch64 trap handler which calls `YARM_AARCH64_TRAP_HANDLE halting` — fatal for all subsequent
tasks. On x86_64, PM catches the error but falls through a long Phase-2B VFS bulk-read chain before
failing, delaying or corrupting PM reply state used by subsequent core-service spawns.

Extending `spawn_image_path_for_image_id` to cover IDs 10/11/12 is a kernel behavior change
deferred to the expanded-FS profile stage. It must not be done as part of Stage 80R/81.

### Fix: profile gate `INIT_SPAWN_OPTIONAL_FS_SERVERS`

All optional FS live spawns (ramfs_srv, fat_srv, ext4_srv) are now gated behind:

```rust
const INIT_SPAWN_OPTIONAL_FS_SERVERS: bool = false;
```

When `false`, `init/service.rs` emits log markers and continues without attempting any spawn:

```
INIT_RAMFS_SPAWN_SKIPPED reason=profile_disabled
INIT_FAT_SPAWN_SKIPPED reason=profile_disabled
INIT_EXT4_SPAWN_SKIPPED reason=profile_disabled
```

The gated section appears **after** all core service spawns and smoke checks (driver_manager,
blkcache, virtio_blk), so even when enabled in a future profile it cannot block core startup.

### N+73.1 — `INIT_SPAWN_OPTIONAL_FS_SERVERS` must default `false` in the core profile

`init/service.rs` must contain `const INIT_SPAWN_OPTIONAL_FS_SERVERS: bool = false;`.
The gated live-spawn code must only be reachable when this constant is `true`.

**Test:** `stage81_optional_fs_spawn_disabled_in_core_profile`

### N+73.2 — SKIPPED log markers must be emitted when optional FS gate is false

When `INIT_SPAWN_OPTIONAL_FS_SERVERS` is `false`, all three SKIPPED markers must be emitted:
`INIT_RAMFS_SPAWN_SKIPPED reason=profile_disabled`, `INIT_FAT_SPAWN_SKIPPED reason=profile_disabled`,
`INIT_EXT4_SPAWN_SKIPPED reason=profile_disabled`.

**Test:** `stage81_optional_fs_skipped_markers_present`

### N+73.3 — `driver_manager` spawn must precede the optional FS section in source order

`INIT_DRIVER_MANAGER_SPAWN_V5_CALL_BEGIN` must appear at a lower byte offset than
`INIT_SPAWN_OPTIONAL_FS_SERVERS` in `init/service.rs`. Core services are never deferred for
optional FS work.

**Test:** `stage81_core_spawn_order_driver_manager_before_optional_fs`

### N+73.4 — Kernel spawn-path blocker must be documented in init source

`init/service.rs` must contain the string `spawn_image_path_for_image_id` and
`SyscallError::InvalidArgs` as inline documentation of the kernel blocker that prevents live
spawning of optional FS servers in the current core profile.

**Test:** `stage81_kernel_spawn_path_table_blocker_documented`

### N+73.5 — Optional FS live-spawn code must appear inside the gate, not before it

`INIT_SPAWN_OPTIONAL_FS_SERVERS` (gate declaration) must appear at a lower byte offset than
`INIT_RAMFS_SPAWN_BEGIN` in `init/service.rs`. Direct unconditional optional-FS spawn calls
before the gate are forbidden.

**Test:** `stage81_optional_fs_spawn_code_gates_not_direct_spawns`

### N+73.6 — Stage 80 CPIO, PM, and FS backend artifacts must not be removed

All Stage 80 artifacts must remain intact in Stage 80R/81:
- CPIO staging: `sbin/ramfs_srv`, `sbin/fat_srv`, `sbin/ext4_srv` present in initramfs.
- ALIGN_PROOF coverage: `test_stage80_ramfs_fat_ext4_elfs_are_aligned_and_emit_proof` must pass.
- PM image-ID table: `VFS_SERVICE_IMAGE_ID_MAX = 12`, `12 => b"/initramfs/sbin/ext4_srv"`.
- init spawn wiring (gated): `spawn_v5_cap(pm_send, pm_recv, 12, ...)` inside optional gate.
- Stage 80 mod.rs tests: all five `stage80_*` tests must continue to pass.

**Stage 80R/81 test inventory:**

*`crates/yarm-control-plane-servers/src/control_plane/mod.rs` — `mod tests`:*
- `stage81_optional_fs_spawn_disabled_in_core_profile`
- `stage81_optional_fs_skipped_markers_present`
- `stage81_core_spawn_order_driver_manager_before_optional_fs`
- `stage81_kernel_spawn_path_table_blocker_documented`
- `stage81_optional_fs_spawn_code_gates_not_direct_spawns`

**Hard invariants:**
- `INIT_SPAWN_OPTIONAL_FS_SERVERS = false` in all core profiles.
- No kernel syscall/IPC/VM/cap internals changed.
- `SYSCALL_COUNT` remains 31; `SpawnV5` ABI, Phase2B/Phase3B unchanged.
- Core smoke scripts must not weaken existing `BLKCACHE_SRV_READY`, `VIRTIO_BLK_SRV_READY`,
  `DRIVER_MANAGER_READY` checks.
- Stage 80 CPIO/ALIGN_PROOF/PM/init artifacts preserved.

**Run commands:**
- `cargo test -p yarm-control-plane-servers --features hosted-dev -- stage81` (5 gate tests)
- `cargo test -p yarm-control-plane-servers --features hosted-dev -- stage80` (5 regression checks)
- `bash -n scripts/qemu-x86_64-core-smoke.sh && bash -n scripts/qemu-aarch64-core-smoke.sh`
- `cargo test -p yarm-fs-servers --features hosted-dev` (full regression)

---

## Rule N+74 — Stage 81A: Syscall errors must be encoded in the trap frame, not propagated as TrapHandleError

**Applies to:** `src/kernel/boot/fault_state.rs` `handle_trap`, all arch entry points.

**Problem (pre-81A):** `dispatch_syscall(self, trapframe)?` inside `handle_trap` propagated any
`SyscallError` as `TrapHandleError::Syscall(...)`. All three arch entry points treat
`Err(TrapHandleError)` as a fatal kernel halt (AArch64: WFE loop; x86_64: `halt_forever()`; RISC-V:
`?` propagation). A normal user error like `InvalidArgs` or `MissingRight` would lock up the CPU.

**Rule:** The `Trap::Syscall` arm of `handle_trap` MUST encode errors into the frame
(`trapframe.set_err(e.code())`) and return `Ok(())`. Never use `?` to propagate `SyscallError`
out of `handle_trap` for the syscall case.

**Kernel-internal wrappers** (e.g. `control_plane_set_process_cnode_slots_via_syscall`) that call
`handle_trap` synthetically and need to observe policy-denial errors MUST read `frame.error_code()`
after dispatch and translate it back to `Err(TrapHandleError::Syscall(...))` using
`SyscallError::from_code(code)`.

**Gate tests (in `src/kernel/syscall.rs`):**
- `stage81a_unknown_syscall_nr_is_encoded_in_frame_not_fatal`
- `stage81a_invalid_args_from_dispatch_encoded_not_propagated`
- `stage81a_parity_fix_dispatch_no_longer_propagates_via_question_mark`
- `stage81a_aarch64_halt_path_requires_trap_handle_err_not_syscall_err`

**Run:** `cargo test --lib --features hosted-dev stage81a`

---

## Rule N+75 — Stage 81B: Kernel spawn path table must cover optional-FS image IDs 10/11/12

**Applies to:** `spawn_image_path_for_image_id()` in `src/kernel/syscall.rs`.

**Rule:** Image IDs 10 (`fat_srv`), 11 (`ramfs_srv`), 12 (`ext4_srv`) must have path-table entries
so that Phase 2B (`handle_spawn_process_from_user_buf`, NR=24) and Phase 3A
(`handle_spawn_from_memory_object`, NR=29) return valid CPIO paths instead of `InvalidArgs` when
optional FS spawning is enabled. IDs ≥ 13 MUST still return `None`.

**`INIT_SPAWN_OPTIONAL_FS_SERVERS` MUST remain `false` in all core profiles** — adding path-table
entries does not enable live spawning.

**`SYSCALL_COUNT` must remain 31; no syscall numbers changed.**

**Gate tests (in `src/kernel/syscall.rs`):**
- `stage81b_spawn_path_table_covers_optional_fs_image_ids`
- `stage81b_spawn_path_table_unknown_high_id_returns_none`
- `stage81b_syscall_count_remains_31`
- `stage81b_spawn_phase2b_and_phase3a_both_use_path_table`
- `stage81a_optional_fs_core_profile_still_disabled`

**Run:** `cargo test --lib --features hosted-dev stage81b`


---

## Rule N+76 — Stage 83: `RecvV3SharedIoMapper` byte-access proof uses real heap backing

**Applies to:** `crates/yarm-fs-servers/src/fs/common/shared_io_adapter.rs` and `vfs_service.rs`.

**Rule:** Tests for `RecvV3SharedIoMapper` byte access MUST use a real heap-allocated buffer
(e.g. `vec![0u8; N]`) as `mapped_base`, not a fake VA constant.  Using a fake VA with
`core::slice::from_raw_parts[_mut]` is undefined behaviour.  Error-path tests that reject
before slice creation may use any non-zero `u64` constant.

**Pattern:**
```rust
let mut backing = vec![0u8; 4096];
let ptr = backing.as_ptr() as u64;         // or as_mut_ptr() for READ_SHARED_REPLY
let mut mapper = RecvV3SharedIoMapper::from_fields(token, ptr, 4096, perm);
// ... dispatch call ...
// backing bytes are observable after dispatch (no live &mut [u8] exists)
```

**Release in hosted-dev:** `release_v3_cleanup_token` calls Linux NR 4 (`write`) with
`cleanup_token` as fd (invalid) → EBADF → `Err(ReleaseFailure)`.  The `released` flag is set
**before** the syscall, so `is_released()` returns `true`.  `dispatch_*` ignores release errors
with `let _ = mapper.release(descriptor)`.  Tests verify `mapper.is_released() == true`, not the
release return value.

**Gate tests:**
- `stage83_write_shared_request_recv_v3_mapper_byte_proof`
- `stage83_read_shared_reply_recv_v3_mapper_byte_proof`
- `stage83_write_shared_request_release_exactly_once`
- `stage83_read_shared_reply_release_exactly_once`
- `stage83_write_shared_request_backend_error_releases_mapper`
- `stage83_read_shared_reply_backend_error_releases_mapper`
- `stage83_read_shared_reply_short_eof_reports_exact_bytes_read`
- Plus 7 negative tests and 1 gate regression.

**Run:** `cargo test -p yarm-fs-servers --lib stage83`

## Stage 84 test rules

**Module:** `stage84_tests` in `vfs_service.rs` (20 tests).

**Coverage areas:**
1. Gate constant: `VFS_STAGE84_RAMFS_BRIDGE_ENABLED = true`.
2. Lifecycle store: `new()` produces 0 active slots.
3. Write byte proof: heap backing content reaches RAMFS file via `handle_write_shared_request_gated`.
4. Read byte proof: RAMFS file bytes reach heap backing via `handle_read_shared_reply_gated`.
5–6. `op_sequence` advances exactly once per successful call (write and read directions).
7–8. Lifecycle slot is `None` (freed) after success (write and read directions).
9–10. Reply fields: `request_id`, `status == VFS_SHARED_IO_STATUS_OK`, `flags == 0`.
11. Short `requested_len`: only the prefix bytes are written; `bytes_completed == requested_len`.
12. RequesterExit matched TID: `deliver_requester_exit_all` cleans the slot; returns 1.
13. RequesterExit unmatched TID: slot survives; returns 0.
14. Duplicate RequesterExit: second call returns 0 (idempotent).
15. Multiple slots: only the matching TID's slot is cleaned.
16. Backend error: `handle_write_shared_request_gated` returns `Err(Malformed)` and frees the slot.
17–18. `parse_request` still rejects `VFS_OP_WRITE_SHARED_REQUEST` and `VFS_OP_READ_SHARED_REPLY`.
19. `VFS_SUPERVISOR_TASK_EXIT_NOTIFICATION_ENABLED = false`.
20. Prior gate flags unchanged: `VFS_WRITE_SHARED_REQUEST_ENABLED`, `VFS_READ_SHARED_REPLY_ENABLED`,
    `VFS_SHARED_IO_ENABLED`.

**RequesterExit test infrastructure:** `test_stage84_insert_inflight(request_id, requester_tid,
direction, requested_len)` — a `#[cfg(test)]` helper on `VfsService` that inserts an InFlight
lifecycle directly into the lifecycle store using the service's own handle table.  This allows
RequesterExit tests to run independently without needing to interrupt a live shared-I/O call.

**Heap-backed delivery pattern:**
```rust
let mut backing = vec![0u8; 4096];
let ptr = backing.as_mut_ptr() as u64; // or as_ptr() for WRITE_SHARED_REQUEST
let delivery = RecvSharedV3Delivery {
    sender_tid, cleanup_token: TOKEN, mapped_base: ptr,
    page_rounded_mapped_len: 4096, actual_mapping_perm: 3, // or 1 for write
    ..Default::default()
};
// request.buffer.object_handle == TOKEN, request.buffer.object_generation == TOKEN >> 16
```

**Run:** `cargo test -p yarm-fs-servers --lib stage84`
