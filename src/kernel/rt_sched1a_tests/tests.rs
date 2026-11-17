// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! RT-SCHED1A — the RT-P1 selection policy on the shared production owners
//! (`doc/SCHEDULING_CONTRACT.md` §23).
//!
//! Every test drives the owners production uses: the per-CPU queues through `CpuView` (enqueue,
//! the acceptance-filtered selection, the tick), the enqueue planner (`enqueue_on_cpu_split`,
//! `task_enqueue::configure_priority_locked`), the one yield transaction over its split owners,
//! the RT scheduling-point owner (`syscall_split::settle_rt_reschedule`) and genuine kernel
//! threads on the hosted pool. Queue positions are read back from the queues themselves, in
//! dispatch order, never inferred.

use std::sync::Mutex;
use std::vec::Vec;

use crate::kernel::boot::Bootstrap;
use crate::kernel::ipc::ThreadId;
use crate::kernel::kthread::{KThreadCtx, is_kthread_tid};
use crate::kernel::scheduler::{
    AcceptedSelection, CpuId, DispatchSelection, MAX_RUN_QUEUE, MAX_RUN_QUEUE_TOTAL, SchedDomain,
    SchedProfile, SchedulerError, TaskPriority,
};
use crate::kernel::syscall::yield_txn::{
    SharedPreemptOwners, SharedYieldOwners, run_yield_transaction,
};
use crate::kernel::syscall_split::{RT_CAUSE_IPI, RtReschedSettlement, settle_rt_reschedule};
use crate::kernel::task::TaskClass;
use crate::kernel::task_enqueue::{self as te, PriorityConfigRefusal};
use crate::runtime::SharedKernel;

/// A CPU no other hosted test of this module shares with the bootstrap's own work.
const CPU: CpuId = CpuId(5);

fn p(level: usize) -> TaskPriority {
    TaskPriority::from_level(level).expect("level")
}

/// A kernel whose CPU 5 is online, in `profile`. CPU 5's per-CPU deferral and trap cells are
/// reset first, so a test that panicked before its own `reset()` cannot leak into the next.
fn kernel(profile: SchedProfile) -> SharedKernel {
    reset();
    let k = SharedKernel::new(Bootstrap::init().expect("init"));
    k.with_all_cpus_sched_split(|all| {
        let present = all.present_cpu_bitmap() | (1 << CPU.0);
        all.set_present_cpu_bitmap(present);
        all.bring_up_cpu(CPU).expect("cpu online");
    });
    k.sched_domain().set_profile(profile);
    k
}

/// Register an App task and CONFIGURE its priority (before its first placement).
fn task(k: &SharedKernel, tid: u64, level: usize) {
    k.with(|s| s.register_task_with_class(tid, TaskClass::App))
        .expect("register");
    k.with_task_enqueue_policy_split_mut(|tcbs, _| {
        te::configure_priority_locked(tcbs, tid, p(level))
    })
    .expect("configure");
}

/// Place `tid` on its home through the production planner + commit (a wake, a spawn).
fn wake(k: &SharedKernel, tid: u64) {
    assert_eq!(k.enqueue_on_cpu_split(CPU, tid), Ok(CPU));
}

/// Make `tid` CPU 5's current task, `Running`, through the broad dispatch (fixture only).
fn make_current(k: &SharedKernel, tid: u64) {
    k.with(|s| {
        s.set_current_cpu(CPU).expect("cpu");
        s.dispatch_next_task().expect("dispatch");
    });
    assert_eq!(current(k), Some(tid), "fixture: {tid} is current");
}

fn current(k: &SharedKernel) -> Option<u64> {
    k.with_cpu_sched_split(CPU, |c| c.current_tid().map(|t| t.0))
        .expect("cpu")
}

/// CPU 5's queue in dispatch order: (level, tid).
fn queue(k: &SharedKernel) -> Vec<(usize, u64)> {
    let mut v = Vec::new();
    k.with_all_cpus_sched_split(|all| {
        all.for_each_queued_on(CPU, |prio, t| v.push((prio.level(), t.0)))
    });
    v
}

