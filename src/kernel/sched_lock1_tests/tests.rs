// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! SCHED-LOCK1 §5 — deterministic and CONCURRENT tests on the real scheduler owners.
//!
//! Every test drives the production `SchedDomain` (its per-CPU locks, admission lock, clock and
//! mirrors) or the production `SharedKernel` seams built on it. The concurrent tests run real OS
//! threads against those owners; they are evidence about the lock protocol, not about hardware
//! latency. The serial hosted suite remains the regression gate — it says nothing about
//! concurrency — and these tests are what speaks to it.
//!
//! No test waits unboundedly: every cross-thread wait has a deadline and fails (rather than hangs)
//! when the deadline passes, so a protocol that serialises or deadlocks shows up as a failure.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};
use std::vec::Vec;

use crate::kernel::boot::Bootstrap;
use crate::kernel::ipc::ThreadId;
use crate::kernel::scheduler::{
    CpuId, DispatchSelection, MAX_RUN_QUEUE, SchedDomain, SchedulerError, TaskPlacement,
    TaskPriority, WithdrawOutcome,
};
use crate::kernel::task::TaskClass;
use crate::runtime::SharedKernel;

const DEADLINE: Duration = Duration::from_secs(20);

fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let end = Instant::now() + DEADLINE;
    while !cond() {
        assert!(Instant::now() < end, "timed out waiting for: {what}");
        std::hint::spin_loop();
    }
}

/// A two-CPU domain (CPU 0 and CPU 1 online, both dispatching).
fn domain2() -> SchedDomain {
    let mut d = SchedDomain::new(0b11, 1);
    let mut all = d.exclusive();
    all.bring_up_cpu(CpuId(1)).expect("cpu1 online");
    drop(all);
    d
}

/// A four-CPU domain.
fn domain4() -> SchedDomain {
    let mut d = SchedDomain::new(0b1111, 1);
    let mut all = d.exclusive();
    for c in 1..4 {
        all.bring_up_cpu(CpuId(c)).expect("online");
    }
    drop(all);
    d
}

fn t(n: u64) -> ThreadId {
    ThreadId(n)
}

// ── per-CPU authority (deterministic) ────────────────────────────────────────────────────────

/// A 1L guard projects exactly its CPU: everything it does lands on that CPU, and every other
/// CPU's state — queues, current slot, mirror — is untouched.
#[test]
fn sl1_a_cpu_guard_projects_only_its_own_cpu() {
    let d = domain2();
    {
        let mut g = d.lock_cpu(CpuId(1)).expect("cpu1");
        let mut v = g.view();
        assert_eq!(v.cpu(), CpuId(1));
        v.enqueue_with_priority(t(7), TaskPriority::Normal)
            .expect("enqueue");
        assert_eq!(v.dispatch_next(), Some(t(7)));
    }
    assert_eq!(d.current_mirror(1), Some(t(7)), "published at release");
    assert_eq!(d.load_mirror(1), 1);
    assert_eq!(d.current_mirror(0), None, "CPU 0 untouched");
    assert_eq!(d.load_mirror(0), 0);
    let mut g0 = d.lock_cpu(CpuId(0)).expect("cpu0");
    let v0 = g0.view();
    assert_eq!(v0.current_tid(), None);
    assert_eq!(v0.runnable_count(), 0);
    assert!(!v0.contains_or_current(t(7)));
}

/// A CPU outside the domain has no lock: refused with `InvalidCpu`, nothing locked or changed.
/// A plan naming another CPU is refused by the per-CPU commit before any mutation.
#[test]
fn sl1_unauthorized_cpu_access_is_refused_before_mutation() {
    let d = domain2();
    assert!(matches!(
        d.lock_cpu(CpuId(200)),
        Err(SchedulerError::InvalidCpu)
    ));
    assert!(matches!(
        d.lock_cpu(CpuId(64)),
        Err(SchedulerError::InvalidCpu)
    ));
    // An offline-but-present CPU's view refuses placement.
    let mut d3 = SchedDomain::new(0b111, 1);
    {
        let mut g = d3.lock_cpu(CpuId(2)).expect("cell exists");
        let mut v = g.view();
        assert_eq!(
            v.enqueue_with_priority(t(1), TaskPriority::Normal),
            Err(SchedulerError::CpuOffline)
        );
        assert_eq!(v.dispatch_next_selection(), DispatchSelection::Idle);
    }
    assert!(!d3.exclusive().tid_present_anywhere(t(1)));
    // A plan for CPU 0 handed to CPU 1's commit is refused, and CPU 1 is unchanged.
    let plan = crate::kernel::task_enqueue::EnqueuePlan {
        tid: 9,
        priority: TaskPriority::Normal,
        target: crate::kernel::task_enqueue::EnqueueTarget::Pinned(CpuId(0)),
    };
    let mut g1 = d.lock_cpu(CpuId(1)).expect("cpu1");
    let mut v1 = g1.view();
    assert!(crate::kernel::task_enqueue::commit_enqueue_on_cpu_locked(&mut v1, &plan).is_err());
    assert_eq!(v1.runnable_count(), 0);
    assert!(!v1.contains_or_current(t(9)));
}

