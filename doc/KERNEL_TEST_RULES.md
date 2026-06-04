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
