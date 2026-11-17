// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! RT-SCHED1B — budgeted fixed-priority scheduling on the shared production owners
//! (`doc/SCHEDULING_CONTRACT.md` §24).
//!
//! Every test drives the owners production uses: the per-CPU views and their ledger sync, the
//! enqueue planner and commit (which bind configured budgets), `configure_budget_locked`, the RT
//! scheduling point (`settle_rt_reschedule`), the IPC receiver wake, the reap owner's release and
//! genuine hosted kernel threads. Time is advanced the way production advances it — the clock
//! owner's own tick (`CpuView::tick` on CPU 0) — and read back from the contexts themselves.

use std::sync::Mutex;
use std::vec::Vec;

use crate::kernel::boot::Bootstrap;
use crate::kernel::ipc::ThreadId;
use crate::kernel::kthread::{KThreadCtx, is_kthread_tid};
use crate::kernel::sched_budget::{BudgetParams, SchedContext};
use crate::kernel::scheduler::{
    AcceptedSelection, CpuId, DispatchSelection, SchedProfile, SchedulerError, TaskPriority,
};
use crate::kernel::syscall_split::{RT_CAUSE_IPI, RtReschedSettlement, settle_rt_reschedule};
use crate::kernel::task::TaskClass;
use crate::kernel::task_enqueue as te;
use crate::runtime::SharedKernel;

const CPU: CpuId = CpuId(5);

fn p(level: usize) -> TaskPriority {
    TaskPriority::from_level(level).expect("level")
}

fn params(b: u64, t: u64) -> BudgetParams {
    BudgetParams::validate(b, t).expect("valid budget")
}

fn reset() {
    crate::kernel::boot::GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE[CPU.0 as usize]
        .store(false, core::sync::atomic::Ordering::Relaxed);
    crate::kernel::boot::yield_dispatch_clear(CPU.0 as usize);
}

fn open_trap() {
    crate::kernel::boot::GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE[CPU.0 as usize]
        .store(true, core::sync::atomic::Ordering::Relaxed);
}

/// A kernel with CPU 5 online, the RT profile and (`budget`) budget enforcement requested.
fn kernel_with(profile: SchedProfile, budget: bool) -> SharedKernel {
    reset();
    let k = SharedKernel::new(Bootstrap::init().expect("init"));
    k.with_all_cpus_sched_split(|all| {
        let present = all.present_cpu_bitmap() | (1 << CPU.0);
        all.set_present_cpu_bitmap(present);
        all.bring_up_cpu(CPU).expect("cpu online");
    });
    k.sched_domain().set_profile(profile);
    let _ = k.sched_domain().set_budget(budget);
    k
}

fn kernel() -> SharedKernel {
    kernel_with(SchedProfile::RealTime, true)
}

/// Register an App task, CONFIGURE its priority and (optionally) its budget.
fn task(k: &SharedKernel, tid: u64, level: usize, budget: Option<(u64, u64)>) {
    k.with(|s| s.register_task_with_class(tid, TaskClass::App))
        .expect("register");
    k.with_task_enqueue_policy_split_mut(|tcbs, _| {
        te::configure_priority_locked(tcbs, tid, p(level)).expect("priority");
        if let Some((b, t)) = budget {
            te::configure_budget_locked(tcbs, tid, params(b, t)).expect("budget");
        }
    });
}

/// Place `tid` through the production planner + commit (a wake, a first placement).
fn wake(k: &SharedKernel, tid: u64) {
    assert_eq!(k.enqueue_on_cpu_split(CPU, tid), Ok(CPU));
}

/// The clock owner's tick, `n` times: production's only way time advances.
fn advance(k: &SharedKernel, n: u64) {
    for _ in 0..n {
        k.with_cpu_sched_split(CpuId(0), |c| c.tick())
            .expect("cpu0");
    }
}

fn now(k: &SharedKernel) -> u64 {
    k.sched_domain().clock_now().0
}

