#!/usr/bin/env python3
"""SEND-TIMEOUT-ARB1: re-derive the guards and tests that pinned the old shape (run on the
rustfmt'ed tree). Usage: sta1_testfix.py <wt>"""
import pathlib, sys
wt = pathlib.Path(sys.argv[1]); T = wt / "src/kernel/boot/tests.rs"; s = T.read_text()
def patch(old, new, count=1):
    global s
    assert s.count(old) == count, (old[:100], s.count(old))
    s = s.replace(old, new)
# ISI1's claim owner test: the deadline is no longer the claim's concern — the drain checks it
# before withdrawing the waiter (R6 pins that refusal with no mutation). The TimedOut ending is
# refused, like Consumed, for any other cycle.
patch('''            (
                exact,
                SendCycleEnding::TimedOut {
                    deadline: d + 1,
                    now: d + 5,
                },
                "a deadline that is not the armed one",
            ),
            (
                exact,
                SendCycleEnding::TimedOut {
                    deadline: d,
                    now: d - 1,
                },
                "a deadline not yet due",
            ),
        ];''', '''            // SEND-TIMEOUT-ARB1: the deadline moved out of the claim — the drain refuses work
            // that is not yet due BEFORE withdrawing the waiter (`r6_...`), and a withdrawal is
            // what makes the TimedOut claim the timeout's. The ending is still refused for any
            // other cycle.
            (
                SenderWakeTarget {
                    asid: Some(Asid(6)),
                    ..exact
                },
                SendCycleEnding::TimedOut,
                "a timeout for another incarnation",
            ),
            (
                SenderWakeTarget {
                    send_generation: 2,
                    ..exact
                },
                SendCycleEnding::TimedOut,
                "a timeout for a newer cycle",
            ),
        ];''')
patch('''                SendCycleEnding::TimedOut {
                    deadline: d,
                    now: d
                }
            )))''', '''                SendCycleEnding::TimedOut
            )))''')
patch('''        let d = block_send(&k, SENDER, cap, b"one", 3);
        let exact = SenderWakeTarget {''', '''        let _d = block_send(&k, SENDER, cap, b"one", 3);
        let exact = SenderWakeTarget {''')

# ISI1 S2 / S3b / S6 manufactured their stale target by letting a timeout end the cycle AFTER a
# receiver took its waiter — the very disagreement SEND-TIMEOUT-ARB1 removes (that timeout now
# finds no waiter and refuses). A receiver holding the waiter owns the cycle, so a stale target now
# arises only by delivering the same target again later; the protections are unchanged.
patch('''            let d = block_send(&k, SENDER, cap, b"one", 1);
            let (_msg, old) = take(&k, ep);
            timeout(&k, CpuId(1), d + 1);
            assert_eq!(
                completion(&k, SENDER),
                Some((KernelState::SEND_COMPLETION_TIMED_OUT, 1))
            );''', '''            let d = block_send(&k, SENDER, cap, b"one", 1);
            let (_msg, old) = take(&k, ep);
            // SEND-TIMEOUT-ARB1: the receiver holding the waiter owns cycle 1 — a timeout now
            // refuses — so cycle 1 ends through the receiver's own claim, and the stale target is
            // that same target delivered again after the sender re-blocked.
            timeout(&k, CpuId(1), d + 1);
            assert_eq!(wake(&k, seam, CpuId(0), old), Ok(()));
            assert_eq!(
                completion(&k, SENDER),
                Some((KernelState::SEND_COMPLETION_OK, 1))
            );''')
patch('''            .expect("the timed-out cycle's completion");''', '''            .expect("the first cycle's completion");''')
patch('''    /// S3 — the timeout claims first (on CPU 1) after the receiver took the waiter on CPU 0. The
    /// receiver's wake must wake nothing, publish nothing and FAIL nothing: its receive already
    /// holds the message. On every seam.''', '''    /// S3 — the receiver took the waiter on CPU 0 and a timeout runs on CPU 1. SEND-TIMEOUT-ARB1:
    /// the timeout finds no waiter and refuses, so the receiver's claim ends the cycle with
    /// success; a second delivery of the same target must then wake nothing, publish nothing and
    /// FAIL nothing — the receive already holds the message. On every seam.''')
patch('''            let (_msg, target) = take(&k, ep);
            timeout(&k, CpuId(1), d + 1);
            assert_eq!(queued(&k, SENDER), 1, "the timeout woke it");''', '''            let (_msg, target) = take(&k, ep);
            timeout(&k, CpuId(1), d + 1);
            assert_eq!(queued(&k, SENDER), 0, "the timeout found no waiter and woke nothing");
            assert_eq!(wake(&k, seam, CpuId(0), target), Ok(()));
            assert_eq!(queued(&k, SENDER), 1, "the receiver woke it");''')