/// The real selection primitive, admitting every candidate, then a block: the order in which the
/// queue would hand out the CPU.
fn drain_order(d: &mut SchedDomain, cpu: CpuId) -> Vec<u64> {
    let mut all = d.exclusive();
    let mut out = Vec::new();
    loop {
        let mut c = all.cpu(cpu).expect("cpu");
        match c.dispatch_next_accepted_selection(|_| true) {
            AcceptedSelection::Selected(DispatchSelection::Dequeued { tid }) => {
                out.push(tid.0);
                c.block_current();
            }
            _ => return out,
        }
    }
}

/// Open CPU 5's trap window so the queue-advance admission admits, as a live trap does.
fn open_trap() {
    crate::kernel::boot::GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE[CPU.0 as usize]
        .store(true, core::sync::atomic::Ordering::Relaxed);
}

fn reset() {
    crate::kernel::boot::GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE[CPU.0 as usize]
        .store(false, core::sync::atomic::Ordering::Relaxed);
    crate::kernel::boot::yield_dispatch_clear(CPU.0 as usize);
}

fn rt_domain() -> SchedDomain {
    let d = SchedDomain::new(0b11, 1);
    d.set_profile(SchedProfile::RealTime);
    d
}

// ── SC-PRI-10: 32 levels, 0 highest ─────────────────────────────────────────────────────────

/// Priority extremes: level 0 runs before everything, level 31 after everything, and every level
/// between is strictly ordered.
#[test]
fn rt1a_priority_extremes_level_0_first_level_31_last() {
    let mut d = rt_domain();
    let cpu = CpuId(0);
    let mut all = d.exclusive();
    for (tid, level) in [(1, 31), (2, 0), (3, 16), (4, 31), (5, 0), (6, 1), (7, 30)] {
        all.cpu(cpu)
            .expect("cpu")
            .enqueue_with_priority(ThreadId(tid), p(level))
            .expect("enqueue");
    }
    drop(all);
    assert_eq!(drain_order(&mut d, cpu), [2, 5, 6, 3, 7, 1, 4]);
}

// ── SC-PRI-11: FIFO among equals ────────────────────────────────────────────────────────────

/// Equal priority is FIFO by readiness, including across interleaved selections.
#[test]
fn rt1a_equal_priority_is_fifo_by_readiness() {
    let mut d = rt_domain();
    let cpu = CpuId(0);
    {
        let mut all = d.exclusive();
        let mut c = all.cpu(cpu).expect("cpu");
        for tid in [11, 12, 13] {
            c.enqueue_with_priority(ThreadId(tid), p(9)).expect("enq");
        }
        let first = c.dispatch_next_accepted_selection(|_| true);
        assert_eq!(
            first,
            AcceptedSelection::Selected(DispatchSelection::Dequeued { tid: ThreadId(11) })
        );
        c.block_current();
        for tid in [14, 15] {
            c.enqueue_with_priority(ThreadId(tid), p(9)).expect("enq");
        }
    }
    assert_eq!(drain_order(&mut d, cpu), [12, 13, 14, 15]);
}

// ── SC-PRI-12 (d): a wake joins its level's tail ────────────────────────────────────────────

#[test]
fn rt1a_a_wake_joins_its_levels_tail() {
    let k = kernel(SchedProfile::RealTime);
    for (tid, level) in [(41_001, 9), (41_002, 9), (41_003, 9), (41_004, 3)] {
        task(&k, tid, level);
    }
    wake(&k, 41_001);
    wake(&k, 41_002);
    wake(&k, 41_003);
    assert_eq!(queue(&k), [(9, 41_001), (9, 41_002), (9, 41_003)]);
    // A wake at another level does not touch this one's order.
    wake(&k, 41_004);
    assert_eq!(
        queue(&k),
        [(3, 41_004), (9, 41_001), (9, 41_002), (9, 41_003)]
    );
}

// ── SC-PRI-12 (a) + SC-PRI-13: a strictly higher wake preempts; the preempted task heads its level

