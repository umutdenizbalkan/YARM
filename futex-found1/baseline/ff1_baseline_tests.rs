
/// FUTEX-FOUND1 — baseline reproducers, written against the futex ABI of `a7a27fb4`
/// (`futex_wait_current(addr, expected, observed)`, `futex_wake(addr, max_wake)`, the split
/// `futex_wake_split_mut(cpu, addr, max_wake)`). Each one asserts the behaviour the foundation
/// requires; a failure here is a recorded baseline defect, a pass an invariant that already held.
mod futex_found1_baseline {
    use super::*;
    use crate::kernel::capabilities::CapId;
    use crate::kernel::ipc::ThreadId;
    use crate::kernel::task::{TaskClass, TaskStatus, WaitReason};
    use crate::kernel::vm::{Asid, Mapping, PageFlags, PhysAddr, VirtAddr};

    const X: usize = 0x6_0000;
    const Y: usize = 0x7_0000;

    fn space(s: &mut KernelState) -> (Asid, CapId) {
        s.create_user_address_space().expect("address space")
    }
    /// A user task of `asid`, runnable with a resumable context, homed on CPU 0.
    fn task(s: &mut KernelState, tid: u64, asid: Asid) {
        let _ = TaskClass::App;
        s.register_task(tid).expect("task");
        s.bind_task_asid(tid, asid).expect("bind");
        s.set_task_status_for_test(tid, TaskStatus::Runnable);
        s.seed_resumable_user_context_for_test(tid);
        s.set_task_home_cpu(tid, crate::kernel::scheduler::CpuId(0))
            .expect("home");
        s.enqueue_task(tid).expect("enqueue");
    }
    /// A page of anonymous MemoryObject backing at `va` in the space `aspace_cap` names.
    fn object_page(s: &mut KernelState, aspace_cap: CapId, va: usize) -> CapId {
        let (_id, mem) = s.alloc_anonymous_memory_object().expect("object");
        s.map_user_page_with_caps(aspace_cap, mem, VirtAddr(va as u64), PageFlags::USER_RW)
            .expect("map object page");
        mem
    }
    /// A raw private user frame at `va` in `asid` (no MemoryObject).
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
    fn word(s: &mut KernelState, tid: u64, va: usize, v: u32) {
        s.write_user_memory(tid, va, &v.to_ne_bytes()).expect("word");
    }
    fn run(s: &mut KernelState, tid: u64) {
        if s.current_tid() != Some(tid) {
            s.yield_current_to(ThreadId(tid)).expect("switch");
        }
        assert_eq!(s.current_tid(), Some(tid));
    }
    fn futex_blocked(s: &KernelState, tid: u64) -> bool {
        matches!(
            s.task_status(tid),
            Some(TaskStatus::Blocked(WaitReason::Futex(_)))
        )
    }
    fn wait(s: &mut KernelState, tid: u64, va: usize, v: u32) {
        run(s, tid);
        assert!(s.futex_wait_current(va, v, v).expect("wait"), "{tid} parks");
        assert!(futex_blocked(s, tid));
    }

    /// F1 — two unrelated address spaces, the same VA, private frames.
    #[test]
    fn f01_a_private_futex_is_not_woken_from_another_address_space() {
        let mut s = Bootstrap::init().expect("init");
        let (a, _) = space(&mut s);
        let (b, _) = space(&mut s);
        task(&mut s, 41001, a);
        task(&mut s, 41002, b);
        private_page(&mut s, a, X);
        private_page(&mut s, b, X);
        word(&mut s, 41001, X, 5);
        word(&mut s, 41002, X, 5);
        wait(&mut s, 41002, X, 5);
        run(&mut s, 41001);
        let woke = s.futex_wake(X, 1).expect("wake");
        assert_eq!(woke, 0, "A's wake reached B's private waiter");
        assert!(futex_blocked(&s, 41002));
    }

    /// F2 — two threads of one address space share the private key.
    #[test]
    fn f02_threads_of_one_space_synchronise() {
        let mut s = Bootstrap::init().expect("init");
        let (a, _) = space(&mut s);
        task(&mut s, 41011, a);
        task(&mut s, 41012, a);
        private_page(&mut s, a, X);
        word(&mut s, 41011, X, 5);
        wait(&mut s, 41011, X, 5);
        run(&mut s, 41012);
        assert_eq!(s.futex_wake(X, 1).expect("wake"), 1);
        assert_eq!(s.task_status(41011), Some(TaskStatus::Runnable));
    }

