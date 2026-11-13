// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! SCHED-TIME-FOUNDATION1 checkpoint A (SCHED-FULL-ROT1) — a full user run queue must never, by
//! itself, stop the current task from rotating.
//!
//! Every rotation owner (`on_preempt_selection`, `preempt_reenqueue_only`,
//! `on_preempt_prefer_selection`) returns the OUTGOING current task to the tail of its own
//! priority queue. That task already holds a placement on this CPU, so its return is not an
//! admission: it is never refused for user capacity. The admission limit for every other user
//! push ([`MAX_RUN_QUEUE`], counting reservations) is unchanged. These tests pin both halves and
//! the bound that makes the return always fit (`r09`, `r10`).

use super::*;
use std::vec::Vec;

const N: TaskPriority = TaskPriority::Normal;
const H: TaskPriority = TaskPriority::High;
const L: TaskPriority = TaskPriority::Low;

fn kt(i: u64) -> ThreadId {
    ThreadId((0x4B54u64 << 48) | (i + 1))
}

/// One CPU's queue contents per priority, FIFO order.
fn q(s: &PriorityScheduler, p: TaskPriority) -> Vec<u64> {
    let r = &s.queues[PriorityScheduler::priority_index(p)];
    (0..r.len)
        .map(|o| r.tids[RingQueue::index(r.head + o)].0)
        .collect()
}

fn user_len(s: &PriorityScheduler, p: TaskPriority) -> usize {
    q(s, p)
        .iter()
        .filter(|&&t| !crate::kernel::kthread::is_kthread_tid(t))
        .count()
}

/// Every task placed on this scheduler (current + queued), each exactly once.
fn placed_once(s: &PriorityScheduler) -> Vec<u64> {
    let mut all: Vec<u64> = [H, N, L].iter().flat_map(|&p| q(s, p)).collect();
    if let Some(c) = s.current_tid() {
        all.push(c.0);
    }
    let mut sorted = all.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), all.len(), "a task is placed twice: {all:?}");
    sorted
}

/// `cur` current at `p`, then `p`'s queue filled to the user limit with `base..`.
fn full(p: TaskPriority, cur: u64, base: u64) -> PriorityScheduler {
    let mut s = PriorityScheduler::default();
    s.enqueue_with_priority(ThreadId(cur), p).expect("current");
    assert_eq!(
        s.dispatch_next_selection(),
        DispatchSelection::Dequeued { tid: ThreadId(cur) }
    );
    for i in 0..MAX_RUN_QUEUE as u64 {
        s.enqueue_with_priority(ThreadId(base + i), p)
            .expect("fill");
    }
    assert_eq!(
        s.enqueue_with_priority(ThreadId(base + 999), p),
        Err(SchedulerError::QueueFull),
        "the queue is at its user limit"
    );
    s
}

#[test]
fn r01_full_queue_reenqueue_only_rotates_the_current_task() {
    for p in [H, N, L] {
        let mut s = full(p, 1, 100);
        assert_eq!(
            s.preempt_reenqueue_only(),
            Some(ThreadId(1)),
            "{p:?}: the current task returns"
        );
        assert_eq!(s.current_tid(), None);
        let mut want: Vec<u64> = (100..164).collect();
        want.push(1);
        assert_eq!(q(&s, p), want, "{p:?}: at the tail, FIFO kept");
        assert_eq!(
            s.dispatch_next_selection(),
            DispatchSelection::Dequeued { tid: ThreadId(100) }
        );
        placed_once(&s);
    }
}

#[test]
fn r02_full_queue_preempt_dispatches_the_head() {
    for p in [H, N, L] {
        let mut s = full(p, 1, 100);
        assert_eq!(
            s.on_preempt_selection(),
            DispatchSelection::Dequeued { tid: ThreadId(100) },
            "{p:?}"
        );
        assert_eq!(q(&s, p).last(), Some(&1));
        assert_eq!(q(&s, p).len(), MAX_RUN_QUEUE);
        placed_once(&s);
    }
}

