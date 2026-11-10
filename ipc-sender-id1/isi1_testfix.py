#!/usr/bin/env python3
"""IPC-SENDER-ID1: re-derive the guards and tests that pinned the old shape. Usage: <wt>"""
import pathlib, sys
wt = pathlib.Path(sys.argv[1]); T = wt / "src/kernel/boot/tests.rs"
s = T.read_text()
def patch(old, new, count=1):
    global s
    assert s.count(old) == count, (old[:100], s.count(old))
    s = s.replace(old, new)

# ── census A: per-file counts ────────────────────────────────────────────────────────────────
patch('''            ("src/kernel/boot/scheduler_state.rs", 1),
            // U9-SPAWN1 SP-2: `spawn_user_thread`'s Runnable write moved''', '''            ("src/kernel/boot/scheduler_state.rs", 1),
            // IPC-SENDER-ID1: the blocking-send timeout's `Blocked(EndpointSend) -> Runnable`
            // write moved out of `drain_send_timeout_post_work` into THE claim of a blocking-send
            // cycle, `sender_waiter::claim_blocked_send_cycle_locked`, which the receiver-side
            // publication now makes too. It moved; it did not multiply (runtime.rs 13 -> 12).
            ("src/kernel/boot/sender_waiter.rs", 1),
            // U9-SPAWN1 SP-2: `spawn_user_thread`'s Runnable write moved''')
patch('''            // off-lock twin of that owner, needed because off-lock code can never obtain a
            // `&mut KernelState`; it applies the identical three-part proof.
            ("src/runtime.rs", 13),''', '''            // off-lock twin of that owner, needed because off-lock code can never obtain a
            // `&mut KernelState`; it applies the identical three-part proof.
            // IPC-SENDER-ID1: 13 -> 12. `drain_send_timeout_post_work`'s write moved into
            // `sender_waiter::claim_blocked_send_cycle_locked` (counted there).
            ("src/runtime.rs", 12),''')

# ── census B: classification ────────────────────────────────────────────────────────────────
patch('''        // U7 (199E): the blocking-SEND timeout wake. It is proven unable to act on a
        // `Blocked(EndpointReceive)` task by a precondition no caller can bypass — the rank-2
        // claim refuses anything whose status is not `Blocked(EndpointSend(_))`, and refuses
        // before any mutation.
        (
            "src/runtime.rs",
            "drain_send_timeout_post_work",
            1,
            Verdict::Cannot,
        ),''', '''        // U7 (199E): the blocking-SEND timeout wake. It is proven unable to act on a
        // `Blocked(EndpointReceive)` task by a precondition no caller can bypass — the rank-2
        // claim refuses anything whose status is not `Blocked(EndpointSend(_))`, and refuses
        // before any mutation.
        // IPC-SENDER-ID1: the write moved, unchanged in class, into THE claim of a blocking-send
        // cycle, which both the timeout drain and the receiver-side publication now make. The
        // same precondition refuses before any mutation, and a target can only be a cycle a
        // sender waiter recorded.
        (
            "src/kernel/boot/sender_waiter.rs",
            "claim_blocked_send_cycle_locked",
            1,
            Verdict::Cannot,
        ),''')