    /// F3 — one MemoryObject mapped by two processes at different VAs.
    #[test]
    fn f03_shared_backing_at_different_vas_synchronises() {
        let mut s = Bootstrap::init().expect("init");
        let (a, acap) = space(&mut s);
        let (b, bcap) = space(&mut s);
        task(&mut s, 41021, a);
        task(&mut s, 41022, b);
        let mem = object_page(&mut s, acap, X);
        s.map_user_page_with_caps(bcap, mem, VirtAddr(Y as u64), PageFlags::USER_RW)
            .expect("map the same object in B");
        word(&mut s, 41021, X, 5);
        word(&mut s, 41022, Y, 5);
        wait(&mut s, 41022, Y, 5);
        run(&mut s, 41021);
        assert_eq!(
            s.futex_wake(X, 1).expect("wake"),
            1,
            "a wake on the shared word must reach the other mapping's waiter"
        );
    }

    /// F4 — distinct MemoryObjects at the same VA in two processes.
    #[test]
    fn f04_distinct_backing_objects_never_alias() {
        let mut s = Bootstrap::init().expect("init");
        let (a, acap) = space(&mut s);
        let (b, bcap) = space(&mut s);
        task(&mut s, 41031, a);
        task(&mut s, 41032, b);
        object_page(&mut s, acap, X);
        object_page(&mut s, bcap, X);
        word(&mut s, 41031, X, 5);
        word(&mut s, 41032, X, 5);
        wait(&mut s, 41032, X, 5);
        run(&mut s, 41031);
        assert_eq!(s.futex_wake(X, 1).expect("wake"), 0);
        assert!(futex_blocked(&s, 41032));
    }

    /// F5 / F6 — a wake before the wait is never lost; a changed word never parks.
    #[test]
    fn f05_f06_a_changed_word_never_parks() {
        let mut s = Bootstrap::init().expect("init");
        let (a, _) = space(&mut s);
        task(&mut s, 41041, a);
        private_page(&mut s, a, X);
        word(&mut s, 41041, X, 7);
        run(&mut s, 41041);
        assert!(!s.futex_wait_current(X, 5, 5).expect("wait"), "word 7 != 5");
        assert_eq!(s.current_tid(), Some(41041), "nothing parked");
    }

    /// F11 — FIFO within one key, unrelated keys untouched.
    #[test]
    fn f11_wake_is_fifo_within_a_key() {
        let mut s = Bootstrap::init().expect("init");
        let (a, _) = space(&mut s);
        for t in [41051, 41052, 41053, 41054, 41055] {
            task(&mut s, t, a);
        }
        private_page(&mut s, a, X);
        private_page(&mut s, a, Y);
        word(&mut s, 41051, X, 5);
        word(&mut s, 41051, Y, 5);
        // Arrival order 41053, 41051, 41052 on X; 41054 on Y.
        wait(&mut s, 41053, X, 5);
        wait(&mut s, 41051, X, 5);
        wait(&mut s, 41052, X, 5);
        wait(&mut s, 41054, Y, 5);
        run(&mut s, 41055);
        assert_eq!(s.futex_wake(X, 1).expect("wake"), 1);
        assert_eq!(
            s.task_status(41053),
            Some(TaskStatus::Runnable),
            "the first arrival is woken first"
        );
        assert!(futex_blocked(&s, 41051) && futex_blocked(&s, 41052));
        assert_eq!(s.futex_wake(X, 1).expect("wake"), 1);
        assert_eq!(s.task_status(41051), Some(TaskStatus::Runnable));
        assert!(futex_blocked(&s, 41054), "the other key is untouched");
    }

    /// F8 — an exiting waiter takes only itself away.
    #[test]
    fn f08_exit_removes_only_the_exiting_waiter() {
        let mut s = Bootstrap::init().expect("init");
        let (a, _) = space(&mut s);
        for t in [41061, 41062, 41063] {
            task(&mut s, t, a);
        }
        private_page(&mut s, a, X);
        word(&mut s, 41061, X, 5);
        wait(&mut s, 41061, X, 5);
        wait(&mut s, 41062, X, 5);
        run(&mut s, 41063);
        s.exit_task(41061, 0).expect("exit the first waiter");
        assert_eq!(s.futex_wake(X, u32::MAX).expect("wake"), 1);
        assert_eq!(s.task_status(41062), Some(TaskStatus::Runnable));
    }