/// The scheduler tick has ONE writer: only the clock owner's tick advances it; every other CPU
/// consumes only its own quantum.
#[test]
fn sl1_only_the_clock_owner_advances_time_and_each_cpu_owns_its_quantum() {
    let d = domain2();
    {
        let mut g = d.lock_cpu(CpuId(1)).expect("cpu1");
        let mut v = g.view();
        v.set_quantum(2);
        assert_eq!(v.tick(), (crate::kernel::time::TickInstant(0), false));
        assert_eq!(v.tick(), (crate::kernel::time::TickInstant(0), true));
    }
    assert_eq!(d.clock_now().0, 0, "an AP tick is not time");
    {
        let mut g = d.lock_cpu(CpuId(0)).expect("cpu0");
        let mut v = g.view();
        v.set_quantum(3);
        assert_eq!(v.tick().0.0, 1);
        assert_eq!(v.tick().0.0, 2);
        assert!(v.would_preempt_next());
    }
    assert_eq!(d.clock_now().0, 2);
    // CPU 1's quantum is untouched by CPU 0's ticks.
    let mut g = d.lock_cpu(CpuId(1)).expect("cpu1");
    assert!(!g.view().would_preempt_next());
}

// ── independence and exclusion (concurrent) ─────────────────────────────────────────────────

/// **Independence.** While CPU 0's actual 1L is HELD, CPU 1's whole local scheduling cycle —
/// enqueue, dispatch, tick, preempt-requeue, block — completes, many times over. The holder does
/// not release until it has seen the other side finish, so a protocol that still serialised the
/// two CPUs on any shared scheduler lock would time out here.
#[test]
fn sl1_unrelated_cpu_scheduling_completes_while_another_cpus_lock_is_held() {
    let d = Arc::new(domain2());
    let holding = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicUsize::new(0));
    const ROUNDS: usize = 2_000;
    std::thread::scope(|s| {
        let (dh, hh, dn) = (d.clone(), holding.clone(), done.clone());
        s.spawn(move || {
            let g = dh.lock_cpu(CpuId(0)).expect("cpu0");
            hh.store(true, Ordering::Release);
            wait_until("CPU 1 to finish while CPU 0's lock is held", || {
                dn.load(Ordering::Acquire) == ROUNDS
            });
            assert!(hh.load(Ordering::Acquire));
            drop(g);
        });
        let (db, hb, dn) = (d.clone(), holding.clone(), done.clone());
        s.spawn(move || {
            wait_until("CPU 0's lock to be held", || hb.load(Ordering::Acquire));
            for r in 0..ROUNDS {
                let tid = t(1_000 + r as u64);
                let mut g = db.lock_cpu(CpuId(1)).expect("cpu1");
                let mut v = g.view();
                v.enqueue_with_priority(tid, TaskPriority::Normal)
                    .expect("enqueue");
                assert_eq!(v.dispatch_next(), Some(tid));
                let _ = v.tick();
                assert_eq!(v.preempt_reenqueue_only(), Some(tid));
                assert_eq!(v.dispatch_next(), Some(tid));
                assert_eq!(v.block_current(), Some(tid));
                drop(g);
                // Each committed step is published to CPU 1's mirror at release.
                assert_eq!(db.current_mirror(1), None);
                dn.fetch_add(1, Ordering::AcqRel);
            }
        });
    });
    assert_eq!(done.load(Ordering::Acquire), ROUNDS);
}