# ── census C: fingerprints (source order: sender_waiter.rs sorts between scheduler_state.rs and
#    spawn_thread_core.rs) ───────────────────────────────────────────────────────────────────
patch('''        // U7 (199E): the blocking-SEND timeout wake, moved off the broad lock. The previous
        // line is the close of the `if let Some(asid)` arm that parks the `TimedOut`
        // completion, which is the whole point of the fingerprint: the completion is published
        // in the SAME rank-2 acquisition, immediately before the status write.
        (
            "src/runtime.rs",
            "drain_send_timeout_post_work",
            "tcb.status",
            "TaskStatus::Runnable",
            "}",
            "tcb.ipc_timeout_deadline = None;",
        ),
''', '''        // U7 (199E): the blocking-SEND timeout wake, moved off the broad lock.
        // IPC-SENDER-ID1 moved this row to `sender_waiter::claim_blocked_send_cycle_locked` (see
        // below, in that file's source position).
''')
patch('''        (
            "src/kernel/boot/scheduler_state.rs",
            "apply_cross_cpu_wake_task",
            "tcb.status",
            "TaskStatus::Runnable",
            "TaskStatus::Blocked(_) => {",
            "Ok(CrossCpuWakeApplyResult::Applied)",
        ),
''', '''        (
            "src/kernel/boot/scheduler_state.rs",
            "apply_cross_cpu_wake_task",
            "tcb.status",
            "TaskStatus::Runnable",
            "TaskStatus::Blocked(_) => {",
            "Ok(CrossCpuWakeApplyResult::Applied)",
        ),
        // IPC-SENDER-ID1: THE claim of a blocking-send cycle (timeout drain and receiver-side
        // publication). The previous line is the close of the `if let Some(asid)` arm that parks
        // the cycle's completion: it is published in the SAME rank-2 acquisition, immediately
        // before the status write — the fingerprint U7 pinned on the drain, unchanged.
        (
            "src/kernel/boot/sender_waiter.rs",
            "claim_blocked_send_cycle_locked",
            "tcb.status",
            "TaskStatus::Runnable",
            "}",
            "tcb.ipc_timeout_deadline = None;",
        ),
''')

# ── census D: the CANNOT proof moves with its row ───────────────────────────────────────────
patch('''            (
                "runtime.rs",
                "drain_send_timeout_post_work",
                "if !matches!(tcb.status, TaskStatus::Blocked(WaitReason::EndpointSend(_))) {",
                "while let Some(work) = crate::kernel::boot::send_timeout_work_drain_next(cpu_idx)",
            ),''', '''            // IPC-SENDER-ID1: the row moved into THE claim. The guard is unchanged; the closure
            // fact is that every target names a recorded blocking-send cycle — the timeout drain
            // builds it from collector work for a task that was `Blocked(EndpointSend)`, and the
            // receiver side only ever has `SenderWaiter::wake_target()`.
            (
                "sender_waiter.rs",
                "claim_blocked_send_cycle_locked",
                "if !matches!(tcb.status, TaskStatus::Blocked(WaitReason::EndpointSend(_))) {",
                "target: SenderWakeTarget,",
            ),''')
patch('''                "exit_claim.rs" => EXIT_CLAIM,
                "runtime.rs" => RUNTIME,
                other => panic!("unknown census source {other}"),''', '''                "exit_claim.rs" => EXIT_CLAIM,
                "runtime.rs" => RUNTIME,
                "sender_waiter.rs" => include_str!("sender_waiter.rs"),
                other => panic!("unknown census source {other}"),''')

# ── U7 source-order test: the claim precedes the settle and the enqueue; inside the claim the
#    completion precedes the status write ───────────────────────────────────────────────────
patch('''        let publish = body
            .find("tcb.pending_syscall_completion =")
            .expect("the publication");
        let runnable = body
            .find("tcb.status = TaskStatus::Runnable;")
            .expect("the wake");
        let settle = body
            .find("self.settle_blocked_send_envelope_split(")
            .expect("the envelope settle");
        let enqueue = body
            .find("self.enqueue_reply_timeout_wake_split(work.tid);")
            .expect("the enqueue");
        assert!(publish < runnable, "publish precedes the status transition");
        assert!(
            runnable < settle && settle < enqueue,
            "the envelope is settled before the sender can be dispatched"
        );''', '''        // IPC-SENDER-ID1: the publication and the status transition are THE claim of the
        // cycle (`sender_waiter`), made in one rank-2 acquisition; the drain calls it first.
        let claim = include_str!("sender_waiter.rs")
            .split("pub(crate) fn claim_blocked_send_cycle_locked(")
            .nth(1)
            .expect("the claim")
            .split("\\n}\\n")
            .next()
            .expect("its body");
        let publish = claim
            .find("tcb.pending_syscall_completion =")
            .expect("the publication");
        let runnable = claim
            .find("tcb.status = TaskStatus::Runnable;")
            .expect("the wake");
        assert!(publish < runnable, "publish precedes the status transition");
        let runnable = body
            .find("claim_blocked_send_cycle_locked(")
            .expect("the drain makes the claim");
        let settle = body
            .find("self.settle_blocked_send_envelope_split(")
            .expect("the envelope settle");
        let enqueue = body
            .find("self.enqueue_reply_timeout_wake_split(work.tid);")
            .expect("the enqueue");
        assert!(
            runnable < settle && settle < enqueue,
            "the envelope is settled before the sender can be dispatched"
        );''')