/// The current task (level 20) with two queued equals behind it; a level-4 wake. The RT
/// scheduling point preempts it, it returns to the HEAD of level 20 — ahead of the two that were
/// queued before it was preempted — and the next selection is the level-4 task.
#[test]
fn rt1a_a_strictly_higher_wake_preempts_and_the_preempted_task_heads_its_level() {
    let k = kernel(SchedProfile::RealTime);
    for (tid, level) in [(42_001, 20), (42_002, 20), (42_003, 20), (42_009, 4)] {
        task(&k, tid, level);
    }
    wake(&k, 42_001);
    make_current(&k, 42_001);
    wake(&k, 42_002);
    wake(&k, 42_003);
    open_trap();
    // Nothing higher yet: the arrival is declined and nothing moves.
    assert_eq!(
        settle_rt_reschedule(&k, CPU, RT_CAUSE_IPI),
        RtReschedSettlement::Declined
    );
    assert_eq!(current(&k), Some(42_001));
    wake(&k, 42_009);
    assert_eq!(
        settle_rt_reschedule(&k, CPU, RT_CAUSE_IPI),
        RtReschedSettlement::Committed { outgoing: 42_001 }
    );
    assert_eq!(current(&k), None, "the drain owns the slot");
    assert_eq!(
        queue(&k),
        [(4, 42_009), (20, 42_001), (20, 42_002), (20, 42_003)],
        "the preempted task returns to the HEAD of its level"
    );
    assert_eq!(
        k.with_task_tcbs_split_mut(|tcbs| tcbs
            .iter()
            .flatten()
            .find(|t| t.tid.0 == 42_001)
            .map(|t| t.status)),
        Some(crate::kernel::task::TaskStatus::Runnable),
        "the exact Running -> Runnable transition"
    );
    // The selection owner the drain uses hands out the CPU in exactly that order.
    let order: Vec<u64> = (0..4)
        .map(|_| {
            let sel = k
                .with_cpu_sched_split(CPU, |c| {
                    let s = c.dispatch_next_accepted_selection(|_| true);
                    c.block_current();
                    s
                })
                .expect("cpu");
            sel.selection().and_then(|s| s.tid()).expect("tid").0
        })
        .collect();
    assert_eq!(order, [42_009, 42_001, 42_002, 42_003]);
    reset();
}

/// The same through a preempting TICK: under RT a tick reports `Preempt` only when a strictly
/// higher entry is queued, and the transaction it drives heads the level too.
#[test]
fn rt1a_a_preempting_tick_preempts_only_for_strictly_higher_and_heads_the_level() {
    let k = kernel(SchedProfile::RealTime);
    for (tid, level) in [(43_001, 20), (43_002, 20), (43_009, 4)] {
        task(&k, tid, level);
    }
    wake(&k, 43_001);
    make_current(&k, 43_001);
    wake(&k, 43_002);
    for _ in 0..50 {
        assert!(
            matches!(
                k.scheduler_tick_split_mut(CPU),
                crate::runtime::SchedulerTickOutcome::NoSwitch { .. }
            ),
            "an ordinary quantum tick never rotates an RT task"
        );
    }
    wake(&k, 43_009);
    assert!(k.timer_would_preempt_split_read(CPU));
    assert!(matches!(
        k.scheduler_tick_split_mut(CPU),
        crate::runtime::SchedulerTickOutcome::Preempt { .. }
    ));
    open_trap();
    let mut owners = SharedPreemptOwners {
        split: SharedYieldOwners { shared: &k },
        head: true,
    };
    assert!(run_yield_transaction(&mut owners, CPU).is_ok());
    assert_eq!(queue(&k), [(4, 43_009), (20, 43_001), (20, 43_002)]);
    reset();
}

// ── SC-PRI-11: an equal (or lower) wake does not preempt or rotate ──────────────────────────

#[test]
fn rt1a_an_equal_or_lower_priority_wake_is_declined_and_takes_the_tail() {
    let k = kernel(SchedProfile::RealTime);
    for (tid, level) in [(44_001, 20), (44_002, 20), (44_003, 25)] {
        task(&k, tid, level);
    }
    wake(&k, 44_001);
    make_current(&k, 44_001);
    open_trap();
    wake(&k, 44_002);
    assert_eq!(
        settle_rt_reschedule(&k, CPU, RT_CAUSE_IPI),
        RtReschedSettlement::Declined,
        "equal priority: no preemption"
    );
    wake(&k, 44_003);
    assert_eq!(
        settle_rt_reschedule(&k, CPU, RT_CAUSE_IPI),
        RtReschedSettlement::Declined,
        "lower priority: no preemption"
    );
    for _ in 0..20 {
        assert!(matches!(
            k.scheduler_tick_split_mut(CPU),
            crate::runtime::SchedulerTickOutcome::NoSwitch { .. }
        ));
    }
    assert_eq!(current(&k), Some(44_001), "the running task keeps the CPU");
    assert_eq!(queue(&k), [(20, 44_002), (25, 44_003)]);
    reset();
}

