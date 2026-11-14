import sys
p = sys.argv[1] + "/src/kernel/boot/tests.rs"; src = open(p).read()
anchor = """            if s.enqueue_task(t).is_err() {
                s.exit_task(t, 0).expect("retire the refused filler");
                return n;
            }
            n += 1;
        }
    }
"""
assert src.count(anchor) == 1
test = '''
    /// SCHED-TIME-FOUNDATION1 A (SCHED-FULL-ROT1) — the FUTEX-TIME1 live defect, deterministically:
    /// an UNTIMED wake takes the last free slot of a CPU whose current task is outside the queue.
    /// The CPU must keep rotating: the current task returns to its tail, and the woken waiter is
    /// dispatched within one pass over the queue. (On `8062c216` every rotation was refused.)
    #[test]
    fn sfr1_k01_a_wake_into_the_last_slot_never_stops_rotation() {
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
        assert_eq!(w.s.preempt_reenqueue_current_cpu(), Some(cur), "the current task rotates");
        assert_eq!(queue(&w.s).last(), Some(&cur), "at its tail");
        assert_eq!(queue(&w.s).iter().filter(|&&t| t == cur).count(), 1);
        let mut ran_waiter = false;
        for _ in 0..(n + 2) {
            let next = w.s.dispatch_next_current_cpu();
            if next == Some(wt) {
                ran_waiter = true;
                break;
            }
            assert!(w.s.preempt_reenqueue_current_cpu().is_some(), "rotation never refused");
        }
        assert!(ran_waiter, "the woken waiter was dispatched");
    }
'''
src = src.replace(anchor, anchor + test)
open(p, "w").write(src); print("inserted")