#[test]
fn r03_full_queue_preferred_handoff_rotates() {
    let mut s = full(N, 1, 100);
    assert_eq!(
        s.on_preempt_prefer_selection(ThreadId(130)),
        DispatchSelection::Dequeued { tid: ThreadId(130) }
    );
    let qn = q(&s, N);
    assert_eq!(qn.last(), Some(&1));
    assert!(!qn.contains(&130));
    assert_eq!(qn.len(), MAX_RUN_QUEUE);
    placed_once(&s);
}

#[test]
fn r04_round_robin_over_a_full_queue_dispatches_everyone() {
    let mut s = full(N, 1, 100);
    let mut runs = std::collections::BTreeMap::<u64, usize>::new();
    for _ in 0..(3 * (MAX_RUN_QUEUE + 1)) {
        let t = s.on_preempt_selection().tid().expect("a task").0;
        *runs.entry(t).or_default() += 1;
    }
    assert_eq!(runs.len(), MAX_RUN_QUEUE + 1, "every placed task ran");
    assert!(
        runs.values().all(|&n| n == 3),
        "strict FIFO rotation: {runs:?}"
    );
}

#[test]
fn r05_a_higher_priority_task_runs_and_the_current_keeps_its_place() {
    let mut s = full(N, 1, 100);
    s.enqueue_with_priority(ThreadId(900), H)
        .expect("High has room");
    assert_eq!(
        s.on_preempt_selection(),
        DispatchSelection::Dequeued { tid: ThreadId(900) }
    );
    let mut want: Vec<u64> = (100..164).collect();
    want.push(1);
    assert_eq!(
        q(&s, N),
        want,
        "the displaced Normal task waits at its own tail"
    );
    assert_eq!(
        user_len(&s, N),
        MAX_RUN_QUEUE + 1,
        "the one returning occupant above the limit"
    );
    // External admission at Normal stays refused while it is above the limit.
    assert_eq!(
        s.enqueue_with_priority(ThreadId(555), N),
        Err(SchedulerError::QueueFull)
    );
    assert_eq!(s.block_current(), Some(ThreadId(900)));
    assert_eq!(
        s.dispatch_next_selection(),
        DispatchSelection::Dequeued { tid: ThreadId(100) }
    );
    assert_eq!(user_len(&s, N), MAX_RUN_QUEUE);
    placed_once(&s);
}

#[test]
fn r06_a_lower_priority_task_never_displaces_a_higher_one() {
    // High current, its own queue full, Low work waiting: the rotation stays within High.
    let mut s = full(H, 1, 100);
    for i in 0..8 {
        s.enqueue_with_priority(ThreadId(700 + i), L).expect("low");
    }
    for _ in 0..200 {
        let t = s.on_preempt_selection().tid().expect("task").0;
        assert!(t < 700, "Low task {t} ran while High work was runnable");
    }
    // High current alone: preemption re-selects it, Low still waits.
    let mut s = PriorityScheduler::default();
    s.enqueue_with_priority(ThreadId(1), H).expect("cur");
    s.dispatch_next_selection();
    for i in 0..MAX_RUN_QUEUE as u64 {
        s.enqueue_with_priority(ThreadId(700 + i), L)
            .expect("low fill");
    }
    assert_eq!(
        s.on_preempt_selection(),
        DispatchSelection::Dequeued { tid: ThreadId(1) }
    );
    assert_eq!(q(&s, L).len(), MAX_RUN_QUEUE);
    placed_once(&s);
}

