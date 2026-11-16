// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! SCHED-LOCK2A — home-CPU placement regressions (SC-CPU-2, structural).
//!
//! Once an exact live incarnation has a home, only that CPU may reserve, commit or restore its
//! placement; the first home is recorded exactly once even when CPUs race; every check is bound
//! to the incarnation and to the CPU whose lock commits, across dropped locks, reservation
//! rollback, exit and TID reuse; a mismatch fails closed before anything irreversible.
//!
//! The first two tests drive only seams that predate SCHED-LOCK2A (`enqueue_on_cpu_split`,
//! `enqueue_task_split`, the queue counts and the home read), so they run unchanged against the
//! old code — where they fail — and are the off-home demonstration. The rest drive the new owners
//! directly.

use std::sync::Barrier;
use std::vec::Vec;

use crate::kernel::boot::{Bootstrap, KernelError};
use crate::kernel::ipc::ThreadId;
use crate::kernel::scheduler::{CpuId, SchedDomain, TaskPriority};
use crate::kernel::task::{TaskClass, TaskStatus, ThreadControlBlock};
use crate::kernel::task_enqueue::{
    self as te, EnqueuePlan, EnqueueTarget, ReservedCommit, admit_home_locked, bind_home_locked,
};
use crate::runtime::SharedKernel;

fn kernel2() -> SharedKernel {
    let k = SharedKernel::new(Bootstrap::init().expect("init"));
    k.with(|s| s.bring_up_cpu(CpuId(1)).expect("cpu1 online"));
    k
}

fn register(k: &SharedKernel, tids: impl IntoIterator<Item = u64>) {
    k.with(|s| {
        for tid in tids {
            s.register_task_with_class(tid, TaskClass::App)
                .expect("register");
        }
    });
}

fn queued_on(k: &SharedKernel, cpu: CpuId, tid: u64) -> bool {
    k.with_cpu_sched_split(cpu, |c| c.contains_or_current(ThreadId(tid)))
        .unwrap_or(false)
}

fn home(k: &SharedKernel, tid: u64) -> Option<CpuId> {
    k.with(|s| s.task_home_cpu(tid))
}

fn domain2() -> SchedDomain {
    let mut d = SchedDomain::new(0b11, 1);
    let mut all = d.exclusive();
    all.bring_up_cpu(CpuId(1)).expect("cpu1 online");
    drop(all);
    d
}

fn tcb(tid: u64, home: Option<CpuId>) -> ThreadControlBlock {
    tcb_with(tid, home, TaskStatus::Runnable)
}

fn tcb_with(tid: u64, home: Option<CpuId>, status: TaskStatus) -> ThreadControlBlock {
    ThreadControlBlock {
        cpu_affinity: home,
        status,
        ..ThreadControlBlock::new(ThreadId(tid), Some(crate::kernel::vm::Asid(3)))
    }
}

fn plan_for(t: &ThreadControlBlock, cpu: CpuId) -> EnqueuePlan {
    EnqueuePlan {
        tid: t.tid.0,
        priority: TaskPriority::Normal,
        target: EnqueueTarget::Pinned(cpu),
        incarnation: t.incarnation,
    }
}

// ── off-home refusal (old-path demonstration) ────────────────────────────────────────────────

/// A homed task named for another CPU is refused, and nothing anywhere changes: not the target
/// queue, not the home queue, not the home record. On the old code the pinned plan overrode the
/// home and the task was queued on CPU 0 while its home stayed CPU 1 — this test fails there.
#[test]
fn sl2a_an_off_home_pinned_enqueue_is_refused_with_no_partial_placement() {
    let k = kernel2();
    register(&k, [91_001]);
    assert_eq!(k.enqueue_on_cpu_split(CpuId(1), 91_001), Ok(CpuId(1)));
    assert_eq!(home(&k, 91_001), Some(CpuId(1)));
    let cpu0_before = k.runnable_count_on_cpu_split_read(CpuId(0));
    let cpu1_before = k.runnable_count_on_cpu_split_read(CpuId(1));
    assert!(
        k.enqueue_on_cpu_split(CpuId(0), 91_001).is_err(),
        "an explicit placement off the home must be refused"
    );
    assert_eq!(k.runnable_count_on_cpu_split_read(CpuId(0)), cpu0_before);
    assert_eq!(k.runnable_count_on_cpu_split_read(CpuId(1)), cpu1_before);
    assert!(!queued_on(&k, CpuId(0), 91_001));
    assert!(queued_on(&k, CpuId(1), 91_001));
    assert_eq!(
        home(&k, 91_001),
        Some(CpuId(1)),
        "the home record is unchanged"
    );
}