/// `tid`'s context on CPU 5, read in a CPU 5 section (so the ledger is synced to now first).
fn ctx(k: &SharedKernel, tid: u64) -> SchedContext {
    k.with_cpu_sched_split(CPU, |c| c.context_of(ThreadId(tid)))
        .expect("cpu")
        .expect("bound")
}

fn current(k: &SharedKernel) -> Option<u64> {
    k.with_cpu_sched_split(CPU, |c| c.current_tid().map(|t| t.0))
        .expect("cpu")
}

/// CPU 5's queues in dispatch order, read in a CPU 5 section (ledger synced to now first).
fn queue(k: &SharedKernel) -> Vec<(usize, u64)> {
    let mut v = Vec::new();
    k.with_cpu_sched_split(CPU, |c| {
        c.for_each_queued(|prio, t| v.push((prio.level(), t.0)))
    })
    .expect("cpu");
    v
}

fn reserved(k: &SharedKernel, level: usize) -> usize {
    k.with_cpu_sched_split(CPU, |c| c.reserved_user_slots(p(level)))
        .expect("cpu")
}

fn wanted(k: &SharedKernel) -> bool {
    k.with_cpu_sched_split(CPU, |c| c.rt_preempt_wanted())
        .expect("cpu")
}

/// Dispatch the next task on CPU 5 through the broad dispatch (fixture: it also makes the TCB
/// `Running`, which the preemption transaction's exact transition requires).
fn select(k: &SharedKernel) -> Option<u64> {
    k.with(|s| {
        s.set_current_cpu(CPU).expect("cpu");
        let _ = s.dispatch_next_task();
    });
    current(k)
}

/// Clear CPU 5's current `tid` (a block), then leave its TCB `Runnable` (fixture: the state a
/// later wake re-places from; the block's own rank-2 transition is not under test).
fn block_current(k: &SharedKernel, tid: u64) {
    assert!(
        k.with_cpu_sched_split(CPU, |c| c.block_current_exact(ThreadId(tid)))
            .expect("cpu")
            .is_some()
    );
    k.with(|s| s.set_task_status_for_test(tid, crate::kernel::task::TaskStatus::Runnable));
}

/// SC-CTX-3 / SC-PRI-12 (f): a high-priority task that exhausts its budget is ineligible
/// whatever its priority. The next scheduling point (here the RT reschedule arrival) takes the
/// CPU from it although nothing else outranks it; it holds NO queue position — only a reserved
/// rejoin slot — and the lower-priority task runs. It returns, at its level's tail, only at its
/// next release instant.
#[test]
fn rt1b_an_exhausted_high_task_yields_to_lower_work_and_returns_only_at_its_release() {
    let k = kernel();
    task(&k, 51_001, 4, Some((2, 10)));
    task(&k, 51_002, 20, None);
    wake(&k, 51_002);
    wake(&k, 51_001);
    let t0 = ctx(&k, 51_001).bound_at();
    assert_eq!(ctx(&k, 51_001).params(), params(2, 10));
    assert_eq!(select(&k), Some(51_001), "the high task runs first");
    advance(&k, 1);
    assert!(!ctx(&k, 51_001).exhausted() && !wanted(&k));
    advance(&k, 1);
    let c = ctx(&k, 51_001);
    assert!(c.exhausted() && c.remaining() == 0 && c.debt() == 0);
    assert!(wanted(&k), "exhaustion alone requires a switch");
    open_trap();
    assert_eq!(
        settle_rt_reschedule(&k, CPU, RT_CAUSE_IPI),
        RtReschedSettlement::Committed { outgoing: 51_001 }
    );
    assert_eq!(
        queue(&k),
        [(20, 51_002)],
        "no queue position while exhausted"
    );
    assert!(ctx(&k, 51_001).parked());
    assert_eq!(reserved(&k, 4), 1, "its rejoin slot is reserved");
    reset();
    assert_eq!(select(&k), Some(51_002), "lower-priority work runs");
    while now(&k) < t0 + 9 {
        advance(&k, 1);
        assert_eq!(queue(&k), [], "not back before its release instant");
    }
    advance(&k, 1);
    assert_eq!(now(&k), t0 + 10);
    assert_eq!(queue(&k), [(4, 51_001)], "rejoined at its release instant");
    assert_eq!(reserved(&k, 4), 0, "the reservation became its position");
    let c = ctx(&k, 51_001);
    assert!(!c.parked() && c.remaining() == 2 && c.releases() == 1);
    assert!(wanted(&k), "and outranks the running lower task again");
    reset();
}