#[test]
fn r07_reserved_slots_are_never_spent_by_the_rotation() {
    let mut s = PriorityScheduler::default();
    s.enqueue_with_priority(ThreadId(1), N).expect("cur");
    s.dispatch_next_selection();
    for i in 0..(MAX_RUN_QUEUE as u64 - 1) {
        s.enqueue_with_priority(ThreadId(100 + i), N)
            .expect("fill 63");
    }
    s.reserve_user_slot(N).expect("the last slot promised");
    assert_eq!(
        s.enqueue_with_priority(ThreadId(888), N),
        Err(SchedulerError::QueueFull),
        "promise held"
    );
    for _ in 0..10 {
        assert!(
            matches!(s.on_preempt_selection(), DispatchSelection::Dequeued { .. }),
            "rotation proceeds"
        );
    }
    assert_eq!(s.reserved_user_slots(N), 1, "the promise is still owed");
    s.enqueue_reserved_with_priority(ThreadId(500), N)
        .expect("the promise is kept");
    assert_eq!(s.reserved_user_slots(N), 0);
    assert_eq!(
        s.enqueue_with_priority(ThreadId(889), N),
        Err(SchedulerError::QueueFull)
    );
    assert!(matches!(
        s.on_preempt_selection(),
        DispatchSelection::Dequeued { .. }
    ));
    placed_once(&s);
}

#[test]
fn r08_admission_capacity_is_unchanged() {
    let mut s = PriorityScheduler::default();
    for i in 0..MAX_RUN_QUEUE as u64 {
        s.enqueue_with_priority(ThreadId(100 + i), N).expect("fill");
    }
    assert_eq!(
        s.enqueue_with_priority(ThreadId(999), N),
        Err(SchedulerError::QueueFull)
    );
    s.dispatch_next_selection();
    s.enqueue_with_priority(ThreadId(999), N)
        .expect("the dequeue freed one admission slot");
    assert_eq!(
        s.enqueue_with_priority(ThreadId(998), N),
        Err(SchedulerError::QueueFull)
    );
}

#[test]
fn r09_cross_priority_displacement_is_bounded_by_one() {
    // The adversarial loop: every rotation is displaced by a High arrival, every High blocks,
    // and external pushes keep trying to fill Normal. Normal never holds more than the limit
    // plus the one returning occupant.
    let mut s = full(N, 1, 100);
    let mut next = 10_000u64;
    for round in 0..2_000 {
        let _ = s.enqueue_with_priority(ThreadId(next), N);
        next += 1;
        s.enqueue_with_priority(ThreadId(next), H).expect("high");
        next += 1;
        let sel = s.on_preempt_selection();
        assert!(
            matches!(sel, DispatchSelection::Dequeued { .. }),
            "round {round}: {sel:?}"
        );
        assert!(
            user_len(&s, N) <= MAX_RUN_QUEUE + 1,
            "round {round}: {}",
            user_len(&s, N)
        );
        assert!(s.block_current().is_some());
        let sel = s.dispatch_next_selection();
        assert!(matches!(sel, DispatchSelection::Dequeued { .. }));
        // A block/restore round trip (U9-EXIT3 rollback) of the new current.
        let cur = s.current_tid().expect("cur");
        let prio = s.block_current_exact(cur).expect("exact");
        let _ = s.enqueue_with_priority(ThreadId(next), N);
        next += 1;
        assert!(s.restore_exact_current(cur, prio));
        assert!(user_len(&s, N) <= MAX_RUN_QUEUE + 1);
        placed_once(&s);
    }
}

#[test]
fn r10_kernel_threads_keep_their_room_beside_the_returning_occupant() {
    let mut s = full(N, 1, 100);
    for i in 0..crate::kernel::kthread::KTHREAD_CAPACITY as u64 {
        s.enqueue_with_priority(kt(i), N)
            .expect("a kernel thread is never refused");
    }
    s.enqueue_with_priority(ThreadId(900), H).expect("high");
    assert_eq!(
        s.on_preempt_selection(),
        DispatchSelection::Dequeued { tid: ThreadId(900) }
    );
    assert_eq!(
        q(&s, N).len(),
        MAX_QUEUED_PER_PRIORITY,
        "64 users + the returning one + every kthread"
    );
    assert_eq!(q(&s, N).last(), Some(&1));
    placed_once(&s);
}

