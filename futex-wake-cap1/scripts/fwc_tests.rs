
/// FUTEX-WAKE-CAP1 — the wake-to-scheduler liveness contract. Every waiter a FutexWake claims is
/// placed exactly once (queued, or already current and owned by its own park), the count it
/// reports includes only those, and a waiter it cannot place is never claimed: it stays parked.
mod futex_wake_cap1 {
    use super::*;
    use crate::kernel::boot::futex::{self as fx, FutexKind};
    use crate::kernel::capabilities::CapId;
    use crate::kernel::ipc::ThreadId;
    use crate::kernel::scheduler::{CpuId, TaskPlacement, TaskPriority};
    use crate::kernel::syscall::sched::FutexParkOutcome;
    use crate::kernel::task::{TaskStatus, WaitReason};
    use crate::kernel::vm::{Asid, Mapping, PageFlags, PhysAddr, VirtAddr};
    use crate::runtime::SharedKernel;

    const P: FutexKind = FutexKind::Private;
    const SH: FutexKind = FutexKind::Shared;
    const X: usize = 0x6_0000;
    const BASE: u64 = 44000;

    fn space(s: &mut KernelState) -> (Asid, CapId) {
        s.create_user_address_space().expect("address space")
    }
    fn task(s: &mut KernelState, tid: u64, asid: Asid) {
        s.register_task(tid).expect("task");
        s.bind_task_asid(tid, asid).expect("bind");
        s.set_task_status_for_test(tid, TaskStatus::Runnable);
        s.seed_resumable_user_context_for_test(tid);
        s.set_task_home_cpu(tid, CpuId(0)).expect("home");
        s.enqueue_task(tid).expect("enqueue");
    }
    fn private_page(s: &mut KernelState, asid: Asid, va: usize) {
        let frame = s.alloc_user_data_frame().expect("frame");
        s.map_user_page_in_asid_raw(
            asid,
            VirtAddr(va as u64),
            Mapping {
                phys: PhysAddr(frame),
                flags: PageFlags::USER_RW,
            },
        )
        .expect("map private page");
    }
    fn run(s: &mut KernelState, tid: u64) {
        if s.current_tid() != Some(tid) {
            s.yield_current_to(ThreadId(tid)).expect("switch");
        }
        assert_eq!(s.current_tid(), Some(tid));
    }
    fn placement(s: &KernelState, tid: u64) -> TaskPlacement {
        s.with_scheduler_state(|all| all.placement_of(ThreadId(tid)))
    }
    fn queued_count(s: &KernelState, tid: u64) -> usize {
        let mut n = 0;
        s.for_each_queued_on_for_test(CpuId(0), |_, t| n += usize::from(t.0 == tid));
        n
    }
    fn reserved(s: &KernelState) -> usize {
        s.with_scheduler_state(|all| all.reserved_user_slots_on(CpuId(0)))
    }
    fn parked(s: &KernelState, tid: u64) -> bool {
        matches!(
            s.task_status(tid),
            Some(TaskStatus::Blocked(WaitReason::Futex(_)))
        )
    }
    /// `n` waiters parked on X in `kind`, one at a time (the queue never holds more than one of
    /// them before the wake); then the waker current. Returns (waiters, waker, asid).
    fn park_many(s: &mut KernelState, n: u64, kind: FutexKind) -> (alloc::vec::Vec<u64>, u64, Asid) {
        let (a, acap) = space(s);
        if kind == SH {
            let (_id, mem) = s.alloc_anonymous_memory_object().expect("object");
            s.map_user_page_with_caps(acap, mem, VirtAddr(X as u64), PageFlags::USER_RW)
                .expect("map object page");
        } else {
            private_page(s, a, X);
        }
        let waker = BASE + 999;
        let mut waiters = alloc::vec::Vec::new();
        for i in 0..n {
            let t = BASE + i;
            task(s, t, a);
            if i == 0 {
                s.write_user_memory(t, X, &5u32.to_ne_bytes()).expect("word");
            }
            run(s, t);
            assert!(s.futex_wait_current(X, 5, kind).expect("wait"), "{t} parks");
            waiters.push(t);
        }
        task(s, waker, a);
        run(s, waker);
        (waiters, waker, a)
    }
    /// The contract over every waiter: parked, or Runnable and queued exactly once. Returns how
    /// many are Runnable-and-queued (the wakes that had a continuation).
    fn check_contract(s: &KernelState, waiters: &[u64]) -> usize {
        let mut placed = 0;
        for &t in waiters {
            if parked(s, t) {
                assert_eq!(placement(s, t), TaskPlacement::Nowhere, "{t}: parked and placed");
                continue;
            }
            assert_eq!(s.task_status(t), Some(TaskStatus::Runnable), "{t}");
            assert_eq!(placement(s, t), TaskPlacement::Queued(CpuId(0)), "{t}: Runnable and stranded");
            assert_eq!(queued_count(s, t), 1, "{t}: queued exactly once");
            placed += 1;
        }
        assert_eq!(reserved(s), 0, "every reservation was filled or released");
        placed
    }

