# YARM Scheduling Contract

**Package:** SCHED-CONTRACT1, corrected by SCHED-CONTRACT1-CORRECTION (§13), with the overflow
latch closed by SCHED-OVERFLOW1 (§15) and the kernel FP/SIMD rule implemented by KERNEL-NOFP1
(§8.1, §8.2). **Baseline:** commit
`635374140c59586b11ce1d3456fb77dffe1b059f` (tree `127827ce276d1a7038f61690e1a96141c40f5d2d`); the
correction changes no production source. **Linked from:** `doc/KERNEL_UNLOCKING.md` §0.8.

This document is the normative scheduling contract for YARM. It governs the independent kernel
threads of KTHREAD1, the real-time scheduler of RT-SCHED1 and the partition windows of
PARTITION1. It **defines and tests** the contract. It does not enable kernel threads, change
production scheduling policy, or claim conformance to any safety standard (§11).

Every statement about current behaviour was derived from source at the baseline — callers and
writers traced, not comments inherited. Where a source comment disagreed with the code, §1.12
records the disagreement.

---

## 0. How to read this contract

**Requirement IDs** are stable. They are never renumbered or reused; a requirement that stops
applying is marked **WITHDRAWN** in place, with the reason.

**Every requirement carries exactly one status:**

| Status | Meaning |
|---|---|
| **ENFORCED** | Production enforces it today. The evidence is named, and it is production evidence (P or L below) — never a model test. |
| **SPECIFIED → PKG** | Normative for the named future package. Not enforced today. Production behaviour is unchanged until that package deliberately changes it. |
| **UNRESOLVED** | Cannot be specified or discharged yet. The exact missing fact and its owner are named. |

**Evidence classes** cited below:

| Class | What it is | What it can prove |
|---|---|---|
| **P** | A test against the production owners (`tests/scheduling_contract.rs` module `production`, or an existing in-tree test it names). | That production enforces the property, for the inputs exercised. |
| **L** | Existing, correctly bound live QEMU evidence from an earlier qualified package. | That the production route is reached and behaves so on QEMU. Not silicon timing. |
| **S** | Source inspection, recorded here with the owner symbol. | What the code says. Not run-time behaviour. |
| **M** | The executable policy model (`tests/scheduling_contract.rs` module `model`). | That the *future* rule is unambiguous and its predicates are mutation-checked. **It proves nothing about production.** |

**Profiles** are deployment configurations of ONE set of production dispatch and lifecycle owners.
They are never alternate dispatchers:

* **GP**: the general-purpose profile, which is current production behaviour (§7).
* **RT-P1**: the first real-time profile, specified here and implemented by RT-SCHED1 and
  PARTITION1 (§2–§5).

---

## 1. Source baseline: what production does today

### 1.1 Scheduler storage, capacity and authority

* **Per-CPU storage, one lock.** `SmpScheduler` (`src/kernel/scheduler.rs`) holds one
  `PriorityScheduler` per CPU, indexed by `CpuId`, for `MAX_CPUS = 64` slots. The whole
  `SchedulerState` (`src/kernel/boot/defs.rs`) sits behind ONE rank-1 `SpinLockIrq`, reached by
  `SharedKernel::with_scheduler_split_mut`. That state is every CPU's queues, the clock owner's
  `timer`, every other CPU's `ap_quanta` and the ambient `current_cpu`. So the queues are
  **per-CPU storage, but not independently owned or independently locked**. Every enqueue,
  selection, block and tick on any CPU serialises on that one lock. Per-CPU scheduler lock types
  are restricted by `AI_AGENT_RULES.md` §14.4 (the D6 bullet), restated in
  `doc/KERNEL_UNLOCKING.md` §7.1 and §8, and pinned by a source guard. `doc/KERNEL_LOCKING.md`'s
  "Stage 4", by contrast, *plans* per-CPU lock domains. §12 (SCHED-LOCK1) records how a future
  package must reconcile them. *(Corrected by SCHED-CONTRACT1-CORRECTION: this line previously
  said the `KERNEL_LOCKING.md` rank table forbids per-CPU locks. It does not.)*
  **Superseded by SCHED-LOCK1 (§16–§17):** `SmpScheduler` and `SchedulerState` are gone. The
  `SchedDomain` gives each CPU its own lock (rank 1L) over its queues, current slot and quantum;
  admission has its own lock (rank 1G); the clock, admission bitmaps and per-CPU mirrors are
  atomics; the ambient `current_cpu` is removed. The paragraph above is kept as the baseline.
* **Priorities.** There are three levels, `TaskPriority::{High, Normal, Low}`
  (`crates/yarm-kernel/src/scheduler.rs`). They are derived only by
  `task_enqueue::priority_for_class`: `SystemServer` is High, and `Driver` and `App` are Normal.
  Nothing produces `Low` for a live task except the scheduler-owned idle placeholder (tid 0).
* **Capacities.** Each CPU has three `RingQueue`s of `MAX_RUN_QUEUE = 64` entries each (192
  runnable per CPU). A 256-slot open-addressed membership table falls back to a linear scan when
  full. The TCB array holds `MAX_TASKS = 512`. The production CPU ceiling is 8 on every port
  (`*_cpu_admission.rs` `PRODUCTION_CPU_CEILING`). Because 192 < 512, a run queue can overflow
  while TCB slots remain free. Overflow is the explicit `SchedulerError::QueueFull` (SC-POOL-1).
* **CPU authority.** Every production drain authenticates against a `DispatchAuthority`
  (`src/runtime.rs`), minted only by `arch::trap_entry::TrapPathWindow::establish` from the
  hardware-identified CPU (x86_64 per-CPU record, AArch64 `MPIDR_EL1`, RISC-V hart id). It carries
  a per-trap epoch, so an authority kept past its trap authorizes nothing. The ambient
  `SchedulerState::current_cpu` is no longer read by any production selection.
* **Selection has one owner.** Every production queue advance — the D2 send/recv, FutexWait,
  Yield and exit/terminal drains — reaches `SharedKernel::queue_advance_select_step_split`. That
  calls `SmpScheduler::dispatch_next_accepted_selection_on`. Three other selection callers reuse
  the same `SmpScheduler` order:
  * `owner_revalidation_select_split` (x86_64 post-drain revalidation);
  * `enqueue_then_dispatch_on_cpu_split` (knob-gated AP paths);
  * `d6_genuine_local_dispatch_step_mut` (default-off diagnostic, queue-neutral by construction).

### 1.2 Lock and interrupt discipline

* **The kernel is non-preemptive on all three ISAs.** Every trap runs with interrupts masked:
  * x86_64: `IA32_FMASK` clears `RFLAGS.IF` on syscall entry, and interrupt gates clear IF.
  * AArch64: hardware `DAIF` masking, with no `daifclr` on the trap path.
  * RISC-V: `sstatus.SIE` is clear, with the one re-enable confined to the terminal-idle tail.

  This is stated in `src/kernel/syscall/exit_txn.rs`. Interrupts are taken only in user mode and
  inside the idle windows: x86_64 `sti; hlt`, AArch64 `yarm_aarch64_idle_wfi_window`, RISC-V
  `riscv_trap_halt`'s `wfi`. There is no interrupt nesting.
* **`SpinLockIrq`** (`src/kernel/lock.rs`):
  * It is test-and-test-and-set. It saves the caller's IRQ state and masks for the CAS. A failed
    CAS restores the caller's state and spins, so a waiter inside a trap spins **masked**.
  * There is no queueing or ticket, so acquisition is **not FIFO-fair**, and the wait has no
    structural bound (§6).
  * The critical section always runs masked.
  * `SpinLock` (no IRQ save) serves `PRINTK_DRAIN_LOCK` and spins on a weak CAS.
* **No production broad lock.** The `with_cpu`/`with` census is 0
  (`tests/broad_lock_census_guard.rs`). `SharedKernel::with_cpu` exists only for tests
  (`test_exclusive`).

### 1.3 Selection order and queue position (GP, production)

| Event | Owner | Effect on queue position | Evidence |
|---|---|---|---|
| Selection from empty `current` | `dispatch_next_selection` / `dispatch_next_accepted_selection` | High → Normal → Low, FIFO within a level; an unacceptable candidate is skipped **in place** | P `sc_pri_1`, `sc_pri_2`, `sc_elig_1` |
| Non-idle `current` at a dispatch point | same | `ContinuedCurrent`: no preemption, whatever is queued | P `sc_pri_4` |
| Quantum expiry (preempting tick) | `run_yield_transaction` → `preempt_reenqueue_only_on` | running task → **tail of its own level**; selection deferred to the post-lock drain | P `sc_pri_3`, S `settle_recognized_timer` |
| Explicit Yield (NR 0) | the same `run_yield_transaction` | tail of its own level | S `yield_txn.rs` |
| Block (IPC, futex, exit) | `block_current_exact_on` / `block_current_on` | removed from `current`; on no queue | S |
| Wake / remote enqueue | `task_enqueue::commit_enqueue_locked` | **tail** of its class level, on its home CPU; never displaces the running task | P `sc_pri_4`, `sc_cpu_1` |
| Unwind restoring the trapping task | `restore_entering_incarnation_exact_split` → `on_preempt_prefer_on_cpu_split` | the SAME task re-installed, bypassing FIFO order; it is a continuation of the interrupted task, not a handoff to another | S |

### 1.4 Placement and migration

* **The home rule.** `task_enqueue::fix_home_locked` records a task's CPU at its first placement:
  the balanced least-loaded choice, or the explicitly named CPU. A wake targets that home.
  Wake-only CPUs are never placed on. `balance_excluded` CPUs accept only explicit placement.
* **No migration is enforced for Balanced plans only.** `fix_plan_home` (broad twin) and
  `resolve_plan_home_split` (split) rewrite a **Balanced** plan to the home. A **Pinned** plan
  keeps the CPU its caller named even when the task already has a different home, and the home
  record is then left unchanged (P: existing unit test
  `task_enqueue::tests::the_pinned_planner_names_its_own_cpu_regardless_of_affinity`).
* **Which pinned callers could place an already-homed task off its home?**
  * In the default production configuration, no traced caller does so for an already-homed task.
    Spawn and fork place new tasks. The owner-revalidation rollback re-enqueues on the CPU the task
    was just dispatched on, which is its home.
  * The callers that could do so are reachable only from tests or from default-off knobs:
    * `apply_cross_cpu_wake_task`, reached only through the broad `handle_trap_entry` and the
      tests;
    * `enqueue_then_dispatch_on_cpu_split` and `live_ap_user_dispatch`, under
      `yarm.ap_user_dispatch=1`.

  "No migration" therefore holds in production **by caller discipline, not structurally**.
  SC-CPU-2 makes it structural.

### 1.5 Preemption and wake

* **A preempting tick is a yield with timer provenance.** `settle_recognized_timer` drives the
  same `run_yield_transaction` as NR 0.
* **Every decline postpones the preemption.** Every decline except `NotRunning` settles as
  `ContinueCurrent`, which postpones the preemption to a later tick. In the source's own words, "a
  quantum boundary that cannot switch simply extends the quantum". `NotRunning` panics.
* **A wake never preempts a running task.** A reschedule IPI/SGI that lands on a running CPU
  settles `SGI_RESCHEDULE_SETTLED … settlement=return_to_interrupted`
  (`handle_trap_entry_shared`): "the running task keeps running and the committed enqueue is
  picked up at its next scheduling point". Only an IPI taken at the authenticated idle boundary
  owes an immediate queue advance. So **priority is applied only at selection points**: the
  quantum boundary, a block, a yield, or idle. GP is quantum round-robin with priority-ordered
  selection, **not** fixed-priority preemptive. Low-priority work can starve behind runnable High
  or Normal work.

### 1.6 Time

* **What time is.** The scheduler tick is a count of the **clock owner's** timer interrupts
  (`CLOCK_OWNER_CPU` = the bootstrap CPU). Only `SchedulerState::tick_on(CLOCK_OWNER_CPU)`
  advances it. Every other CPU's `ap_quanta[cpu]` counts its own interrupts as a quantum and is
  never read as time. Tick arithmetic is `u64` `wrapping_*` (`src/kernel/time.rs`). IPC timeouts
  are expressed in these ticks.
* **Quantum.** `Timer` is a per-CPU quantum counter, not a per-task budget, and voluntary switches
  reset it. Per-port `SCHED_QUANTUM_TICKS` = 1 (x86_64), 2 (AArch64), 10 (RISC-V) interrupts.
* **Re-arm is relative, so the tick drifts.** Every port re-arms relative to the moment of re-arm:
  * x86_64: LAPIC one-shot initial count, divide-by-16.
  * AArch64: `cntp_tval_el0`.
  * RISC-V: SBI `set_timer(rdtime + interval)`.

  The tick period is therefore *interval + interrupt-to-rearm latency*: monotonic, but drifting
  and jittering.
* **Interval constants.** The `BOOTSTRAP_TIMER_DEADLINE_TICKS` constants are 50 000 000 LAPIC
  counts, 3 125 000 counter ticks and 100 000 timebase ticks. They are not calibrated against a
  discovered clock frequency in the traced timer path.
* **Idle secondaries.** An idle AArch64 or RISC-V secondary parks its timer behind an idle
  backstop of `AP_IDLE_BACKSTOP_PERIODS` = 100 quantum periods. The backstop bounds how long a
  CPU can miss work whose reschedule IPI failed. Every remote enqueue is followed by
  `hal::notify_enqueued(target)`, which sends that IPI.

### 1.7 Idle

The idle primitives (`idle_halt_loop`, `idle_no_eret_loop`, `riscv_trap_halt`) are entered by
**diverging** out of a trap handler, at a per-CPU stack anchor (`kernel::idle_boundary`). Idle is
not a context; it holds no continuation. A trap taken there may resume a user task only if
`idle_boundary::take_parked` authenticates that it interrupted the halt loop itself. That
authorization is published by the primitive and consumed destructively, exactly once.

### 1.8 Return paths and context authentication

* **Mark token.** A selected task is resumed only through a `DispatchMarkToken`, minted by
  `d6_genuine_mark_running_via_task_seam`. The token carries the exact `{tid, asid}` incarnation,
  the CPU and the `Running` mark, applied under one rank-2 acquisition.
* **Resume conventions.** Each port classifies a resume convention. x86_64 uses
  `direct_dispatch_classify_and_consume_convention_split` with `ExactTokenResume`. AArch64 uses
  `apply_restored_thread_state`. RISC-V's S-mode timer constructs a user return, choosing one of
  three conventions.
* **x86_64 CR3.** The pre-IRET CR3 invariant is `ensure_user_return_cr3_split`.
* **Timer authority.** A timer route captures the entering incarnation before ticking. It
  refuses (fatally) to return through a frame whose incarnation is not resumable.

### 1.9 Kernel-mode execution assumptions (relevant to KTHREAD1)

Every port assumes that kernel-mode execution, when interrupted or faulting, is **idle or fatal**:

* **RISC-V:** `riscv_trap_entry`'s `!from_u` arm admits exactly one supervisor trap, the audited
  kernel-idle timer. Everything else goes to `riscv_trap_halt("trap_from_s_mode")`.
* **x86_64:** a ring-0 `#GP` decodes as `Unknown` and is kernel-fatal. Every supervisor-origin
  page fault settles `PF3_SUPERVISOR_FAULT … settlement=kernel_fatal`
  (`guard_supervisor_origin`). A timer or IPI taken in ring 0 is converted to a user return only
  at the authenticated idle boundary.
* **AArch64:** current-EL vectors are classified by kind. An EL1 IRQ is converted only at the
  idle take point (`aarch64_take_point_return_spsr`).
* **Shared bridge:** `settle_unowned_trap` and the `idle_boundary` authorization assume that no
  other kernel continuation exists to resume.

### 1.10 FP/SIMD

* **User state.** The user policy is eager (`src/kernel/user_fpu.rs`). Every user trap captures
  FP/SIMD in assembly before any kernel instruction and commits it to the TCB. Every user return
  loads it. The image is FXSAVE64 on x86_64, `q0..q31` with FPCR/FPSR/TPIDR_EL0 on AArch64, and
  nothing on RISC-V (soft-float userspace, `FS/VS = Off`).
* **The kernel itself uses FP/SIMD registers on two ports.** This was **measured** at the
  baseline, on the default `--no-default-features` builds, by disassembly:
  * x86_64: 22 755 instructions reference `%xmm`. The custom target JSON declares no `features`,
    so it does not inherit `x86_64-unknown-none`'s soft-float.
  * AArch64: 2 429 instructions reference `q`/`v` registers (`+neon`).
  * RISC-V: 0 instructions reference FP registers.

  This was safe only because the kernel is non-preemptive and user state is saved first.
  **KERNEL-NOFP1 removed it** (§8.2). These counts are the baseline that package measured
  against.

### 1.11 Logging on scheduling-critical paths

`yarm_log!` expands to `pr_info!` followed by `printk_flush()`, on every call. `printk_flush`
takes the cross-CPU `PRINTK_DRAIN_LOCK` and drains the 128 × 192-byte ring to the UART
synchronously, polling the transmitter per byte. It runs inside traps with interrupts masked,
including the per-tick `TIMER_SPLIT_TICK_OK` marker, every queue-advance refusal marker and the
reschedule-IPI settlement marker. `printk_emit_sync` additionally writes the line directly.

### 1.12 Source comments found to disagree with the code (not inherited)

