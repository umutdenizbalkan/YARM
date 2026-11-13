# FUTEX-TIME1 Addendum C: insert the scratch starvation reproductions (never committed).
import sys
p = sys.argv[1] + "/src/kernel/boot/tests.rs"; with_time = sys.argv[2] == "F"
src = open(p).read()
anchor = """            if s.enqueue_task(t).is_err() {
                s.exit_task(t, 0).expect("retire the refused filler");
                return n;
            }
            n += 1;
        }
    }
"""
assert src.count(anchor) == 1, "fill_queue anchor"
common = '''
    /// FUTEX-TIME1 Addendum C (scratch, never committed): an UNTIMED wake that takes the last
    /// free slot of a CPU whose current task is outside the queue leaves that CPU unable to
    /// rotate: every re-enqueue of the current task is refused, the timer preempt continues it,
    /// and the queued tasks (the woken waiter among them) are never dispatched.
    #[test]
    fn zz_ft1c_untimed_wake_into_the_last_slot_starves_the_queue() {
        let mut w = world(47000);
        let ws = w.park_n(1, SQ, 5);
        let wt = ws[0];
        let a = w.a;
        let n = fill_queue(&mut w.s, a, 47100);
        assert!(n > 2, "filled {n}");
        let cur = w.s.current_tid().expect("a current task");
        assert_eq!(w.s.futex_wake(SQ, 1, P), Ok(0), "a full queue refuses the wake (WAKE-CAP1)");
        let last = 47100 + n as u64 - 1;
        assert_eq!(w.s.withdraw_queued_tid_on(CpuId(0), last), crate::kernel::scheduler::WithdrawOutcome::Removed);
        w.s.set_task_status_for_test(last, TaskStatus::Blocked(WaitReason::Join(ThreadId(1))));
        assert_eq!(w.s.futex_wake(SQ, 1, P), Ok(1), "the wake takes the one free slot");
        assert!(queue(&w.s).contains(&wt));
        for _ in 0..1000 {
            assert_eq!(w.s.preempt_reenqueue_current_cpu(), None, "rotation refused");
            assert_eq!(w.s.current_tid(), Some(cur));
            let _ = w.s.on_preempt_current_cpu();
            assert_eq!(w.s.current_tid(), Some(cur), "the timer preempt continues the current task");
        }
        assert!(queue(&w.s).contains(&wt), "queued, never dispatched");
        // Escape: only a slot freed by someone else lets the CPU rotate again.
        assert_eq!(w.s.withdraw_queued_tid_on(CpuId(0), 47100), crate::kernel::scheduler::WithdrawOutcome::Removed);
        w.s.set_task_status_for_test(47100, TaskStatus::Blocked(WaitReason::Join(ThreadId(1))));
        assert_eq!(w.s.preempt_reenqueue_current_cpu(), Some(cur), "rotation resumes");
        std::println!("FT1C_REPRO untimed n={n} cur={cur} waiter={wt} result=starved_then_escaped");
    }
'''
src = src.replace(anchor, anchor + common)
if with_time:
    a2 = "        // ── Requeue ──────────────────────────────────────────────────────────────────────────\n\n        /// 15 (T4)"
    assert src.count(a2) == 1, "time1 anchor"
    timed = '''        /// FUTEX-TIME1 Addendum C (scratch, never committed): t14's placement into the one freed
        /// slot, then the same rotation probe — the live LT-x86_64-1 state.
        #[test]
        fn zz_ft1c_timeout_into_the_last_slot_starves_the_queue() {
            let mut w = world(71900);
            let t = tpark(&mut w, SQ, 5, P, 10);
            w.back();
            let a = w.a;
            let fillers = fill(&mut w.s, a, 76500);
            let k = leak(w.s);
            k.with(|s| drain_one(s, fillers[0]));
            assert_eq!(collect(k, 10).timed_out, 1);
            k.with(|s| {
                let cur = s.current_tid().expect("current");
                assert!(timed_out(s, t));
                for _ in 0..1000 {
                    assert_eq!(s.preempt_reenqueue_current_cpu(), None, "rotation refused");
                    let _ = s.on_preempt_current_cpu();
                    assert_eq!(s.current_tid(), Some(cur));
                }
                assert!(timed_out(s, t), "placed with TimedOut, never dispatched");
                drain_one(s, fillers[1]);
                assert_eq!(s.preempt_reenqueue_current_cpu(), Some(cur), "rotation resumes");
                std::println!("FT1C_REPRO timed cur={cur} waiter={t} result=starved_then_escaped");
            });
        }

'''
    src = src.replace(a2, timed + a2)
open(p, "w").write(src)
print("inserted", "common+timed" if with_time else "common")