    /// 65 same-priority waiters, one CPU, no backlog. The queue holds 64 user entries (the idle
    /// task is one), so 63 are placed and counted, and the remaining two stay parked — FIFO: the
    /// first 63 to arrive are the ones woken.
    #[test]
    fn c1_sixty_five_waiters_one_cpu_none_stranded_and_count_exact() {
        let mut s = Bootstrap::init().expect("init");
        let (waiters, _, _) = park_many(&mut s, 65, P);
        let woke = s.futex_wake(X, 65, P).expect("wake");
        let placed = check_contract(&s, &waiters);
        assert_eq!(woke as usize, placed, "the count is the waiters with a continuation");
        assert_eq!(placed, 63);
        assert!(waiters[..63].iter().all(|&t| !parked(&s, t)), "oldest first");
        assert!(waiters[63..].iter().all(|&t| parked(&s, t)));
        assert_eq!(s.futex_waiter_count(X), 2, "the unplaced waiters keep their records");
    }

    /// A partly filled queue refuses sooner, and the refusal still strands nobody.
    #[test]
    fn c2_a_partly_filled_queue_places_fewer_and_strands_none() {
        let mut s = Bootstrap::init().expect("init");
        let (waiters, waker, a) = park_many(&mut s, 60, P);
        for i in 0..10 {
            task(&mut s, BASE + 500 + i, a);
        }
        assert_eq!(s.current_tid(), Some(waker));
        let woke = s.futex_wake(X, u32::MAX, P).expect("wake");
        let placed = check_contract(&s, &waiters);
        assert_eq!(woke as usize, placed);
        assert_eq!(placed, 53, "64 slots - idle - 10 already queued");
    }

    /// Repeated wakes: each places only what it can, nothing is ever placed twice, and once the
    /// queue drains the remaining waiters are woken by the next call.
    #[test]
    fn c3_repeated_wakes_never_double_place() {
        let mut s = Bootstrap::init().expect("init");
        let (waiters, waker, _) = park_many(&mut s, 70, P);
        assert_eq!(s.futex_wake(X, u32::MAX, P), Ok(63));
        assert_eq!(s.futex_wake(X, u32::MAX, P), Ok(0), "the queue is still full");
        check_contract(&s, &waiters);
        // Drain: the placed waiters leave the queue, as if each had run and blocked elsewhere.
        for &t in &waiters[..63] {
            assert_eq!(
                s.withdraw_queued_tid_on(CpuId(0), t),
                crate::kernel::scheduler::WithdrawOutcome::Removed
            );
            s.set_task_status_for_test(t, TaskStatus::Blocked(WaitReason::Join(ThreadId(1))));
        }
        assert_eq!(s.current_tid(), Some(waker));
        assert_eq!(s.futex_wake(X, u32::MAX, P), Ok(7), "the rest, now that there is room");
        let placed = check_contract(&s, &waiters[63..]);
        assert_eq!(placed, 7);
        assert_eq!(s.futex_waiter_count(X), 0);
    }

    /// The shared key behaves the same way.
    #[test]
    fn c4_shared_key_none_stranded() {
        let mut s = Bootstrap::init().expect("init");
        let (waiters, _, _) = park_many(&mut s, 65, SH);
        let woke = s.futex_wake(X, 65, SH).expect("wake");
        assert_eq!(woke as usize, check_contract(&s, &waiters));
        assert_eq!(woke, 63);
    }