/// **Same-CPU exclusion.** While CPU 1's 1L is held, a second acquisition of CPU 1's lock does
/// not complete; it completes after the release. (The holder samples the contender's flag for a
/// bounded interval; a lock that admitted two holders would set it immediately.)
#[test]
fn sl1_the_same_cpus_operations_are_mutually_excluded() {
    let d = Arc::new(domain2());
    let acquired = Arc::new(AtomicBool::new(false));
    let attempting = Arc::new(AtomicBool::new(false));
    std::thread::scope(|s| {
        let g = d.lock_cpu(CpuId(1)).expect("cpu1");
        let (dc, ac, at) = (d.clone(), acquired.clone(), attempting.clone());
        s.spawn(move || {
            at.store(true, Ordering::Release);
            let _g = dc.lock_cpu(CpuId(1)).expect("cpu1");
            ac.store(true, Ordering::Release);
        });
        wait_until("the contender to start", || {
            attempting.load(Ordering::Acquire)
        });
        let end = Instant::now() + Duration::from_millis(200);
        while Instant::now() < end {
            assert!(
                !acquired.load(Ordering::Acquire),
                "a second holder entered CPU 1's critical section"
            );
        }
        drop(g);
        wait_until("the contender to acquire after release", || {
            acquired.load(Ordering::Acquire)
        });
    });
}

/// The coherent view excludes every CPU: while it is held no 1L can be taken; after it is
/// released every one can.
#[test]
fn sl1_the_coherent_view_holds_every_cpu() {
    let d = Arc::new(domain4());
    let got = Arc::new(AtomicUsize::new(0));
    std::thread::scope(|s| {
        let all = d.lock_all();
        for c in 0..4u8 {
            let (dc, gc) = (d.clone(), got.clone());
            s.spawn(move || {
                let _g = dc.lock_cpu(CpuId(c)).expect("cpu");
                gc.fetch_add(1, Ordering::AcqRel);
            });
        }
        let end = Instant::now() + Duration::from_millis(200);
        while Instant::now() < end {
            assert_eq!(
                got.load(Ordering::Acquire),
                0,
                "a 1L was taken under the coherent view"
            );
        }
        drop(all);
        wait_until("every CPU after release", || {
            got.load(Ordering::Acquire) == 4
        });
    });
}

// ── the coherent snapshots and their replacements (concurrent) ─────────────────────────────

/// `placement_of` is a COHERENT all-CPU answer. A task that CPU 1 moves between its queue and its
/// current slot — each move ONE critical section — is always seen either queued or current on
/// CPU 1: never nowhere, never doubly placed, never elsewhere.
#[test]
fn sl1_the_coherent_placement_never_tears_against_local_moves() {
    let d = Arc::new(domain2());
    {
        let mut g = d.lock_cpu(CpuId(1)).expect("cpu1");
        g.view()
            .enqueue_with_priority(t(42), TaskPriority::Normal)
            .expect("enqueue");
    }
    let stop = Arc::new(AtomicBool::new(false));
    let seen = Arc::new(AtomicU64::new(0));
    std::thread::scope(|s| {
        let (dm, sm) = (d.clone(), stop.clone());
        s.spawn(move || {
            while !sm.load(Ordering::Acquire) {
                let mut g = dm.lock_cpu(CpuId(1)).expect("cpu1");
                let mut v = g.view();
                if v.current_tid() == Some(t(42)) {
                    assert_eq!(v.preempt_reenqueue_only(), Some(t(42)));
                } else {
                    assert_eq!(v.dispatch_next(), Some(t(42)));
                }
            }
        });
        let (dq, sq, se) = (d.clone(), stop.clone(), seen.clone());
        s.spawn(move || {
            for _ in 0..5_000 {
                let mut all = dq.lock_all();
                let p = all.view().placement_of(t(42));
                assert!(
                    matches!(
                        p,
                        TaskPlacement::Queued(CpuId(1)) | TaskPlacement::Current(CpuId(1))
                    ),
                    "torn placement {p:?}"
                );
                se.fetch_add(1, Ordering::Relaxed);
            }
            sq.store(true, Ordering::Release);
        });
    });
    assert_eq!(seen.load(Ordering::Relaxed), 5_000);
}

/// The FPU-home "current elsewhere" refusal no longer takes other CPUs' locks; it reads their
/// published current mirrors. If one incarnation is installed on two CPUs at once, at least one of
/// the two checks — each made under its own CPU's lock after its own install was published —
/// observes the other, in every round (§16.4).
#[test]
fn sl1_a_duplicate_current_is_seen_by_at_least_one_side() {
    const ROUNDS: usize = 3_000;
    let d = Arc::new(domain2());
    let barrier = Arc::new(Barrier::new(2));
    let saw: Arc<[AtomicBool; 2]> = Arc::new([AtomicBool::new(false), AtomicBool::new(false)]);
    std::thread::scope(|s| {
        for me in 0..2u8 {
            let (dd, bb, sw) = (d.clone(), barrier.clone(), saw.clone());
            s.spawn(move || {
                for _ in 0..ROUNDS {
                    bb.wait();
                    {
                        let mut g = dd.lock_cpu(CpuId(me)).expect("cpu");
                        assert!(g.view().restore_exact_current(t(77), TaskPriority::Normal));
                    } // published here
                    let elsewhere = {
                        let _g = dd.lock_cpu(CpuId(me)).expect("cpu");
                        dd.current_elsewhere(CpuId(me), t(77))
                    };
                    sw[me as usize].store(elsewhere, Ordering::Release);
                    bb.wait();
                    if me == 0 {
                        assert!(
                            sw[0].load(Ordering::Acquire) || sw[1].load(Ordering::Acquire),
                            "a duplicate current went unseen by both CPUs"
                        );
                    }
                    bb.wait();
                    let mut g = dd.lock_cpu(CpuId(me)).expect("cpu");
                    assert!(g.view().block_current_exact(t(77)).is_some());
                }
            });
        }
    });
}