// ── SC-PRI-12 (b): an explicit yield goes to the tail ───────────────────────────────────────

#[test]
fn rt1a_an_explicit_yield_goes_to_its_levels_tail() {
    let k = kernel(SchedProfile::RealTime);
    for (tid, level) in [(45_001, 20), (45_002, 20), (45_009, 4)] {
        task(&k, tid, level);
    }
    wake(&k, 45_001);
    make_current(&k, 45_001);
    wake(&k, 45_002);
    // Even with a higher-priority task queued, NR 0 is a yield: the tail (SC-PRI-12 (b)).
    wake(&k, 45_009);
    open_trap();
    let mut owners = SharedYieldOwners { shared: &k };
    assert!(run_yield_transaction(&mut owners, CPU).is_ok());
    assert_eq!(queue(&k), [(4, 45_009), (20, 45_002), (20, 45_001)]);
    reset();
}

// ── SC-CPU / SC-PRI-10: the configured priority binds to the exact incarnation ──────────────

#[test]
fn rt1a_the_configured_priority_binds_to_the_incarnation_and_is_sealed_at_its_first_placement() {
    let k = kernel(SchedProfile::RealTime);
    task(&k, 46_001, 7);
    // Reconfiguring BEFORE the first placement is configuration, not a run-time change.
    assert_eq!(
        k.with_task_enqueue_policy_split_mut(|tcbs, _| te::configure_priority_locked(
            tcbs,
            46_001,
            p(6)
        )),
        Ok(())
    );
    wake(&k, 46_001);
    assert_eq!(
        queue(&k),
        [(6, 46_001)],
        "queued on its home at its configured level"
    );
    assert_eq!(k.task_priority_split_read(46_001), Some(p(6)));
    assert_eq!(
        k.with_task_enqueue_policy_split_mut(|tcbs, _| te::configure_priority_locked(
            tcbs,
            46_001,
            p(2)
        )),
        Err(PriorityConfigRefusal::Sealed),
        "fixed after the first placement: no run-time change, no inheritance, no aging"
    );
    // An unconfigured task keeps the GP class rule.
    k.with(|s| s.register_task_with_class(46_002, TaskClass::App))
        .expect("register");
    assert_eq!(
        k.task_priority_split_read(46_002),
        Some(TaskPriority::Normal)
    );
    assert_eq!(
        k.with_task_enqueue_policy_split_mut(|tcbs, _| te::configure_priority_locked(
            tcbs,
            46_099,
            p(1)
        )),
        Err(PriorityConfigRefusal::NoTask)
    );
}

// ── the full-queue rotation case under the head rule ────────────────────────────────────────

/// SCHED-FULL-ROT1 under RT: the preempted task's level is at its user admission limit, yet the
/// head requeue succeeds (the returning occupant owns its room), and admission still refuses a new
/// user entry at that level afterwards.
#[test]
fn rt1a_a_full_level_still_takes_back_its_preempted_occupant_at_the_head() {
    let k = kernel(SchedProfile::RealTime);
    task(&k, 47_000, 20);
    wake(&k, 47_000);
    make_current(&k, 47_000);
    for i in 0..MAX_RUN_QUEUE as u64 {
        task(&k, 47_001 + i, 20);
        wake(&k, 47_001 + i);
    }
    task(&k, 47_500, 20);
    assert!(
        k.enqueue_on_cpu_split(CPU, 47_500).is_err(),
        "the level is at its user admission limit"
    );
    task(&k, 47_900, 4);
    wake(&k, 47_900);
    open_trap();
    assert_eq!(
        settle_rt_reschedule(&k, CPU, RT_CAUSE_IPI),
        RtReschedSettlement::Committed { outgoing: 47_000 }
    );
    let q = queue(&k);
    assert_eq!(q[0], (4, 47_900));
    assert_eq!(q[1], (20, 47_000), "the occupant heads its full level");
    assert_eq!(
        q.len(),
        2 + MAX_RUN_QUEUE,
        "one rotation slot beyond the limit"
    );
    assert!(k.enqueue_on_cpu_split(CPU, 47_500).is_err());
    reset();
}

// ── occupancy bound: the CPU-wide admission ─────────────────────────────────────────────────

