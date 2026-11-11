
/// FUTEX-FOUND1 — the foundation's reproducers F1–F15, against the keyed futex ABI.
///
/// F1, F3, F4, F10, F11 and F14 FAILED on `a7a27fb4` (address-only matching, no shared key, no
/// FIFO, address-only robust cleanup; see `futex_found1_baseline` in the stage record). F2, F5/F6,
/// F8 and F13 PASSED there and are kept as invariants. F7, F9 and F12 could not be exercised on
/// the baseline (no window hook, no per-cycle identity, no waiter store). F15 is the live boot
/// matrix; its hosted half is the ABI agreement below.
mod futex_found1 {
    use super::*;
    use crate::kernel::boot::futex::{
        self as fx, FUTEX_FLAG_SHARED, FutexKey, FutexKind, FutexWaitToken,
    };
    use crate::kernel::capabilities::CapId;
    use crate::kernel::ipc::ThreadId;
    use crate::kernel::scheduler::CpuId;
    use crate::kernel::syscall::sched::FutexParkOutcome;
    use crate::kernel::task::{TaskClass, TaskStatus, WaitReason};
    use crate::kernel::vm::{Asid, Mapping, PageFlags, PhysAddr, VirtAddr};
    use crate::runtime::SharedKernel;

    const P: FutexKind = FutexKind::Private;
    const SH: FutexKind = FutexKind::Shared;
    const X: usize = 0x6_0000;
    const Y: usize = 0x7_0000;