patch('''            assert_eq!(
                completion(&k, SENDER),
                Some((KernelState::SEND_COMPLETION_TIMED_OUT, 1)),
                "{seam:?}: exactly one completion, the winner's"
            );''', '''            assert_eq!(
                completion(&k, SENDER),
                Some((KernelState::SEND_COMPLETION_OK, 1)),
                "{seam:?}: exactly one completion, the winner's"
            );''')
patch('''    /// S6 — many cycles in a row, each ended by a timeout after a receiver took its waiter, each
    /// followed by the delayed receiver wake of the PREVIOUS cycle.''', '''    /// S6 — many cycles in a row, each taken by a receiver while a timeout also runs (and, since
    /// SEND-TIMEOUT-ARB1, refuses), each ended by the receiver's claim, each followed later by a
    /// delayed re-delivery of the PREVIOUS cycle's target.''')
patch('''            assert_eq!(target.send_generation, round as u64 + 1);
            timeout(&k, CpuId((round % 2) as u8), d + 1);''', '''            assert_eq!(target.send_generation, round as u64 + 1);
            timeout(&k, CpuId((round % 2) as u8), d + 1);
            // the receiver's own wake runs on its CPU (0); the delayed re-deliveries below
            // still alternate CPUs
            assert_eq!(wake(&k, SEAMS[round % 3], CpuId(0), target), Ok(()));''')
# U6: the in-lock route now reports the park as a typed outcome
patch('''            assert_eq!(
                produced,
                Err(KernelError::WouldBlock),
                "…and blocked in-lock instead"
            );''', '''            // SEND-TIMEOUT-ARB1: the park is reported as a typed outcome (it was
            // `Err(WouldBlock)`), so the syscall knows the waiter owns the transfer envelope.
            assert_eq!(
                produced,
                Ok(BlockingSendProducerOutcome::BlockedInLock),
                "…and blocked in-lock instead"
            );''')
# U6: endpoint destruction now ends the cycles it removes, with an error — never success
patch('''            assert!(sender_waiters(&k, eidx).is_empty(), "waiters are wiped");
            assert!(
                parked_completion(&k, SENDER).is_none(),
                "no completion is fabricated for an undelivered message"
            );''', '''            assert!(sender_waiters(&k, eidx).is_empty(), "waiters are wiped");
            // SEND-TIMEOUT-ARB1: removing the waiter makes the destruction the owner of the
            // cycle, so it ends it — with the endpoint-gone error, never a fabricated success.
            let done = parked_completion(&k, SENDER).expect("the cycle is ended");
            assert_eq!(
                done.result,
                crate::kernel::syscall::SyscallError::WrongObject.code() as u64,
                "no success is fabricated for an undelivered message"
            );''')
patch('''                assert!(
                    parked_completion(&f.k, SENDER).is_none(),
                    "no success is fabricated for an undelivered message"
                );''', '''                // SEND-TIMEOUT-ARB1: the destruction ends the cycle it removed, with the
                // endpoint-gone error — never a fabricated success.
                assert_eq!(
                    parked_completion(&f.k, SENDER).map(|c| c.result),
                    Some(crate::kernel::syscall::SyscallError::WrongObject.code() as u64),
                    "no success is fabricated for an undelivered message"
                );''')
# Stage 169's source window: the orchestrator gained the refused-publication unwind
patch('''        let region = &IPC_STATE_SRC[block_idx..block_idx + 4600];''', '''        // SEND-TIMEOUT-ARB1 widened it again (4600 -> 7600): Phase C gained the refused-
        // publication unwind between the publish and the deferral arming.
        let region = &IPC_STATE_SRC[block_idx..block_idx + 7600];''')
# U7's nesting guard: the drain now takes THREE sequential claims (the read-only rank-2 precheck,
# the rank-3 withdrawal, the rank-2 claim) — still never nested.
patch('''        assert_eq!(
            open.len(),
            2,
            "exactly the rank-2 claim and the rank-3 claim"
        );''', '''        // SEND-TIMEOUT-ARB1: three sequential claims — the read-only rank-2 precheck, the rank-3
        // withdrawal (the arbitration point) and the rank-2 claim — none nested in another.
        assert_eq!(
            open.len(),
            3,
            "exactly the rank-2 precheck, the rank-3 withdrawal and the rank-2 claim"
        );''')
T.write_text(s)
print("sta1 test re-derivation applied")