/// SC-CTX-8: a wake while exhausted keeps the thread Ready but grants no position and no budget.
#[test]
fn rt1b_a_wake_while_exhausted_preserves_readiness_without_a_position_or_budget() {
    use crate::kernel::capabilities::CapId;
    use crate::kernel::task::{TaskStatus, WaitReason};
    let k = kernel();
    task(&k, 52_001, 6, Some((1, 20)));
    wake(&k, 52_001);
    assert_eq!(select(&k), Some(52_001));
    advance(&k, 1);
    block_current(&k, 52_001);
    assert!(ctx(&k, 52_001).exhausted());
    k.with(|s| {
        s.set_task_status_for_test(
            52_001,
            TaskStatus::Blocked(WaitReason::EndpointReceive(CapId(1))),
        )
    });
    // An IPC wake: the production endpoint wake owner.
    k.wake_tid_to_runnable_split(CPU, ThreadId(52_001))
        .expect("the wake succeeds");
    let status = k.with_task_tcbs_split_mut(|tcbs| {
        tcbs.iter()
            .flatten()
            .find(|t| t.tid.0 == 52_001)
            .map(|t| t.status)
    });
    assert_eq!(status, Some(TaskStatus::Runnable), "readiness preserved");
    assert_eq!(queue(&k), [], "no position");
    let c = ctx(&k, 52_001);
    assert!(c.parked() && c.remaining() == 0, "no budget either");
    assert!(
        k.with_cpu_sched_split(CPU, |c| c.contains_or_current(ThreadId(52_001)))
            .expect("cpu"),
        "still placed here: a second wake cannot place it twice"
    );
    assert_eq!(
        k.with_cpu_sched_split(CPU, |c| c.enqueue_with_priority(ThreadId(52_001), p(6)))
            .expect("cpu"),
        Err(SchedulerError::AlreadyQueued)
    );
}

/// SC-CTX-7: a running thread exhausted and replenished at the SAME instant (charge first, then
/// the release) never becomes ineligible, and keeps the CPU.
#[test]
fn rt1b_same_instant_exhaustion_and_replenishment_keeps_the_selected_thread_running() {
    let k = kernel();
    task(&k, 53_001, 4, Some((2, 2)));
    task(&k, 53_002, 20, None);
    wake(&k, 53_002);
    wake(&k, 53_001);
    assert_eq!(select(&k), Some(53_001));
    advance(&k, 2);
    let c = ctx(&k, 53_001);
    assert_eq!((c.remaining(), c.releases(), c.exhausted()), (2, 1, false));
    assert!(!wanted(&k));
    assert_eq!(current(&k), Some(53_001));
}

/// SC-CTX-7 step 3: simultaneous releases rejoin in ascending CONTEXT id — not in the order the
/// contexts parked.
#[test]
fn rt1b_simultaneous_releases_rejoin_in_context_id_order() {
    let k = kernel();
    let tids = [54_001, 54_002, 54_003];
    for t in tids {
        task(&k, t, 6, Some((1, 10)));
    }
    for t in tids {
        wake(&k, t); // bound at the same instant, in this order: ids ascending
    }
    for t in tids {
        assert_eq!(select(&k), Some(t));
        advance(&k, 1);
        block_current(&k, t);
        assert!(ctx(&k, t).exhausted());
    }
    for t in tids.iter().rev() {
        wake(&k, *t); // parked in REVERSE order
    }
    assert_eq!(queue(&k), []);
    let release = ctx(&k, tids[0]).next_release();
    assert!(tids.iter().all(|t| ctx(&k, *t).next_release() == release));
    while now(&k) < release {
        advance(&k, 1);
    }
    assert_eq!(
        queue(&k),
        [(6, 54_001), (6, 54_002), (6, 54_003)],
        "ascending context id"
    );
}