/// Thirty-two levels with the per-level rule alone would admit 32 × 64 user entries. The CPU-wide
/// rule stops user admission at `MAX_RUN_QUEUE_TOTAL` (195) over all levels; kernel threads keep
/// their reserved room.
#[test]
fn rt1a_user_admission_is_bounded_per_cpu_across_all_levels() {
    let mut d = rt_domain();
    let cpu = CpuId(0);
    let mut all = d.exclusive();
    let mut c = all.cpu(cpu).expect("cpu");
    let mut admitted = 0usize;
    let mut tid = 100_000u64;
    'fill: for level in 0..TaskPriority::LEVELS {
        for _ in 0..MAX_RUN_QUEUE {
            tid += 1;
            match c.enqueue_with_priority(ThreadId(tid), p(level)) {
                Ok(()) => admitted += 1,
                Err(SchedulerError::QueueFull) => break 'fill,
                Err(e) => panic!("{e:?}"),
            }
        }
    }
    assert_eq!(admitted, MAX_RUN_QUEUE_TOTAL);
    assert_eq!(
        c.enqueue_with_priority(ThreadId(999_999), p(31)),
        Err(SchedulerError::QueueFull)
    );
    assert_eq!(
        c.reserve_user_slot(p(31)),
        Err(SchedulerError::QueueFull),
        "a reservation is refused by the same rule"
    );
    // A kernel thread is admitted beyond it.
    let kt = ThreadId((0x4B54u64 << 48) | 1);
    assert!(is_kthread_tid(kt.0));
    assert_eq!(c.enqueue_with_priority(kt, p(31)), Ok(()));
    drop(all);
    let order = drain_order(&mut d, cpu);
    assert_eq!(order.len(), MAX_RUN_QUEUE_TOTAL + 1);
}

// ── GP is unchanged ─────────────────────────────────────────────────────────────────────────

/// The general-purpose profile: a wake never preempts (the RT owner answers `NotRealTime`), a
/// quantum expiry preempts and rotates to the TAIL, and an equal-priority task gets its turn.
#[test]
fn rt1a_the_gp_profile_keeps_quantum_rotation_and_wake_never_preempts() {
    let k = kernel(SchedProfile::GeneralPurpose);
    k.with(|s| {
        s.register_task_with_class(48_001, TaskClass::App)
            .expect("r");
        s.register_task_with_class(48_002, TaskClass::App)
            .expect("r");
        s.register_task_with_class(48_003, TaskClass::SystemServer)
            .expect("r");
    });
    wake(&k, 48_001);
    make_current(&k, 48_001);
    wake(&k, 48_002);
    wake(&k, 48_003);
    open_trap();
    assert_eq!(
        settle_rt_reschedule(&k, CPU, RT_CAUSE_IPI),
        RtReschedSettlement::NotRealTime
    );
    assert_eq!(
        current(&k),
        Some(48_001),
        "a GP wake never displaces the running task"
    );
    // The quantum (1 tick in this hosted domain? the shipped one) expires on some tick.
    let mut preempted = false;
    for _ in 0..1000 {
        if matches!(
            k.scheduler_tick_split_mut(CPU),
            crate::runtime::SchedulerTickOutcome::Preempt { .. }
        ) {
            preempted = true;
            break;
        }
    }
    assert!(preempted, "GP quantum expiry preempts");
    let mut owners = SharedYieldOwners { shared: &k };
    assert!(run_yield_transaction(&mut owners, CPU).is_ok());
    assert_eq!(
        queue(&k),
        [(0, 48_003), (1, 48_002), (1, 48_001)],
        "GP: High first, and the rotated task goes to its level's TAIL"
    );
    reset();
}

// ── kernel threads: cooperative, honoured at their next legal scheduling point ──────────────

static KLOG: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
static KSPAWN_LEVEL: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

fn log(s: &'static str) {
    KLOG.lock().unwrap().push(s);
}

fn k_high(_ctx: &KThreadCtx, _arg: usize) {
    log("H_ran");
}

fn k_equal(_ctx: &KThreadCtx, _arg: usize) {
    log("E_ran");
}