    fn space(s: &mut KernelState) -> (Asid, CapId) {
        s.create_user_address_space().expect("address space")
    }
    /// A user task of `asid`, runnable with a resumable context, homed on CPU 0.
    fn task(s: &mut KernelState, tid: u64, asid: Asid) {
        s.register_task(tid).expect("task");
        s.bind_task_asid(tid, asid).expect("bind");
        s.set_task_status_for_test(tid, TaskStatus::Runnable);
        s.seed_resumable_user_context_for_test(tid);
        s.set_task_home_cpu(tid, CpuId(0)).expect("home");
        s.enqueue_task(tid).expect("enqueue");
    }
    /// A page of an anonymous MemoryObject at `va` in the space `aspace_cap` names.
    fn object_page(s: &mut KernelState, aspace_cap: CapId, va: usize) -> CapId {
        let (_id, mem) = s.alloc_anonymous_memory_object().expect("object");
        s.map_user_page_with_caps(aspace_cap, mem, VirtAddr(va as u64), PageFlags::USER_RW)
            .expect("map object page");
        mem
    }
    /// A raw user frame at `va` in `asid` that no MemoryObject owns.
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
        s.write_user_memory(tid, va, &v.to_ne_bytes())
            .expect("word");
    }
    fn run(s: &mut KernelState, tid: u64) {
        if s.current_tid() != Some(tid) {
            s.yield_current_to(ThreadId(tid)).expect("switch");
        }
        assert_eq!(s.current_tid(), Some(tid));
    }
    fn token_of(s: &KernelState, tid: u64) -> Option<FutexWaitToken> {
        match s.task_status(tid) {
            Some(TaskStatus::Blocked(WaitReason::Futex(token))) => Some(token),
            _ => None,
        }
    }
    fn parked(s: &KernelState, tid: u64) -> bool {
        token_of(s, tid).is_some()
    }
    fn wait(s: &mut KernelState, tid: u64, va: usize, v: u32, kind: FutexKind) -> FutexWaitToken {
        run(s, tid);
        assert!(
            s.futex_wait_current(va, v, kind).expect("wait"),
            "{tid} parks"
        );
        token_of(s, tid).expect("parked on a token")
    }
    fn occupied(s: &mut KernelState) -> usize {
        s.with_task_futex_mut(|_t, table| table.occupied())
    }
    fn live_on(s: &mut KernelState, key: FutexKey) -> usize {
        s.with_task_futex_mut(|tcbs, table| {
            fx::live_futex_waiters_locked(tcbs.as_slice(), table, key)
        })
    }

    /// A split-route caller: `tid` in a fresh address space whose page at `X` is an anonymous
    /// object holding `v`, current on `cpu`.
    fn split_caller(k: &SharedKernel, tid: u64, cpu: CpuId, v: u32) -> Asid {
        let asid = k.with(|s| {
            s.register_task_with_class(tid, TaskClass::App)
                .expect("reg");
            let (asid, cap) = space(s);
            s.bind_task_asid(tid, asid).expect("bind");
            object_page(s, cap, X);
            s.enqueue_on_cpu(cpu, tid).expect("enqueue");
            s.set_current_cpu(cpu).expect("cpu");
            s.dispatch_next_task().expect("dispatch");
            word(s, tid, X, v);
            asid
        });
        assert_eq!(k.with(|s| s.current_tid_on_cpu(cpu)), Some(tid));
        asid
    }
    fn leak(s: KernelState) -> &'static SharedKernel {
        alloc::boxed::Box::leak(alloc::boxed::Box::new(SharedKernel::new(s)))
    }

    /// F1 — two unrelated address spaces, the same VA: a private wake reaches only its own space.
    #[test]
    fn f01_a_private_futex_is_not_woken_from_another_address_space() {
        let mut s = Bootstrap::init().expect("init");
        let (a, _) = space(&mut s);
        let (b, _) = space(&mut s);
        task(&mut s, 41001, a);
        task(&mut s, 41002, b);
        task(&mut s, 41003, b);
        private_page(&mut s, a, X);
        private_page(&mut s, b, X);
        word(&mut s, 41001, X, 5);
        word(&mut s, 41002, X, 5);
        wait(&mut s, 41002, X, 5, P);
        run(&mut s, 41001);
        assert_eq!(
            s.futex_wake(X, 1, P),
            Ok(0),
            "A's wake reached B's private waiter"
        );
        assert!(parked(&s, 41002));
        // B's own thread does reach it.
        run(&mut s, 41003);
        assert_eq!(s.futex_wake(X, 1, P), Ok(1));
        assert_eq!(s.task_status(41002), Some(TaskStatus::Runnable));
    }

    /// F2 — threads of one address space synchronise through the one private key.
    #[test]
    fn f02_threads_of_one_space_share_the_private_key() {
        let mut s = Bootstrap::init().expect("init");
        let (a, _) = space(&mut s);
        task(&mut s, 41011, a);
        task(&mut s, 41012, a);
        private_page(&mut s, a, X);
        word(&mut s, 41011, X, 5);
        let t = wait(&mut s, 41011, X, 5, P);
        let key = s.private_futex_key_for_test(41012, X);
        assert_eq!(
            s.with_task_futex_mut(|_t, table| table.key_of(t)),
            Some(key),
            "the waiter's key is the space's private key"
        );
        run(&mut s, 41012);
        assert_eq!(s.futex_wake(X, 1, P), Ok(1));
        assert_eq!(s.task_status(41011), Some(TaskStatus::Runnable));
        assert_eq!(occupied(&mut s), 0, "the claim retired the record");
    }

    /// F3 — one MemoryObject mapped by two processes at different VAs is one shared key, and a
    /// shared wait is never silently treated as private (or the reverse).
    #[test]
    fn f03_shared_backing_at_different_vas_is_one_key() {
        let mut s = Bootstrap::init().expect("init");
        let (a, acap) = space(&mut s);
        let (b, bcap) = space(&mut s);
        task(&mut s, 41021, a);
        task(&mut s, 41022, b);
        let mem = object_page(&mut s, acap, X);
        s.map_user_page_with_caps(bcap, mem, VirtAddr(Y as u64), PageFlags::USER_RW)
            .expect("map the same object in B");
        // Hosted user memory is per address space, so each mapping's view is written.
        word(&mut s, 41022, Y + 8, 0);
        for off in [4, 8] {
            word(&mut s, 41021, X + off, 0);
        }
        let t = wait(&mut s, 41022, Y + 8, 0, SH);
        let key = s
            .with_task_futex_mut(|_t, table| table.key_of(t))
            .expect("key");
        assert!(
            matches!(key, FutexKey::Shared { offset: 8, .. }),
            "the shared key is the object and the offset in it, not the VA: {key:?}"
        );
        run(&mut s, 41021);
        assert_eq!(
            s.futex_wake(X + 8, 1, P),
            Ok(0),
            "a PRIVATE wake does not reach a shared waiter"
        );
        assert_eq!(
            s.futex_wake(X + 4, 1, SH),
            Ok(0),
            "another offset is another key"
        );
        assert_eq!(
            s.futex_wake(X + 8, 1, SH),
            Ok(1),
            "the other mapping's waiter"
        );
        assert_eq!(s.task_status(41022), Some(TaskStatus::Runnable));
        // A shared wait on memory no allocator-owned object backs is refused, not made private.
        private_page(&mut s, a, Y);
        word(&mut s, 41021, Y, 0);
        assert_eq!(
            s.futex_wait_current(Y, 0, SH),
            Err(KernelError::WrongObject)
        );
        assert_eq!(s.current_tid(), Some(41021), "and nothing parked");
    }

    /// F4 — identical offsets in distinct objects, at the same VA in two processes, never alias.
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
        wait(&mut s, 41032, X, 5, SH);
        run(&mut s, 41031);
        assert_eq!(s.futex_wake(X, 1, SH), Ok(0));
        assert!(parked(&s, 41032));
    }

    /// F5 — a wake BEFORE the wait (the waker changed the word first) is not lost, one DURING
    /// publication is detected, one AFTER it is delivered.
    #[test]
    fn f05_wake_before_during_and_after_publication() {
        // Before: the waker stores, wakes nobody; the waiter's stale `expected` does not park.
        let mut s = Bootstrap::init().expect("init");
        let (a, _) = space(&mut s);
        task(&mut s, 41041, a);
        task(&mut s, 41042, a);
        private_page(&mut s, a, X);
        word(&mut s, 41041, X, 5);
        run(&mut s, 41042);
        word(&mut s, 41042, X, 6);
        assert_eq!(s.futex_wake(X, 1, P), Ok(0));
        run(&mut s, 41041);
        assert_eq!(
            s.futex_wait_current(X, 5, P),
            Ok(false),
            "the store is seen"
        );
        assert_eq!(s.current_tid(), Some(41041));
        // After: park on the current value, then a wake delivers it.
        wait(&mut s, 41041, X, 6, P);
        run(&mut s, 41042);
        assert_eq!(s.futex_wake(X, 1, P), Ok(1));
        assert_eq!(s.task_status(41041), Some(TaskStatus::Runnable));

        // During: a waker that lands between the split registration and the compare-and-clear.
        let k = leak(Bootstrap::init().expect("init"));
        let cpu = CpuId(0);
        let asid = split_caller(k, 41043, cpu, 5);
        fx::futex_phase_hook::arm_after_register(alloc::boxed::Box::new(move || {
            assert_eq!(k.futex_wake_split_mut(CpuId(0), asid, X, 1, P), Ok(1));
        }));
        let outcome = k.futex_wait_park_exact_split(cpu, 41043, asid, X, 5, P);
        assert!(
            matches!(outcome, FutexParkOutcome::WokenDuringPublication { .. }),
            "{outcome:?}"
        );
    }

    /// F6 — a word that changes before the park commits prevents the obsolete wait: at the
    /// decision (broad and split), and AFTER the waiter is registered (the re-read).
    #[test]
    fn f06_a_changed_word_never_commits_an_obsolete_wait() {
        let mut s = Bootstrap::init().expect("init");
        let (a, _) = space(&mut s);
        task(&mut s, 41051, a);
        private_page(&mut s, a, X);
        word(&mut s, 41051, X, 7);
        run(&mut s, 41051);
        assert_eq!(s.futex_wait_current(X, 5, P), Ok(false), "word 7 != 5");
        assert_eq!(s.task_status(41051), Some(TaskStatus::Running));
        assert_eq!(occupied(&mut s), 0);

        let k = leak(Bootstrap::init().expect("init"));
        let cpu = CpuId(0);
        let asid = split_caller(k, 41052, cpu, 7);
        assert_eq!(
            k.futex_wait_decide_split_read(41052, X, 5, P),
            Ok(crate::kernel::syscall::sched::FutexWaitDecision::Proceed)
        );
        k.with(|s| word(s, 41052, X, 5));
        // The word moves once the waiter is visible, before the compare-and-clear.
        fx::futex_phase_hook::arm_after_register(alloc::boxed::Box::new(move || {
            k.with(|s| word(s, 41052, X, 6));
        }));
        assert_eq!(
            k.futex_wait_park_exact_split(cpu, 41052, asid, X, 5, P),
            FutexParkOutcome::ValueChanged
        );
        assert_eq!(k.with(|s| s.task_status(41052)), Some(TaskStatus::Running));
        assert_eq!(k.with(|s| s.current_tid_on_cpu(cpu)), Some(41052));
        assert_eq!(
            k.with(|s| s.futex_waiter_count(X)),
            0,
            "the registration was undone"
        );
    }

    /// F7 — a wake during publication neither strands the caller nor resumes it through a frame
    /// it no longer owns: it is Running and current on its CPU, in no run queue, with no record.
    #[test]
    fn f07_a_wake_during_publication_restores_the_exact_caller() {
        let k = leak(Bootstrap::init().expect("init"));
        let cpu = CpuId(0);
        let asid = split_caller(k, 41061, cpu, 5);
        fx::futex_phase_hook::arm_after_register(alloc::boxed::Box::new(move || {
            assert_eq!(k.futex_wake_split_mut(CpuId(0), asid, X, 1, P), Ok(1));
        }));
        let FutexParkOutcome::WokenDuringPublication {
            entering,
            recovered,
        } = k.futex_wait_park_exact_split(cpu, 41061, asid, X, 5, P)
        else {
            panic!("the window wake is detected");
        };
        assert_eq!((entering.tid, entering.asid), (41061, asid));
        assert!(recovered.may_resume_entering_frame(), "{recovered:?}");
        assert_eq!(k.with(|s| s.task_status(41061)), Some(TaskStatus::Running));
        assert_eq!(k.with(|s| s.current_tid_on_cpu(cpu)), Some(41061));
        let queued = k.with(|s| {
            let mut v = alloc::vec::Vec::new();
            s.for_each_queued_on_for_test(cpu, |_, t| v.push(t.0));
            v
        });
        assert!(!queued.contains(&41061), "not also queued: {queued:?}");
        assert_eq!(k.with(|s| s.futex_waiter_count(X)), 0);
    }

    /// F8 — exit removes exactly the exiting waiter; the others keep their records.
    #[test]
    fn f08_exit_removes_only_the_exiting_waiter() {
        let mut s = Bootstrap::init().expect("init");
        let (a, _) = space(&mut s);
        for t in [41071, 41072, 41073] {
            task(&mut s, t, a);
        }
        private_page(&mut s, a, X);
        word(&mut s, 41071, X, 5);
        wait(&mut s, 41071, X, 5, P);
        let t2 = wait(&mut s, 41072, X, 5, P);
        assert_eq!(occupied(&mut s), 2);
        run(&mut s, 41073);
        s.exit_task(41071, 0).expect("exit the first waiter");
        assert_eq!(
            occupied(&mut s),
            1,
            "exactly the exiting waiter's record went"
        );
        assert_eq!(
            token_of(&s, 41072),
            Some(t2),
            "the other waiter is untouched"
        );
        assert_eq!(s.futex_wake(X, u32::MAX, P), Ok(1));
        assert_eq!(s.task_status(41072), Some(TaskStatus::Runnable));
    }

    /// F9 — repeated wait cycles: an earlier cycle's delayed cleanup cannot wake or remove the
    /// newer cycle, and its stale record is reclaimed without being counted.
    #[test]
    fn f09_an_earlier_cycle_cannot_touch_a_newer_one() {
        let mut s = Bootstrap::init().expect("init");
        let (a, _) = space(&mut s);
        task(&mut s, 41081, a);
        task(&mut s, 41082, a);
        private_page(&mut s, a, X);
        word(&mut s, 41081, X, 5);
        let t1 = wait(&mut s, 41081, X, 5, P);
        // Cycle 1 ends through some OTHER owner — its record is left behind, stale.
        s.set_task_status_for_test(41081, TaskStatus::Runnable);
        s.enqueue_task(41081).expect("requeue");
        let t2 = wait(&mut s, 41081, X, 5, P);
        assert_ne!(t1, t2, "each cycle has its own token");
        let key = s.private_futex_key_for_test(41081, X);
        assert_eq!(live_on(&mut s, key), 1, "only the newer cycle is live");
        // Cycle 1's delayed undo and read-back see nothing of theirs.
        let undone = s.with_task_futex_mut(|tcbs, table| {
            fx::undo_futex_registration_locked(tcbs.as_mut_slice(), table, 41081, t1)
        });
        assert!(!undone, "a stale undo changes nothing");
        assert!(
            !s.with_task_futex_mut(|tcbs, _| fx::futex_still_parked_locked(
                tcbs.as_slice(),
                41081,
                t1
            ))
        );
        assert_eq!(token_of(&s, 41081), Some(t2));
        // The wake meets the stale record first (FIFO), retires it uncounted, and claims cycle 2.
        run(&mut s, 41082);
        assert_eq!(s.futex_wake(X, 1, P), Ok(1));
        assert_eq!(s.task_status(41081), Some(TaskStatus::Runnable));
        assert_eq!(occupied(&mut s), 0);
    }

    /// F10 — a replacement address space that reuses a destroyed ASID number inherits nothing.
    #[test]
    fn f10_a_reused_asid_inherits_no_waiter() {
        let mut s = Bootstrap::init().expect("init");
        let (a, _) = space(&mut s);
        task(&mut s, 41091, a);
        private_page(&mut s, a, X);
        word(&mut s, 41091, X, 5);
        let t = wait(&mut s, 41091, X, 5, P);
        let old_key = s
            .with_task_futex_mut(|_t, table| table.key_of(t))
            .expect("key");
        s.with_user_spaces_mut(|m| m.destroy(a, 0))
            .expect("destroy");
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
            s.with_user_spaces_mut(|m| m.destroy(n, 0))
                .expect("destroy");
        }
        let n = reused.expect("the ASID number is reused");
        task(&mut s, 41092, n);
        private_page(&mut s, n, X);
        word(&mut s, 41092, X, 5);
        let new_key = s.private_futex_key_for_test(41092, X);
        assert_ne!(new_key, old_key, "the same number, a new incarnation");
        run(&mut s, 41092);
        assert_eq!(
            s.futex_wake(X, 1, P),
            Ok(0),
            "the replacement space woke a waiter of the destroyed one"
        );
        assert!(parked(&s, 41091));
    }

    /// F10 (object half) — a waiter on a reclaimed object holds nothing of it: a NEW object at the
    /// same VA (on the same frame, when the allocator hands it back) is a new key, and the old
    /// waiter's record leaves exactly once, with its task.
    #[test]
    fn f10b_a_replacement_object_inherits_no_waiter() {
        let mut s = Bootstrap::init().expect("init");
        let (a, _) = space(&mut s);
        task(&mut s, 41093, a);
        task(&mut s, 41094, a);
        let (old_id, old_phys) = s.alloc_anonymous_object_without_cap().expect("object");
        let map = |s: &mut KernelState, phys: PhysAddr| {
            s.map_user_page_in_asid_raw(
                a,
                VirtAddr(X as u64),
                Mapping {
                    phys,
                    flags: PageFlags::USER_RW,
                },
            )
            .expect("map");
            s.note_mapping_inserted(phys);
        };
        map(&mut s, old_phys);
        word(&mut s, 41093, X, 5);
        let t = wait(&mut s, 41093, X, 5, SH);
        assert_eq!(
            s.with_task_futex_mut(|_t, table| table.key_of(t)),
            Some(FutexKey::Shared {
                object_id: old_id,
                offset: 0
            })
        );
        // The last mapping goes, and with it the object.
        run(&mut s, 41094);
        s.unmap_user_page_in_asid(a, VirtAddr(X as u64))
            .expect("unmap");
        let (new_id, new_phys) = s.alloc_anonymous_object_without_cap().expect("object");
        assert_ne!(new_id, old_id, "object ids are never reused");
        map(&mut s, new_phys);
        word(&mut s, 41094, X, 5);
        assert_eq!(
            s.futex_wake(X, u32::MAX, SH),
            Ok(0),
            "the new object has no waiter"
        );
        assert!(parked(&s, 41093));
        s.exit_task(41093, 0).expect("exit");
        assert_eq!(occupied(&mut s), 0, "released exactly once, with its task");
    }

    /// F11 — FIFO within a key, the count honoured exactly, unrelated keys untouched.
    #[test]
    fn f11_wake_is_fifo_and_counted_within_a_key() {
        let mut s = Bootstrap::init().expect("init");
        let (a, _) = space(&mut s);
        for t in [41101, 41102, 41103, 41104, 41105] {
            task(&mut s, t, a);
        }
        private_page(&mut s, a, X);
        private_page(&mut s, a, Y);
        word(&mut s, 41101, X, 5);
        word(&mut s, 41101, Y, 5);
        // Arrival order 41103, 41101, 41102 on X; 41104 on Y.
        wait(&mut s, 41103, X, 5, P);
        wait(&mut s, 41101, X, 5, P);
        wait(&mut s, 41102, X, 5, P);
        wait(&mut s, 41104, Y, 5, P);
        run(&mut s, 41105);
        assert_eq!(s.futex_wake(X, 0, P), Ok(0), "a zero count wakes nobody");
        assert_eq!(s.futex_wake(X, 1, P), Ok(1));
        assert_eq!(
            s.task_status(41103),
            Some(TaskStatus::Runnable),
            "first arrival first"
        );
        assert!(parked(&s, 41101) && parked(&s, 41102));
        assert_eq!(s.futex_wake(X, 1, P), Ok(1));
        assert_eq!(
            s.task_status(41101),
            Some(TaskStatus::Runnable),
            "then the second"
        );
        assert!(parked(&s, 41102));
        assert_eq!(s.futex_wake(X, 9, P), Ok(1), "only as many as there are");
        assert!(parked(&s, 41104), "the other key is untouched");
        assert_eq!(occupied(&mut s), 1);
    }

    /// F12 — a full waiter store refuses before writing: the task is not left blocked and keeps
    /// its CPU (broad and split).
    #[test]
    fn f12_capacity_refusal_leaves_the_caller_running() {
        let mut s = Bootstrap::init().expect("init");
        let (a, _) = space(&mut s);
        task(&mut s, 41111, a);
        task(&mut s, 41112, a);
        private_page(&mut s, a, X);
        word(&mut s, 41111, X, 5);
        s.with_task_futex_mut(|_t, table| table.set_capacity_for_test(1));
        wait(&mut s, 41111, X, 5, P);
        run(&mut s, 41112);
        assert_eq!(
            s.futex_wait_current(X, 5, P),
            Err(KernelError::TaskTableFull)
        );
        assert_eq!(s.task_status(41112), Some(TaskStatus::Running));
        assert_eq!(s.current_tid(), Some(41112), "it keeps its CPU");
        assert_eq!(occupied(&mut s), 1);
        assert_eq!(s.futex_wake(X, 1, P), Ok(1), "the live waiter is intact");

        let k = leak(Bootstrap::init().expect("init"));
        let cpu = CpuId(0);
        let asid = split_caller(k, 41113, cpu, 5);
        k.with(|s| s.with_task_futex_mut(|_t, table| table.set_capacity_for_test(0)));
        assert_eq!(
            k.futex_wait_park_exact_split(cpu, 41113, asid, X, 5, P),
            FutexParkOutcome::CapacityExhausted
        );
        assert_eq!(k.with(|s| s.task_status(41113)), Some(TaskStatus::Running));
        assert_eq!(k.with(|s| s.current_tid_on_cpu(cpu)), Some(41113));
    }

    /// F13 — a wake from another CPU makes the exact waiter Runnable and enqueues it exactly once,
    /// on its home CPU; a second wake finds nobody.
    #[test]
    fn f13_a_remote_wake_enqueues_once_on_the_home_cpu() {
        let k = leak(Bootstrap::init().expect("init"));
        k.with(|s| s.bring_up_cpu(CpuId(1))).expect("cpu 1");
        let asid = split_caller(k, 41121, CpuId(0), 5);
        k.with(|s| s.set_task_home_cpu(41121, CpuId(0)))
            .expect("home");
        assert!(matches!(
            k.futex_wait_park_exact_split(CpuId(0), 41121, asid, X, 5, P),
            FutexParkOutcome::Parked { .. }
        ));
        assert_eq!(k.futex_wake_split_mut(CpuId(1), asid, X, 1, P), Ok(1));
        assert_eq!(k.futex_wake_split_mut(CpuId(1), asid, X, 1, P), Ok(0));
        let on = |cpu| {
            k.with(|s| {
                let mut v = alloc::vec::Vec::new();
                s.for_each_queued_on_for_test(cpu, |_, t| v.push(t.0));
                v
            })
        };
        assert_eq!(on(CpuId(0)).iter().filter(|t| **t == 41121).count(), 1);
        assert!(!on(CpuId(1)).contains(&41121));
        assert_eq!(k.with(|s| s.task_status(41121)), Some(TaskStatus::Runnable));
    }

    /// F14 — robust cleanup wakes in the EXITING task's namespace: its private key, and the shared
    /// key when an object backs the word; never an unrelated process's private waiter.
    #[test]
    fn f14_robust_cleanup_uses_the_exiting_tasks_keys() {
        let mut s = Bootstrap::init().expect("init");
        let (a, acap) = space(&mut s);
        let (b, bcap) = space(&mut s);
        let (c, _) = space(&mut s);
        task(&mut s, 41131, a); // the exiting owner
        task(&mut s, 41132, a); // its sibling, private
        task(&mut s, 41133, b); // another process sharing the object, at another VA
        task(&mut s, 41134, c); // an unrelated process, same VA, private
        task(&mut s, 41135, c); // the driver of the exit
        let mem = object_page(&mut s, acap, X);
        s.map_user_page_with_caps(bcap, mem, VirtAddr(Y as u64), PageFlags::USER_RW)
            .expect("share");
        private_page(&mut s, c, X);
        word(&mut s, 41131, X, 5);
        word(&mut s, 41133, Y, 5);
        word(&mut s, 41134, X, 5);
        s.set_robust_futex_head(41131, X, 1).expect("robust");
        wait(&mut s, 41132, X, 5, P);
        wait(&mut s, 41133, Y, 5, SH);
        wait(&mut s, 41134, X, 5, P);
        run(&mut s, 41135);
        s.exit_task(41131, 0)
            .expect("exit, driven from another process");
        assert_eq!(
            s.task_status(41132),
            Some(TaskStatus::Runnable),
            "the sibling"
        );
        assert_eq!(
            s.task_status(41133),
            Some(TaskStatus::Runnable),
            "the shared waiter"
        );
        assert!(parked(&s, 41134), "the unrelated process's waiter");
    }

    /// F15 (hosted half) — the ABI the callers encode is the ABI the kernel decodes: slot 0 the
    /// 4-byte-aligned word, slot 1 the value, slot 2 the flags with bit 0 = shared and every other
    /// bit reserved. The live half is the boot matrix.
    #[test]
    fn f15_the_encoded_abi_is_the_decoded_abi() {
        use crate::kernel::syscall::sched::decode_futex_args;
        use crate::kernel::syscall::{SYSCALL_FUTEX_WAIT_NR, SyscallError};
        let frame = |a: usize, v: usize, f: usize| {
            crate::kernel::trapframe::TrapFrame::new(SYSCALL_FUTEX_WAIT_NR, [a, v, f, 7, 7, 7])
        };
        let ok = decode_futex_args(&frame(0x1000, 3, 0)).expect("private");
        assert_eq!((ok.addr, ok.value, ok.kind), (0x1000, 3, P));
        assert_eq!(
            decode_futex_args(&frame(0x1004, 3, FUTEX_FLAG_SHARED)).map(|a| a.kind),
            Ok(SH)
        );
        for (a, v, f) in [
            (0x1002, 3, 0),                       // unaligned
            (0x1000, (u32::MAX as usize) + 1, 0), // value wider than 32 bits
            (0x1000, 3, 2),                       // a reserved flag bit
            (0x1000, 3, 1 << 63),                 // another
        ] {
            assert_eq!(
                decode_futex_args(&frame(a, v, f)),
                Err(SyscallError::InvalidArgs),
                "({a:#x}, {v:#x}, {f:#x})"
            );
        }
        const USER_RT: &str = include_str!("../../../crates/yarm-user-rt/src/lib.rs");
        assert!(
            USER_RT.contains("Self::Private => 0,") && USER_RT.contains("Self::Shared => 1,"),
            "user-rt encodes the flag the kernel decodes"
        );
        assert!(
            USER_RT.contains("[addr as usize, expected as usize, scope.flags(), 0, 0, 0]")
                && USER_RT.contains("[addr as usize, count as usize, scope.flags(), 0, 0, 0]"),
            "and in the slots the kernel reads"
        );
    }
}