/// SC-CTX-4/5: an overrun becomes debt that is repaid across several periods and never forgiven:
/// budget 3 with debt 8 replenishes to (0, 5), (0, 2), then (1, 0).
#[test]
fn rt1b_overrun_debt_spanning_several_periods_is_repaid_not_forgiven() {
    let k = kernel();
    task(&k, 55_001, 4, Some((3, 5)));
    wake(&k, 55_001);
    let t0 = ctx(&k, 55_001).bound_at();
    assert_eq!(select(&k), Some(55_001));
    // 11 ticks pass with NO scheduling point on CPU 5 (a non-preemptible stretch): the next
    // section charges all of it — 3 of budget, 8 of overrun — then applies the two due releases.
    advance(&k, 11);
    block_current(&k, 55_001);
    let c = ctx(&k, 55_001);
    assert_eq!(c.overrun(), 8, "the reported overrun");
    assert_eq!(
        (c.remaining(), c.debt(), c.releases()),
        (0, 2, 2),
        "(0, 5) then (0, 2)"
    );
    while now(&k) < t0 + 15 {
        advance(&k, 1);
    }
    let c = ctx(&k, 55_001);
    assert_eq!((c.remaining(), c.debt()), (1, 0), "then (1, 0)");
    while now(&k) < t0 + 20 {
        advance(&k, 1);
    }
    assert_eq!(
        ctx(&k, 55_001).remaining(),
        3,
        "unused budget is not accumulated"
    );
}

/// SCHED-OVERFLOW1 (constructed state): a debt that saturates through the production charge
/// latches the context; parked, it never rejoins however many releases pass, and its release
/// instants still advance. The near-limit debt is constructed (2^64 ticks cannot elapse).
#[test]
fn rt1b_an_overflow_latched_context_never_rejoins_constructed() {
    let k = kernel();
    task(&k, 56_001, 4, Some((1, 4)));
    wake(&k, 56_001);
    assert_eq!(select(&k), Some(56_001));
    k.with_cpu_sched_split(CPU, |c| {
        c.budget_context_mut_for_test(ThreadId(56_001))
            .expect("bound")
            .debt = u64::MAX - 1;
    })
    .expect("cpu");
    advance(&k, 4); // 1 of budget, 3 of overrun: the debt saturates
    let c = ctx(&k, 56_001);
    assert!(c.latched() && c.debt() == u64::MAX && c.exhausted());
    open_trap();
    assert!(matches!(
        settle_rt_reschedule(&k, CPU, RT_CAUSE_IPI),
        RtReschedSettlement::Committed { .. }
    ));
    reset();
    let first = ctx(&k, 56_001).next_release();
    for _ in 0..5 {
        advance(&k, 4);
        assert_eq!(queue(&k), [], "a latched context takes no position");
    }
    let c = ctx(&k, 56_001);
    assert!(c.parked() && c.exhausted() && c.remaining() == 0);
    assert!(
        c.next_release() > first,
        "its release instants still advance"
    );
}