* `settle_recognized_timer`'s documentation says "no AArch64 AP arms a timer". AP-SCHED3 §C gave
  AArch64 secondaries their own timers, parked behind the idle backstop
  (`arch/aarch64/smp.rs`, `timer_distance_for_this_cpu`).
* `task_enqueue::fix_home_locked`'s documentation says "no enqueue path can pick a CPU for a task
  that already has one". This holds for Balanced plans only (§1.4).

---

## 2. Path inventory

Columns: **owner** (the scheduling owner reached), **CPU authority**, **locks** (ranks: 1
scheduler, 2 task, 3 IPC, 4 capability, 5 VM, 6 memory, 8 fault, 10 telemetry), **IRQ**
(interrupt state), **continuation** (whose saved context the CPU leaves with), **work** (the
bounded and potentially unbounded parts).

**Lock notation:**

* `a → b`: rank *a* is taken and **released** before rank *b* (sequential).
* `a(→b)`: rank *b* is taken **nested** inside *a*. This is ascending only. It is how the
  acceptance-filtered selection reads the task table under the scheduler lock.
* `a/b`: both ranks are taken, each released before the next, in an order the route determines.

**Common to every trap row:**

* Every trap row is IRQ-masked for the whole trap.
* Every trap row may spin on an unfair `SpinLockIrq`, with an unbounded wait (§6 W1).
* Every trap row may run synchronous logging (§6 W7).

| Path | Owner | CPU authority | Locks | IRQ | Continuation | Work |
|---|---|---|---|---|---|---|
| First dispatch at boot (x86_64 traced) | `enter_dispatched_user_task_if_available` under `BootExclusive` | the boot CPU, exclusive phase | none (exclusive token; published immediately before `iretq`) | masked | the first user task's initial context | bounded by boot sequence; AArch64/RISC-V equivalents not individually traced here |
| Secondary admission | `admit_ap_dispatch_split` → `SmpScheduler::admit_dispatch`; `install_ap_idle_current` | the admitting CPU's own trap/boot path | 1 | masked | none (idle) | O(1) |
| Timer, non-preempting | `settle_recognized_timer` → `scheduler_tick_split_mut` | `DispatchAuthority` | 1 | masked | interrupted task continues | O(1) + `TIMER_SPLIT_TICK_OK` log |
| Timer, preempting | `run_yield_transaction` → queue-advance drain | `DispatchAuthority`; entering incarnation captured first | 2 → 1, then drain 1(→2) | masked | outgoing saved by `capture_outgoing_user_context_split`; incoming via `DispatchMarkToken` | selection ≤ 192 candidates × rank-2 TCB scan ≤ 512 |
| Timer at idle boundary | `TimerIdleQueueAdvance` + drain | authority + `idle_boundary::take_parked` | 1(→2) | masked | idle has none; user return constructed | as selection |
| Yield (NR 0) | `run_yield_transaction`, Yield drain | authority | 2 → 1; drain 1(→2) | masked | as preempting timer | as selection |
| IPC send/recv blocking (D2) | `commit_blocking_send_split`, `recv_block_phase_a_split`; `d2_*_dispatch_step_mut` | authority | 2/3 sequential, then 1; drain 1(→2) | masked | caller's saved context + parked completion | endpoint/TCB scans ≤ fixed capacities |
| IPC direct call/reply (NR 6/7) | `direct_eligibility` classify → direct transaction → dispatch-debt drain (`direct_dispatch`) | authority + per-CPU dispatch lease | 2/3/4 sequential, then 1 | masked | typed, lease-bearing debt item | bounded by payload max and fixed slots |
| IPC timeouts | `run_due_ipc_timeout_work` (collect + reply/send/recv drains) | the trapping CPU | 2, 3, 1 | masked | timed-out caller's parked completion | TCB scan ≤ 512 per collection; deadline store 4 slots |
| Server death / reap / exit | `drain_server_death_post_work`, exit/terminal drains | authority | 2, 3, 4, 1 | masked | exiting task has none; replacement via token | per-object teardown; bounded by fixed tables |
| FutexWait | `try_split_futex_wait_recognized` → `futex_wait_park_exact_split` → drain | authority + per-CPU deferral cell | 2 → 1; drain 1(→2) | masked | caller parked; incoming via token | O(1) + selection |
| FutexWake | `futex_wake_split_mut` | trapping CPU | 2 (scan), then 1 per woken | masked | caller continues | ≤ 512 TCB scan; ≤ `max_wake` enqueues |
| Page fault (COW/demand recovered) | COW / demand routes | authority | 2, 5, 6 | masked | faulting task resumes | one frame allocation; page-table walk |
| Page fault (terminal) | terminal route + queue advance | authority + reserved deferral | 2, 8, 1 | masked | faulting task terminated | as selection |
| Overtaken deferral | `settle_overtaken_deferral_split` | authority | 1, 2 | masked | as recorded in the deferral | O(1) |
| Remote enqueue + IPI | `commit_enqueue_locked` then `hal::notify_enqueued` | the waking CPU names the target's home | 1 (enqueue); IPI with no lock | masked | none on the target until its next scheduling point | O(1); RISC-V IPI is an SBI call (§6 W5) |
| Reschedule IPI arrival | `handle_trap_entry_shared` SGI/0xF1 arm | `idle_boundary` authenticated or not | none / drain | masked | `return_to_interrupted` unless at idle | O(1) + log |
| TLB shootdown service | vector 0xF1 / SGI / SBI handlers (VMI1/VMI2) | target CPU | none on target | masked | interrupted context | invalidate + ACK; requester waits (§6 W4) |
| Diagnostic dispatch owners | D6 (`d6_genuine_*`), witnesses (`*-witness` features) | as their routes | as routes | masked | as routes | default-off; not production |
| Arch user return | x86_64 IRET + CR3 invariant; AArch64 `eret`; RISC-V `sret` | `DispatchMarkToken` / entering incarnation | none | masked → user | the token's incarnation | O(1) + FP/SIMD load |

---

## 3. RT-P1: architectural decisions (normative defaults)

The first real-time profile is the **smallest analysable configuration**:

* static CPU assignment and cyclic partition windows;
* fixed-priority preemptive scheduling inside the active partition;
* one budget per scheduling context, consumed on one CPU.

### 3.1 CPU assignment

* **SC-CPU-1 — Placement is confined to the named CPU.** **ENFORCED** for the scheduler. A
  placement mutates only the named CPU's queue, never displaces that CPU's running task, and
  leaves other CPUs untouched. Evidence: P `sc_cpu_1`;
  `withdraw_leaves_a_tid_queued_on_another_cpu_untouched`.
* **SC-CPU-2 — No migration after OPERATIONAL.** **SPECIFIED → RT-SCHED1.**
  * After the initialization-to-operational transition (SC-POOL-4), every scheduling context is
    bound to exactly one CPU. No enqueue may place its thread elsewhere.
  * Structurally: a Pinned plan for a task with a home must name that home, or fail closed. It
    must not silently override the home.
  * Today this holds by caller discipline only (§1.4).
* **SC-CPU-3 — A remote wake moves nothing.** **ENFORCED (GP) / SPECIFIED → RT-SCHED1 (RT-P1).**
  * A wake from any CPU changes only readiness. It never changes the woken context's CPU and
    never manufactures budget.
  * GP: wakes target the home (P `sc_cpu_1`; S `enqueue_woken_task`, `resolve_plan_home_split`).
  * RT-P1 budget half: M `model_remote_wake_keeps_cpu_and_manufactures_no_budget`.

### 3.2 Priorities and ordering

* **SC-PRI-1 — Strict priority at selection.** **ENFORCED (GP, 3 levels).** No lower level is
  selected while a higher level holds an *acceptable* candidate. P `sc_pri_1`.
* **SC-PRI-2 — Equal priority is FIFO and deterministic.** **ENFORCED.** P `sc_pri_2`.
* **SC-PRI-3 — Quantum expiry requeues at the tail of the task's own level.** **ENFORCED (GP).**
  P `sc_pri_3`.
* **SC-PRI-4 — A wake never displaces the running task (GP).** **ENFORCED (GP).** This is the
  baseline RT-P1 changes: P `sc_pri_4`; S §1.5.
* **SC-PRI-10 — RT-P1 has 32 fixed priority levels.** **SPECIFIED → RT-SCHED1.** Levels run
  0 (highest) to 31. A priority is a property of the scheduling context, fixed at configuration,
  and never changed at run time in RT-P1. There is no inheritance and no aging (§5.4).
* **SC-PRI-11 — RT-P1 equal-priority order is FIFO by readiness.** **SPECIFIED → RT-SCHED1.**
  There is no time-slicing among equals; budgets bound execution instead. M
  `model_equal_priority_order_is_deterministic_fifo`.
* **SC-PRI-12 — RT-P1 queue position rules.** **SPECIFIED → RT-SCHED1.**
  * M `model_preemption_yield_block_and_wake_queue_positions` checks (a) to (e).
  * (f) is checked by:
    * `model_exhausted_context_is_ineligible_despite_priority`;
    * `model_exhausted_thread_rejoins_the_tail_only_at_replenishment`;
    * `model_debt_consumed_replenishment_grants_no_position`;
    * `model_wake_while_exhausted_takes_no_position`;
    * `model_queue_membership_has_no_duplicates_or_stale_positions`.

  | Event | Queue position |
  |---|---|
  | (a) preempted by a strictly higher-priority eligible thread | **head** of its level |
  | (b) explicit Yield | **tail** of its level |
  | (c) block | removed |
  | (d) wake | **tail** of its level |
  | (e) displaced by a window boundary | keeps its position (head if it was running) for its next window |
  | (f) budget exhaustion | leaves its queue **entirely**, holding no position while exhausted. A wake of an exhausted thread gives it none either. It takes the **tail** only at a replenishment that leaves it budget (`remaining > 0`); one wholly consumed by debt gives it nothing (SC-CTX-4, SC-CTX-8) |

  *(Corrected by SCHED-CONTRACT1-CORRECTION: the delivered model appended the thread at
  exhaustion, so it kept an earlier position while ineligible. §13 records the reproduction.)*
* **SC-PRI-13 — In RT-P1, a wake is a scheduling point.** **SPECIFIED → RT-SCHED1.**
  * A wake of a strictly higher-priority eligible thread preempts the running thread on that CPU
    at its next preemption point.
  * A remote wake MUST deliver a preempting IPI, and its arrival on a running CPU MUST reschedule
    rather than `return_to_interrupted`.

### 3.3 Partitions and windows

* **SC-PART-1 — One cyclic schedule per system.** **SPECIFIED → PARTITION1.**
  * The schedule has a major frame `M > 0` ticks and an epoch `E` fixed at the OPERATIONAL
    transition.
  * Each window is `{cpu, partition, offset, duration}`, active on `[E + kM + offset,
    E + kM + offset + duration)`. Windows are half-open.
  * Instants between windows on a CPU are **gaps**: no partition is active, and the CPU runs only
    essential kernel work and idle.
* **SC-PART-2 — The highest priority cannot run outside its partition's window.** **SPECIFIED →
  PARTITION1.** M `model_highest_priority_cannot_run_outside_its_partition_window`.
* **SC-PART-3 — Invalid schedules are rejected at configuration, never repaired.** **SPECIFIED →
  PARTITION1.** Validation rejects each of the following with a distinct error, using checked
  arithmetic throughout:
  * zero frame;
  * no windows;
  * more than `MAX_WINDOWS` = 64 windows;
  * epoch overflow;
  * unknown CPU;
  * unknown partition;
  * zero duration;
  * `offset + duration` overflow;
  * a window beyond the frame;
  * overlap on one CPU;
  * a partition on two CPUs.

  M `model_invalid_overlapping_and_overflowing_schedules_are_rejected`.
* **SC-PART-4 — In RT-P1, a partition does not span CPUs.** **SPECIFIED → PARTITION1.**
  * Every window of a partition names the same CPU. Several partitions may share a CPU through
    disjoint windows.
  * This removes, by construction, any question of one window or one budget being consumed in
    parallel on two CPUs.
  * A multi-CPU partition is a later profile and needs its own budget-concurrency rule first.
* **SC-PART-5 — Boundary semantics.** **SPECIFIED → PARTITION1.**
  * A boundary is a fixed instant. The switch happens at the first preemption point at or after
    it. The delay (boundary latency) is charged to the CPU's system allowance (SC-EXEC-5).
  * **The incoming window is not extended and the outgoing window is not stretched.** Window
    durations must therefore include the boundary latency bound (UNRESOLVED, §6 W1/W3).
* **SC-PART-6 — Boundaries advance whether or not anything runs.** **SPECIFIED → PARTITION1.** An
  idle CPU, or a partition with nothing eligible, does not delay the next window. M
  `model_window_boundaries_advance_while_the_cpu_is_idle`.
* **SC-PART-7 — Empty windows.** **SPECIFIED → PARTITION1.**
  * When the active partition has nothing eligible, the CPU idles for the rest of the window.
  * RT-P1 performs **no slack donation** to other partitions. Slack reclaiming is a later
    profile.

### 3.4 Scheduling contexts and budgets

* **SC-CTX-1 — A scheduling context is bound at configuration.** **SPECIFIED → RT-SCHED1.** Each
  context is `{cpu, partition, priority, budget B, period T, budget owner}`, with `0 < B ≤ T`.
  In RT-P1 the budget owner is the context itself, with **exactly one thread per context**; there
  are no shared contexts.
* **SC-CTX-2 — Only the context's own thread consumes its budget.** **SPECIFIED → RT-SCHED1.** It
  consumes it only by executing on the context's CPU inside its partition's window, including the
  synchronous kernel work of its own traps (SC-EXEC-2).
* **SC-CTX-3 — An exhausted context is ineligible whatever its priority.** **SPECIFIED →
  RT-SCHED1.** M `model_exhausted_context_is_ineligible_despite_priority`.
* **SC-CTX-4 — Periodic replenishment with conserved repayment.** **SPECIFIED → RT-SCHED1.**
  * Replenishment happens at fixed release instants `E + kT`. Each one pays the debt it can and
    carries the rest:

    ```text
    paid      := min(B, debt)
    remaining := B − paid
    debt      := debt − paid
    ```

  * Debt is never forgiven. Budget 3 with debt 8 replenishes to (0, 5), (0, 2), then (1, 0).
  * The previous `remaining` is discarded, not added to: unused budget does **not** accumulate.
  * Sporadic servers are not part of RT-P1.
  * M:
    * `model_debt_beyond_one_replenishment_is_conserved`;
    * `model_debt_repayment_cases_and_no_budget_accumulation` (zero, below-, equal-to- and
      above-budget debt; repayment across periods with a fresh overrun between them; no
      accumulation);
    * `model_overrun_debt_is_repaid_at_replenishment`.

  *(Corrected by SCHED-CONTRACT1-CORRECTION: the delivered rule `remaining := B − debt,
  saturating`, then clearing all debt, silently forgave whatever one replenishment could not
  repay. §13.)*