/// Two CPUs racing to place a homeless task FIRST, on different CPUs: exactly one wins, its CPU
/// becomes the home, and the task is placed there and nowhere else. On the old code both pinned
/// plans were committed (the second ignored the home the first recorded), so the task could be
/// queued on both CPUs.
#[test]
fn sl2a_competing_first_placements_record_exactly_one_home() {
    let k = kernel2();
    let tids: Vec<u64> = (92_000..92_200).collect();
    register(&k, tids.iter().copied());
    for &tid in &tids {
        let barrier = Barrier::new(2);
        let (a, b) = std::thread::scope(|s| {
            let ha = s.spawn(|| {
                barrier.wait();
                k.enqueue_on_cpu_split(CpuId(0), tid)
            });
            let hb = s.spawn(|| {
                barrier.wait();
                k.enqueue_on_cpu_split(CpuId(1), tid)
            });
            (ha.join().expect("a"), hb.join().expect("b"))
        });
        let won: Vec<CpuId> = [a, b].into_iter().filter_map(Result::ok).collect();
        assert_eq!(won.len(), 1, "tid {tid}: exactly one first placement wins");
        let h = home(&k, tid).expect("a home was recorded");
        assert_eq!(won[0], h, "tid {tid}: the winner's CPU is the home");
        assert!(queued_on(&k, h, tid));
        let other = CpuId(1 - h.0);
        assert!(!queued_on(&k, other, tid), "tid {tid}: placed off its home");
        // Keep both 64-slot queues from filling over the 200 rounds.
        let _ = k.with_cpu_sched_split(h, |c| c.withdraw_queued_tid(ThreadId(tid)));
    }
}

/// The balanced twin of the race: two balanced first placements of one homeless task land on one
/// CPU — the home the first recorded — and the loser is the scheduler's duplicate refusal there.
#[test]
fn sl2a_competing_balanced_first_placements_converge_on_the_one_home() {
    let k = kernel2();
    let tids: Vec<u64> = (93_000..93_100).collect();
    register(&k, tids.iter().copied());
    for &tid in &tids {
        let barrier = Barrier::new(2);
        let (a, b) = std::thread::scope(|s| {
            let ha = s.spawn(|| {
                barrier.wait();
                k.enqueue_task_split(CpuId(0), tid)
            });
            let hb = s.spawn(|| {
                barrier.wait();
                k.enqueue_task_split(CpuId(1), tid)
            });
            (ha.join().expect("a"), hb.join().expect("b"))
        });
        let h = home(&k, tid).expect("home");
        for r in [a, b] {
            match r {
                Ok(cpu) => assert_eq!(cpu, h),
                Err(e) => assert_eq!(e, KernelError::WouldBlock, "only the duplicate refusal"),
            }
        }
        assert!(queued_on(&k, h, tid));
        assert!(!queued_on(&k, CpuId(1 - h.0), tid));
        let _ = k.with_cpu_sched_split(h, |c| c.withdraw_queued_tid(ThreadId(tid)));
    }
}

// ── the owners themselves ───────────────────────────────────────────────────────────────────