    /// F10 — a replacement address space reusing a destroyed ASID number inherits nothing.
    #[test]
    fn f10_a_reused_asid_inherits_no_waiter() {
        let mut s = Bootstrap::init().expect("init");
        let (a, _) = space(&mut s);
        task(&mut s, 41071, a);
        private_page(&mut s, a, X);
        word(&mut s, 41071, X, 5);
        wait(&mut s, 41071, X, 5);
        s.with_user_spaces_mut(|m| m.destroy(a, 0)).expect("destroy");
        // Cycle the ASID allocator until the destroyed number comes back.
        let mut reused = None;
        for _ in 0..70_000 {
            // The allocator directly: a capability per cycle would exhaust the cspace.
            let n = s
                .with_user_spaces_mut(|m| m.create_user_space())
                .expect("create");
            if n == a {
                reused = Some(n);
                break;
            }
            // Never resident: destroy without a shootdown retirement.
            s.with_user_spaces_mut(|m| m.destroy(n, 0)).expect("destroy");
        }
        let n = reused.expect("the ASID number is reused");
        task(&mut s, 41072, n);
        private_page(&mut s, n, X);
        word(&mut s, 41072, X, 5);
        run(&mut s, 41072);
        assert_eq!(
            s.futex_wake(X, 1).expect("wake"),
            0,
            "the replacement space woke a waiter of the destroyed one"
        );
    }

    /// F13 — a wake from another CPU enqueues the waiter exactly once on its home CPU.
    #[test]
    fn f13_a_remote_wake_enqueues_once_on_the_home_cpu() {
        use crate::kernel::scheduler::CpuId;
        let k = crate::runtime::SharedKernel::new(Bootstrap::init().expect("init"));
        let (w, asid) = k.with(|s| {
            s.bring_up_cpu(CpuId(1)).expect("cpu 1");
            let (a, _) = space(s);
            task(s, 41081, a);
            private_page(s, a, X);
            word(s, 41081, X, 5);
            s.set_task_home_cpu(41081, CpuId(0)).expect("home");
            run(s, 41081);
            (41081u64, a)
        });
        let parked = k.futex_wait_park_exact_split(CpuId(0), w, asid, X, 5);
        assert!(matches!(
            parked,
            crate::kernel::syscall::sched::FutexParkOutcome::Parked { .. }
        ));
        assert_eq!(k.futex_wake_split_mut(CpuId(1), X, 1), 1);
        let queued = k.with(|s| {
            s.with_scheduler_state(|sched| {
                let mut v = alloc::vec::Vec::new();
                sched.for_each_queued_on(CpuId(0), |_, t| v.push(t.0));
                v
            })
        });
        assert_eq!(queued.iter().filter(|t| **t == w).count(), 1);
        assert_eq!(k.with(|s| s.task_status(w)), Some(TaskStatus::Runnable));
    }

    /// F14 — robust cleanup wakes only the exiting task's own namespace.
    #[test]
    fn f14_robust_cleanup_uses_the_exiting_tasks_key() {
        let mut s = Bootstrap::init().expect("init");
        let (a, _) = space(&mut s);
        let (b, _) = space(&mut s);
        task(&mut s, 41091, a); // the exiting owner
        task(&mut s, 41092, a); // its sibling, waiting on the robust word
        task(&mut s, 41093, b); // an unrelated process, same VA
        private_page(&mut s, a, X);
        private_page(&mut s, b, X);
        word(&mut s, 41091, X, 5);
        word(&mut s, 41093, X, 5);
        s.set_robust_futex_head(41091, X, 1).expect("robust");
        wait(&mut s, 41092, X, 5);
        wait(&mut s, 41093, X, 5);
        s.exit_task(41091, 0).expect("exit");
        assert_eq!(s.task_status(41092), Some(TaskStatus::Runnable), "the sibling");
        assert!(futex_blocked(&s, 41093), "the unrelated process's waiter");
    }
}