/// Exit / TID reuse: the reap owner releases the retired incarnation's context (and its parked
/// rejoin slot); a reused TID is bound fresh — no budget, debt or latch is inherited. A stale
/// binding met at a reused TID's first placement is replaced the same way.
#[test]
fn rt1b_exit_and_tid_reuse_carry_no_budget_or_debt() {
    let k = kernel();
    task(&k, 57_001, 4, Some((2, 10)));
    wake(&k, 57_001);
    let stamp = k.with_task_tcbs_split_mut(|tcbs| te::incarnation_of_locked(tcbs, 57_001));
    assert_eq!(select(&k), Some(57_001));
    advance(&k, 5);
    open_trap();
    assert!(matches!(
        settle_rt_reschedule(&k, CPU, RT_CAUSE_IPI),
        RtReschedSettlement::Committed { .. }
    ));
    reset();
    let c = ctx(&k, 57_001);
    assert!(c.parked() && c.debt() == 3);
    assert_eq!(reserved(&k, 4), 1);
    // The reap's release of exactly this incarnation.
    assert!(k.release_sched_context_split(57_001, stamp));
    assert_eq!(reserved(&k, 4), 0, "its rejoin slot is given back");
    assert!(
        k.with_cpu_sched_split(CPU, |c| c.context_of(ThreadId(57_001)))
            .expect("cpu")
            .is_none()
    );
    assert!(
        !k.with_cpu_sched_split(CPU, |c| c.contains_or_current(ThreadId(57_001)))
            .expect("cpu")
    );
    // A context for the TID again, then a stale one met by a NEW incarnation's first placement.
    wake(&k, 57_001);
    assert_eq!(select(&k), Some(57_001));
    advance(&k, 4);
    block_current(&k, 57_001);
    assert!(ctx(&k, 57_001).exhausted());
    k.with(|s| s.register_task_with_class(57_002, TaskClass::App))
        .expect("spare");
    let reused = crate::kernel::task::ThreadControlBlock::new(ThreadId(57_001), None);
    let new_stamp = reused.incarnation;
    k.with_task_tcbs_split_mut(|tcbs| {
        let idx = tcbs
            .iter()
            .position(|s| s.as_ref().is_some_and(|t| t.tid.0 == 57_001))
            .expect("slot");
        let mut t = reused;
        t.sched_priority = Some(p(4));
        t.sched_budget = Some(params(2, 10));
        tcbs[idx] = Some(t);
    });
    assert_ne!(new_stamp, stamp);
    wake(&k, 57_001);
    let c = ctx(&k, 57_001);
    assert_eq!(
        (c.remaining(), c.debt(), c.latched()),
        (2, 0, false),
        "fresh"
    );
    assert_eq!(queue(&k), [(4, 57_001)]);
}

/// SC-CTX-1 / SC-POOL-2: the configuration is validated, sealed at the first placement, and the
/// pool is fixed — a binding beyond it is a typed refusal that places nothing.
#[test]
fn rt1b_budget_configuration_is_validated_sealed_and_the_pool_is_bounded() {
    let k = kernel();
    assert!(BudgetParams::validate(0, 4).is_err() && BudgetParams::validate(5, 4).is_err());
    task(&k, 58_000, 20, Some((1, 4)));
    wake(&k, 58_000);
    assert_eq!(
        k.with_task_enqueue_policy_split_mut(|tcbs, _| te::configure_budget_locked(
            tcbs,
            58_000,
            params(2, 4)
        )),
        Err(te::PriorityConfigRefusal::Sealed)
    );
    let bound = k
        .with_cpu_sched_split(CPU, |c| c.budget_ledger().4)
        .expect("cpu");
    for i in 0..(crate::kernel::sched_budget::CTX_PER_CPU - bound) as u64 {
        task(&k, 58_100 + i, 25, None);
        wake(&k, 58_100 + i);
    }
    task(&k, 58_999, 25, None);
    assert_eq!(
        k.enqueue_on_cpu_split(CPU, 58_999),
        Err(crate::kernel::boot::KernelError::SchedulerFull),
        "a full pool refuses the placement"
    );
    assert!(!queue(&k).iter().any(|(_, t)| *t == 58_999));
}