/// The balanced choice now reads the per-CPU LOAD MIRRORS under 1G alone. Serially it makes the
/// same decision as the coherent choice over the locked state, for every load pattern tried.
#[test]
fn sl1_the_mirror_balanced_choice_matches_the_coherent_one() {
    let mut seed = 0x5EED_u64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for round in 0..200u64 {
        let mut d_mirror = domain4();
        let mut d_coherent = domain4();
        let mut tid = 10_000 + round * 100;
        for _ in 0..(next() % 12) {
            let cpu = CpuId((next() % 4) as u8);
            for d in [&mut d_mirror, &mut d_coherent] {
                d.exclusive()
                    .enqueue_on_with_priority(cpu, t(tid), TaskPriority::Normal)
                    .expect("enqueue");
            }
            tid += 1;
        }
        let a = d_mirror.balanced_choice_and_note();
        let b = {
            let mut all = d_coherent.exclusive();
            let c = all.balanced_choice();
            if let Ok(c) = c {
                all.note_balanced_choice(c);
            }
            c
        };
        assert_eq!(a, b, "round {round}");
    }
}

/// The direct-IPC membership check is a per-CPU scan of a FROZEN subject. While other CPUs churn
/// their own queues, the scan answers exactly: a task frozen in CPU 2's queue is present, a task
/// placed nowhere is absent.
#[test]
fn sl1_the_frozen_subject_scan_is_exact_under_unrelated_traffic() {
    let d = Arc::new(domain4());
    {
        let mut g = d.lock_cpu(CpuId(2)).expect("cpu2");
        g.view()
            .enqueue_with_priority(t(500), TaskPriority::Low)
            .expect("enqueue");
    }
    let stop = Arc::new(AtomicBool::new(false));
    std::thread::scope(|s| {
        for c in [0u8, 1, 3] {
            let (dd, st) = (d.clone(), stop.clone());
            s.spawn(move || {
                let mut n = 0u64;
                while !st.load(Ordering::Acquire) {
                    let tid = t(20_000 + c as u64 * 100_000 + n);
                    let mut g = dd.lock_cpu(CpuId(c)).expect("cpu");
                    let mut v = g.view();
                    v.enqueue_with_priority(tid, TaskPriority::Normal)
                        .expect("enqueue");
                    assert_eq!(v.dispatch_next(), Some(tid));
                    assert_eq!(v.block_current(), Some(tid));
                    n += 1;
                }
            });
        }
        for _ in 0..3_000 {
            assert!(d.present_anywhere_scan(t(500)));
            assert!(!d.present_anywhere_scan(t(501)));
        }
        stop.store(true, Ordering::Release);
    });
}

// ── remote wakes, capacity, duplicates, cancellation (concurrent) ───────────────────────────

/// Two threads enqueue the SAME task onto CPU 1 at once: exactly one commit wins, the other is
/// `AlreadyQueued`, and the task holds exactly one placement.
#[test]
fn sl1_a_duplicate_wake_has_exactly_one_winner() {
    for round in 0..500u64 {
        let d = Arc::new(domain2());
        let tid = t(70_000 + round);
        let oks = Arc::new(AtomicUsize::new(0));
        let dups = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(4));
        std::thread::scope(|s| {
            for _ in 0..4 {
                let (dd, ok, dup, b) = (d.clone(), oks.clone(), dups.clone(), barrier.clone());
                s.spawn(move || {
                    b.wait();
                    let mut g = dd.lock_cpu(CpuId(1)).expect("cpu1");
                    match g.view().enqueue_with_priority(tid, TaskPriority::Normal) {
                        Ok(()) => ok.fetch_add(1, Ordering::AcqRel),
                        Err(SchedulerError::AlreadyQueued) => dup.fetch_add(1, Ordering::AcqRel),
                        Err(e) => panic!("unexpected {e:?}"),
                    };
                });
            }
        });
        assert_eq!(oks.load(Ordering::Acquire), 1);
        assert_eq!(dups.load(Ordering::Acquire), 3);
        let mut all = d.lock_all();
        assert_eq!(
            all.view().placement_of(tid),
            TaskPlacement::Queued(CpuId(1))
        );
        assert_eq!(all.view().runnable_count_on(CpuId(1)), 1);
    }
}