/// The rank-2 binding: a homeless incarnation takes the first CPU named, and only that one.
#[test]
fn sl2a_the_binding_records_the_first_home_and_refuses_any_other() {
    let mut tcbs = [Some(tcb(7, None))];
    let inc = tcbs[0].as_ref().unwrap().incarnation;
    assert_eq!(bind_home_locked(&mut tcbs, 7, CpuId(1)), Ok(inc));
    assert_eq!(tcbs[0].as_ref().unwrap().cpu_affinity, Some(CpuId(1)));
    let before = tcbs.clone();
    assert_eq!(
        bind_home_locked(&mut tcbs, 7, CpuId(0)),
        Err(KernelError::WrongObject)
    );
    assert_eq!(tcbs, before, "a refused binding writes nothing");
    assert_eq!(bind_home_locked(&mut tcbs, 7, CpuId(1)), Ok(inc));
    // TID 0 and an unknown TID bind nothing.
    assert_eq!(bind_home_locked(&mut tcbs, 0, CpuId(0)), Ok(0));
    assert_eq!(bind_home_locked(&mut tcbs, 99, CpuId(0)), Ok(0));
    assert_eq!(tcbs, before);
}

/// Every 1L step refuses an off-home plan before it mutates: the commit queues nothing, the
/// reservation promises nothing.
#[test]
fn sl2a_off_home_commit_and_reservation_mutate_nothing() {
    let d = domain2();
    let t = tcb(11, Some(CpuId(1)));
    let plan = plan_for(&t, CpuId(0));
    let tcbs = [Some(t)];
    let mut g = d.lock_cpu(CpuId(0)).expect("cpu0");
    let mut v = g.view();
    assert!(te::commit_enqueue_on_cpu_locked(&mut v, &plan, &tcbs).is_err());
    assert!(te::reserve_enqueue_on_cpu_locked(&mut v, &plan, &tcbs).is_err());
    assert_eq!(v.runnable_count(), 0);
    assert_eq!(v.witness_occupancy(TaskPriority::Normal), (0, 0));
    assert!(!v.contains_or_current(ThreadId(11)));
}

/// TID reuse across a dropped lock: a plan made for incarnation A is refused once the TID names
/// incarnation B — whether B has another home, the same home, or none — and an unreaped TID that
/// is gone entirely is refused too. Nothing is queued or promised.
#[test]
fn sl2a_a_stale_plan_after_tid_reuse_places_nothing() {
    let d = domain2();
    let a = tcb(21, Some(CpuId(0)));
    let plan = plan_for(&a, CpuId(0));
    for b_home in [Some(CpuId(1)), Some(CpuId(0)), None] {
        // Same TID and the same ASID (a sibling thread reusing it): only the stamp differs.
        let b = tcb(21, b_home);
        assert_ne!(b.incarnation, a.incarnation);
        let tcbs = [Some(b)];
        let mut g = d.lock_cpu(CpuId(0)).expect("cpu0");
        let mut v = g.view();
        assert_eq!(
            admit_home_locked(&tcbs, 21, plan.incarnation, CpuId(0)),
            Err(KernelError::WrongObject)
        );
        assert!(te::reserve_enqueue_on_cpu_locked(&mut v, &plan, &tcbs).is_err());
        assert!(te::commit_enqueue_on_cpu_locked(&mut v, &plan, &tcbs).is_err());
        assert_eq!(v.runnable_count(), 0);
        assert_eq!(v.witness_occupancy(TaskPriority::Normal), (0, 0));
    }
    // Reaped: no TCB names the TID at all.
    let none: [Option<ThreadControlBlock>; 1] = [None];
    assert_eq!(
        admit_home_locked(&none, 21, plan.incarnation, CpuId(0)),
        Err(KernelError::WrongObject)
    );
}

/// Reservation settlement across reuse: the slot was promised for incarnation A, A was claimed,
/// and before the commit the TID came to name B. The commit refuses (`Refused`), queues nothing
/// and releases the promise itself, so the slot is settled exactly once.
#[test]
fn sl2a_a_reserved_commit_for_a_reused_tid_releases_its_slot() {
    let d = domain2();
    let a = tcb(31, Some(CpuId(1)));
    let plan = plan_for(&a, CpuId(1));
    let mut g = d.lock_cpu(CpuId(1)).expect("cpu1");
    let mut v = g.view();
    te::reserve_enqueue_on_cpu_locked(&mut v, &plan, &[Some(a)]).expect("A reserves on home");
    assert_eq!(v.witness_occupancy(TaskPriority::Normal), (0, 1));
    let b = tcb(31, Some(CpuId(1)));
    assert_eq!(
        te::commit_reserved_enqueue_on_cpu_locked(&mut v, &plan, &[Some(b)]),
        Ok(ReservedCommit::Refused(CpuId(1)))
    );
    assert_eq!(
        v.witness_occupancy(TaskPriority::Normal),
        (0, 0),
        "promise released"
    );
    assert!(!v.contains_or_current(ThreadId(31)));
}