    /// Broad and split parity: the same population gives the same count, the same placed set and
    /// the same parked set.
    #[test]
    fn c5_broad_and_split_wakes_agree() {
        let mut b = Bootstrap::init().expect("init");
        let (wb, _, _) = park_many(&mut b, 66, P);
        let broad = b.futex_wake(X, u32::MAX, P).expect("broad");
        let parked_b: alloc::vec::Vec<u64> = wb.iter().copied().filter(|&t| parked(&b, t)).collect();
        check_contract(&b, &wb);

        let mut st = Bootstrap::init().expect("init");
        let (ws, _, asid) = park_many(&mut st, 66, P);
        let k = SharedKernel::new(st);
        let split = k.futex_wake_split_mut(CpuId(0), asid, X, u32::MAX, P).expect("split");
        let parked_s: alloc::vec::Vec<u64> =
            k.with(|s| ws.iter().copied().filter(|&t| parked(s, t)).collect());
        k.with(|s| check_contract(s, &ws));
        assert_eq!((broad, parked_b), (split, parked_s));
    }

    /// A waker racing the split wait's publication: the waiter is claimed while still current, the
    /// reserved commit finds it already placed (counted), and its own read-back restores it.
    #[test]
    fn c6_a_wake_in_the_publication_window_is_counted_and_restored() {
        let k: &'static SharedKernel =
            alloc::boxed::Box::leak(alloc::boxed::Box::new(SharedKernel::new(Bootstrap::init().expect("init"))));
        let asid = k.with(|s| {
            s.register_task_with_class(BASE + 600, crate::kernel::task::TaskClass::App)
                .expect("reg");
            let (asid, cap) = space(s);
            s.bind_task_asid(BASE + 600, asid).expect("bind");
            let (_id, mem) = s.alloc_anonymous_memory_object().expect("object");
            s.map_user_page_with_caps(cap, mem, VirtAddr(X as u64), PageFlags::USER_RW)
                .expect("map");
            s.enqueue_on_cpu(CpuId(0), BASE + 600).expect("enqueue");
            s.set_current_cpu(CpuId(0)).expect("cpu");
            s.dispatch_next_task().expect("dispatch");
            s.write_user_memory(BASE + 600, X, &5u32.to_ne_bytes()).expect("word");
            asid
        });
        fx::futex_phase_hook::arm_after_register(alloc::boxed::Box::new(move || {
            assert_eq!(k.futex_wake_split_mut(CpuId(0), asid, X, 1, P), Ok(1));
            assert_eq!(k.with(|s| reserved(s)), 0);
        }));
        let outcome = k.futex_wait_park_exact_split(CpuId(0), BASE + 600, asid, X, 5, P);
        assert!(matches!(outcome, FutexParkOutcome::WokenDuringPublication { .. }), "{outcome:?}");
        assert_eq!(k.with(|s| s.task_status(BASE + 600)), Some(TaskStatus::Running));
        assert_eq!(k.with(|s| placement(s, BASE + 600)), TaskPlacement::Current(CpuId(0)));
    }

    /// Teardown between the reservation and the claim: the claim fails, the reservation is
    /// released, and the next waiter is woken in its place.
    #[test]
    fn c7_death_before_the_claim_releases_the_reservation() {
        let mut st = Bootstrap::init().expect("init");
        let (waiters, _, asid) = park_many(&mut st, 3, P);
        let k: &'static SharedKernel = alloc::boxed::Box::leak(alloc::boxed::Box::new(SharedKernel::new(st)));
        let victim = waiters[0];
        fx::futex_phase_hook::arm_wake_step(alloc::boxed::Box::new(move |step, tid| {
            if step == fx::futex_phase_hook::WAKE_AFTER_RESERVE && tid == victim {
                k.with(|s| s.exit_task(victim, 9)).expect("exit in the window");
                return false;
            }
            true
        }));
        assert_eq!(k.futex_wake_split_mut(CpuId(0), asid, X, 1, P), Ok(1));
        k.with(|s| {
            assert_eq!(s.task_status(victim), Some(TaskStatus::Exited(9)));
            assert_eq!(placement(s, victim), TaskPlacement::Nowhere, "the dead task was not queued");
            assert_eq!(s.task_status(waiters[1]), Some(TaskStatus::Runnable), "the next one instead");
            check_contract(s, &waiters[1..]);
        });
    }