/// Capacity: many threads race for CPU 1's 64 Normal slots. Exactly 64 commits succeed; every
/// other is refused with `QueueFull` BEFORE any mutation — a refused task is placed nowhere and the
/// accepted set is exactly what is queued. A wake already committed is never displaced by later
/// refusals.
#[test]
fn sl1_capacity_exhaustion_refuses_before_mutation_and_never_drops_a_committed_wake() {
    let d = Arc::new(domain2());
    // A wake committed BEFORE the queue fills.
    {
        let mut g = d.lock_cpu(CpuId(1)).expect("cpu1");
        g.view()
            .enqueue_with_priority(t(1), TaskPriority::Normal)
            .expect("first");
    }
    let accepted = Arc::new(std::sync::Mutex::new(Vec::new()));
    let refused = Arc::new(std::sync::Mutex::new(Vec::new()));
    let barrier = Arc::new(Barrier::new(8));
    std::thread::scope(|s| {
        for w in 0..8u64 {
            let (dd, acc, rej, b) = (
                d.clone(),
                accepted.clone(),
                refused.clone(),
                barrier.clone(),
            );
            s.spawn(move || {
                b.wait();
                for i in 0..16u64 {
                    let tid = t(100 + w * 16 + i);
                    let r = {
                        let mut g = dd.lock_cpu(CpuId(1)).expect("cpu1");
                        g.view().enqueue_with_priority(tid, TaskPriority::Normal)
                    };
                    match r {
                        Ok(()) => acc.lock().unwrap().push(tid),
                        Err(SchedulerError::QueueFull) => rej.lock().unwrap().push(tid),
                        Err(e) => panic!("unexpected {e:?}"),
                    }
                }
            });
        }
    });
    let accepted = accepted.lock().unwrap().clone();
    let refused = refused.lock().unwrap().clone();
    assert_eq!(
        accepted.len(),
        MAX_RUN_QUEUE - 1,
        "the committed first wake holds one slot"
    );
    assert_eq!(refused.len(), 128 - (MAX_RUN_QUEUE - 1));
    let mut all = d.lock_all();
    let v = all.view();
    assert!(
        v.tid_present_anywhere(t(1)),
        "the committed wake is still there"
    );
    for tid in &accepted {
        assert!(v.tid_present_anywhere(*tid));
    }
    for tid in &refused {
        assert!(!v.tid_present_anywhere(*tid), "a refused wake left a trace");
    }
    assert_eq!(v.runnable_count_on(CpuId(1)), MAX_RUN_QUEUE);
    let mut order = Vec::new();
    v.for_each_queued_on(CpuId(1), |_, tid| order.push(tid));
    assert_eq!(order[0], t(1), "the committed wake keeps its FIFO position");
}

/// Cancellation against dispatch: CPU 1 dispatches while a withdrawal of the same queued task is
/// attempted. Exactly one wins — the task is either dispatched (and the withdrawal reports it is
/// current) or withdrawn (and nothing is dispatched) — never both, never neither.
#[test]
fn sl1_a_withdrawal_and_a_dispatch_never_both_win() {
    for round in 0..1_000u64 {
        let d = Arc::new(domain2());
        let tid = t(90_000 + round);
        {
            let mut g = d.lock_cpu(CpuId(1)).expect("cpu1");
            g.view()
                .enqueue_with_priority(tid, TaskPriority::Normal)
                .expect("enqueue");
        }
        let barrier = Arc::new(Barrier::new(2));
        let (dispatched, withdrawn) = std::thread::scope(|s| {
            let (da, ba) = (d.clone(), barrier.clone());
            let a = s.spawn(move || {
                ba.wait();
                let mut g = da.lock_cpu(CpuId(1)).expect("cpu1");
                g.view().dispatch_next() == Some(tid)
            });
            let (db, bb) = (d.clone(), barrier.clone());
            let b = s.spawn(move || {
                bb.wait();
                let mut g = db.lock_cpu(CpuId(1)).expect("cpu1");
                g.view().withdraw_queued_tid(tid)
            });
            (a.join().unwrap(), b.join().unwrap())
        });
        match (dispatched, withdrawn) {
            (true, WithdrawOutcome::RefusedCurrent) => {}
            (false, WithdrawOutcome::Removed) => {}
            other => panic!("round {round}: both or neither won: {other:?}"),
        }
    }
}