/// Capacity (§24.4): on a budgeted CPU every placed thread holds a context, so the pool bound
/// (`CTX_PER_CPU`) caps the CPU's entries below every queue admission bound — a placement beyond
/// it is refused by the pool, never by a queue — and a parked thread's reserved slot is counted as
/// well. So a due replenishment can never be stranded: with the pool full, the parked thread still
/// rejoins its level's tail at its release instant.
#[test]
fn rt1b_a_due_replenishment_cannot_be_stranded_by_a_full_cpu() {
    const _: () =
        assert!(crate::kernel::sched_budget::CTX_PER_CPU < crate::kernel::scheduler::MAX_RUN_QUEUE);
    let k = kernel();
    task(&k, 59_000, 20, Some((1, 6)));
    wake(&k, 59_000);
    assert_eq!(select(&k), Some(59_000));
    advance(&k, 1);
    open_trap();
    assert!(matches!(
        settle_rt_reschedule(&k, CPU, RT_CAUSE_IPI),
        RtReschedSettlement::Committed { .. }
    ));
    reset();
    assert_eq!(reserved(&k, 20), 1);
    // Fill the CPU: every remaining context, all at the parked thread's level.
    let bound = k
        .with_cpu_sched_split(CPU, |c| c.budget_ledger().4)
        .expect("cpu");
    let room = crate::kernel::sched_budget::CTX_PER_CPU - bound;
    for i in 0..room as u64 {
        task(&k, 59_100 + i, 20, None);
        wake(&k, 59_100 + i);
    }
    task(&k, 59_999, 20, None);
    assert_eq!(
        k.enqueue_on_cpu_split(CPU, 59_999),
        Err(crate::kernel::boot::KernelError::SchedulerFull),
        "the pool, not a queue, is what refuses"
    );
    let release = ctx(&k, 59_000).next_release();
    while now(&k) < release {
        advance(&k, 1);
    }
    let q = queue(&k);
    assert_eq!(q.len(), room + 1);
    assert_eq!(
        q.last(),
        Some(&(20, 59_000)),
        "rejoined at the tail of the full CPU"
    );
    assert_eq!(reserved(&k, 20), 0);
}

/// The ledger telescopes: on a budgeted CPU every elapsed tick is charged exactly once — to a
/// context, the idle ledger, or (never, here) the unaccounted one — across switches and blocks.
#[test]
fn rt1b_every_elapsed_tick_is_charged_exactly_once() {
    let k = kernel();
    let start = now(&k);
    let _ = k.with_cpu_sched_split(CPU, |c| c.budget_ledger());
    task(&k, 60_001, 6, Some((3, 7)));
    task(&k, 60_002, 20, None);
    wake(&k, 60_001);
    wake(&k, 60_002);
    assert_eq!(select(&k), Some(60_001));
    advance(&k, 2);
    block_current(&k, 60_001);
    assert_eq!(select(&k), Some(60_002));
    advance(&k, 3);
    block_current(&k, 60_002);
    advance(&k, 4); // idle
    wake(&k, 60_001);
    assert_eq!(select(&k), Some(60_001));
    advance(&k, 1);
    let (idle, unaccounted, ..) = k
        .with_cpu_sched_split(CPU, |c| c.budget_ledger())
        .expect("cpu");
    let charged = ctx(&k, 60_001).charged() + ctx(&k, 60_002).charged();
    assert_eq!(charged + idle + unaccounted, now(&k) - start);
    assert_eq!(unaccounted, 0, "every runnable thread is accounted");
    assert_eq!(
        (ctx(&k, 60_001).charged(), ctx(&k, 60_002).charged()),
        (3, 3)
    );
}

/// GP is unaffected: the budget request is refused (fails closed), nothing is bound, and the
/// quantum still rotates.
#[test]
fn rt1b_gp_refuses_budgets_and_binds_nothing() {
    let k = kernel_with(SchedProfile::GeneralPurpose, true);
    assert!(!k.sched_domain().budget_enabled());
    assert!(!k.sched_domain().set_budget(true));
    task(&k, 61_001, 1, Some((1, 4)));
    wake(&k, 61_001);
    assert!(
        k.with_cpu_sched_split(CPU, |c| c.context_of(ThreadId(61_001)))
            .expect("cpu")
            .is_none()
    );
    assert_eq!(select(&k), Some(61_001));
    advance(&k, 20);
    assert!(!wanted(&k));
}

/// The deployment configuration's knob: `yarm.sched_budget=on|off|1|0`, anything else ignored.
#[test]
fn rt1b_the_budget_knob_parses_on_off_and_ignores_anything_else() {
    use crate::kernel::boot_command_line::parse_yarm_boot_options as parse;
    assert_eq!(
        parse(b"yarm.sched_profile=rt yarm.sched_budget=on").sched_budget,
        Some(true)
    );
    assert_eq!(parse(b"yarm.sched_budget=1").sched_budget, Some(true));
    assert_eq!(parse(b"yarm.sched_budget=off").sched_budget, Some(false));
    assert_eq!(parse(b"yarm.sched_budget=0").sched_budget, Some(false));
    assert_eq!(parse(b"yarm.sched_budget=maybe").sched_budget, None);
    assert_eq!(parse(b"yarm.sched_profile=rt").sched_budget, None);
}