    /// Teardown between the claim and the reserved commit: the commit still lands (it cannot be
    /// refused), no reservation leaks, and the dead task's queued entry is the scheduler's to
    /// discard, exactly as for any task that exits while queued.
    #[test]
    fn c8_death_after_the_claim_leaks_nothing() {
        let mut st = Bootstrap::init().expect("init");
        let (waiters, _, asid) = park_many(&mut st, 2, P);
        let k: &'static SharedKernel = alloc::boxed::Box::leak(alloc::boxed::Box::new(SharedKernel::new(st)));
        let victim = waiters[0];
        fx::futex_phase_hook::arm_wake_step(alloc::boxed::Box::new(move |step, tid| {
            if step == fx::futex_phase_hook::WAKE_AFTER_CLAIM && tid == victim {
                k.with(|s| s.exit_task(victim, 9)).expect("exit in the window");
                return false;
            }
            true
        }));
        assert_eq!(k.futex_wake_split_mut(CpuId(0), asid, X, 2, P), Ok(2));
        k.with(|s| {
            assert_eq!(reserved(s), 0);
            assert_eq!(s.task_status(victim), Some(TaskStatus::Exited(9)));
            assert_eq!(s.task_status(waiters[1]), Some(TaskStatus::Runnable));
            assert_eq!(queued_count(s, waiters[1]), 1);
        });
    }

    /// Forced refusal: the waiter's home CPU cannot take it (offline). The production wake makes
    /// no claim — the waiter stays parked with its record — and reports nothing woken.
    #[test]
    fn c9_a_refused_placement_claims_nothing() {
        let mut s = Bootstrap::init().expect("init");
        let (waiters, _, _) = park_many(&mut s, 2, P);
        s.with_tcbs_mut(|tcbs| {
            for t in tcbs.iter_mut().flatten().filter(|t| waiters.contains(&t.tid.0)) {
                t.cpu_affinity = Some(CpuId(5)); // present nowhere, online nowhere
            }
        });
        assert_eq!(s.futex_wake(X, u32::MAX, P), Ok(0));
        for &t in &waiters {
            assert!(parked(&s, t), "{t} keeps its wait");
            assert_eq!(placement(&s, t), TaskPlacement::Nowhere);
        }
        assert_eq!(s.futex_waiter_count(X), 2);
        assert_eq!(reserved(&s), 0);
    }

    /// The scheduler's half: a reservation is room no other user push may take, a reserved fill
    /// cannot be refused for capacity even when kernel threads fill their headroom, and a released
    /// reservation gives the room back.
    #[test]
    fn c10_reservations_are_exact_room() {
        use crate::kernel::scheduler::PriorityScheduler;
        let mut rq = PriorityScheduler::default();
        let n = crate::kernel::scheduler::MAX_RUN_QUEUE as u64;
        for t in 1..n {
            rq.enqueue_with_priority(ThreadId(t), TaskPriority::Normal).expect("room");
        }
        rq.reserve_user_slot(TaskPriority::Normal).expect("the last slot");
        assert_eq!(
            rq.enqueue_with_priority(ThreadId(n), TaskPriority::Normal),
            Err(crate::kernel::scheduler::SchedulerError::QueueFull),
            "a reserved slot is not anyone else's"
        );
        assert_eq!(
            rq.reserve_user_slot(TaskPriority::Normal),
            Err(crate::kernel::scheduler::SchedulerError::QueueFull)
        );
        for k in 0..crate::kernel::kthread::KTHREAD_CAPACITY as u64 {
            // A kernel-thread TID carries the `KT` tag in its top 16 bits.
            let kt = (0x4B54u64 << 48) | k;
            assert!(crate::kernel::kthread::is_kthread_tid(kt));
            rq.enqueue_with_priority(ThreadId(kt), TaskPriority::Normal)
                .expect("kernel-thread headroom");
        }
        assert_eq!(rq.enqueue_reserved_with_priority(ThreadId(n), TaskPriority::Normal), Ok(()));
        assert_eq!(rq.reserved_user_slots(TaskPriority::Normal), 0);
    }
}