/// A refused dispatch keeps its candidate in place while remote wakes keep arriving: the
/// acceptance-filtered selection skips the refused head, the refused task keeps its position, and
/// the concurrently committed wakes queue behind it in commit order.
#[test]
fn sl1_a_refused_candidate_keeps_its_position_under_remote_wakes() {
    let d = Arc::new(domain2());
    {
        let mut g = d.lock_cpu(CpuId(1)).expect("cpu1");
        let mut v = g.view();
        v.enqueue_with_priority(t(1), TaskPriority::Normal).unwrap();
        v.enqueue_with_priority(t(2), TaskPriority::Normal).unwrap();
    }
    std::thread::scope(|s| {
        let dr = d.clone();
        s.spawn(move || {
            for n in 0..40u64 {
                let mut g = dr.lock_cpu(CpuId(1)).expect("cpu1");
                g.view()
                    .enqueue_with_priority(t(100 + n), TaskPriority::Normal)
                    .unwrap();
            }
        });
        let ds = d.clone();
        s.spawn(move || {
            let mut g = ds.lock_cpu(CpuId(1)).expect("cpu1");
            let sel = g.view().dispatch_next_accepted_selection(|tid| tid != t(1));
            assert_eq!(
                sel.selection(),
                Some(DispatchSelection::Dequeued { tid: t(2) })
            );
        });
    });
    let mut all = d.lock_all();
    let v = all.view();
    let mut order = Vec::new();
    v.for_each_queued_on(CpuId(1), |_, tid| order.push(tid.0));
    assert_eq!(order[0], 1, "the refused task keeps the head");
    assert_eq!(
        &order[1..],
        &(100..140).collect::<Vec<u64>>()[..],
        "remote wakes in commit order"
    );
}

// ── the production seams (concurrent) ────────────────────────────────────────────────────────

fn kernel2() -> SharedKernel {
    let k = SharedKernel::new(Bootstrap::init().expect("init"));
    k.with(|s| s.bring_up_cpu(CpuId(1)).expect("cpu1 online"));
    k
}

/// A producer's settlement of a full destination queue: the commit refused BEFORE any mutation
/// (`QueueFull`, mapped), so the producer still owns the wake and retries it; nothing was lost.
fn wake_until_committed(k: &SharedKernel, cpu: CpuId, tid: u64) {
    let full = crate::kernel::boot::map_scheduler_error(SchedulerError::QueueFull);
    let end = Instant::now() + DEADLINE;
    loop {
        match k.enqueue_on_cpu_split(cpu, tid) {
            Ok(c) => {
                assert_eq!(c, cpu);
                return;
            }
            Err(e) => {
                assert_eq!(e, full, "only a full queue may refuse here");
                assert!(Instant::now() < end, "the destination never drained");
                std::hint::spin_loop();
            }
        }
    }
}

fn register(k: &SharedKernel, tids: impl IntoIterator<Item = u64>) {
    k.with(|s| {
        for tid in tids {
            s.register_task_with_class(tid, TaskClass::App)
                .expect("register");
        }
    });
}