/// Same-home rollback and settlement are unchanged: reserve-then-release leaves no promise;
/// reserve-then-commit on the home queues once; a second plan on the home for the same
/// incarnation is admitted (the dispatch-rollback requeue), and the duplicate is the queue's own
/// refusal, not the home's.
#[test]
fn sl2a_same_home_rollback_and_settlement_are_admitted() {
    let d = domain2();
    let t = tcb(41, Some(CpuId(1)));
    let plan = plan_for(&t, CpuId(1));
    let tcbs = [Some(t)];
    let mut g = d.lock_cpu(CpuId(1)).expect("cpu1");
    let mut v = g.view();
    te::reserve_enqueue_on_cpu_locked(&mut v, &plan, &tcbs).expect("reserve");
    te::release_enqueue_reservation_on_cpu_locked(&mut v, &plan);
    assert_eq!(v.witness_occupancy(TaskPriority::Normal), (0, 0));
    te::reserve_enqueue_on_cpu_locked(&mut v, &plan, &tcbs).expect("reserve again");
    assert_eq!(
        te::commit_reserved_enqueue_on_cpu_locked(&mut v, &plan, &tcbs),
        Ok(ReservedCommit::Queued(CpuId(1)))
    );
    assert_eq!(v.witness_occupancy(TaskPriority::Normal), (1, 0));
    // Rollback-style direct requeue of the same incarnation on its home: admitted by the home,
    // refused only as a duplicate by the queue.
    assert_eq!(
        te::commit_enqueue_on_cpu_locked(&mut v, &plan, &tcbs),
        Err(KernelError::WouldBlock)
    );
    assert_eq!(v.runnable_count(), 1);
}

/// Capacity refusal is unchanged and still comes before any mutation, on the home.
#[test]
fn sl2a_capacity_refusal_is_unchanged_on_the_home() {
    let d = domain2();
    let mut g = d.lock_cpu(CpuId(0)).expect("cpu0");
    let mut v = g.view();
    for n in 0..crate::kernel::scheduler::MAX_RUN_QUEUE as u64 {
        v.enqueue_with_priority(ThreadId(50_000 + n), TaskPriority::Normal)
            .expect("fill");
    }
    let t = tcb(51, Some(CpuId(0)));
    let plan = plan_for(&t, CpuId(0));
    let tcbs = [Some(t)];
    assert_eq!(
        te::reserve_enqueue_on_cpu_locked(&mut v, &plan, &tcbs),
        Err(KernelError::SchedulerFull)
    );
    assert_eq!(
        te::commit_enqueue_on_cpu_locked(&mut v, &plan, &tcbs),
        Err(KernelError::SchedulerFull)
    );
    assert!(!v.contains_or_current(ThreadId(51)));
}

/// An exited but unreaped incarnation keeps its home: an entry for it is admitted there only, so
/// an exited entry that is still queued is always queued on its home.
#[test]
fn sl2a_an_exited_unreaped_incarnation_is_admitted_on_its_home_only() {
    let t = tcb_with(61, Some(CpuId(1)), TaskStatus::Exited(0));
    let inc = t.incarnation;
    let tcbs = [Some(t)];
    assert_eq!(admit_home_locked(&tcbs, 61, inc, CpuId(1)), Ok(()));
    assert_eq!(
        admit_home_locked(&tcbs, 61, inc, CpuId(0)),
        Err(KernelError::WrongObject)
    );
}