// ── a cooperative kernel thread is charged and settles at its safe point ────────────────────

static KLOG: Mutex<Vec<(&'static str, u64)>> = Mutex::new(Vec::new());

fn log(k: &SharedKernel, what: &'static str) {
    KLOG.lock().unwrap().push((what, now(k)));
}

/// Simulate CPU 5's timer interrupt landing on the running kernel thread: the production tick,
/// and the pending reschedule the kernel-thread vector records when it preempts.
fn ktick(ctx: &KThreadCtx) {
    advance(ctx.shared(), 1);
    let (_, preempt) = ctx
        .shared()
        .with_cpu_sched_split(ctx.home(), |c| c.tick())
        .expect("cpu");
    if preempt {
        ctx.pool().note_resched_interrupt(ctx.home(), true);
    }
}

fn k_budgeted(ctx: &KThreadCtx, _arg: usize) {
    let k = ctx.shared();
    log(k, "K_start");
    // Run until the budget is gone; its scheduling point then parks it.
    for _ in 0..10 {
        ktick(ctx);
        if ctx.sched_point().expect("sched point") {
            log(k, "K_resumed");
            return;
        }
    }
    log(k, "K_never_exhausted");
}

fn k_low(ctx: &KThreadCtx, _arg: usize) {
    let k = ctx.shared();
    log(k, "W_ran");
    for _ in 0..40 {
        ktick(ctx);
        if ctx.sched_point().expect("sched point") {
            log(k, "W_resumed");
            return;
        }
    }
}

fn run_cpu(k: &SharedKernel) {
    for _ in 0..100 {
        let sel = k
            .with_cpu_sched_split(CPU, |c| {
                c.dispatch_next_accepted_selection(|t| k.kthreads.admit_dispatch(CPU, t))
            })
            .expect("cpu");
        match sel {
            AcceptedSelection::Selected(DispatchSelection::Dequeued { tid })
                if is_kthread_tid(tid.0) =>
            {
                let _ = k.kthreads.dispatch_selected(CPU, tid);
            }
            _ => return,
        }
    }
    panic!("the dispatcher did not settle");
}

/// SC-EXEC-3 / SC-BND-2: a budgeted kernel worker is charged like any context; its exhaustion is
/// settled at its next safe point (`sched_point`), which parks it; lower work runs meanwhile; it
/// resumes only after its release.
#[test]
fn rt1b_a_kernel_thread_is_charged_and_settles_at_its_safe_point() {
    KLOG.lock().unwrap().clear();
    let k = kernel();
    let budgeted = k
        .kthreads
        .create_budgeted(k_budgeted, 0, CPU, p(6), Some(params(2, 8)))
        .expect("budgeted worker");
    k.kthreads.create(k_low, 0, CPU, p(20)).expect("low worker");
    let t0 = ctx(&k, budgeted.raw()).bound_at();
    run_cpu(&k);
    let log = KLOG.lock().unwrap().clone();
    let names: Vec<_> = log.iter().map(|(n, _)| *n).collect();
    assert_eq!(
        names,
        ["K_start", "W_ran", "K_resumed", "W_resumed"],
        "{log:?}"
    );
    let parked_at = log[1].1;
    let resumed_at = log[2].1;
    assert_eq!(
        parked_at,
        t0 + 2,
        "charged its whole budget, then parked at that safe point"
    );
    assert_eq!(
        resumed_at,
        t0 + 8,
        "resumed at its release instant, not before"
    );
    assert!(
        k.with_cpu_sched_split(CPU, |c| c.context_of(budgeted.tid()))
            .expect("cpu")
            .is_none(),
        "its exit's reclaim released the context"
    );
}