/// **Independence through the production seams.** CPU 0's actual 1L is held while CPU 1 runs the
/// production owners: the pinned enqueue (plan under rank 2, THE per-CPU commit, notification),
/// the queue-advancing dispatch, the timer tick, the terminal-idle decision, the authoritative
/// current read, and the lock-free reads (clock, admission bitmaps, reap shootdown bitmap). All
/// complete while CPU 0 is held; an operation that names CPU 0 does NOT complete until release —
/// CPU 0's authoritative current, and an address space's shootdown target set, which reads each
/// candidate CPU's current under that CPU's own 1L (§16.5) and so waits for CPU 0's section.
#[test]
fn sl1_production_seams_on_cpu1_complete_while_cpu0_is_held() {
    let k = kernel2();
    register(&k, 30_000..30_050);
    let held = AtomicBool::new(false);
    let cpu1_done = AtomicBool::new(false);
    let cpu0_op_done = AtomicBool::new(false);
    let targets_done = AtomicBool::new(false);
    std::thread::scope(|s| {
        s.spawn(|| {
            let g = k.sched_domain().lock_cpu(CpuId(0)).expect("cpu0");
            held.store(true, Ordering::Release);
            wait_until("CPU 1's production work", || {
                cpu1_done.load(Ordering::Acquire)
            });
            // The CPU-0 operation must still be waiting for this lock.
            assert!(
                !cpu0_op_done.load(Ordering::Acquire),
                "an operation on CPU 0 completed while CPU 0's lock was held"
            );
            assert!(
                !targets_done.load(Ordering::Acquire),
                "a shootdown target set read CPU 0's current while CPU 0's section was in progress"
            );
            drop(g);
        });
        s.spawn(|| {
            wait_until("CPU 0 held", || held.load(Ordering::Acquire));
            for tid in 30_000..30_050u64 {
                assert_eq!(k.enqueue_on_cpu_split(CpuId(1), tid), Ok(CpuId(1)));
                let sel = k
                    .with_cpu_sched_split(CpuId(1), |c| c.dispatch_next_selection())
                    .expect("cpu1");
                assert_eq!(sel, DispatchSelection::Dequeued { tid: t(tid) });
                assert_eq!(k.current_tid_authoritative(CpuId(1)), Some(tid));
                let _ = k.scheduler_tick_split_mut(CpuId(1));
                assert_eq!(k.block_current_on_cpu_split(CpuId(1)), Ok(Some(tid)));
                assert_eq!(k.terminal_idle_on_cpu_split(CpuId(1)), Ok(true));
                let _ = k.scheduler_tick_now_split_read();
                let _ = k.reap_shootdown_cpu_bitmap_split();
                let _ = k.online_cpu_count_split_read();
            }
            cpu1_done.store(true, Ordering::Release);
        });
        s.spawn(|| {
            wait_until("CPU 0 held", || held.load(Ordering::Acquire));
            let _ = k.current_tid_authoritative(CpuId(0));
            cpu0_op_done.store(true, Ordering::Release);
        });
        s.spawn(|| {
            wait_until("CPU 0 held", || held.load(Ordering::Acquire));
            let _ = k.live_cpu_bitmap_for_asid_split(crate::kernel::vm::Asid(7));
            targets_done.store(true, Ordering::Release);
        });
    });
    assert!(
        cpu0_op_done.load(Ordering::Acquire),
        "released, the CPU-0 operation completes"
    );
    assert!(
        targets_done.load(Ordering::Acquire),
        "released, the shootdown target set completes"
    );
}

/// **Opposite-direction remote wakes.** CPU 0's thread wakes tasks onto CPU 1 while CPU 1's
/// thread wakes tasks onto CPU 0, through the production pinned enqueue; each CPU's own thread
/// consumes its queue. No deadlock (each commit takes only its destination's lock), no lost wake,
/// no duplicate: every task is dispatched exactly once, on the CPU it was sent to.
#[test]
fn sl1_opposite_direction_remote_wakes_neither_deadlock_nor_lose() {
    const N: u64 = 200;
    let k = kernel2();
    register(&k, 40_000..40_000 + N); // → CPU 1
    register(&k, 50_000..50_000 + N); // → CPU 0
    let consumed: [Arc<std::sync::Mutex<Vec<u64>>>; 2] =
        [Arc::new(Default::default()), Arc::new(Default::default())];
    std::thread::scope(|s| {
        // Producers.
        s.spawn(|| {
            for tid in 40_000..40_000 + N {
                wake_until_committed(&k, CpuId(1), tid);
            }
        });
        s.spawn(|| {
            for tid in 50_000..50_000 + N {
                wake_until_committed(&k, CpuId(0), tid);
            }
        });
        // Consumers: each drains its own CPU, through its own 1L only.
        for cpu in 0..2u8 {
            let k = &k;
            let out = consumed[cpu as usize].clone();
            s.spawn(move || {
                let end = Instant::now() + DEADLINE;
                while (out.lock().unwrap().len() as u64) < N {
                    assert!(Instant::now() < end, "cpu {cpu}: wakes were lost");
                    let next = k
                        .with_cpu_sched_split(CpuId(cpu), |c| {
                            let next = c.dispatch_next();
                            if let Some(n) = next
                                && n.0 != 0
                            {
                                c.block_current();
                            }
                            next
                        })
                        .expect("cpu");
                    if let Some(n) = next
                        && n.0 != 0
                    {
                        out.lock().unwrap().push(n.0);
                    }
                }
            });
        }
    });
    let mut on1 = consumed[1].lock().unwrap().clone();
    let mut on0 = consumed[0].lock().unwrap().clone();
    on1.sort_unstable();
    on0.sort_unstable();
    assert_eq!(
        on1,
        (40_000..40_000 + N).collect::<Vec<_>>(),
        "CPU 1: exactly once each"
    );
    assert_eq!(
        on0,
        (50_000..50_000 + N).collect::<Vec<_>>(),
        "CPU 0: exactly once each"
    );
}