#[test]
fn r11_the_returning_task_is_never_duplicated() {
    let mut s = full(N, 1, 100);
    assert_eq!(s.preempt_reenqueue_only(), Some(ThreadId(1)));
    assert_eq!(
        s.enqueue_with_priority(ThreadId(1), N),
        Err(SchedulerError::AlreadyQueued)
    );
    assert_eq!(
        s.preempt_reenqueue_only(),
        None,
        "nothing is current: no second return"
    );
    assert_eq!(q(&s, N).iter().filter(|&&t| t == 1).count(), 1);
    placed_once(&s);
}

#[test]
fn r12_a_dispatch_rollback_succeeds_on_a_queue_filled_meanwhile() {
    // A Dequeued selection is undone with `preempt_reenqueue_only` (its exact inverse). Another
    // CPU's pushes may fill the queue between the two; the undo must still return the task.
    let mut s = PriorityScheduler::default();
    for i in 0..MAX_RUN_QUEUE as u64 {
        s.enqueue_with_priority(ThreadId(100 + i), N).expect("fill");
    }
    assert_eq!(
        s.dispatch_next_selection(),
        DispatchSelection::Dequeued { tid: ThreadId(100) }
    );
    s.enqueue_with_priority(ThreadId(777), N)
        .expect("the freed slot is taken meanwhile");
    assert_eq!(
        s.preempt_reenqueue_only(),
        Some(ThreadId(100)),
        "the rollback returns the task"
    );
    assert_eq!(s.current_tid(), None);
    assert_eq!(q(&s, N).last(), Some(&100));
    placed_once(&s);
}

#[test]
fn r13_kernel_thread_current_rotates_on_a_full_queue() {
    let mut s = PriorityScheduler::default();
    for i in 0..MAX_RUN_QUEUE as u64 {
        s.enqueue_with_priority(ThreadId(100 + i), N).expect("fill");
    }
    s.enqueue_with_priority(kt(0), N).expect("kthread");
    // Install the kernel thread through the accepting selection, the only path that runs one.
    let sel = s.dispatch_next_accepted_selection(|t| crate::kernel::kthread::is_kthread_tid(t.0));
    assert_eq!(
        sel,
        AcceptedSelection::Selected(DispatchSelection::Dequeued { tid: kt(0) })
    );
    assert_eq!(s.preempt_reenqueue_only(), Some(kt(0)));
    assert_eq!(q(&s, N).last(), Some(&kt(0).0));
    placed_once(&s);
}

#[test]
fn r14_the_idle_current_is_left_to_the_idle_dispatch() {
    // An idle (tid 0) current with a full queue: the idle-aware dispatch takes the head; nothing
    // is lost.
    let mut s = PriorityScheduler::default();
    s.current = Some(ScheduledTask {
        tid: ThreadId(0),
        priority: N,
    });
    for i in 0..MAX_RUN_QUEUE as u64 {
        s.enqueue_with_priority(ThreadId(100 + i), N).expect("fill");
    }
    assert_eq!(
        s.dispatch_next_selection(),
        DispatchSelection::Dequeued { tid: ThreadId(100) }
    );
    assert_eq!(q(&s, N).len(), MAX_RUN_QUEUE - 1);
    placed_once(&s);
}

#[test]
fn r15_a_task_withdrawn_from_a_full_queue_leaves_rotation_intact() {
    let mut s = full(N, 1, 100);
    assert_eq!(
        s.withdraw_queued_tid(ThreadId(100)),
        WithdrawOutcome::Removed,
        "removed mid-flight"
    );
    assert_eq!(
        s.on_preempt_selection(),
        DispatchSelection::Dequeued { tid: ThreadId(101) }
    );
    assert!(!q(&s, N).contains(&100));
    // 63 queued + the current: the withdrawal freed exactly one admission slot, and only one.
    assert_eq!(
        s.enqueue_with_priority(ThreadId(100), N),
        Ok(()),
        "the freed slot is admissible"
    );
    assert_eq!(
        s.enqueue_with_priority(ThreadId(999), N),
        Err(SchedulerError::QueueFull)
    );
    placed_once(&s);
}