/// The exact-current restore is the home's alone: with BOTH CPUs' current slots empty and the task
/// queued nowhere, restoring a homed task as another CPU's current is refused and that slot stays
/// empty; restoring it on its home is admitted.
#[test]
fn sl2a_an_off_home_exact_current_restore_is_refused() {
    let k = kernel2();
    register(&k, [94_001]);
    assert_eq!(k.enqueue_on_cpu_split(CpuId(1), 94_001), Ok(CpuId(1)));
    let _ = k.with_cpu_sched_split(CpuId(1), |c| c.withdraw_queued_tid(ThreadId(94_001)));
    for cpu in [CpuId(0), CpuId(1)] {
        let _ = k.with_cpu_sched_split(cpu, |c| c.block_current());
        assert_eq!(
            k.with_cpu_sched_split(cpu, |c| c.current_tid()).unwrap(),
            None
        );
    }
    assert!(!k.restore_current_exact_split(CpuId(0), 94_001, TaskPriority::Normal));
    assert_eq!(
        k.with_cpu_sched_split(CpuId(0), |c| c.current_tid())
            .unwrap(),
        None,
        "CPU 0's current slot is untouched"
    );
    assert!(k.restore_current_exact_split(CpuId(1), 94_001, TaskPriority::Normal));
    assert_eq!(
        k.with_cpu_sched_split(CpuId(1), |c| c.current_tid())
            .unwrap(),
        Some(ThreadId(94_001))
    );
}

/// The incarnation stamp is unique per constructed TCB and survives a clone (a clone is the same
/// incarnation, not a new one).
#[test]
fn sl2a_incarnation_stamps_are_unique_and_clone_preserving() {
    let a = ThreadControlBlock::new(ThreadId(5), None);
    let b = ThreadControlBlock::new(ThreadId(5), None);
    assert_ne!(a.incarnation, b.incarnation);
    assert_ne!(a.incarnation, 0);
    assert_eq!(a.clone().incarnation, a.incarnation);
}

// ── the x86_64 SMP oracle fact gate ─────────────────────────────────────────────────────────

/// Disarmed, the oracle's facts are not collected at all (the collector — which on x86_64 takes
/// the task lock, the IPC lock and every CPU's 1L for the run-queue scan — never runs); armed,
/// the body receives exactly what the collector read.
#[test]
fn sl2a_the_smp_oracle_facts_are_collected_only_when_armed() {
    use std::cell::Cell;
    let runs = Cell::new(0u32);
    let collect = || {
        runs.set(runs.get() + 1);
        (true, false, Some(CpuId(1)))
    };
    assert_eq!(
        crate::kernel::boot::oracle_gated_facts(false, collect),
        None
    );
    assert_eq!(runs.get(), 0, "disarmed: no read at all");
    assert_eq!(
        crate::kernel::boot::oracle_gated_facts(true, collect),
        Some((true, false, Some(CpuId(1))))
    );
    assert_eq!(
        runs.get(),
        1,
        "armed: exactly one collection, its facts unchanged"
    );
}

/// Source guard for the gate's one production call site: the x86_64 split blocked-server marker
/// collects its facts — including the run-queue scan — only inside the gate, under the existing
/// oracle predicate, and nowhere else on that route.
#[test]
fn sl2a_source_the_blocked_server_scan_sits_behind_the_oracle_predicate() {
    const SRC: &str = include_str!("../syscall_split.rs");
    let start = SRC
        .find("SCHED-LOCK2A: the facts are collected only when the SMP oracle is armed")
        .expect("gate comment");
    let end = start + SRC[start..].find("let _ = server_ack;").expect("block end");
    let block = &SRC[start..end];
    let gate = block
        .find("oracle_gated_facts(\n                crate::kernel::boot::x86_ipccall_direct_smp_oracle_enabled(),")
        .expect("the gate names the existing predicate");
    let scan = block
        .find("receiver_has_scheduler_membership_split_read(tid)")
        .expect("the scan is inside the block");
    let facts = block
        .find("smp_blocked_server_task_facts_split_read(tid)")
        .expect("the task facts are inside the block");
    assert!(gate < facts && gate < scan, "every read follows the gate");
    // The scan appears exactly once in the file's marker route: inside the gate.
    let route_start = SRC
        .find("U9-RECV-BLOCK1 §2 — the AUTHORITATIVE cross-CPU blocked-server marker")
        .unwrap();
    let route = &SRC[route_start..end];
    assert_eq!(
        route
            .matches("receiver_has_scheduler_membership_split_read")
            .count(),
        1
    );
}
