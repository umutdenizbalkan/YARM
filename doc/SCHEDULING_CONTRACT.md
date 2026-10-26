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
| SC-KT-4 | Asynchronous preemption of kernel mode. Not to be enabled before SCHED-LOCK1 (SC-LOCK-8) and KERNEL-NOFP1 (SC-FP-1). | none: masked kernel | none | none; S-mode traps other than idle halt | **SPECIFIED → KTHREAD1**, gated by SCHED-LOCK1 and KERNEL-NOFP1 |
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

What this establishes is exact. For the decoded bytes, the disassembler's linear decoding has no
FP/SIMD or FP-enable instruction outside an audited exception. The undecoded bytes are exactly the
reviewed inventory's anchored ranges and the justified literal-pool words. It is **not** a proof
of control-flow safety: nothing shows that execution never jumps into an excluded range or into
the middle of a decoded instruction. It is **not** a proof that the disassembler decodes what the
CPU executes.

**Remaining limits (OPEN):**

* **OPEN — the AArch64 core grader's fresh-record assumption.** It still pins the U9-RX4 reply
  record to generation 1. It passed every scheduled run, but it would misjudge a reused-slot
  boot, as the x86 grader did.
* **OPEN — smoke build paths outside automatic enforcement.** Smoke scripts that build a kernel
  directly with `cargo` rather than through the artifact scripts are not gated at build time.
  The qualification runner checks every kernel it boots.
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
| SC-LOCK-1…8 (§12) | one rank-1 lock over every CPU's scheduler state | CPU-local ownership, bounded local exclusion, cross-CPU wake protocol | P + L + S (progress argument) | **SCHED-LOCK1** (OPEN, next) |

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

## 12. SCHED-LOCK1: the scheduler domain lock is an OPEN prerequisite

**The problem, from source (§1.1, §1.2).**

* The per-CPU run queues, current slots and quanta all live inside ONE rank-1 scheduler domain
  lock (`SchedulerState` behind `with_scheduler_split_mut`).
* So unrelated CPUs serialise enqueue, selection, blocking and timer bookkeeping on one critical
  section, every one of them with interrupts masked.
* That lock is a TTAS `SpinLockIrq` with no fairness (§6 W1).
* This is a **different lock** from the retired broad `KernelState` lock, whose production
  census is 0. Retiring the broad lock did not make the scheduler CPU-local.

**SCHED-LOCK1 is OPEN.** Its requirements:

* **SC-LOCK-1 — CPU-local scheduling state has explicit local ownership.** **SPECIFIED →
  SCHED-LOCK1.** Each CPU's runnable queues, current slot and quantum belong to that CPU. Another
  CPU's scheduling operation does not enter that CPU's critical section, except through SC-LOCK-3.
* **SC-LOCK-2 — Local exclusion is bounded.** **SPECIFIED → SCHED-LOCK1.** The exclusion that
  protects CPU-local state has a stated progress argument and a bound in the §6 kinds. A local
  lock is acceptable only with that argument. An owner-consumed queue is acceptable only with
  that argument. **"Lock-free" is not mandated and is not by itself an argument.**
* **SC-LOCK-3 — There is an explicit cross-CPU wake protocol.** **SPECIFIED → SCHED-LOCK1.** A
  remote wake publishes to the target CPU by a defined protocol, which must specify:
  * the remote-queue capacity, and the behaviour when it is full — never silent loss;
  * publication ordering against the reschedule IPI;
  * cancellation: a wake for an incarnation that died or was already woken;
  * lost-wakeup prevention, including the target entering idle concurrently with publication.
* **SC-LOCK-4 — Existing guarantees are preserved.** **SPECIFIED → SCHED-LOCK1.** The
  decomposition preserves:
  * exact task-incarnation authentication (`{tid, asid}` and generations);
  * queue membership (one place per runnable incarnation, SC-CTX-8 for RT-P1);
  * wake arbitration (exactly one waker wins);
  * `DispatchAuthority` and `DispatchMarkToken`;
  * the ascending lock order.
* **SC-LOCK-5 — Global coordination is separated from local dispatch.** **SPECIFIED →
  SCHED-LOCK1.** Genuinely global state is owned separately from per-CPU dispatch and is never
  taken on a CPU's ordinary dispatch path. That state includes the clock owner's tick
  (SC-TIME-2), topology, admission (wake-only and balance-excluded sets), balanced placement and
  the ambient `current_cpu`.
* **SC-LOCK-6 — Remaining shared dependencies are inventoried.** **SPECIFIED → SCHED-LOCK1.**
  Splitting this one lock does not make scheduling bounded. The package inventories every shared
  dependency left on a dispatch path, and claims nothing about the whole scheduler's bound. The
  known ones are:
  * the rank-2 task table read by the acceptance filter (§6 W2);
  * `PRINTK_DRAIN_LOCK` (W7);
  * the IPC and timeout domains;
  * TLB shootdown waits (W4).
* **SC-LOCK-7 — The legacy restrictions are reconciled explicitly.** **SPECIFIED → SCHED-LOCK1.**
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
  relax any of them.
* **SC-LOCK-8 — Ordering in the roadmap.** **SPECIFIED.**
  * SCHED-LOCK1 is scheduled **before** preemptible kernel threads are enabled, and **before** any
    claim of bounded RT scheduling latency.
  * Basic kernel-context preparation (KTHREAD1's stacks, entry ABI and cooperative switch) may
    proceed separately. It must not enable preemption that SCHED-LOCK1's exclusion rules would
    make unsafe.
* **SC-LOCK-9 — No lock is split before SCHED-LOCK1.** **ENFORCED** by the legacy guard above.
  SCHED-CONTRACT1 and its correction change no lock.

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
  * SC-LOCK-9.
* **SPECIFIED, with an implementing package:**
  * **SCHED-LOCK1** (next): SC-LOCK-1…8.
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
