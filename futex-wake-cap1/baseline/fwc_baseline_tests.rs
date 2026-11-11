
/// FUTEX-WAKE-CAP1 — baseline reproducers for the wake-to-scheduler liveness contract: every waiter
/// a FutexWake claims ends `Runnable` AND placed (queued exactly once, or current), and the count
/// the wake reports includes only waiters that got that continuation.
mod futex_wake_cap1_baseline {
    use super::*;
    use crate::kernel::boot::futex::FutexKind;
    use crate::kernel::capabilities::CapId;
    use crate::kernel::ipc::ThreadId;
    use crate::kernel::scheduler::{CpuId, TaskPlacement};
    use crate::kernel::task::TaskStatus;
    use crate::kernel::vm::{Asid, Mapping, PageFlags, PhysAddr, VirtAddr};
    use crate::runtime::SharedKernel;

    const P: FutexKind = FutexKind::Private;
    const X: usize = 0x6_0000;
    const BASE: u64 = 42000;

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
    /// `n` waiters parked on X, one at a time, so the run queue never holds more than one of them
    /// before the wake; then the waker current. Returns (waiters, waker, asid).
    fn park_many(s: &mut KernelState, n: u64) -> (alloc::vec::Vec<u64>, u64, Asid) {
        let (a, _) = space(s);
        private_page(s, a, X);
        let waker = BASE + 999;
        let mut waiters = alloc::vec::Vec::new();
        for i in 0..n {
            let t = BASE + i;
            task(s, t, a);
            if i == 0 {
                s.write_user_memory(t, X, &5u32.to_ne_bytes()).expect("word");
            }
            run(s, t);
            assert!(s.futex_wait_current(X, 5, P).expect("wait"), "{t} parks");
            waiters.push(t);
        }
        task(s, waker, a);
        run(s, waker);
        (waiters, waker, a)
    }
    /// The contract, checked for every waiter: Runnable, and placed exactly once.
    fn stranded(s: &KernelState, waiters: &[u64]) -> alloc::vec::Vec<u64> {
        waiters
            .iter()
            .copied()
            .filter(|&t| {
                s.task_status(t) == Some(TaskStatus::Runnable)
                    && !matches!(placement(s, t), TaskPlacement::Queued(_))
            })
            .collect()
    }

    /// 65 same-priority waiters, one CPU, an empty backlog: wake 65.
    #[test]
    fn w1_sixty_five_waiters_on_one_cpu_are_all_placed() {
        let mut s = Bootstrap::init().expect("init");
        let (waiters, _waker, _) = park_many(&mut s, 65);
        let woke = s.futex_wake(X, 65, P).expect("wake");
        let lost = stranded(&s, &waiters);
        assert!(lost.is_empty(), "Runnable but on no queue and in no current slot: {lost:?}");
        let placed = waiters
            .iter()
            .filter(|&&t| matches!(placement(&s, t), TaskPlacement::Queued(_)))
            .count() as u32;
        assert_eq!(woke, placed, "the reported count includes only placed waiters");
    }

    /// A partly filled queue fails sooner: 10 runnable tasks already queued, then 60 woken.
    #[test]
    fn w2_a_partly_filled_queue_strands_no_waiter() {
        let mut s = Bootstrap::init().expect("init");
        let (waiters, waker, a) = park_many(&mut s, 60);
        for i in 0..10 {
            task(&mut s, BASE + 500 + i, a);
        }
        assert_eq!(s.current_tid(), Some(waker));
        let woke = s.futex_wake(X, u32::MAX, P).expect("wake");
        let lost = stranded(&s, &waiters);
        assert!(lost.is_empty(), "stranded: {lost:?} (woke={woke})");
    }

    /// The split wake, the same population.
    #[test]
    fn w3_the_split_wake_strands_no_waiter() {
        let mut st = Bootstrap::init().expect("init");
        let (waiters, waker, asid) = park_many(&mut st, 65);
        let k = SharedKernel::new(st);
        let woke = k
            .futex_wake_split_mut(CpuId(0), asid, X, 65, P)
            .expect("split wake");
        let lost = k.with(|s| stranded(s, &waiters));
        assert!(lost.is_empty(), "stranded: {lost:?} (woke={woke}, waker={waker})");
    }
}