/// **Wake against idle entry.** CPU 1 alternates the production terminal-idle decision with a wait
/// for its enqueue notification; another thread commits wakes onto CPU 1 through the production
/// enqueue, which notifies after its commit. The idle decision and the commit share CPU 1's 1L, so
/// either the decision sees the work or the notification arrives after it: no wake is stranded
/// behind an idle CPU.
#[test]
fn sl1_a_wake_racing_idle_entry_is_never_stranded() {
    const N: u64 = 250;
    let k = kernel2();
    register(&k, 60_000..60_000 + N);
    let got = AtomicU64::new(0);
    std::thread::scope(|s| {
        s.spawn(|| {
            for tid in 60_000..60_000 + N {
                wake_until_committed(&k, CpuId(1), tid);
            }
        });
        s.spawn(|| {
            let end = Instant::now() + DEADLINE;
            while got.load(Ordering::Acquire) < N {
                let seen = crate::arch::hal::enqueues_notified(CpuId(1));
                if k.terminal_idle_on_cpu_split(CpuId(1)) == Ok(true) {
                    // Idle: wait for the next notification, bounded. A stranded wake shows up as
                    // queued work with no notification left to deliver.
                    while crate::arch::hal::enqueues_notified(CpuId(1)) == seen {
                        if Instant::now() > end {
                            panic!(
                                "stranded: runnable={} after idle",
                                k.runnable_count_on_cpu_split_read(CpuId(1))
                            );
                        }
                        std::hint::spin_loop();
                    }
                    continue;
                }
                let next = k
                    .with_cpu_sched_split(CpuId(1), |c| {
                        let n = c.dispatch_next();
                        if n.is_some_and(|n| n.0 != 0) {
                            c.block_current();
                        }
                        n
                    })
                    .expect("cpu1");
                if next.is_some_and(|n| n.0 != 0) {
                    got.fetch_add(1, Ordering::AcqRel);
                }
            }
        });
    });
    assert_eq!(got.load(Ordering::Acquire), N);
}

/// A stale wake — for a task whose incarnation no longer exists — is refused by the rank-2 planner
/// before any 1L is taken; the target queue is unchanged.
#[test]
fn sl1_a_stale_incarnation_wake_is_refused_before_any_queue_mutation() {
    let k = kernel2();
    register(&k, [80_001]);
    let before = k.runnable_count_on_cpu_split_read(CpuId(1));
    assert!(
        k.enqueue_on_cpu_split(CpuId(1), 80_999).is_err(),
        "no such task"
    );
    assert_eq!(k.runnable_count_on_cpu_split_read(CpuId(1)), before);
    assert_eq!(k.enqueue_on_cpu_split(CpuId(1), 80_001), Ok(CpuId(1)));
    // A second wake of the same, still-queued incarnation is the scheduler's duplicate refusal.
    assert!(k.enqueue_on_cpu_split(CpuId(1), 80_001).is_err());
    assert_eq!(k.runnable_count_on_cpu_split_read(CpuId(1)), before + 1);
}

/// VMI2 coverage through the per-CPU locked current reads (§16.5): a task installed as CPU 1's
/// current is named by the shootdown target computation once the install's section ends, and no
/// longer named
/// by the placement half once it is blocked (the residency half — not exercised in a hosted build
/// — is what covers a loaded root with an empty slot).
#[test]
fn sl1_shootdown_targets_follow_each_cpus_settled_current() {
    let k = kernel2();
    register(&k, [90_001]);
    let asid = crate::kernel::vm::Asid(0x5157);
    k.with(|s| {
        s.with_tcbs_mut(|tcbs| {
            let tcb = tcbs
                .iter_mut()
                .flatten()
                .find(|t| t.tid.0 == 90_001)
                .expect("tcb");
            tcb.asid = Some(asid);
        })
    });
    assert_eq!(k.enqueue_on_cpu_split(CpuId(1), 90_001), Ok(CpuId(1)));
    let _ = k.with_cpu_sched_split(CpuId(1), |c| c.dispatch_next());
    assert_ne!(
        k.live_cpu_bitmap_for_asid_split(asid) & 0b10,
        0,
        "CPU 1 named"
    );
    let _ = k.block_current_on_cpu_split(CpuId(1));
    assert_eq!(
        k.live_cpu_bitmap_for_asid_split(asid)
            & 0b10
            & !crate::arch::hal::address_space_loaded_bitmap(asid),
        0,
        "placement no longer names CPU 1"
    );
}