# ── U3 ordinary-cap sender wake: the fixture's "blocked sender" is now really blocked in a send
#    cycle. Its target is the ASID-less cycle `{SENDER, None, 0}`; the old fixture parked the
#    "sender" in `Blocked(EndpointReceive)`, which a sender wake must never touch. ───────────
a = s.index("mod u3_ordinary_cap_sender_wake {")
b = s.index("\nmod ", a + 10)
u3 = s[a:b]
n = u3.count("TaskStatus::Blocked(WaitReason::EndpointReceive(CapId(1)))")
assert n >= 6, n
u3 = u3.replace("TaskStatus::Blocked(WaitReason::EndpointReceive(CapId(1)))",
                "TaskStatus::Blocked(WaitReason::EndpointSend(CapId(1)))")
old_missing = '''        candidate(&new, CpuId(0), ThreadId(SENDER));
        assert_eq!(obs_old, observe(&new));
        assert_eq!(
            new.test_apply_split_sender_wake_plan_split(CpuId(0), ThreadId(SENDER)),
            Err(crate::kernel::boot::types::KernelError::TaskMissing)
        );
    }'''
assert u3.count(old_missing) == 1
u3 = u3.replace(old_missing, '''        candidate(&new, CpuId(0), ThreadId(SENDER));
        assert_eq!(obs_old, observe(&new));
        // IPC-SENDER-ID1: a missing sender is a refused claim. It wakes nothing and is not an
        // error — the receiver already holds the message, and failing its receive here would
        // drop it. (The retired body returned `TaskMissing` after the message was taken.)
        let before = observe(&new);
        assert_eq!(
            new.test_apply_split_sender_wake_plan_split(CpuId(0), ThreadId(SENDER)),
            Ok(())
        );
        assert_eq!(observe(&new), before, "nothing moved");
    }

    /// IPC-SENDER-ID1: a sender wake names ONE blocking-send cycle. A task that is not blocked in
    /// that cycle — blocked in a receive, or blocked in a different send generation — is not
    /// woken, its deadline is not disarmed, and nothing is enqueued.
    #[test]
    fn a_task_not_blocked_in_the_named_send_cycle_is_never_woken() {
        for (label, status, generation) in [
            (
                "blocked in a receive",
                TaskStatus::Blocked(WaitReason::EndpointReceive(CapId(1))),
                0,
            ),
            (
                "blocked in a newer send cycle",
                TaskStatus::Blocked(WaitReason::EndpointSend(CapId(1))),
                1,
            ),
        ] {
            let (k, ..) = fixture(status, None);
            k.with(|s| {
                s.with_tcbs_mut(|tcbs| {
                    for tcb in tcbs.iter_mut().flatten() {
                        if tcb.tid.0 == SENDER {
                            tcb.blocked_send_generation = generation;
                        }
                    }
                })
            });
            let before = observe(&k);
            assert_eq!(
                k.test_apply_split_sender_wake_plan_split(CpuId(0), ThreadId(SENDER)),
                Ok(()),
                "{label}"
            );
            assert_eq!(observe(&k), before, "{label}: nothing may move");
            let _ = k.with_cpu(CpuId(0), |kernel| {
                kernel.apply_split_sender_wake_plan(crate::kernel::ipc::SenderWakeTarget {
                    tid: ThreadId(SENDER),
                    asid: None,
                    send_generation: 0,
                })
            });
            assert_eq!(observe(&k), before, "{label}: the in-lock seam refuses identically");
        }
    }''')
s = s[:a] + u3 + s[b:]
T.write_text(s)
print("isi1 test re-derivation applied")