* **SC-CTX-5 — Overrun.** **SPECIFIED → RT-SCHED1.**
  * Execution past exhaustion is an **overrun**. It arises from tick granularity and from a
    non-preemptible kernel section that outlived the budget.
  * It is recorded as debt, repaid from the next replenishment, and **reported** as a
    health-monitor event carrying the context, the amount and the instant. M
    `model_overrun_debt_is_repaid_at_replenishment`.
  * Debt accumulation uses checked arithmetic. It **never wraps and is never forgiven**. At the
    limit it saturates at `Tick::MAX` and is **reported** as a `DebtOverflow` health event for
    that context. M `model_debt_overflow_saturates_and_is_reported`.
  * **The overflow is a latched eligibility refusal** (SCHED-OVERFLOW1). Saturation sets a
    per-context fault latch.
    * While the latch is set, the eligibility predicate refuses the context **independently of
      its `remaining` and `debt` values**. It holds no queue position (SC-CTX-8).
    * Ordinary replenishment neither repays nor credits a latched context. Its release instants
      advance, but `remaining` stays 0.
    * Wake, yield, block, window changes and other contexts' scheduling never clear or bypass
      the latch.
    * Only an explicit recovery owner may clear it, and none exists yet. Until one is specified
      (PARTITION1's health monitor), a latched context stays ineligible for good.
  * **Saturation loses the exact numeric excess.** The debt reads `Tick::MAX`, and the amount
    beyond it is unknown. The latch is what stops that loss from becoming usable budget: the
    context cannot run on the saturated value, nor on any later value of its counters.
  * The refusal does not rely on the counter horizon. In the 64-bit model, repaying a saturated
    debt needs more than 2^64 ticks of replenishment, and the model's own time overflows first.
    That incidental bound would not survive a narrower counter or a recovery path, so the
    contract states the latch explicitly.
  * M:
    * `model_overflow_latch_holds_across_scheduling_events` (reached organically);
    * `model_overflow_latch_refuses_restored_budget_constructed`;
    * `model_overflow_latch_survives_replenishment_constructed`.

    The last two set `remaining` and `debt` directly, and are labelled as constructed state
    (§15).
  * Recovery policy is configuration (PARTITION1's health monitor). It is selected per context
    and never inferred from priority.
* **SC-CTX-6 — Enforcement granularity.** **UNRESOLVED.**
  * Missing fact: a time source with sub-quantum resolution, and a per-context accounting point
    at every trap entry and exit.
  * Owner: RT-SCHED1, after SC-TIME-3.
  * Until then a budget can be enforced only to tick granularity, and overrun bounds inherit §6
    W3.
* **SC-CTX-7 — Same-instant event order.** **SPECIFIED → RT-SCHED1/PARTITION1.** Events falling
  on one instant are applied in this order:
  1. charge the running contexts for the elapsed tick;
  2. advance time;
  3. apply every replenishment whose release instant has arrived, in **ascending context id**.
     Each exhausted Ready thread left with budget joins its tail in that order;
  4. on each CPU, in ascending CPU id, re-evaluate the running thread, then select. A window
     boundary displaces it to the head (SC-PRI-12e). Exhaustion removes it from the queues
     (SC-PRI-12f).

  Consequences:
  * A running thread that exhausts and is replenished at the same instant never becomes
    ineligible, and keeps running.
  * A running thread whose window ends at its replenishment instant is replenished first, then
    displaced to the head.
  * A running thread that is both outside its window and exhausted after step 3 takes **no**
    position: exhaustion overrides the head placement of (a) and (e) until a replenishment.
  * M `model_simultaneous_replenishments_rejoin_in_context_order`.
* **SC-CTX-8 — Queue membership.** **SPECIFIED → RT-SCHED1.** At every instant:
  * every Ready thread with budget holds **exactly one** queue position;
  * every exhausted Ready thread holds **none**;
  * Running and Blocked threads hold none.

  No path — wake, block, yield, preemption, boundary, charge or replenishment — may create a
  duplicate or a stale position. M `model_queue_membership_has_no_duplicates_or_stale_positions`
  (randomized, invariant checked after every operation).

### 3.5 Separation of scheduling attributes

* **SC-SEP-1 — Five attributes, each with one job.** **SPECIFIED → RT-SCHED1.**

  | Attribute | Role |
  |---|---|
  | **CPU affinity** | where the context may run |
  | **partition eligibility** | when it may run |
  | **budget eligibility** | how much it may run |
  | **priority** | the order among eligible threads only |
  | **criticality** | configuration-time input to overload and fault policy only; **never** consulted by selection |

  The dispatch predicate is `affinity ∧ partition-window ∧ budget ∧ ready`, and only then is
  priority applied. **Priority never overrides a partition boundary or an exhausted budget**
  (SC-PART-2, SC-CTX-3). M `Model::eligible`.
* **SC-SEP-2 — Eligibility attaches to the existing acceptance-filtered selection.** **ENFORCED
  (mechanism) / SPECIFIED → RT-SCHED1 (predicate).**
  * The production selection already takes an acceptance predicate. It skips refused candidates
    in place, and it measures exhaustion as a count (P `sc_elig_1`).
  * RT-SCHED1 extends that predicate with window and budget. It must not add a second selection
    loop.

### 3.6 Bounded pools and the operational transition

* **SC-POOL-1 — Run queues are fixed-capacity, and exhaustion is explicit.** **ENFORCED.** P
  `sc_pool_1`.
* **SC-POOL-2 — RT-P1 objects come from fixed pools sized at configuration.** **SPECIFIED →
  RT-SCHED1/PARTITION1.**
  * Scheduling contexts, partitions, windows (≤ 64), per-(CPU, partition, priority) ready queues,
    kernel worker stacks (KTHREAD1) and health records are all fixed pools.
  * Exhaustion returns a typed error and never falls back.
* **SC-POOL-3 — No run queue may be smaller than its admissible population.** **UNRESOLVED.**
  * Missing fact: the per-CPU bound on runnable threads in an RT-P1 configuration, which is the
    configuration's thread-to-context map.
  * Today's 64-per-level ring can refuse an admissible wake (`QueueFull`) with 512 TCB slots.
  * Owner: RT-SCHED1, by sizing queues from the configuration or rejecting the configuration.
* **SC-POOL-4 — The INITIALIZATION → OPERATIONAL transition.** **SPECIFIED → PARTITION1.**
  * The transition is one-way. Before it, GP boot scheduling applies (BSP-first, the current
    `BootExclusive` phase and admission).
  * At the transition, all of the following happen at one instant: the schedule is validated
    (SC-PART-3); every context is bound (SC-CTX-1); every pool is allocated; the epoch `E` is
    fixed.
  * After it, no context, partition, window or pool is created or destroyed. Thread creation
    binds only to a pre-allocated context slot.

---

## 4. Execution-time ownership

* **SC-EXEC-1 — Every category of execution has an owner.** **SPECIFIED → RT-SCHED1.** Every
  category below has an explicit accounting rule or a reserved system allowance. **There is no
  unlimited "kernel time" exemption.**

| Category | Today (source) | RT-P1 owner |
|---|---|---|
| User execution | per-CPU quantum (`Timer`); no per-task accounting | the running context's budget |
| Synchronous kernel work in the caller's trap (syscall, fault) | inside the caller's quantum | **SC-EXEC-2**: the caller's context |
| Independent kernel workers | do not exist (§1.9) | **SC-EXEC-3**: each worker has its own context (CPU, partition or system partition, priority, budget); never unbudgeted |
| Interrupt top halves (timer, device, IPI) | run in whatever trap; uncounted | **SC-EXEC-5**: the CPU's system allowance |
| Deferred interrupt work | run inline in the trap today | **SC-EXEC-3**: the worker context bound to the device's partition |
| IPC server execution | the server runs under its own quantum | the server's own context (no donation, SC-IPC-2) |
| IPC timeout handling (`run_due_ipc_timeout_work`) | inline in the post-lock area of any trap | **SC-EXEC-5** (collection) + the woken caller's own context thereafter |
| Cleanup (server death, reap, exit teardown) | inline in the exiting or reaping trap | the requesting thread's context (SC-EXEC-2); asynchronous remainder → a worker (SC-EXEC-3) |
| Remote requests (TLB shootdown service, reschedule IPI) | inline in the target's trap; the requester waits for ACK | **SC-EXEC-5** on the target; the requester's own context for its wait |
| Essential scheduler work (tick, selection, switch, boundary) | inline | **SC-EXEC-5** |

* **SC-EXEC-2 — Synchronous kernel work is charged to the caller.** **SPECIFIED → RT-SCHED1.**
  It is charged to the context of the thread whose trap performs it. M (`KernelWork::
  CallerSynchronous`) `model_overrun_debt_is_repaid_at_replenishment`.
* **SC-EXEC-3 — Deferrable work runs in a budgeted worker.** **SPECIFIED → KTHREAD1 / RT-SCHED1.**
  Deferrable kernel work runs in an independent kernel worker under its own budgeted context. It
  is never charged to whichever thread happens to run. M (`KernelWork::Worker`)
  `model_essential_kernel_work_is_charged_and_overrun_reported`.
* **SC-EXEC-4 — Essential and deferrable work are classified by name.** **SPECIFIED →
  RT-SCHED1.** "Essential" means the work cannot be deferred without violating a kernel
  invariant: the interrupt acknowledgement, the scheduler's own decision, a requested
  invalidation and its ACK, a boundary switch. Everything else is deferrable. Every essential
  item is named in an enumerable list, not inferred.
* **SC-EXEC-5 — Each CPU has a reserved system allowance per major frame.** **SPECIFIED →
  PARTITION1.**
  * Essential work is charged to the allowance.
  * Work beyond the allowance is still performed (it is essential) and is recorded as a
    `SystemAllowanceOverrun` with its amount.
  * The allowance renews each frame.
  * The overrun is **not** silently absorbed: it consumes time inside whatever window is active,
    and that window is not extended.

  M `model_essential_kernel_work_is_charged_and_overrun_reported`.
* **SC-EXEC-6 — The allowance is sized from a bound.** **UNRESOLVED.**
  * Missing fact: the per-frame upper bound on essential work per CPU. That bound is the
    interrupt arrival rates × top-half WCET + shootdown requests × service WCET + scheduler-path
    WCET.
  * None of these is established (§6).
  * Owner: RT-SCHED1 (scheduler-path bounds), PARTITION1 (rate configuration), and hardware
    bring-up (device rates).

### 4.1 IPC handoff is not budget donation

* **SC-IPC-1 — A handoff satisfies the same eligibility as selection.** **SPECIFIED →
  RT-SCHED1.**
  * A synchronous call, reply or direct handoff makes its target *ready*. It runs only if normal
    selection on the target's own CPU would choose it.
  * It may not run outside its window, with an exhausted budget, or on another CPU.
  * **Today:** delivery eligibility (`direct_eligibility`) is separate from scheduling, and every
    production handoff reaches the ordinary acceptance-filtered selection. The one
    preferred-target primitive re-installs the trapping task itself (§1.3), not a different
    target.

  M `model_ipc_handoff_cannot_bypass_eligibility_or_donate_budget`.
* **SC-IPC-2 — Budget donation is disabled.** **SPECIFIED → RT-SCHED1.**
  * A server executes and pays under its own context.
  * Donation stays disabled until all of the following are specified and implemented: the
    donated budget's owner, nested calls, timeouts and cancellation mid-donation, and server death
    while holding a donated budget.
* **SC-IPC-3 — Cross-partition IPC does not extend either window.** **SPECIFIED → PARTITION1.**
  The call does not extend the caller's window, and the reply does not extend the server's. A
  server preempted at its window end resumes in its next window. M
  `model_cross_partition_ipc_does_not_extend_either_window`.

### 4.2 Partition boundaries and unfinished work

* **SC-BND-1 — Boundaries never preempt the kernel today.** **ENFORCED (structurally, today).**
  The kernel is non-preemptive (§1.2) and no kernel lock is held across a return to user mode. So
  a boundary can only take effect at a trap boundary, and it can never abandon a kernel lock
  holder or an unfinished ownership transition: the post-lock drains, the per-CPU deferral cells
  and the dispatch-debt items. Each of these is settled inside its trap before the CPU returns.
  The cost is boundary latency (SC-PART-5).
* **SC-BND-2 — With kernel threads, boundaries take effect only at safe points.** **SPECIFIED →
  KTHREAD1.** Once kernel threads exist, a boundary or budget-exhaustion preemption of a kernel
  thread takes effect only at a **preemption point**. A preemption point holds:
  * no ranked lock;
  * no `PRINTK_DRAIN_LOCK`;
  * no reserved per-CPU deferral cell;
  * no open `TrapPathWindow`;
  * no pending dispatch debt.

  The bound is the longest such region (UNRESOLVED, §6).
* **SC-BND-3 — No user-level lock is shared across partitions in RT-P1.** **SPECIFIED →
  PARTITION1.** RT-P1 forbids futex words or other user-level locks in memory shared between
  partitions. A cross-partition lock holder can be suspended at its window end for up to one
  major frame, which is a bounded but large inversion. Configuration validation must reject
  shared writable regions mapped into two partitions, unless the region is declared lock-free
  by configuration.

### 4.3 Priority inversion

* **SC-INV-1 — Kernel spinlock blocking.** **UNRESOLVED.**
  * Kernel spinlocks are taken non-preemptively with IRQs masked, so in-CPU inversion is bounded
    by the holder's section.
  * Cross-CPU waiting on the TTAS `SpinLockIrq` has **no bound**: there is no fairness, so a
    waiter can lose every race.
  * Missing fact: a fair (FIFO/ticket or queue) lock, or a proof of bounded contention, plus the
    maximum critical-section length per rank.
  * Owner: RT-SCHED1.
  * **Priority inheritance does not bound this:** the waiter is not a schedulable entity while
    it spins.
* **SC-INV-2 — IPC servers run at or above their clients.** **SPECIFIED → RT-SCHED1.**
  * RT-P1 bounds client/server inversion by **configuration**, not by inheritance. A server's
    priority must be ≥ the highest priority of any client that can call it, in its partition: an
    immediate ceiling.
  * The rule is checked at the OPERATIONAL transition.
  * Inheritance alone would not bound nested calls or chains, and it is not part of RT-P1.
* **SC-INV-3 — User-level locks carry no kernel inheritance.** **SPECIFIED → RT-SCHED1.**
  * Futexes provide no priority inheritance. Within a partition, threads sharing a futex-backed
    lock must be configured at one priority or use a ceiling protocol in user space.
  * Across partitions, SC-BND-3 applies.
* **SC-INV-4 — Today the run queue is unbounded inversion.** **ENFORCED as a known property
  (GP).** A runnable High task can wait behind a running Low task for a whole quantum
  (SC-PRI-4). It waits indefinitely if declines keep postponing the preemption (§1.5). This is
  GP behaviour and is acceptable only for GP.

---

## 5. Timer contract

* **SC-TIME-1 — The scheduler tick is monotonic.** **ENFORCED.** It is incremented by exactly one
  per clock-owner timer interrupt, with `u64` wrapping arithmetic. S `Timer::tick_and_check`,
  `TickDuration::has_elapsed_since`.
* **SC-TIME-2 — There is one global time source.** **ENFORCED.** Only `CLOCK_OWNER_CPU` advances
  time. Every other CPU's interrupts count only its own quantum. Adding a CPU never multiplies
  timeout progression. S `SchedulerState::tick_on`; L AP-SCHED1 to AP-SCHED4 live witnesses.
* **SC-TIME-3 — Time is not uniform.** **UNRESOLVED.**
  * The tick period is *interval + interrupt-to-rearm latency* (§1.6), so it drifts and jitters.
    The interval constants are not calibrated against a discovered frequency.
  * Missing facts:
    * a free-running hardware counter per port, with its frequency discovered (x86_64 TSC
      invariance and frequency; AArch64 `CNTFRQ_EL0`; the RISC-V timebase from the device tree);
    * absolute-deadline re-arming from that counter.
  * Owner: RT-SCHED1.
* **SC-TIME-4 — Every CPU must see the same time.** **UNRESOLVED.**
  * RT-P1 windows on CPU *n* are instants of the global time. Every CPU must therefore read, or
    derive, the same time.
  * Missing fact: cross-CPU counter synchronization per port. AArch64's system counter and
    RISC-V's `time` are architecturally shared; x86_64 TSC synchronization is not established.
  * Owner: RT-SCHED1.
  * Until it is established, only `CLOCK_OWNER_CPU` time is valid. Other CPUs' boundaries must be
    signalled by the clock owner, at a cost of IPI latency.
* **SC-TIME-5 — The next hardware deadline is the earliest pending event.** **SPECIFIED →
  RT-SCHED1/PARTITION1.**
  * The next deadline programmed on a CPU is the earliest of:
    * the next partition boundary;
    * the running context's budget-exhaustion instant;
    * the earliest timed wake or IPC timeout owned by that CPU;
    * the next replenishment of an eligible context;
    * the next required health-monitoring instant.
  * The periodic tick is retained until RT-SCHED1. A **tickless** implementation is deferred and
    is not part of RT-P1.
* **SC-TIME-6 — Time has a unit.** **UNRESOLVED.**
  * IPC timeouts, quanta and every RT-P1 duration are in ticks today, and ticks have no fixed
    duration.
  * Missing fact: the tick-to-nanosecond conversion, after SC-TIME-3.
  * Owner: RT-SCHED1.

---

## 6. Timing audit of scheduling-critical paths

| Kind | Meaning |
|---|---|
| **S** | Structural operation-count bound from source |
| **A** | Bound requiring assumptions about another CPU or device |
| **M** | Measurement |
| **N** | Architectural or hardware timing bound not yet established |

A fixed array, a passing stress test or a lock-free algorithm is **not** by itself a WCET proof,
and nothing below is claimed as one. **No measurement was taken in this package.** QEMU timing is
not representative of silicon, and no target hardware is in the loop.

| # | Item | Where | Kind | Bound |
|---|---|---|---|---|
| W1 | `SpinLockIrq` / `SpinLock` acquisition wait | `kernel/lock.rs` | **A / N** | No structural bound: TTAS with no fairness. A bound needs a fair lock **and** every other CPU's maximum hold time. A weak-CAS spurious failure adds unbounded retries on LL/SC ports in theory. |
| W2 | Run-queue selection (accepted) | `dispatch_next_accepted_selection` | **S** | ≤ 3 × 64 = 192 candidates. Each runs one rank-2 acquisition and a TCB linear scan ≤ 512. Operation count O(192 × 512) per selection, with IRQs masked. Time **N**. |
| W3 | Whole trap (IRQ-masked region) | every route | **N** | The sum of its route's work, W1, W7, allocations (W6) and page-table walks. No per-route bound is established. This is also the boundary latency (SC-PART-5) and the overrun bound (SC-CTX-5). |
| W4 | TLB shootdown completion | VMI1/VMI2 owners | **A** | The requester waits for generation-matched ACKs. A bound needs every target to take its IPI within its own W3. A target cannot take it while it is in a masked trap. |
| W5 | Firmware calls | RISC-V SBI `set_timer`, `send_ipi` | **N** | OpenSBI execution time is not established. |
| W6 | Frame allocation on fault/spawn paths | memory rank 6 | **S/N** | A fixed-capacity allocator. The search bound is not analysed here. |
| W7 | Synchronous logging | `yarm_log!` → `printk_flush` | **A** | Up to 128 × 192 bytes per flush, at UART rate, polled per byte, plus cross-CPU `PRINTK_DRAIN_LOCK` contention. It runs on every tick and on many refusal and settlement markers. This is the largest **known** variable term. |
| W8 | Membership table | `PriorityScheduler::membership_*` | **S** | ≤ 256 probes; the linear fallback is ≤ 192 + 1. |
| W9 | `RingQueue::remove_tid` | scheduler | **S** | ≤ 64 shifts. |
| W10 | FutexWake | `futex_wake_split_mut` | **S** | ≤ 512 TCB scan + ≤ `max_wake` (≤ 512) enqueues. |
| W11 | IPC timeout collection | `collect_due_ipc_timeout_work` | **S** | TCB scan ≤ 512; deadline store 4 slots. |
| W12 | Retry loops in selection | queue-advance owner | **S** | None. The acceptance-filtered selection is single-pass. Contended marks are reported, not retried. |
| W13 | Timer period | per-port re-arm | **N** | Relative re-arm; drift and jitter (SC-TIME-3). |
| W14 | Idle-CPU work latency after a failed IPI | AArch64/RISC-V backstop | **S (in ticks)** | ≤ `AP_IDLE_BACKSTOP_PERIODS` = 100 quantum periods. In time: **N**. |

**SC-WCET-1 — Every claimed bound names its kind.** **SPECIFIED → all packages.** Every timing
bound claimed by RT-SCHED1, PARTITION1 or KTHREAD1 states its kind (S/A/M/N) as in the table. A
bound of kind A names its assumption and the owner that discharges it. No package may convert an
S operation count into a time bound without an N bound for the instruction sequence on named
hardware.

**SC-WCET-2 — RT-P1 removes synchronous console output from scheduling paths.** **SPECIFIED →
RT-SCHED1.** No synchronous console output on any path covered by SC-EXEC-5 or W2/W3 in RT-P1.
Diagnostics are recorded in bounded per-CPU buffers and drained by a worker (SC-EXEC-3). Today
this is violated by W7.

---

## 7. The general-purpose profile, through the same mechanisms

**SC-GP-1 — GP is a configuration of the RT-P1 mechanisms.** **ENFORCED (differential, for the
mechanisms exercised).**

GP is one partition per CPU whose single window spans the whole major frame, so it is always
active. It also has:

* three priority levels (High, Normal, Low);
* no budget limit on user execution (`Budget::Unlimited`, the GP configuration only);
* wake is not a scheduling point (SC-PRI-4);
* quantum expiry is a Yield (tail).

Kernel-work accounting (§4) still applies to GP once implemented. There is no unlimited kernel
exemption in either profile.

Evidence: P `sc_gp_1_general_purpose_model_agrees_with_production_priority_scheduler`. The model,
configured as GP, matches the production `PriorityScheduler` decision for decision. That covers 24
seeded random sequences of 3 000 operations each: wake, dispatch point, quantum expiry and block.
The test proves agreement for those operations on one CPU. It does not prove anything about
placement, IPC, time or the RT-P1 rules.

**SC-GP-2 — Profiles are deployment configurations.** **SPECIFIED → RT-SCHED1/PARTITION1.** GP
and RT-P1 share the production dispatch and lifecycle owners:

* one selection owner (`queue_advance_select_step_split`);
* one enqueue planner (`task_enqueue`);
* one transition owner (`task_transition`);
* one mark token.

RT-SCHED1 changes GP behaviour only by deliberate, separately qualified changes to those owners,
never by adding an alternative dispatcher selected by profile.

---

## 8. KTHREAD1 prerequisites (all three ISAs)

**SC-KT-0 — The eager user FP/SIMD policy is kept.** **ENFORCED.** The qualified user policy
(§1.10) is unchanged by KTHREAD1 unless a later package proves an alternative.

| ID | Prerequisite | x86_64 today | AArch64 today | RISC-V today | Status |
|---|---|---|---|---|---|
| SC-KT-1 | Owned kernel stack, from a bounded pool, with a guard page; distinct from user and idle stacks (idle is a diverging primitive at a per-CPU anchor, §1.7). Stacks are pre-mapped: supervisor page faults are kernel-fatal (§1.9). | `KernelExecutionContext {stack_base, stack_top, owns_stack}` exists (D6 scaffolding); `refresh_boot_tss_rsp0` | same field | same field | **SPECIFIED → KTHREAD1** |
| SC-KT-2 | Kernel continuation saved and resumed, distinct from the user `UserRegisterContext` | `yarm_x86_switch_frame` (callee-saved, `rsp`, `rip`) | `yarm_aarch64_switch_frame` | `yarm_riscv64_switch_frame` | **SPECIFIED → KTHREAD1** (cooperative switch exists on all three; production-dormant) |
| SC-KT-3 | First entry: an explicit entry ABI (stack alignment, argument registers, return-to-exit) | `yarm_kernel_thread_switch_trampoline` (D6 proof) | not established | not established | **SPECIFIED → KTHREAD1** (canonical 202A already requires the ABI contract) |
| SC-KT-4 | Asynchronous preemption of kernel mode. Not to be enabled before SCHED-LOCK1 (SC-LOCK-8) and KERNEL-NOFP1 (SC-FP-1); both are done, and neither enables or qualifies it. | none: masked kernel | none | none; S-mode traps other than idle halt | **SPECIFIED → KTHREAD1**, gated by SCHED-LOCK1 and KERNEL-NOFP1 |
| SC-KT-5 | Kernel FP/SIMD state. Ordinary kernel code and its linked runtime helpers use no FP/SIMD registers, so a kernel thread carries no FP/SIMD state; audited context routines are the exceptions (§8.2). | 0 in ordinary code (was 22 779 FP/SIMD-classified instructions) | 0 in ordinary code (was 2 547) | 0 | **ENFORCED by KERNEL-NOFP1** (final-image check on every artifact build) |
| SC-KT-6 | Flags and TLS: the kernel's per-CPU base and flags are restored on a kernel-to-kernel switch | not traced in this package | not traced | not traced | **UNRESOLVED**: missing inventory of kernel per-CPU and TLS registers; owner KTHREAD1 |
| SC-KT-7 | Interrupt nesting and safe preemption points: preemption only at points holding none of SC-BND-2's items | no nesting | no nesting | no nesting | **SPECIFIED → KTHREAD1** |
| SC-KT-8 | No blocking or switching while a prohibited guard is held. Today non-hosted lock-rank tracking is placeholder-only (`doc/KERNEL_LOCKING.md`). A per-CPU held-guard count must make a switch under a guard fail closed. | `SpinLockIrqGuard` is `!Send`; no run-time check | same | same | **SPECIFIED → KTHREAD1** |
| SC-KT-9 | Wake arbitration through the existing exact-incarnation owners and `DispatchMarkToken`; no kernel-thread-only wake path | shared owners | shared | shared | **SPECIFIED → KTHREAD1** |
| SC-KT-10 | Exit and stack reclamation only after execution has provably ceased: a stack-residency record, cleared only after the CPU has switched away and is no longer on that stack (by analogy with VMI2's root residency) | none | none | none | **SPECIFIED → KTHREAD1** |
| SC-KT-11 | Trap classifiers accept kernel-thread-origin traps. Every "kernel mode is idle or fatal" site in §1.9 must distinguish a kernel thread from idle and from a kernel bug. | ring-0 `#GP`, supervisor `#PF`, idle-only IRQ conversion | current-EL vectors, idle take point | `!from_u` → halt | **SPECIFIED → KTHREAD1** |
| SC-KT-12 | Every kernel thread has a budgeted scheduling context (SC-EXEC-3) and a home CPU (SC-CPU-2) | — | — | — | **SPECIFIED → KTHREAD1 / RT-SCHED1** |

---

### 8.1 Kernel FP/SIMD direction: KERNEL-NOFP1, then FP-POLICY1

* **SC-FP-1 — KERNEL-NOFP1: ordinary kernel code uses no FP/SIMD.** **ENFORCED** (KERNEL-NOFP1,
  §8.2).
  * Ordinary kernel code **and its linked runtime helpers** (compiler-generated `memcpy`,
    `memset` and similar builtins included) must not use FP/SIMD registers on any port.
  * **Audited exceptions** are the explicit architectural routines that *must* touch that state:
    * FP/SIMD initialization;
    * the user FP/SIMD save and restore of the eager policy (SC-KT-0);
    * any FP/SIMD context save and restore a later package adds.

    Each exception is an exact address range recorded next to its code and reviewed; §8.2
    lists them. A range is narrower than a symbol: it names the instructions, not the routine.
* **SC-FP-2 — Qualification inspects the final linked images.** **ENFORCED** (KERNEL-NOFP1).
  * Qualification disassembles the final linked kernel image of every port.
  * Every **decoded** instruction that reads or writes an FP/SIMD register must lie inside an
    audited exception range. The only bytes left undecoded are those of a reviewed, anchored
    exclusion (embedded data, embedded user programs, AArch64 literal pools); an annotation in
    the image cannot create one on its own (§8.2, KERNEL-NOFP1-ACCEPTANCE).
  * A blanket "zero FP/SIMD instructions anywhere" check is **wrong**: it would reject the
    required context routines. A source-level or flag-level check is **insufficient**: it would
    miss compiler-generated helpers.
  * Today's baseline for that inspection is §1.10 (22 755 / 2 429 / 0). That count was of
    register names. The checker's classification also counts x87 and implicit-state
    instructions, which gives 22 779 / 2 547 / 0 on the same base images.
* **SC-FP-3 — Scope limits of KERNEL-NOFP1.** **ENFORCED** (KERNEL-NOFP1 kept every one).
  * The current eager userspace FP/SIMD preservation and the exact continuation-owner
    authentication stay **unchanged**.
  * This contract prescribes no particular compiler flag, target feature string or
    calling-convention change. Choosing a supported mechanism, and proving it by SC-FP-2, is the
    package's job.
  * There is **no** exception-driven lazy state allocation and **no** lazy ownership switching.
* **SC-FP-4 — FP-POLICY1: static per-task FP policy, recorded separately.** **SPECIFIED →
  FP-POLICY1** (after KERNEL-NOFP1).
  * A later package may give each task a *static* FP policy and skip preservation for tasks that
    declare no FP use.
  * A task flag alone **cannot** enforce non-use and **cannot** prevent state disclosure. Before
    any preservation is skipped, FP-POLICY1 must define all of the following:
    * instruction access: the architectural trap or disable control per port that makes non-use
      enforced, not trusted;
    * violation handling: what happens to a task that uses FP/SIMD against its policy;
    * initialization: the register state a policy-enabled task starts from;
    * isolation: that no other task's FP/SIMD state is ever observable;
    * lifecycle: fork, exec, thread creation and policy change.
* **SC-FP-5 — Cost and remaining limits.** **PARTLY RESOLVED.**
  * The mechanism and the code-size and static-frame cost are measured (§8.2).
  * Execution-time cost of the integer replacements on real hardware is **unmeasured**. Only
    QEMU runs exist, and QEMU timing is not hardware timing. Owner: hardware bring-up.
  * Neither this decision nor its implementation is a WCET or certification claim.

### 8.2 KERNEL-NOFP1: what was implemented

**Build rules (from the toolchain, not copied flags).** Each kernel target takes the
configuration the toolchain ships for a soft-float kernel on that ISA:

| Port | Kernel target | Change | User target |
|---|---|---|---|
| x86_64 | `targets/x86_64-yarm-none.json` | `features` and `rustc-abi` of the built-in `x86_64-unknown-none`: `-mmx,-sse,-sse2,-sse3,-ssse3,-sse4.1,-sse4.2,-avx,-avx2,+soft-float`, `rustc-abi: softfloat`. It previously declared no features and so inherited SSE2 | unchanged (`-mmx,+sse2`) |
| AArch64 | `targets/aarch64-yarm-none.json` | `features` and ABI of the built-in `aarch64-unknown-none-softfloat`: `-neon` (kept `-lse`), `abi`/`rustc-abi: softfloat` | unchanged (`+neon`) |
| RISC-V | built-in `riscv64gc-unknown-none-elf` | **none**. The kernel never contained an FP or vector instruction. Changing the ABI to `lp64` would change the kernel's calling convention and ELF float ABI for no gain, so the rule is enforced on the final image instead | unchanged (soft-float `lp64`, FS/VS Off) |

* `core`, `alloc` and `compiler_builtins` are rebuilt for the kernel target (`-Z build-std`), so
  the linked runtime helpers follow the same configuration. This is what removed the SIMD from
  `memcpy`/`memset`-class code, not only from handwritten Rust.
* Calling conventions between kernel components are unchanged: every kernel component is built
  with the same target. Userspace is built by its own targets, and the syscall ABI is
  register-based (no FP registers cross it).
* Toolchain: `rustc 1.101.0-nightly (32dba69d6 2026-10-09)`, LLVM 23.1.3. No toolchain change.
* The toolchain reports one *future-incompatibility* note in `core` for these targets (its NEON
  and SSE intrinsics enable features the soft-float ABI forbids). The built-in
  `x86_64-unknown-none` and `aarch64-unknown-none-softfloat` targets produce the same note when
  `core` is built from source. It is inherited, not introduced, and those intrinsics are never
  linked (the image check proves it).

**Audited exceptions.** Each is an exact range in the source, two local labels around the
instructions, recorded in the image's non-loaded `.yarm_nofp_map` section (never part of a loaded
segment). The checker holds, per kind, the exact ordered FP/control instructions the range may
contain, a size bound, and the instance count. On AArch64 the assembler accepts FP/SIMD only
inside these ranges (`.arch_extension fp/simd` … `nofp/nosimd`), so an FP/SIMD instruction anywhere
else fails to build.

| Kind | Instances | Exact FP/control instructions | Purpose |
|---|---|---|---|
| `X86_TRAP_ENTRY_CAPTURE` | 2 (common trap entry, `syscall` entry) | `fxsave64 [rsp]`, `fninit`, `ldmxcsr [YARM_X86_KERNEL_MXCSR]` | capture the interrupted x87/SSE state before any kernel instruction; install the kernel's own control environment |
| `X86_TRAP_ENTRY_RESTORE` | 2 | `fxrstor64 [rsp]` | restore after the last kernel instruction |
| `X86_FIRST_ENTRY_RESTORE` | 1 | `fxrstor64 [YARM_X86_INITIAL_FXSAVE]` | first ring-3 entry gets the architectural initial image |
| `X86_RESUME_RESTORE` | 1 | `fxrstor64 [rsi]` | saved-frame resume installs the task's own home |
| `X86_SWITCH_SAVE` / `X86_SWITCH_RESTORE` | 1 / 1 | `fxsave [rdi+64]` / `fxrstor [rsi+64]` | existing kernel switch-frame state (kept, not removed) |
| `X86_SWITCH_FRAME_INIT` | 1 | `fninit`, `fxsave [reg]` | seed a new switch frame's area from the reset state |
| `X86_KERNEL_FP_ENV_INIT` | 1 | `fninit`, `ldmxcsr [YARM_X86_KERNEL_MXCSR]` | per-CPU kernel control environment |
| `X86_CR0_WRITE` / `X86_CR4_WRITE` | 4 / 4 | `mov crN, reg` | boot, AP trampoline (16/32-bit), AP CR4 sync, per-CPU FP control (EM/TS/MP/NE, OSFXSR/OSXMMEXCPT) |
| `A64_VECTOR_CAPTURE` | 1 | `stp q0..q31` (16), `mrs fpcr`, `mrs fpsr`, `msr fpcr, xzr` | capture before the first `bl`; kernel FPCR |
| `A64_VECTOR_RESTORE` | 1 | `ldp q0..q31` (16), `msr fpcr`, `msr fpsr` | restore after the last `bl` |
| `A64_FIRST_ENTRY_INIT` | 1 | `movi v0..v31, #0` (32), `msr fpcr, xzr`, `msr fpsr, xzr` | first EL0 entry: architectural zero state |
| `A64_SWITCH_SAVE` / `A64_SWITCH_RESTORE` | 1 / 1 | `stp`/`ldp q8..q15` (4 each) | existing kernel switch-frame state (kept) |
| `A64_CPACR_WRITE` | 3 | `msr cpacr_el1, xN` | FPEN so the vector prologue may capture user state (BSP, secondaries, Rust helper) |
| `A64_CPTR_EL2_WRITE` | 1 | `msr cptr_el2, xzr` | EL2 boot: FP accesses not trapped to EL2 |
| `X86_CTX1_ENV_PROBE`, `A64_CTX1_ENV_PROBE` | 1 each, `context1-witness` only | `stmxcsr`, `fnstcw` / `mrs fpcr` | the CONTEXT1 witness reads the kernel's FP environment |
| `X86_CTX1_CLOBBER`, `A64_CTX1_CLOBBER` | 1 each, `context1-clobber` only | `fxrstor64 [reg]` / `ldp q0..q31`, `msr fpcr`, `msr fpsr` | the CONTEXT1 witness's deliberate interference |

RISC-V has **no** exception: any FP or vector instruction anywhere in its kernel fails.

**Decoded and excluded bytes.** Every byte of every executable section is either decoded or
excluded, and the accounting line proves the two add up. The map records which ranges are not
ordinary 64-bit/native code, but a record only *names* a candidate range; the checker accepts it
only against its own reviewed inventory (KERNEL-NOFP1-ACCEPTANCE, below):

* **Decoded in another mode** (never excluded): x86 `.text.boot32` as 32-bit code; the AP
  trampoline's real-mode part as 16-bit code and its protected-mode part as 32-bit code.
* **Excluded as data:** the AP trampoline's GDTR/GDT image and its handoff block (x86 only), and
  AArch64 literal-pool words.
* **Excluded as embedded user programs:** the witness programs in the kernel's text (`smp1`,
  `apsched`, `overtaken`, `vmi2` on x86; `smp2`, `apsched`, `overtaken` on AArch64; `smp3`,
  `apsched`, `sr_replace`, `vmi` on RISC-V). The kernel copies them into user address spaces and
  never executes them; they may use FP/SIMD as user code, which is not banned.
* Everything else is decoded as native code.

**The check** (`scripts/check-kernel-nofp.py`, run by `scripts/check-kernel-nofp.sh`):

* It reads the ELF that boots. For AArch64 and RISC-V it also proves that the raw image QEMU
  loads equals that ELF's loadable bytes.
* It decodes every executable byte **that no authenticated exclusion covers**, in its mode, zero
  blocks included: GNU objdump for x86, LLVM objdump for AArch64 and RISC-V, with F/D/V/Zfh
  decoding enabled. The instructions must tile each decoded range exactly.
* A data directive the disassembler prints inside decoded code is accepted only as an AArch64
  literal-pool word: inside a `$d` mapping span **and** covered by the target of a PC-relative
  literal load in decoded code. On x86 and RISC-V it is a failure. Data sections inside
  executable segments (`.rodata`) are reported, not decoded.
* Classification covers:
  * FP/SIMD/vector register operands;
  * x87/FP/vector mnemonics;
  * implicit-state instructions (`fxsave`, `ldmxcsr`, `xsave*`, `emms`, `vzeroupper`, …);
  * FP control state (MXCSR, FCW, FPCR/FPSR, `fcsr`/`frm`/`fflags`, vector CSRs);
  * FP-enable writes (CR0, CR4, XCR0, `clts`, `lmsw`; CPACR_EL1/CPTR_ELx).
* An undecodable byte, a gap, an unknown record, a missing disassembler, an output line or
  prefix it cannot interpret, a wrong instance count or an instruction outside its exception's
  exact list is a **failure**, never a pass.
* Every failure carries an obligation code (`NOFP-ORDINARY`, `NOFP-DECODE`, `NOFP-DATA`,
  `NOFP-EXCL-*`, `NOFP-EXCEPTION`, `NOFP-INVENTORY`, `NOFP-METADATA`, `NOFP-BOOT-IMAGE`). The
  result line lists them and binds the verdict to the image SHA-256, the features, the toolchain
  and the disassembler.
* It runs at the end of all three QEMU artifact builds (`common_verify_kernel_nofp`), which fail
  closed. `tests/kernel_nofp_scope.rs` pins the target configuration, the build gate, the
  agreement between recorded kinds and the policy, the agreement between recorded regions and
  the exclusion inventory, and the AArch64 brackets. `tests/kernel_nofp_cli.rs` runs the
  checker's CLI on 80 synthetic images (KERNEL-NOFP1-ACCEPTANCE).

**What is unchanged:**

* the eager user FP/SIMD capture before compiled code and restore after it;
* the kernel's own control environment (x87 reset, MXCSR `0x1F80`, FPCR 0);
* exact continuation-owner authentication;
* first entry, fork, thread creation, TLS, exit and slot-reuse behaviour;
* flags and general-register preservation;
* the switch-frame FP areas;
* user instruction sets and the syscall ABI.

There is no `USES_FP` optimisation, no conditional save, no lazy ownership and no first-use
allocation; those belong to FP-POLICY1 and KTHREAD1.

**Cost (default kernels, base `91030225` against the candidate; static, from the images).**

| Port | `.text` | `.rodata` | FP/SIMD in ordinary code | static frames |
|---|---|---|---|---|
| x86_64 | 1 317 130 → 1 220 790 B (−7.3%) | −0.4% | 22 779 → 0 | largest frame unchanged (262 744 B, `remove_asid_root`); sum over the 928 functions in both builds 818 283 → 755 465 B |
| AArch64 | 905 296 → 885 740 B (−2.2%) | −0.3% | 2 547 → 0 | largest 5 388 416 → 5 388 352 B (`init_state_into`); sum over 894 shared functions 6 204 282 → 6 193 198 B |
| RISC-V | identical | identical | 0 → 0 | identical (the booted binary is byte-identical) |

* Witness variants move by the same proportions: x86 `.text` −6.5% to −8.0%, AArch64 −2.1% to
  −2.7%.
* Frame sizes are per-function maxima from `-Z emit-stack-sizes`. Individual functions moved
  both ways, by up to about 8 KiB, because inlining changed. No call-chain worst case is computed,
  and the kernel has no runtime stack-watermark instrumentation. A smaller sum is therefore **not**
  a stack-bound claim.
* Removing compiler SIMD makes bulk copies use integer code; nothing here shows that is faster or
  slower on hardware.

**Qualification.**

* Final commit `13cf33f0` (tree `9dc5adaf`), against base `91030225`. The full record, with every
  failure kept, is in `doc/KERNEL_UNLOCKING.md` §0.8.
* 20/20 kernel variants pass the image check, and 29 negative controls fail as intended.
* Six strict CONTEXT1 boots pass, three on x86_64 and three on AArch64.
* Strict cores pass on all three ports, and the production AP-scheduling witnesses pass on all
  three.
* The IRQ, SMP/lock, overtaken-return, IPC, shared-region, VMI/COW and lifecycle regressions
  pass.
* Two x86 strict-core runs failed on a grader assumption (a fixed reply-record generation). They
  were repaired and rerun on the final commit.

**KERNEL-NOFP1-ACCEPTANCE: two false passes closed.** Run unmodified through its real CLI on
synthetic ELF images that carry the whole legitimate exception inventory, the delivered checker
(`fec063af`) passed two images it should have rejected: bytes `49 db e3` in ordinary code, which
GNU objdump 2.42 prints as `rex.WB fninit` (its prefix list held only single-letter REX forms),
and an ordinary `fninit` covered by a newly added `DATA arbitrary` map record (any `DATA` record
was accepted on its word). These are verifier defects shown on synthetic images, not defects
found in the delivered kernels: every qualified kernel image also passes the repaired checker.
The repairs:

* **x86 prefixes** are parsed with the decoder's own closed prefix grammar, taken from GNU objdump
  2.42 / libopcodes, per decoding mode: all 16 REX spellings, `{rex2 0xNN}`, `data16`/`data32`,
  `addr16`/`addr32`, segment overrides, `lock`, `rep`/`repz`/`repnz`, `xacquire`/`xrelease`,
  `bnd`, `notrack`, `{vex}`/`{vex3}`/`{evex}`, in any combination, and branch-hint suffixes.
  Classification reaches the instruction after them. Anything else in prefix position, a token
  after the operands, a malformed mnemonic or operand list, or an output line that is neither
  an instruction nor a known header fails as `inspection incomplete`.
* **Exclusions are authenticated.** The checker holds a closed, reviewed inventory keyed by the
  exact record name. Each entry fixes the decoding mode, its purpose, the owning source, the
  output section, and its **anchors**: the symbols or section bounds the owner defines. For a
  user program these are the copy-out symbols the kernel itself uses (`yarm_vmi2_start` …), and
  multi-segment programs must tile exactly. Each entry also fixes the cargo feature that compiles
  it, read through Cargo.toml's feature graph, whether the linker may eliminate it, and its boot
  mode. 32-bit boot code must be identity-loaded below 4 GiB. Trampoline parts must lie inside
  the one-page real-mode blob.
  * A record is refused, and then excludes **nothing**, when its name is unknown, its bounds
    differ from its anchors (shifted, enlarged), or an anchor is missing or defined twice.
  * It is also refused when it is duplicated, present without its feature, missing although its
    feature compiles it, or linker-eliminated while its anchors are still linked (or when it is
    never eliminable).
  * It is also refused when a sized kernel function or object, or an unlisted entry point, lies
    inside it, or when it contradicts its architecture or boot mode.
  * An exception record that overlaps excluded data or a user program is a conflict.
  * The anchors are structural (named symbols and section bounds), never whole-image hashes or
    symbol-prefix rules.
* **Disassembler data directives** are justified as above (literal-pool words only), so an
  undecodable or hand-encoded instruction cannot turn itself into data.
* **Metadata** is checked: the ELF machine must match the port, every feature named must exist
  in Cargo.toml, and a malformed map table (unknown word, truncated record, malformed name,
  trailing bytes) fails.

Qualified on `870d9126` (record in doc/KERNEL_UNLOCKING.md §0.8). The fresh default kernels and
all 24 checked variants pass, and they are byte-identical to the KERNEL-NOFP1 images where one
exists. No production byte changed.

What this establishes is exact. For the decoded bytes, the disassembler's linear decoding has no
FP/SIMD or FP-enable instruction outside an audited exception. The undecoded bytes are exactly the
reviewed inventory's anchored ranges and the justified literal-pool words. It is **not** a proof
of control-flow safety: nothing shows that execution never jumps into an excluded range or into
the middle of a decoded instruction. It is **not** a proof that the disassembler decodes what the
CPU executes.

**Remaining limits (OPEN):**

* **CLOSED by BUILD-GATES1 — the AArch64 core grader's fresh-record assumption.** The grader reads
  the reply record's index and generation from the production records and binds them to the one
  request route, like the x86 check. A labeled live witness exercises reuse (doc/KERNEL_UNLOCKING.md,
  BUILD-GATES1).
* **CLOSED by BUILD-GATES1 — smoke build paths outside automatic enforcement.** Every kernel any
  QEMU path boots is published through one path that runs the no-FP check on the final bytes and
  records provenance. Every launcher refuses, before QEMU, a kernel or initramfs without a
  matching record and a passed check.
* **OPEN — the RPi5 bring-up targets (`aarch64-rpi5-*`) keep `+neon`.** Hardware work is outside
  this package; their asm carries the same bracketing.
* None of the above, and nothing in KERNEL-NOFP1 or its acceptance, is a qualification for
  future kernel-mode preemption: that stays gated by SCHED-LOCK1 and KTHREAD1 (SC-KT-4).
* RISC-V enforcement is by the image only (hard-float target, kept for ABI stability). Its
  `sstatus` writes are not classified, because FS/VS share the register with SIE; with no FP
  instruction present, FS has nothing to enable.
* The checker trusts the system disassemblers (binutils 2.42, LLVM 18) to decode correctly. Its
  self-test, CLI fixtures and negative controls exercise them, but do not prove them. The CLI
  fixture test needs both disassemblers, as the artifact builds do; a missing one fails it.

## 9. Evidence index

| Test | Kind | Requirement |
|---|---|---|
| `production::sc_pri_1_strict_priority_order_high_normal_low` | P | SC-PRI-1 |
| `production::sc_pri_2_equal_priority_is_fifo_and_deterministic` | P | SC-PRI-2 |
| `production::sc_pri_3_quantum_preemption_requeues_at_tail_of_own_priority` | P | SC-PRI-3 |
| `production::sc_pri_4_wake_appends_at_tail_and_never_displaces_running_task` | P | SC-PRI-4, SC-INV-4 |
| `production::sc_cpu_1_enqueue_is_confined_to_the_named_cpu` | P | SC-CPU-1, SC-CPU-3 (GP) |
| `production::sc_pool_1_run_queue_exhaustion_is_explicit_and_atomic` | P | SC-POOL-1 |
| `production::sc_elig_1_acceptance_filter_skips_ineligible_highest_in_place` | P | SC-SEP-2 (mechanism) |
| `sc_gp_1_general_purpose_model_agrees_with_production_priority_scheduler` | P (differential) | SC-GP-1 |
| `model::model_*` (11 SCHED-CONTRACT1 tests) | M | SC-PART-2/3/6, SC-CTX-3/5, SC-PRI-11/12, SC-IPC-1/2/3, SC-CPU-3, SC-EXEC-2/3/5 |
| `model::model_debt_beyond_one_replenishment_is_conserved` | M | SC-CTX-4 |
| `model::model_debt_repayment_cases_and_no_budget_accumulation` | M | SC-CTX-4 |
| `model::model_debt_overflow_saturates_and_is_reported` | M | SC-CTX-5 |
| `model::model_overflow_latch_holds_across_scheduling_events` | M | SC-CTX-5 latch: organic overflow, then wake, replenishment, yield, block and window changes |
| `model::model_overflow_latch_refuses_restored_budget_constructed` | M (constructed state) | SC-CTX-5 latch: refusal independent of `remaining`/`debt` |
| `model::model_overflow_latch_survives_replenishment_constructed` | M (constructed state) | SC-CTX-5 latch: replenishment neither repays nor credits |
| `model::model_exhausted_thread_rejoins_the_tail_only_at_replenishment` | M | SC-PRI-12f |
| `model::model_debt_consumed_replenishment_grants_no_position` | M | SC-PRI-12f, SC-CTX-4, SC-CTX-8 |
| `model::model_wake_while_exhausted_takes_no_position` | M | SC-PRI-12c/d/f, SC-CTX-8 |
| `model::model_simultaneous_replenishments_rejoin_in_context_order` | M | SC-CTX-7 |
| `model::model_queue_membership_has_no_duplicates_or_stale_positions` | M | SC-CTX-8 |
| `scripts/check-kernel-nofp.py` on every artifact build (all three ports, default and witness variants) | S (final image) | SC-FP-1, SC-FP-2, SC-KT-5 |
| `kernel_nofp_scope::*` (6 tests) | S (source guard) | SC-FP-1/3 configuration, gate, exception inventory |
| `model::every_mutation_is_killed_by_its_targeted_check` | M (mutation) | the model predicates above, now including `ForgiveDebtAtReplenish`, `QueueOnExhaustion`, `WrappingDebt` and `IgnoreOverflowLatch` |

**Mutation checks (§5 of the package brief).**

* **Model.** Eleven model mutations are killed in-test by their targeted checks.
* **Production.** Seven source mutations of `src/kernel/scheduler.rs` were applied one at a time
  and recorded in this package's qualification record (`doc/KERNEL_UNLOCKING.md` §0.8). Each
  breaks one enforced predicate:
  * reversed priority order;
  * LIFO ring;
  * a quantum preemption that keeps `current`;
  * a wake that preempts;
  * enqueue on the wrong CPU;
  * no `QueueFull`;
  * an acceptance filter that drops refused entries.

  Each must be killed by its targeted `production` test.

---

## 10. Handoff

| Requirement | Current owner | Missing mechanism | Evidence needed | Package |
|---|---|---|---|---|
| SC-CPU-2 no migration | `task_enqueue` planners | Pinned plan must equal an existing home or fail closed | P: pinned enqueue of a homed task refused | RT-SCHED1 |
| SC-PRI-10/11/12/13 | `PriorityScheduler` (3 levels, tail-on-preempt, wake non-preempting) | 32 levels; head-on-priority-preemption; wake as scheduling point; preempting remote IPI | P on the production owners; L on three ports | RT-SCHED1 |
| SC-PART-1…7 | none | schedule, epoch, window evaluation, boundary switch | P + L (boundary instants, idle advance) | PARTITION1 |
| SC-CTX-1…5, 7, 8 | none (per-CPU quantum only) | scheduling contexts, budgets, conserved-debt replenishment, overrun debt and reports, same-instant order, exhausted-set membership | P + L | RT-SCHED1 |
| SC-CTX-6, SC-TIME-3/4/6 | per-port relative re-arm | calibrated counter, absolute deadlines, cross-CPU time, tick unit | M (measurement) on named hardware + S | RT-SCHED1 |
| SC-TIME-5 | periodic tick | earliest-deadline programming | P + L | RT-SCHED1 / PARTITION1 |
| SC-SEP-1/2 | acceptance-filtered selection | window + budget in the existing predicate | P | RT-SCHED1 |
| SC-POOL-2/3/4 | fixed arrays; `BootExclusive` phase | configured pools, queue sizing, OPERATIONAL transition | P | RT-SCHED1 / PARTITION1 |
| SC-EXEC-1…6 | inline kernel work, uncounted | accounting points, system allowance, overrun events | P + N bounds | RT-SCHED1 / PARTITION1 |
| SC-IPC-1/2/3 | `direct_eligibility`, ordinary selection | eligibility-checked handoff; donation stays off | P | RT-SCHED1 / PARTITION1 |
| SC-BND-2, SC-KT-1…12 | D6 scaffolding (dormant), diverging idle | kernel stacks, entry ABI, kernel preemption (after SCHED-LOCK1 and KERNEL-NOFP1), TLS policy, guard check, stack residency, trap classifiers | P + L on three ports | KTHREAD1 |
| SC-BND-3, SC-INV-2/3 | none | configuration validation (shared regions, ceilings) | P | PARTITION1 / RT-SCHED1 |
| SC-INV-1, W1 | TTAS `SpinLockIrq` | fair lock or bounded-contention proof; per-rank section bounds | S + N | RT-SCHED1 |
| SC-WCET-1/2, W3/W7 | synchronous logging, unbounded routes | per-route bounds, buffered diagnostics | S + N | RT-SCHED1 |
| SC-FP-1…3, SC-KT-5 | **done** (KERNEL-NOFP1, §8.2) | — | final-image check on every artifact build | — |
| SC-FP-5 | QEMU only | execution cost on hardware | N | hardware bring-up |
| SC-FP-4 | none | static per-task FP policy with enforced non-use | P + L | FP-POLICY1 |
| SC-LOCK-1, SC-LOCK-3…7 (§12) | **done** (SCHED-LOCK1, §16–§17): per-CPU 1L, admission 1G, atomic clock / bitmaps / mirrors | — | P + L + S, §17 | — |
| SC-LOCK-2 (§12) | **partial**: lock order excludes the scheduler-lock cycles; no progress argument (§17.3, corrected by SCHED-LOCK1-ACCEPTANCE) | fair lock or bounded-contention proof | S + N | RT-SCHED1 |
| SC-INV-1, W1 for 1L / 1G | TTAS `SpinLockIrq`, now per CPU | fair lock or bounded-contention proof | S + N | RT-SCHED1 |

**Not started and not part of this contract:** VFS work and hardware bring-up remain on the
roadmap in `doc/KERNEL_UNLOCKING.md` §0.8. Hardware bring-up is the prerequisite for every **N**
bound above.

---

## 11. Standards scope

* **No normative standard text was consulted for this package.** No edition of ISO 26262,
  DO-178C or ARINC 653 is cited as consulted, because none was available in the environment the
  contract was derived in. The editions of record are therefore **not established here**.
* **This contract makes no conformance claim and no compliance claim.** It also makes no claim
  that any requirement here satisfies, maps to, or is derived from a clause of those standards.
* **Terms are local.** "Partition", "window", "major frame", "criticality" and "health monitor"
  are defined by this document alone, in the generic architectural sense its requirements give
  them.
* **Intent, not conformance.** The architecture — static CPU assignment, cyclic windows,
  fixed-priority scheduling inside a window, budgeted contexts and explicit accounting — is the
  *intent* the owner stated. Whether it can support a future conformance argument is a separate
  assessment.
* **Who could close this.** Only an assessor working from the normative editions could make that
  assessment, and they would record those editions. Owner: unassigned (project owner).

---

## 12. SCHED-LOCK1: the scheduler domain lock — DONE

**The problem, from source (§1.1, §1.2), as it stood before SCHED-LOCK1.**

* The per-CPU run queues, current slots and quanta all live inside ONE rank-1 scheduler domain
  lock (`SchedulerState` behind `with_scheduler_split_mut`).
* So unrelated CPUs serialise enqueue, selection, blocking and timer bookkeeping on one critical
  section, every one of them with interrupts masked.
* That lock is a TTAS `SpinLockIrq` with no fairness (§6 W1).
* This is a **different lock** from the retired broad `KernelState` lock, whose production
  census is 0. Retiring the broad lock did not make the scheduler CPU-local.

**SCHED-LOCK1 is DONE** (design record §16, implementation and evidence §17, qualification
record `doc/KERNEL_UNLOCKING.md` §0.8). The requirements and their status:

* **SC-LOCK-1 — CPU-local scheduling state has explicit local ownership.** **ENFORCED**
  (SCHED-LOCK1: one rank-1L lock per CPU, §16.2, §17.1). Each CPU's runnable queues, current slot and quantum belong to that CPU. Another
  CPU's scheduling operation does not enter that CPU's critical section, except through SC-LOCK-3.
* **SC-LOCK-2 — Local exclusion is bounded.** **PARTIAL (corrected by SCHED-LOCK1-ACCEPTANCE,
  §17.3).** The lock order excludes cycles among the scheduler locks and between them and
  shootdown waits. No progress argument is established: the 1L/1G wait is the unfair W1 kind, and
  sections can wait on the task-table and printk locks. No starvation-freedom or time bound is
  claimed; a fair lock or a bounded-contention proof is still owed (RT-SCHED1). The exclusion that
  protects CPU-local state has a stated progress argument and a bound in the §6 kinds. A local
  lock is acceptable only with that argument. An owner-consumed queue is acceptable only with
  that argument. **"Lock-free" is not mandated and is not by itself an argument.**
* **SC-LOCK-3 — There is an explicit cross-CPU wake protocol.** **ENFORCED** (§16.4, §17.2). A
  remote wake publishes to the target CPU by a defined protocol, which must specify:
  * the remote-queue capacity, and the behaviour when it is full — never silent loss;
  * publication ordering against the reschedule IPI;
  * cancellation: a wake for an incarnation that died or was already woken;
  * lost-wakeup prevention, including the target entering idle concurrently with publication.
* **SC-LOCK-4 — Existing guarantees are preserved.** **ENFORCED** (§16.6, §17.4). The
  decomposition preserves:
  * exact task-incarnation authentication (`{tid, asid}` and generations);
  * queue membership (one place per runnable incarnation, SC-CTX-8 for RT-P1);
  * wake arbitration (exactly one waker wins);
  * `DispatchAuthority` and `DispatchMarkToken`;
  * the ascending lock order.
* **SC-LOCK-5 — Global coordination is separated from local dispatch.** **ENFORCED** (§16.2:
  1G, the atomic clock and bitmaps; the ambient `current_cpu` removed). Genuinely global state is owned separately from per-CPU dispatch and is never
  taken on a CPU's ordinary dispatch path. That state includes the clock owner's tick
  (SC-TIME-2), topology, admission (wake-only and balance-excluded sets), balanced placement and
  the ambient `current_cpu`.
* **SC-LOCK-6 — Remaining shared dependencies are inventoried.** **ENFORCED** (§16.7, §17.3).
  Splitting this one lock does not make scheduling bounded. The package inventories every shared
  dependency left on a dispatch path, and claims nothing about the whole scheduler's bound. The
  known ones are:
  * the rank-2 task table read by the acceptance filter (§6 W2);
  * `PRINTK_DRAIN_LOCK` (W7);
  * the IPC and timeout domains;
  * TLB shootdown waits (W4).
* **SC-LOCK-7 — The legacy restrictions are reconciled explicitly.** **ENFORCED** (§17.5): the
  D6 bullet, the guard and the rank table were revised in SCHED-LOCK1 itself, as recorded there.
  These restrict per-CPU scheduler lock types today:
  * `doc/AI_AGENT_RULES.md` §14.4, the D6 bullet: "no per-CPU scheduler lock types until the
    x86_64 SMP trampoline split lands and D2/D3 are smoke-stable";
  * `doc/KERNEL_UNLOCKING.md` §7.1 and §8, which restate that gate;
  * the source guard `src/kernel/syscall.rs` `stage106_d6_audit_no_per_cpu_scheduler_locking_started`,
    which rejects per-CPU scheduler lock type names in `runtime.rs` and `scheduler.rs`.

  The rank table in `doc/KERNEL_LOCKING.md` has a single rank-1 scheduler domain, while its
  "Stage 4" plans per-CPU lock domains.

  SCHED-LOCK1 must revise those rules, the guard and the rank table **explicitly**, in the same
  change and with stated justification. It must not route around them. This contract does not
  relax any of them. (Done by SCHED-LOCK1, which the brief explicitly authorized; §17.5.)
* **SC-LOCK-8 — Ordering in the roadmap.** **SPECIFIED** (unchanged; SCHED-LOCK1 now precedes
  KTHREAD1 and RT-SCHED1, neither of which is started).
  * SCHED-LOCK1 is scheduled **before** preemptible kernel threads are enabled, and **before** any
    claim of bounded RT scheduling latency.
  * Basic kernel-context preparation (KTHREAD1's stacks, entry ABI and cooperative switch) may
    proceed separately. It must not enable preemption that SCHED-LOCK1's exclusion rules would
    make unsafe.
* **SC-LOCK-9 — No lock is split before SCHED-LOCK1.** **DISCHARGED.** It held until SCHED-LOCK1
  (SCHED-CONTRACT1 and its correction changed no lock). SCHED-LOCK1 is the split, and it replaced
  the legacy guard with checks for the new ownership rules (§17.5).

---

## 13. Correction record: SCHED-CONTRACT1-CORRECTION

The delivered SCHED-CONTRACT1 model had two defects. Both were **reproduced by executed
regressions against the unmodified model** before either was fixed. The model at `e41b3be9` was
unchanged for that run, except for a read-only membership observer, plain helpers and an inert
report field; 0 lines were removed. All eight new regressions failed, and all 20 earlier tests
passed:

| Defect | Regression | Failure on the unmodified model |
|---|---|---|
| A — debt forgiven | `model_debt_beyond_one_replenishment_is_conserved` | `(remaining, debt) = (0, 0)` at t=10, want `(0, 5)` |
| A | `model_debt_repayment_cases_and_no_budget_accumulation` | debt 4: got `(0, 0)`, want `(0, 1)` |
| A (overflow) | `model_debt_overflow_saturates_and_is_reported` | `attempt to add with overflow` (would wrap in a release build) |
| B — exhausted thread kept its place | `model_exhausted_thread_rejoins_the_tail_only_at_replenishment` | after B blocks, A ran; C, ready before A's replenishment, must run first |
| B | `model_simultaneous_replenishments_rejoin_in_context_order` | order `[2, 1, 0]`, want `[2, 0, 1]` |
| B | `model_wake_while_exhausted_takes_no_position` | an exhausted Ready thread holds 1 queue position |
| B | `model_debt_consumed_replenishment_grants_no_position` | an exhausted Ready thread holds 1 queue position |
| B | `model_queue_membership_has_no_duplicates_or_stale_positions` | seed 1 step 14: an exhausted Ready thread holds a position |

**The repairs.**

* **A — conserved repayment.** SC-CTX-4 (`paid = min(B, debt)`) and SC-CTX-5 (checked
  accumulation, saturating with a report).
* **B — exhausted threads hold no queue position.** An exhausted Ready thread sits in an exhausted
  set with no position. It joins the tail only at a replenishment that leaves it budget, in
  SC-CTX-7 order. The window-boundary and higher-priority-preemption head placements are
  unchanged (SC-PRI-12a/e).

**Independent revert controls.** Each repair was reverted alone in the model source, and the full
suite was run against the frozen tests:

| Revert | Failing tests | Reason |
|---|---|---|
| A1 repayment (back to `B − debt`, clear) | the two debt-repayment regressions | debt forgiven: `(0, 0)` vs `(0, 5)`; debt 4 → `(0, 0)` vs `(0, 1)` |
| A2 overflow (back to a plain add) | the overflow regression | `attempt to add with overflow` |
| B queueing (back to push at exhaustion and wake) | the five queue regressions | A ran before C; order `[2, 1, 0]`; exhausted threads hold positions |

In each revert run every other repair's regressions still passed. The in-test mutation check
also failed, because its unmutated baseline no longer held. The in-test mutations
`ForgiveDebtAtReplenish`, `WrappingDebt` and `QueueOnExhaustion` now keep each defect killed
permanently.

**Qualification.** The frozen candidate `00e7788b` (tree `4b920962`) was qualified against
`e41b3be9`. The unmodified-model and revert controls above were rerun on the frozen tests with
the same results. The contract suite passed 28/28 twice. No production path changed, and the
default kernels are bitwise identical to base on all three ports. The full record is in
`doc/KERNEL_UNLOCKING.md` §0.8.

**These are model tests.** They make the specification executable and check its predicates. They
do not implement or qualify production RT scheduling, which remains SPECIFIED → RT-SCHED1.

---

## 14. Requirement status summary

* **ENFORCED by production:**
  * SC-CPU-1;
  * SC-CPU-3 (GP);
  * SC-PRI-1, SC-PRI-2, SC-PRI-3, SC-PRI-4;
  * SC-POOL-1;
  * SC-SEP-2 (mechanism);
  * SC-BND-1;
  * SC-INV-4 (as a known GP property);
  * SC-TIME-1, SC-TIME-2;
  * SC-GP-1 (differential, for the mechanisms exercised);
  * SC-KT-0;
  * SC-KT-5, SC-FP-1, SC-FP-2, SC-FP-3 (KERNEL-NOFP1);
  * SC-LOCK-1, SC-LOCK-3…7 (SCHED-LOCK1). SC-LOCK-2 is PARTIAL: the lock order excludes the
    scheduler-lock deadlock cycles, but no progress argument is established (§17.3).
* **DISCHARGED:** SC-LOCK-9 (it held until SCHED-LOCK1, which is the split).
* **SPECIFIED, with an implementing package:**
  * SC-LOCK-8 (roadmap order: SCHED-LOCK1 before KTHREAD1 kernel preemption and RT-SCHED1).
  * **KTHREAD1:** SC-KT-1…4, SC-KT-7…12, SC-BND-2, SC-EXEC-3.
  * **RT-SCHED1:**
    * SC-CPU-2;
    * SC-PRI-10…13;
    * SC-CTX-1…5, SC-CTX-7, SC-CTX-8;
    * SC-SEP-1;
    * SC-IPC-1, SC-IPC-2;
    * SC-INV-2, SC-INV-3;
    * SC-EXEC-1, SC-EXEC-2, SC-EXEC-4;
    * SC-TIME-5;
    * SC-WCET-1, SC-WCET-2;
    * SC-GP-2.
  * **PARTITION1:**
    * SC-PART-1…7;
    * SC-POOL-2, SC-POOL-4;
    * SC-EXEC-5;
    * SC-IPC-3;
    * SC-BND-3.
  * **FP-POLICY1:** SC-FP-4.
* **UNRESOLVED (missing fact and owner stated in place):**
  * SC-CTX-6;
  * SC-POOL-3;
  * SC-EXEC-6;
  * SC-INV-1;
  * SC-TIME-3, SC-TIME-4, SC-TIME-6;
  * SC-KT-6;
  * SC-FP-5 (hardware execution cost; the rest is measured).

---

## 15. Correction record: SCHED-OVERFLOW1

**Source finding.** In the model delivered at `04b94fc0`, SC-CTX-5 said an overflowed context
stays ineligible until recovery. The model **recorded** `debt_overflow`, but neither eligibility
nor replenishment consulted it. Its test checked saturation and the report only.

**What was executed on the unmodified model** (the three new regressions added, the model
unchanged; 0 lines removed):

| Test | Result on `04b94fc0`'s model | What it shows |
|---|---|---|
| `model_overflow_latch_holds_across_scheduling_events` (organic) | **passes** | organically overflowed, the context stayed ineligible — but only because its `remaining` stays 0 |
| `model_overflow_latch_refuses_restored_budget_constructed` | **fails**: "t=0 an overflowed context with restored budget ran" | eligibility ignores the overflow record |
| `model_overflow_latch_survives_replenishment_constructed` | **fails**: "the replenishment at t=10 restored an overflowed context: running Some(0), remaining 2" | replenishment repays and credits an overflowed context |

A scratch horizon experiment, not delivered, used the largest budget the model admits
(B = P = 2^63) and jumped time directly to just before each release. Its first replenishment
reached `release overflow`: the model's own time ended before any repayment of a saturated debt
could finish.

**Conclusion.** No *organically reached* overflowed context was observed to resume. The 64-bit
horizon makes that unreachable in this model. The defect is that ineligibility was **incidental
to the debt's magnitude** rather than enforced. The two constructed tests show that any restored
budget — from a narrower counter, a repayment past the horizon, or a recovery path — would have
run the context.

**Repair.**

* `overflow_latched(t)` is consulted by the budget-eligibility predicate before `remaining`.
* Replenishment skips a latched context; only its release instant advances.
* Nothing in the model clears the latch.
* The in-test mutation `IgnoreOverflowLatch` restores the delivered behaviour. It is killed by
  both constructed tests.

**Independent revert controls** (frozen tests, model source reverted one part at a time):

| Revert | Result | Failing |
|---|---|---|
| eligibility refusal only | 29/2 | `…refuses_restored_budget_constructed` ("ran"), mutation self-check |
| replenishment freeze only | 29/2 | `…survives_replenishment_constructed` ("remaining 2", context *not* running because the eligibility latch still refuses), mutation self-check |
| both | 28/3 | both constructed tests, mutation self-check |

**Qualification.** Frozen `adaad66e` (tree `c738e675`) was qualified against `04b94fc0`. The
controls above were rerun on the frozen tests with the same results, and the contract suite
passed 31/31 twice. No production path changed, and the default kernels are bitwise identical to
base on all three ports. The record is in `doc/KERNEL_UNLOCKING.md` §0.8.

**Unchanged.** Debt conservation, FIFO and exhausted-set membership, same-instant ordering, and
the production general-purpose differential are untouched, and every earlier test passes. These
remain **model tests**: SC-CTX-5 stays SPECIFIED → RT-SCHED1, with recovery owned by PARTITION1.

---

## 16. SCHED-LOCK1: ownership decomposition of the scheduler domain (design record)

This section was recorded from the delivered base `adf5fa63` **before any caller was converted**.
It names, for every field of the old rank-1 domain, its new owner, readers, mutation authority
and synchronization, and it fixes the cross-CPU protocol. §12 states the requirements
(SC-LOCK-1…9); the implementation and its evidence are recorded in §17.

### 16.1 Inventory of the old domain (base `adf5fa63`)

One `SpinLockIrq<SchedulerState>` (rank 1, `KernelState::scheduler_state`) held:

| Field (old) | Class | Production writers | Production readers |
|---|---|---|---|
| `SmpScheduler.schedulers[c]` — three FIFO rings (64 each), `current`, the 256-slot membership mirror | **CPU-local** | dispatch / preempt / block / exact clear / restore on `c`; enqueue and withdraw targeting `c` | every route on `c`; remote enqueue; the "anywhere" scans |
| `SchedulerState.ap_quanta[c]`, and the quantum half of `SchedulerState.timer` (c = 0) | **CPU-local** | `tick_on(c)`, `reset_quantum_on(c)`, `set_quantum_all` | `would_preempt_next_on(c)` |
| the tick half of `SchedulerState.timer` | **global time** | `tick_on(CLOCK_OWNER_CPU)` only (AP-SCHED1) | deadline arming and comparison on every CPU |
| `SmpScheduler.topology` (present / started / acknowledged / online) | **topology / admission** | boot bring-up; `set_present_cpu_bitmap` | `validate_online_cpu` on almost every route; bitmaps |
| `wake_only`, `balance_excluded` | **admission** | AP bring-up, `admit_pinned_dispatch`, `admit_dispatch` | enqueue refusal; balanced choice; shootdown sets |
| `next_balance_cpu` | **placement config** | balanced choice | balanced choice |
| `SchedulerState.current_cpu` | **ambient selector** | nine split writers (all also given an explicit `cpu`) | `split_terminal_route_admission`, the x86 `yarm.d6_genuine=1` knob paths, `yield_reverify_ready` (discarded), the hosted `executing_cpu_split` fallback, one log line, `KernelState::current_cpu()` on RISC-V and hosted |

Accessors: `SharedKernel::with_scheduler_split_mut` (65 production sites), the raw
`scheduler_split_mut_ptr_from_raw` reads (7), and the `KernelState` wrappers
(`with_scheduler_state{,_mut}`, `scheduler_state()`), which are reachable at run time only in boot
(exclusive `&mut KernelState`) and in hosted tests.

**Coherent transactions** (one acquisition today), all naming one CPU explicitly: enqueue commit
(`commit_enqueue_locked`), dequeue/withdraw, current install (`dispatch_next*`), block
(`block_current*`), yield and timer preempt (`preempt_reenqueue_only_on`, `on_preempt*`), exit
(`clear_current_exact`), rollback (`restore_exact_current_on`, `block_current_on` +
`enqueue_on_with_priority` on the same CPU), admission (`admit_*` — topology bits + that CPU's
slots), and remote wake (`enqueue_on_with_priority(target)` with an executing-CPU read).
`wake_tid_to_runnable_split` is the one transaction that read the executing CPU's slot and
mutated another CPU's queue in the same acquisition.

**Consumers of multi-CPU snapshots**, and how each is replaced:

| Consumer | Callers | Hotness | Replacement |
|---|---|---|---|
| FPU-home "current elsewhere" (`with_current_fpu_home_split`) | every user trap entry and return on x86/AArch64; RISC-V overtaken settle | hottest | own 1L + **current mirrors** of the other CPUs (Dekker argument, §16.4) |
| `placement_of` (`entering_frame_authority_split_read`, `recv_unwind_placement_outcome_split`, `yield_forfeited_placement_split_read`) | refused-switch and unwind settlements | rare | **coherent all-CPU snapshot** (§16.3) |
| `task_present_anywhere` / `tid_present_anywhere` for exit restore, reap, post-lock exit validation, `task_present_in_any_runqueue` | exit / reap / validation | rare | **coherent all-CPU snapshot** |
| `task_present_anywhere` for the direct-IPC receiver-membership check | every direct call / reply | hot | **per-CPU scan** (each CPU under its own 1L, one at a time); frozen-subject argument §16.4 |
| `live_cpu_bitmap_for_asid_split`, `address_space_loaded_cpus_split` | VMI2 shootdown targets, teardown | warm | each candidate's current under **its own 1L, one at a time** ∪ the hardware residency table (unchanged), §16.5 |
| `reap_shootdown_cpu_bitmap_split`, `split_terminal_route_admission`, `dispatching_cpu_count`, online/present counts | admission predicates | warm | **published admission bitmaps** (atomic, §16.2) |
| `balanced_choice` | home fixing at first placement | rare | 1G + **load mirrors** (§16.4) |

### 16.2 New owners

| State | Owner | Readers | Mutation authority | Synchronization |
|---|---|---|---|---|
| CPU `c`'s run queues, current slot, membership mirror | `CpuSched[c].rq` | routes on `c`; remote enqueuers; scans and snapshots | **current slot**: operations naming `c` (dispatch, preempt, block, exact clear/restore, rollback). **queues**: the same, plus remote enqueue and withdraw naming `c` | **1L[c]**, one `SpinLockIrq` per CPU |
| CPU `c`'s quantum | `CpuSched[c].quantum` | `would_preempt_next_on(c)` | `tick_on(c)`, `reset_quantum_on(c)`; quantum configuration | 1L[c] |
| scheduler tick (time) | `SchedDomain.clock` (`AtomicU64`) | everyone, lock-free | **only** `tick_on(CLOCK_OWNER_CPU)`, under 1L[0] | single writer; `fetch_add` Release, loads Acquire |
| topology, `next_balance_cpu` | `Admission` | admission paths | boot bring-up; `set_present_cpu_bitmap` | **1G** |
| published present / online / wake-only / balance-excluded bitmaps | `SchedDomain.topo` (atomics) | every validation, lock-free | admission transitions only | written under 1G **and** the affected CPU's 1L[c]; Release stores, Acquire loads. Hence a holder of 1L[c] sees `c`'s admission bits stable |
| current mirror[c] (tid + 1, 0 = none) and load mirror[c] (queued + current) | `SchedDomain.mirror[c]` | FPU "elsewhere", balanced choice | written **only** by the holder of 1L[c], when its guard is released (after every mutation it made) | SeqCst stores and loads |
| ambient `current_cpu` | **removed** from the scheduler domain | — | — | Production decisions use the explicit CPU each route was given. Hosted tests keep a hosted-only selector outside the scheduler (`#[cfg(feature = "hosted-dev")]`), and freestanding RISC-V `KernelState::current_cpu()` uses the hart's own identity |

### 16.3 Lock ranks and the projection rule

* **Rank 1G** — the scheduler admission lock (`SpinLockIrq<Admission>`). Never taken on an
  ordinary dispatch, enqueue, block, preempt or tick path.
* **Rank 1L** — the per-CPU scheduler locks. Several are held only by the coherent snapshot,
  which takes them in **ascending CPU index** and releases them in **descending** order (each
  guard restores the interrupt state saved by its own acquisition, so the first-acquired guard,
  which saved the caller's state, is released last).
* Order: 1G → 1L (ascending) → 2 (task) → 3 … unchanged. Nothing takes 1G while holding a 1L.
  Rank 2 nested inside a 1L (the acceptance filter, the FPU home) is unchanged from rank 2
  nested inside the old rank 1.
* **Projection.** A 1L guard dereferences to that CPU's `CpuSched` and to nothing else: no
  `&mut SmpScheduler`, `&mut SchedulerState` or other whole-domain reference is manufactured
  under a per-CPU lock. The only whole-domain view (`AllCpus`) exists while **every** 1L (and,
  for admission, 1G) is held, or under exclusive `&mut KernelState` at boot, where it borrows the
  cells through `get_mut` without locking.
* **`UnsafeCell` / `Send` / `Sync`.** Each `CpuSched` sits in its own `SpinLockIrq` (the
  existing `UnsafeCell` lock with `Sync for T: Send`). The atomics are `Sync` by construction.
  Guards are `!Send` (existing `PhantomData<*const ()>`), so a guard and the interrupt state it
  saved never leave the CPU that acquired it. A view's lifetime is bounded by the guard it
  borrows from; nothing returns a reference into a `CpuSched` past its guard.
* No guard is held across an architectural switch, a remote-completion wait (TLB shootdown,
  `send_to`), or an IPI: the wake IPI is sent after the enqueue guard is dropped, exactly as
  before.

### 16.4 Cross-CPU protocol

**Remote wake (no new mailbox).** The executing CPU `e` wakes task `t` (incarnation `{tid,
asid}`, already settled Runnable under rank 2 by the existing owners) to its home `d`:

1. *Reservation is the run queue itself.* Take 1L[d]; check `d`'s admission bits (online, not
   wake-only) and the queue capacity (`RingQueue` 64 per priority; `QueueFull` is returned
   **before** any mutation, as today); append and update the membership mirror; publish the
   mirrors; release.
2. *Publication → IPI.* `notify_enqueued(d)` is sent after the release. The destination's idle
   decision (`terminal_idle_on_cpu_split` and the per-port idle loops) reads `d`'s queue under
   the **same** 1L[d]. Either the idle check is ordered after the commit (it sees the work), or
   before it (the IPI is sent after the commit and the existing check-then-wait sequence on each
   port wakes on the pending interrupt). This is the base argument with the global lock replaced
   by the destination's lock — the only lock both sides take.
3. *Outstanding work* is the queued entry; its consumer is `d`'s own dispatch. Work is bounded by
   the queue capacity, and a drain is the existing per-selection scan (§6 W2).
4. *Full queue / refusal.* No wake is committed before the queue accepts it, so a full queue
   never makes a committed wake disappear. Each caller keeps its existing settlement of the
   refusal (QueueFull / WakeOnly / CpuOffline mapped to its existing error, rollback or
   withdraw).
5. *Duplicate, stale, cancellation.* Wake arbitration is unchanged and happens before step 1
   (the rank-2/IPC owners pick exactly one winner and settle the incarnation). The membership
   mirror refuses a second queue entry for a TID (`AlreadyQueued`), as today. A wake for an
   exited incarnation is refused by the same rank-2 checks that refused it before; teardown and
   reuse go through `withdraw_queued_tid_on` / exit clear on the CPU named by the token.
6. The executing-CPU read in `wake_tid_to_runnable_split` ("already current here") is taken
   under 1L[e] **before** 1L[d], and the two guards are never held together. The fact is stable:
   only routes executing on `e` change `e`'s current slot to or from a running task.

No path holds two 1L guards except the ascending coherent snapshot, and no path waits for an
interrupt or a remote completion while holding any 1L — so two CPUs never need each other's
scheduler locks.

**Mirrors.** `current mirror[c]` and `load mirror[c]` are rewritten by every 1L[c] guard at
release, so they equal the state after the last completed critical section on `c`.

* *FPU "current elsewhere".* Each CPU stores its own current mirror (SeqCst, at the release of
  the install) before it can reach a user return, and its check loads every other CPU's mirror
  (SeqCst) while holding its own 1L. If one incarnation were installed on two CPUs, the two
  store→load pairs are SeqCst, so at least one of the two checks observes the other: the
  duplicate is still refused, by at least one side, exactly as the coherent scan refused it.
* *Balanced choice.* Under 1G, the least-loaded admitted CPU by load mirrors. The decision is a
  placement heuristic: any CPU it returns is admitted, and the enqueue onto it re-checks
  admission under that CPU's 1L. Under serial execution it is identical to the base choice.

**Per-CPU scan (frozen subject).** The direct-IPC check asks whether a receiver that is
`Blocked` with a committed waiter has any queue or current membership. Such a receiver's
placement cannot change (nothing can wake it while both hold; there is no migration). A scan
that reads each CPU under its own 1L, one at a time, therefore returns exactly what the coherent
snapshot returned for every state the invariant allows; a violating membership that stays on one
CPU for the scan is still reported.

### 16.5 VMI2 coverage

The shootdown target set stays `(placement ∪ hardware residency) ∩ (online ∖ wake-only)`. The
placement half now reads each candidate CPU's current under that CPU's own 1L, one CPU at a time
(never two held together, released before the rank-2 ASID resolution), instead of one locked
scan; the residency half
(`address_space_loaded_bitmap`, set before ring 3 at every activation and cleared only at the
idle-root handoff) is unchanged and is what covers the dispatch gap (empty `current`, root still
loaded). Splitting the read therefore cannot recreate the empty-current / root-still-loaded
omission: that omission was closed by the residency term, not by the placement scan's
coherence.

The placement read is per-CPU locked, not a mirror read. A mirror is published when 1L[c] is
released, so while CPU `c` is in a section that changes its current task, its mirror still shows
the state before that section.

*What was reproduced.* During the first SCHED-LOCK1 qualification (candidate `155fd8d8`, RISC-V
SMP3 witness, fence attempt 18, recorded in KERNEL_UNLOCKING), the kernel's SMP3 verifier ended
the boot with `shoot_targets_inconsistent`. In that attempt the requester was CPU 1 and the
computed target mask was `0x1`: CPU 0 was a target. The placement input the computation recorded
for CPU 0 was its mirror, which showed no current task. The verifier checks the target set against
the placement facts it was computed from, and a CPU named while its recorded current named no task
of the address space is the disagreement it rejects. CPU 0 was in the set, so its fence was
requested.

*What was not reproduced.* No missed invalidation, no fence withheld from a CPU running the
address space, and no premature reclamation was observed. None is claimed. Whether a
mirror-based set can omit a CPU in some other interleaving was not established either way. The
delivered text called that set "safe"; that rested on the residency argument above, not on any
reproduction, and the word is withdrawn.

*The repair (delivered, unchanged).* Each candidate CPU's current is read under 1L[c], one CPU at
a time, so a section in progress completes first and the placement half again agrees with the
CPU's own state. Teardown's `address_space_loaded_cpus_split` uses the same read. Both are
cross-CPU operations and wait for the CPU they read. The concurrent test
`sl1_production_seams_on_cpu1_complete_while_cpu0_is_held` pins that they complete only after
CPU 0's section ends, while CPU 1's local operations do not wait. After the repair the SMP3
witness passed in its scheduled run on `00edb00f`.

### 16.6 What is not changed

Scheduling policy (priorities, FIFO, selection filter, quanta), CPU admission order, fixed
placement and homes, task identities, `DispatchAuthority` / `DispatchMarkToken`, context
preservation, first-start consumption, FP/TLS/flags ownership, idle-root handoff, exit/reap
ordering, CPU 0's timekeeping role and each CPU's own preemption accounting. No kernel threads,
kernel preemption, RT priorities/budgets, migration or partition windows.

### 16.7 Retained shared dependencies and timing kinds

* W1 holds for every 1L and for 1G: TTAS, no fairness, no bound. 1L contention now comes only
  from operations naming that CPU (its own routes, remote enqueues/withdrawals to it, the
  per-CPU scan, the coherent snapshot) instead of from every CPU's operations.
* W2 (rank-2 acquisitions inside the selection filter), W4 (shootdown waits, never under a 1L),
  W7 (logging inside scheduler closures) and W8 (membership probes ≤ 256) are unchanged.
* The coherent snapshot holds every 1L for one O(MAX_CPUS × 3 × 64) scan; it is used only on the
  rare paths in §16.1.
* Nothing here is a latency or WCET claim.

---

## 17. SCHED-LOCK1: implementation and evidence

§16 is the design record, written before any caller was converted. This section records what was
built against it, the arguments it rests on, and the evidence. Qualification results (counts,
runs, hashes) are in `doc/KERNEL_UNLOCKING.md` §0.8, bound to the commits that produced them.

### 17.1 What was built

* **`SchedDomain`** (`src/kernel/scheduler.rs`) replaces `SmpScheduler` and `SchedulerState`:
  * `cpus: [CpuCell; MAX_CPUS]` (boxed in hosted builds). Each `CpuCell` is `#[repr(align(64))]`
    and holds `lock: SpinLockIrq<CpuSched>` (rank **1L**: run queues, current slot, membership
    mirror, quantum) and `mirror` (current tid + 1 and load, SeqCst atomics).
  * `admission: SpinLockIrq<Admission>` (rank **1G**: topology, `next_balance_cpu`).
  * `topo`: the published present / online / wake-only / balance-excluded bitmaps (atomics).
  * `clock`: the scheduler tick, an `AtomicU64` advanced only by `tick_on(CLOCK_OWNER_CPU)`.
* **Access paths**, each projecting only what its authority owns:
  * `lock_cpu(cpu)` → `CpuSchedGuard` → `CpuView`: exactly one CPU's `CpuSched`. The guard's
    drop publishes that CPU's mirror while 1L is still held.
  * `lock_cpu_admission(cpu)` (1G then that CPU's 1L) → `admit_dispatch` /
    `admit_pinned_dispatch`.
  * `lock_all()` (1G, then every 1L ascending; released descending, then 1G) → `SchedAll`, the
    design record's "AllCpus". `exclusive(&mut self)` gives the same view without locking, for
    boot (exclusive `&mut KernelState`) and hosted tests.
  * The boot and hosted `KernelState` readers of one CPU's current task, `current_tid` and
    `current_tid_on_cpu`, take that CPU's 1L alone, not `lock_all`. Building the whole view for
    a one-CPU fact made a 1536-cycle hosted test 4.5 times slower than at base (2.1 s against
    0.45 s; 0.7 s after this change). That longer exposure let it hit a pre-existing race on a
    process-global flag in the parallel hosted suite (KERNEL_UNLOCKING, SCHED-LOCK1 findings).
  * Lock-free: `clock_now`, `topo()`, `current_mirror`, `load_mirror`, `current_elsewhere`.
* **`SharedKernel` seams** (`src/runtime.rs`): `with_cpu_sched_split(cpu, …)` (1L),
  `with_cpu_admission_split` (1G + 1L), `with_all_cpus_sched_split` (the coherent view; exactly
  five production callers: `placement_of` ×3, `tid_placed_anywhere_split`,
  `post_lock_exit_validation_split`), and `commit_enqueue_split`, **the** per-CPU enqueue commit
  (`task_enqueue::commit_enqueue_on_cpu_locked`, which refuses a plan naming another CPU).
* **The ambient `current_cpu` is removed** from the scheduler. Production routes use the CPU they
  were given. Hosted tests keep a selector outside the scheduler
  (`KernelState::hosted_ambient_cpu`, `#[cfg(feature = "hosted-dev")]`); freestanding RISC-V
  `KernelState::current_cpu()` reads the hart's own identity.
* **One implementation**, the default on all three ports. There is no fallback, knob or alternate
  legacy owner.

### 17.2 The cross-CPU protocol as implemented

| Question (brief §3) | Answer |
|---|---|
| Executing CPU | The CPU the route was given (its trap's `DispatchAuthority`, or the explicit `cpu` argument). |
| Destination | The task's home: its pinned affinity, else the home fixed at first placement (`fix_home_cpu_split`; the balanced choice runs under 1G alone). |
| Incarnation authority | Rank 2, before any 1L: the existing owners settle `{tid, asid}` and status and pick exactly one waker. A dead or replaced incarnation is refused there; the queue's membership mirror refuses a second entry (`AlreadyQueued`). |
| Commit | Under the destination's 1L only: admission bits and capacity are checked, then the append and the mirror update; the mirror is published at release. |
| Ordering | commit → mirror publish → 1L release → `notify_enqueued(d)` (wake IPI). No IPI is sent under any scheduler lock. |
| Wake IPI vs idle | The destination's idle decision reads its queue under the same 1L[d]. Either the decision is ordered after the commit and sees the work, or before it, and the IPI sent after the commit wakes the idle wait (§16.4 step 2). |
| Duplicate | `AlreadyQueued` under 1L[d]; the wake arbitration at rank 2 is unchanged. |
| Stale | Refused at rank 2 before any 1L (the planner requires the live incarnation's class and status). |
| Cancellation | `withdraw_queued_tid_on` under the destination's 1L; against a concurrent dispatch exactly one wins. |
| Teardown and reuse | Exit clears the current slot on the CPU named by the token (`clear_current_exact`), under that CPU's 1L; reaping checks absence through the coherent view. |
| Capacity | 64 per priority per CPU. `QueueFull` is returned **before** mutation, so a committed wake is never displaced and a refused one is never half-published. There is no mailbox: the run queue is the reservation. |
| Full-queue settlement | Each caller keeps its existing settlement (the mapped error, rollback or withdraw). The producer still owns a refused wake. |
| Bounded drain | The destination drains by its own selections; at most 192 entries per CPU (§6 W2). |
| Mutual holding / waiting | No path holds two 1L guards except the coherent view, which takes them in ascending order. No path waits for an IPI, a remote completion or another CPU's lock while holding a 1L. `wake_tid_to_runnable_split` reads "already current here" under 1L[e], releases it, then takes 1L[d]. |
| Scanning fallback, migration, new switch | None. |

A source guard (`sched_lock1_d6_per_cpu_scheduler_ownership_rules` (6)) scans every production
scheduler-lock closure in `runtime.rs` and refuses one that reaches a notification, an IPI, a
shootdown, an address-space switch or cross-CPU work.

### 17.3 Progress, fairness and retained dependencies

* **What the lock order establishes (SC-LOCK-2).** Some deadlock cycles are excluded by
  construction:
  * No 1L section holds another CPU's 1L, except the coherent view, which takes every 1L in
    ascending order after 1G.
  * No 1L section takes 1G.
  * No 1L section waits for an IPI acknowledgement or a shootdown (W4 waits are never under a
    1L).

  That excludes cycles among the scheduler locks themselves and between them and shootdown
  waits. It is a statement about cycles, not about progress.
* **What it does not establish.** A 1L section is not free of waits on other CPUs. Inside some
  sections the code takes:
  * the rank-2 task-table lock (the selection filter and the planners);
  * `PRINTK_DRAIN_LOCK` (W7, logging inside scheduler closures).

  Each of those can be held by another CPU. So a section's completion depends on those locks
  being released, and on the holders of those locks progressing.

  A bounded amount of work per section does not make a waiter acquire 1L[c]. The locks are
  unfair TTAS `SpinLockIrq` (§6 W1): the waiter can lose every race to other CPUs, without
  bound, even when every section is short.

  Having finitely many CPUs does not bound the number of competing holders either: the same CPUs
  can return to 1L[c] again and again.

  **No starvation-freedom, latency, WCET or kernel-preemption property is claimed for 1L, 1G or
  the coherent view.** A fair-lock redesign is outside SCHED-LOCK1 and SCHED-LOCK1-ACCEPTANCE.
* **Fairness.** None (§6 W1, unchanged).
* **Who contends for 1L[c]:** routes executing on `c`; remote enqueues, withdrawals and
  "anywhere" scans that name `c`; the coherent view. Previously every CPU's every scheduling
  operation contended for the one rank-1 lock.
* **Retained shared dependencies** (unchanged, §16.7):
  * rank 2 (task table), inside the selection filter and the planners;
  * the IPC and timeout domains;
  * `PRINTK_DRAIN_LOCK` (W7, inside scheduler closures);
  * shootdown waits (W4, never under a 1L);
  * the coherent view (1G and every 1L) on five rare paths, all in `src/runtime.rs`:
    `entering_frame_authority_split_read`, `tid_placed_anywhere_split`,
    `post_lock_exit_validation_split`, `recv_unwind_placement_outcome_split` and
    `yield_forfeited_placement_split_read`.

  Removing those five paths and redesigning the locks for fairness are outside this package.

### 17.4 Invariants and where each is checked

| Invariant (brief §4) | Mechanism | Evidence |
|---|---|---|
| One placement per incarnation | membership mirror under 1L; rank-2 arbitration | `sl1_a_duplicate_wake_has_exactly_one_winner`, `sl1_the_coherent_placement_never_tears_against_local_moves`; witness trace replay (each task in at most one place) |
| No task running on two CPUs | current-elsewhere check (own 1L + SeqCst mirrors) | `sl1_a_duplicate_current_is_seen_by_at_least_one_side`; witness census |
| No lost or duplicate wake | commit before notify; `QueueFull` / `AlreadyQueued` before mutation | `sl1_opposite_direction_remote_wakes_neither_deadlock_nor_lose`, `sl1_a_wake_racing_idle_entry_is_never_stranded`, `sl1_capacity_exhaustion_…`; witness CLAIM ↔ ADD ↔ NOTIFY matching |
| No stale token affects a replacement | rank-2 incarnation checks before any 1L | `sl1_a_stale_incarnation_wake_is_refused_before_any_queue_mutation`; existing exit/reuse tests |
| No return through an unowned continuation; no parked result lost | unchanged owners (`DispatchAuthority`, `DispatchMarkToken`, parked-result settlement) on explicit CPUs | the converted hosted suite (CONTEXT1, overtaken, parked-result tests) |
| VMI2 coverage | per-CPU locked current reads ∪ hardware residency (§16.5) | `sl1_shootdown_targets_follow_each_cpus_settled_current`; VMI2 witness |
| First-start, FP/TLS/flags, idle-root handoff, exit/reap ordering | unchanged, on explicit CPUs | converted hosted suite; apsched witness (reap, idle-root records) |
| Explicit CPU authority, no ambient selector in production | `current_cpu` removed; guard (4), (7) | `sched_lock1_d6_per_cpu_scheduler_ownership_rules`; ambient-binding tests assert no binding |

### 17.5 Legacy restrictions, revised explicitly (SC-LOCK-7)

The brief explicitly authorized this.

* `doc/AI_AGENT_RULES.md` §14.4: the D6 bullet's prohibition of per-CPU scheduler lock types is
  replaced by the ownership rules (projection only, ascending acquisition, no waits under a
  scheduler lock, the coherent view only on named rare paths, no ambient selector). The §14.5
  seam rule names the new seams.
* `doc/KERNEL_UNLOCKING.md` §7.1.2 and §8 keep their historical text and point here.
* The source guard `stage106_d6_audit_no_per_cpu_scheduler_locking_started` is replaced by
  `sched_lock1_d6_per_cpu_scheduler_ownership_rules` (`src/kernel/syscall.rs`), which checks:
  (1) no whole-domain lock; (2) the per-CPU guard projects only its CPU; (3) ascending
  acquisition and descending release; (4) the retired seam and selector are gone; (5) exactly
  five production coherent-view callers; (6) no scheduler-lock closure reaches a notification,
  IPI, shootdown, switch or cross-CPU work; (7) the hosted selector is read by exactly one
  production function. The unrelated D6 fence (x86 core smoke default `-smp 1`) is unchanged.
* `doc/KERNEL_LOCKING.md` §4: rank 1 (numbered 2 there) becomes 1G / 1L (2G / 2L).

### 17.6 Evidence

* **Deterministic and concurrent hosted tests** (`src/kernel/sched_lock1_tests/tests.rs`, 19 tests, on
  the real `SchedDomain` and the real `SharedKernel` seams): projection; unauthorized access
  refused before mutation; the clock owner and per-CPU quanta; independence while another CPU's
  1L is held; mutual exclusion on one CPU; the coherent view; placement never tearing; the
  duplicate-current Dekker argument; the mirror balanced choice against the coherent one; the
  frozen-subject scan; duplicate wake; capacity before and after publication; withdrawal against
  dispatch; a refused candidate keeping its position; production seams on CPU 1 while CPU 0 is
  held; opposite-direction remote wakes; wake against idle entry; stale incarnation; shootdown
  targets. The concurrent tests use real threads; the serial hosted suite is a regression gate,
  not evidence of concurrency correctness.
* **Live witness** (default-off `<port>-sched-lock1-witness`, run by
  `scripts/qemu-sched-lock1-witness-smoke.sh`, graded by `scripts/grade-sched-lock1-witness.py`):
  * **Synchronized** (labelled): in phase A of the AP-scheduling workload (two spinners per CPU,
    rotated by timer preemption), CPU A holds its **actual** 1L inside its own production timer
    tick, interrupts masked, while observer CPU B runs its ordinary production scheduling. Every
    attempt, credited or not, is first validated (§17.8):
    * the schedule's pair and the attempt order;
    * A's cell from the cell map;
    * A's lock held, and interrupts masked, at begin and end;
    * exactly one 1L and no 1G;
    * no reacquisition;
    * A's own sections ordered around the hold;
    * a reported outcome consistent with its counters and the fixed bounds;
    * every progress record linked to a real installation.

    A valid attempt is then **credited** for its round if:
    * it reported success;
    * no RELATED record (B touching A's lock) occurred;
    * its window holds ≥ 2 distinct genuine commits by B's own production owner on B's own cell;
    * one of them installed (selected or restored) a task;
    * that task's execution was then observed on B with the CPU's active address space.

    A valid attempt may stay uncredited (its bound, or interference); every round needs one credited
    attempt.
  * **Fixture synchronization** (labelled, not production behaviour): a hold starts only when B
    has a task current and another queued, and until the rounds are done the AP-scheduling
    witness withholds its preemption counts from the workload so phase A continues.
  * **Natural**: from SNAP (taken under the coherent view once phase-A spinners run on every
    CPU) to CENSUS (taken under the coherent view at the workload's last step), every mutating
    1L section is recorded with its CPU, executing CPU, cell, acquisition generation, the current
    slot before and after, and the queue entries added and removed. The grader:
    * replays the trace and requires it to reproduce the CENSUS exactly, each task in at most one
      place;
    * binds every remote CLAIM to the exact section that added its task, claimed once, and to the
      incarnation its owner staged;
    * requires that incarnation to agree with the address space the CPU had active when the task
      next ran;
    * requires every remote addition to be followed by its notification.
  * Every record carries a CRC, and redundant copies must be identical. The grader validates the
    whole trace, every attempt included, before awarding any coverage, and names the failed
    obligation. An observer record reserved after its hold's HOLD_END is validated but credits
    nothing; one naming a round that never began is invalid. The grader's own `--self-test` keeps
    a 7-case smoke set; the full 48-case corpus is `tests/fixtures/sched_lock1/mkcases.py`, run
    through the CLI by `tests/sched_lock1_grader_cli.rs`.
* **AP-scheduling witness parity (witness builds only).** Its grader accepts one benign race —
  the idle CPU's own timer dispatches a woken sleeper before the wake IPI arrives — only with a
  record that the IPI did arrive later. AArch64 recorded that; x86_64 and RISC-V did not, so an
  x86 run at 8 CPUs failed on that race. RISC-V now records a reschedule IPI taken from user mode
  with a sender; x86_64 records one from its vector-0xF1 arrival count between the timer dispatch
  and the CPU's next idle arrival, only when the TLB acknowledgement generation did not move (the
  vector also carries shootdowns). A wake whose IPI never came still fails.
* **Negative controls** (scratch mutations, never committed; results in the qualification
  record). Hosted: global serialization (one shared flag held inside every 1L section), dropped
  publication, dropped notification, substituted destination, substituted incarnation, duplicate
  consumption (the production selection installs the head but leaves it queued), capacity
  stranding (a full queue reports success). Live (RISC-V, `-smp 2`, graded): global
  serialization (while a CPU holds its lock in a witness hold, every other CPU's 1L section waits
  for it, as for one shared lock), dropped notification (every remote one), and — fired **once,
  inside the recording window**, so the failure is that obligation's own — a dropped remote
  publication, a substituted destination and a duplicate consumption. The first two reach the
  grader; a duplicate consumption wedges that CPU's selection (the task is current and still
  queued) before the dump, so it fails at the workload, and the grader's own duplicate check is
  exercised by its self-test fixtures and the hosted control.

### 17.7 Scope of the measured independence

What is shown: an unrelated CPU's local enqueue, selection, current-slot update and timer
preemption complete — and its task runs — while another CPU holds its own scheduler lock, on
all three ports, in both directions of every scheduled pair, at the CPU counts in the
qualification record.

What is **not** claimed:

* that the whole kernel is independently scheduled: rank 2 (task table), the IPC and timeout
  domains, `PRINTK_DRAIN_LOCK` and shootdown waits are still shared; the shootdown target and
  teardown sets read each candidate CPU's current under that CPU's own 1L and so wait for its
  section (§16.5); and the five coherent-view paths still serialize every CPU;
* any latency, contention or WCET bound (W1 unchanged), or anything about kernel preemption
  (KTHREAD1 is not started) or RT scheduling (RT-SCHED1);
* anything about hardware: all live evidence is QEMU TCG.

### 17.8 SCHED-LOCK1-ACCEPTANCE: the witness evidence contract

The delivered grader (`b7f33257`) accepted four kinds of trace that do not show what it credited.
Each was reproduced through its CLI on checksum-valid alterations of its own passing fixture and of
a real passing RISC-V boot log (KERNEL_UNLOCKING, SCHED-LOCK1-ACCEPTANCE). These are checker
defects, not scheduler failures:

1. **Wrong lock instance.** Every section naming a third address passed. The grader compared a
   section's instance only with the holder's self-declared one.
2. **Wrong claim identity.** A claim's ASID was ignored. A claim matched any earlier addition of
   the same TID, so two repetitions of one claim could be "matched" without either being bound.
3. **Invalid uncredited attempt.** An attempt whose holder did not hold its lock and held two
   locks was skipped because its success flag was 0. A later healthy attempt then covered the
   round.
4. **No scheduling mutation.** Sections with an unchanged current task and no queue change
   counted as the observer's commits, and a progress record naming that unchanged task completed
   the award.

**Witness instrumentation added (default-off, `sched-lock1-witness` builds only; production images
are unchanged):**

* `CELL` records the authoritative CPU → lock-instance map at SNAP and again at CENSUS. It is
  taken under the coherent view from the domain's own cell array (`witness_cell_addrs`); each
  record gives the cell and its lock. Section and hold instances are the cell of the lock their
  producer actually holds.
* Each wake owner stages the incarnation it places, `{tid, asid}`, from the authoritative TCB
  while it holds rank 2 (`stage_incarnation`, `stage_from_tcbs`). Five owners do so inside their
  own rank-2 closure: the two planners, the wake path, the shared-region wake and the
  reply-timeout wake. The two receiver seams do so in a rank-2 read beside their own class
  read.

  A claim consumes that staging. It also carries the generation and record number of the
  section on the same executing CPU that last added to the target's queue. A claim with no
  staging for its TID, or no such section, is recorded unbound. Nothing is looked up by bare TID
  after the commit.
* `RUN` records an installed task's first kernel entry on its CPU: the shared trap entry on
  x86_64 and AArch64, the RISC-V trap entry, or the tick. It gives the CPU's active address space
  (the activation table) and the installing section. `PROGRESS` carries the same link for the
  observer during a hold.
* `HOLD_BEGIN` and `HOLD_END` record the holder's interrupt-mask state.

**Grader obligations** (each failure is `reason=<obligation>: …`):
* `validity`;
* `cell-map`;
* `lock-instance` (every section and every hold, credited or not);
* `no-op-section` (the producer suppresses them, and the verifier now refuses them);
* `natural`;
* `claim-commit`;
* `claim-incarnation`;
* `run`;
* `attempt` (every attempt, before coverage);
* `progress`;
* `coverage`.

The round population (2 rounds at 2 CPUs, `2n` otherwise), the attempt limit (3), the hold bound
(4·10⁸ spins) and the commit requirement (2) are fixed in the grader; the log cannot redefine
them.

**Limits.** A claim's incarnation is confirmed only if its task runs again inside the window, so
the grader reports the confirmed count. In development boots every claim was confirmed but one.
The confirmation relies on the activation table showing the running task's address space at a
kernel entry: kernel code runs with interrupts masked except in the idle halt loop, where no task
is current. A claim for a task without an address space is bound to its commit but carries no
ASID to confirm.