/// The running thread wakes another (at `KSPAWN_LEVEL`) on its own CPU — a local wake, whose
/// reschedule the hosted build records as the interrupt the port would raise — and then takes
/// its scheduling point.
fn k_runner(ctx: &KThreadCtx, _arg: usize) {
    log("L_start");
    let level = KSPAWN_LEVEL.load(core::sync::atomic::Ordering::Acquire);
    let entry = if level < 10 { k_high } else { k_equal };
    ctx.pool()
        .create(entry, 0, ctx.home(), p(level))
        .expect("create");
    ctx.pool().note_resched_interrupt(ctx.home(), true);
    match ctx.sched_point().expect("sched point") {
        true => log("L_resumed_after_yield"),
        false => log("L_kept_cpu"),
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

fn kthread_case(profile: SchedProfile, spawn_level: usize) -> Vec<&'static str> {
    KLOG.lock().unwrap().clear();
    KSPAWN_LEVEL.store(spawn_level, core::sync::atomic::Ordering::Release);
    let k = kernel(profile);
    k.kthreads.create(k_runner, 0, CPU, p(10)).expect("runner");
    run_cpu(&k);
    KLOG.lock().unwrap().clone()
}

/// RT: a strictly higher wake during a running kernel thread is honoured at its next scheduling
/// point — the higher thread runs, then the preempted one resumes.
#[test]
fn rt1a_a_higher_wake_during_a_kernel_thread_is_honoured_at_its_scheduling_point() {
    assert_eq!(
        kthread_case(SchedProfile::RealTime, 4),
        ["L_start", "H_ran", "L_resumed_after_yield"]
    );
}

/// RT: an EQUAL-priority wake does not rotate a running kernel thread; it runs after it.
#[test]
fn rt1a_an_equal_wake_does_not_rotate_a_running_kernel_thread() {
    assert_eq!(
        kthread_case(SchedProfile::RealTime, 10),
        ["L_start", "L_kept_cpu", "E_ran"]
    );
}

/// GP (unchanged): the pending reschedule rotates the thread behind its equal.
#[test]
fn rt1a_gp_kernel_thread_scheduling_point_still_rotates() {
    assert_eq!(
        kthread_case(SchedProfile::GeneralPurpose, 10),
        ["L_start", "E_ran", "L_resumed_after_yield"]
    );
}

/// Set `tid`'s status directly (fixture: the IPC paths' own rank-2 transitions are not under test).
fn set_status(k: &SharedKernel, tid: u64, status: crate::kernel::task::TaskStatus) {
    k.with(|s| s.set_task_status_for_test(tid, status));
}

/// IPC handoffs follow the same selection rules (SC-PRI-10/13, §23): a receiver woken by IPC is
/// placed through the production receiver seams with ITS OWN configured priority — never the
/// sender's (no inheritance, no donation) — and the RT scheduling point then preempts a lower
/// sender, which keeps its own level and heads it.
#[test]
fn rt1a_an_ipc_receiver_wake_carries_its_own_configured_priority() {
    use crate::kernel::capabilities::CapId;
    use crate::kernel::task::{TaskStatus, WaitReason};
    let k = kernel(SchedProfile::RealTime);
    task(&k, 43_001, 20); // the sender
    task(&k, 43_002, 3); // woken from EndpointReceive (`wake_tid_to_runnable_split`)
    task(&k, 43_003, 25); // placed by the direct / shared-region seam
    wake(&k, 43_001);
    make_current(&k, 43_001);
    open_trap();
    set_status(
        &k,
        43_002,
        TaskStatus::Blocked(WaitReason::EndpointReceive(CapId(1))),
    );
    k.wake_tid_to_runnable_split(CPU, ThreadId(43_002))
        .expect("the endpoint wake");
    set_status(&k, 43_003, TaskStatus::Runnable);
    assert!(matches!(
        k.sr_enqueue_committed_receiver_split(43_003, Some(CPU)),
        crate::runtime::ReceiverEnqueue::Enqueued { cpu } if cpu == CPU
    ));
    assert_eq!(
        queue(&k),
        [(3, 43_002), (25, 43_003)],
        "each receiver at its own configured level"
    );
    assert_eq!(
        settle_rt_reschedule(&k, CPU, RT_CAUSE_IPI),
        RtReschedSettlement::Committed { outgoing: 43_001 }
    );
    assert_eq!(
        queue(&k),
        [(3, 43_002), (20, 43_001), (25, 43_003)],
        "the sender keeps its own level and heads it"
    );
    reset();
}
