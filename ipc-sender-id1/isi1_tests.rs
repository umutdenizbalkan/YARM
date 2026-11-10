
/// IPC-SENDER-ID1 — endpoint sender-waiter identity across receive, timeout, exit and reap.
///
/// Every blocking send here is REAL (`ipc_send_with_deadline` by the current task), every
/// receive-side take is the production `ipc_recv_endpoint_take`, every timeout is the production
/// collect + drain, and every wake is one of the three production receiver-side seams. A
/// two-CPU interleaving is expressed by running those production phases in the order two CPUs
/// can run them (the timeout drain runs off the broad lock on whichever CPU's timer scans the
/// sender). Tests that inject state say so and are invariant tests, not reachability proofs.
mod ipc_sender_id1 {
    use super::*;
    use crate::kernel::boot::sender_waiter::{
        SendCycleEnding, claim_blocked_send_cycle_locked, remove_sender_waiter_for_cycle_locked,
        remove_sender_waiters_for_task_locked,
    };
    use crate::kernel::ipc::{SenderWakeTarget, ThreadId};
    use crate::kernel::scheduler::CpuId;
    use crate::kernel::task::{TaskStatus, WaitReason};
    use crate::kernel::vm::Asid;
    use crate::runtime::SharedKernel;

    const SENDER: u64 = 7;
    const PEER: u64 = 8;

    /// The three production receiver-side seams that wake a blocked sender.
    #[derive(Debug, Clone, Copy)]
    enum Seam {
        /// `KernelState::apply_blocked_sender_wake` — the full receive path.
        Broad,
        /// `KernelState::apply_split_sender_wake_plan` — the in-lock refill/handoff seam.
        InLockSplit,
        /// `SharedKernel::apply_split_sender_wake_plan_split` — the off-lock recv boundary.
        OffLock,
    }
    const SEAMS: [Seam; 3] = [Seam::Broad, Seam::InLockSplit, Seam::OffLock];

    fn wake(k: &SharedKernel, seam: Seam, cpu: CpuId, target: SenderWakeTarget) -> Result<(), KernelError> {
        match seam {
            Seam::Broad => k
                .with_cpu(cpu, |s| s.apply_blocked_sender_wake(Some(target)))
                .expect("cpu"),
            Seam::InLockSplit => k
                .with_cpu(cpu, |s| s.apply_split_sender_wake_plan(target))
                .expect("cpu"),
            Seam::OffLock => k.apply_split_sender_wake_plan_split(cpu, target),
        }
    }

    fn fresh() -> SharedKernel {
        for c in 0..2 {
            crate::kernel::boot::reset_ipc_timeout_scan_cursor(c);
            crate::kernel::boot::send_timeout_work_clear(c);
        }
        let k = SharedKernel::new(Bootstrap::init().expect("init"));
        k.with(|s| {
            s.set_timer_for_test(Timer::new(1));
            s.bring_up_cpu(CpuId(1)).expect("cpu1");
        });
        k
    }

    /// A user task with an ASID (as spawn provisioning gives every user task), joining process
    /// `pid` so threads of one process share its endpoint capabilities.
    fn task(k: &SharedKernel, tid: u64, pid: u64, asid: u16) {
        k.with(|s| {
            s.register_task_with_class_in_process(tid, TaskClass::App, pid)
                .expect("task");
            s.with_tcbs_mut(|tcbs| {
                let t = tcbs.iter_mut().flatten().find(|t| t.tid.0 == tid).unwrap();
                t.asid = Some(Asid(asid));
                Ok::<_, KernelError>(())
            })
            .unwrap();
        });
    }

    fn make_current(k: &SharedKernel, tid: u64) {
        k.with(|s| {
            if s.current_tid() == Some(tid) {
                return;
            }
            let _ = s.enqueue_current_cpu(tid);
            for _ in 0..64 {
                if s.current_tid() == Some(tid) {
                    return;
                }
                s.dispatch_next_task().expect("dispatch");
            }
            panic!("task {tid} never became current");
        });
    }

    /// A synchronous endpoint created by `owner` (so `owner`'s process holds the send cap).
    fn endpoint(k: &SharedKernel, owner: u64) -> (usize, CapId) {
        make_current(k, owner);
        k.with(|s| {
            let (idx, send, _recv) = s
                .create_endpoint_with_mode(1, EndpointMode::Synchronous)
                .expect("endpoint");
            (idx, send)
        })
    }

    /// A REAL blocking send by `tid` with a deadline. Returns the armed deadline.
    fn block_send(k: &SharedKernel, tid: u64, cap: CapId, body: &[u8], ticks: u64) -> u64 {
        make_current(k, tid);
        k.with(|s| {
            assert_eq!(
                s.ipc_send_with_deadline(cap, Message::new(tid, body).expect("msg"), ticks),
                Err(KernelError::WouldBlock)
            );
        });
        assert!(matches!(
            status(k, tid),
            Some(TaskStatus::Blocked(WaitReason::EndpointSend(_)))
        ));
        deadline(k, tid).expect("a deadline rode with the block")
    }

    /// The production receive-side take: the message and the exact cycle it names.
    fn take(k: &SharedKernel, ep: usize) -> (Message, SenderWakeTarget) {
        let (msg, target) = k.with(|s| s.ipc_recv_endpoint_take(ep)).expect("take");
        (msg.expect("a message"), target.expect("a blocked sender"))
    }

    /// The production send-timeout pipeline, on `cpu`.
    fn timeout(k: &SharedKernel, cpu: CpuId, now: u64) {
        k.collect_due_ipc_timeout_work(now, now, now, cpu);
        k.drain_send_timeout_post_work(cpu, now);
    }

    fn tcb<R>(k: &SharedKernel, tid: u64, f: impl Fn(&crate::kernel::task::ThreadControlBlock) -> R) -> Option<R> {
        k.with(|s| s.with_tcbs(|tcbs| tcbs.iter().flatten().find(|t| t.tid.0 == tid).map(&f)))
    }
    fn status(k: &SharedKernel, tid: u64) -> Option<TaskStatus> {
        tcb(k, tid, |t| t.status)
    }
    fn deadline(k: &SharedKernel, tid: u64) -> Option<u64> {
        tcb(k, tid, |t| t.ipc_timeout_deadline).flatten()
    }
    fn generation(k: &SharedKernel, tid: u64) -> u64 {
        tcb(k, tid, |t| t.blocked_send_generation).expect("task")
    }
    /// `(result, blocked_generation)` of the parked completion.
    fn completion(k: &SharedKernel, tid: u64) -> Option<(u64, u64)> {
        tcb(k, tid, |t| t.pending_syscall_completion.map(|c| (c.result, c.blocked_generation))).flatten()
    }
    fn waiters(k: &SharedKernel, ep: usize) -> alloc::vec::Vec<(u64, u64)> {
        k.with(|s| {
            s.with_ipc_state(|ipc| {
                ipc.endpoint_sender_waiters[ep]
                    .iter()
                    .map(|w| w.map_or((0, 0), |w| (w.tid.0, w.send_generation)))
                    .collect()
            })
        })
    }
    fn live_waiters(k: &SharedKernel, ep: usize) -> alloc::vec::Vec<(u64, u64)> {
        waiters(k, ep).into_iter().filter(|w| w.0 != 0).collect()
    }
    /// How many run queues hold `tid`, across both CPUs.
    fn queued(k: &SharedKernel, tid: u64) -> usize {
        k.with(|s| {
            let mut n = 0;
            for c in 0..2u8 {
                s.for_each_queued_on_for_test(CpuId(c), |_, t| n += usize::from(t.0 == tid));
            }
            n
        })
    }

    // ── 2 / 9 / 11: a stale target never touches a newer cycle (S2), on every seam ───────────

    /// S2 — the cycle a receiver took is ended by a timeout on another CPU; the sender runs and
    /// blocks again (a NEW cycle, on another endpoint). The receiver's wake for the OLD cycle then
    /// arrives. It must not wake, complete, disarm or strand the new cycle.
    #[test]
    fn s2_a_stale_receiver_wake_cannot_touch_the_senders_next_cycle() {
        for seam in SEAMS {
            let k = fresh();
            task(&k, SENDER, SENDER, 5);
            let (ep, cap) = endpoint(&k, SENDER);
            let (ep2, cap2) = endpoint(&k, SENDER);
            let d = block_send(&k, SENDER, cap, b"one", 1);
            let (_msg, old) = take(&k, ep);
            timeout(&k, CpuId(1), d + 1);
            assert_eq!(completion(&k, SENDER), Some((KernelState::SEND_COMPLETION_TIMED_OUT, 1)));
            // The sender resumes (consuming its result) and blocks again: cycle 2.
            k.with(|s| {
                s.with_tcbs_mut(|tcbs| {
                    crate::kernel::task::take_blocked_syscall_completion_for(
                        tcbs,
                        SENDER,
                        Some(Asid(5)),
                        crate::kernel::task::BlockedSyscallClass::IpcSend,
                    )
                })
            })
            .expect("the timed-out cycle's completion");
            let d2 = block_send(&k, SENDER, cap2, b"two", 100);
            assert_eq!(generation(&k, SENDER), 2);

            assert_eq!(wake(&k, seam, CpuId(0), old), Ok(()), "{seam:?}: refused, not an error");

            assert!(
                matches!(status(&k, SENDER), Some(TaskStatus::Blocked(WaitReason::EndpointSend(_)))),
                "{seam:?}: the new cycle is still blocked"
            );
            assert_eq!(deadline(&k, SENDER), Some(d2), "{seam:?}: its deadline is still armed");
            assert_eq!(completion(&k, SENDER), None, "{seam:?}: nothing was published for it");
            assert_eq!(live_waiters(&k, ep2), alloc::vec![(SENDER, 2)], "{seam:?}: its waiter stands");
            assert_eq!(queued(&k, SENDER), 0, "{seam:?}: and it was enqueued nowhere");
        }
    }

    // ── 3 / 12: timeout and receive competition (S3) ─────────────────────────────────────────

    /// S3 — the receiver claims first; a timeout on another CPU then runs before the receiver's
    /// wake. The timeout must be refused: one completion (success), one wake.
    #[test]
    fn s3_a_timeout_after_the_receivers_claim_cannot_overwrite_its_completion() {
        let k = fresh();
        task(&k, SENDER, SENDER, 5);
        let (ep, cap) = endpoint(&k, SENDER);
        let d = block_send(&k, SENDER, cap, b"one", 1);
        let (msg, target) = take(&k, ep);
        assert_eq!(msg.as_slice(), b"one");
        // The receiver-side seam, phase 1 (the claim) ...
        assert!(k.with(|s| s.publish_blocking_send_completion(target, KernelState::SEND_COMPLETION_OK)));
        // ... a timeout on CPU 1 ...
        timeout(&k, CpuId(1), d + 1);
        assert_eq!(
            completion(&k, SENDER),
            Some((KernelState::SEND_COMPLETION_OK, 1)),
            "the delivered message's success stands: the timeout could not claim the cycle"
        );
        assert_eq!(queued(&k, SENDER), 0, "and the timeout woke nothing");
        // ... and phase 2 (the wake).
        k.with(|s| s.wake_tid_to_runnable_for_test(ThreadId(SENDER))).expect("wake");
        assert_eq!(queued(&k, SENDER), 1, "woken exactly once");
    }

    /// S3 — the timeout claims first (on CPU 1) after the receiver took the waiter on CPU 0. The
    /// receiver's wake must wake nothing, publish nothing and FAIL nothing: its receive already
    /// holds the message. On every seam.
    #[test]
    fn s3_a_receiver_after_the_timeouts_claim_wakes_nothing_and_still_succeeds() {
        for seam in SEAMS {
            let k = fresh();
            task(&k, SENDER, SENDER, 5);
            let (ep, cap) = endpoint(&k, SENDER);
            let d = block_send(&k, SENDER, cap, b"one", 1);
            let (_msg, target) = take(&k, ep);
            timeout(&k, CpuId(1), d + 1);
            assert_eq!(queued(&k, SENDER), 1, "the timeout woke it");
            assert_eq!(wake(&k, seam, CpuId(0), target), Ok(()), "{seam:?}: no receive error");
            assert_eq!(queued(&k, SENDER), 1, "{seam:?}: no second wake, on any CPU");
            assert_eq!(
                completion(&k, SENDER),
                Some((KernelState::SEND_COMPLETION_TIMED_OUT, 1)),
                "{seam:?}: exactly one completion, the winner's"
            );
        }
    }

    // ── 8: duplicate claims ──────────────────────────────────────────────────────────────────

    /// Each cycle has exactly one terminal claim. A repeated receiver claim, a repeated timeout
    /// and a claim of either kind after the other are all refused with no mutation.
    #[test]
    fn exactly_one_claim_ends_a_cycle() {
        let k = fresh();
        task(&k, SENDER, SENDER, 5);
        let (ep, cap) = endpoint(&k, SENDER);
        let d = block_send(&k, SENDER, cap, b"one", 1);
        let (_m, target) = take(&k, ep);
        assert!(k.with(|s| s.publish_blocking_send_completion(target, KernelState::SEND_COMPLETION_OK)));
        let after = (status(&k, SENDER), completion(&k, SENDER), deadline(&k, SENDER));
        assert!(!k.with(|s| s.publish_blocking_send_completion(target, KernelState::SEND_COMPLETION_OK)));
        for _ in 0..2 {
            timeout(&k, CpuId(0), d + 1);
            timeout(&k, CpuId(1), d + 1);
        }
        assert_eq!((status(&k, SENDER), completion(&k, SENDER), deadline(&k, SENDER)), after);
        assert_eq!(queued(&k, SENDER), 0, "a refused claim enqueues nothing");
    }

    /// THE claim, at the owner: every refusal leaves the TCB byte-for-byte unchanged.
    #[test]
    fn the_claim_refuses_every_other_cycle_without_mutation() {
        let k = fresh();
        task(&k, SENDER, SENDER, 5);
        let (_ep, cap) = endpoint(&k, SENDER);
        let d = block_send(&k, SENDER, cap, b"one", 3);
        let exact = SenderWakeTarget { tid: ThreadId(SENDER), asid: Some(Asid(5)), send_generation: 1 };
        let refusals = [
            (SenderWakeTarget { asid: Some(Asid(6)), ..exact }, SendCycleEnding::Consumed { result: 0 }, "another incarnation"),
            (SenderWakeTarget { asid: None, ..exact }, SendCycleEnding::Consumed { result: 0 }, "an absent ASID is not authority"),
            (SenderWakeTarget { send_generation: 0, ..exact }, SendCycleEnding::Consumed { result: 0 }, "an older cycle"),
            (SenderWakeTarget { send_generation: 2, ..exact }, SendCycleEnding::Consumed { result: 0 }, "a newer cycle"),
            (SenderWakeTarget { tid: ThreadId(PEER), ..exact }, SendCycleEnding::Consumed { result: 0 }, "another task"),
            (exact, SendCycleEnding::TimedOut { deadline: d + 1, now: d + 5 }, "a deadline that is not the armed one"),
            (exact, SendCycleEnding::TimedOut { deadline: d, now: d - 1 }, "a deadline not yet due"),
        ];
        let snapshot = |k: &SharedKernel| {
            tcb(k, SENDER, |t| (t.status, t.ipc_timeout_deadline, t.ipc_timeout_fired, t.pending_syscall_completion))
        };
        let before = snapshot(&k);
        for (target, ending, why) in refusals {
            assert!(
                !k.with(|s| s.with_tcbs_mut(|tcbs| claim_blocked_send_cycle_locked(tcbs, target, ending))),
                "{why}"
            );
            assert_eq!(snapshot(&k), before, "{why}: nothing written");
        }
        assert!(k.with(|s| s.with_tcbs_mut(|tcbs| claim_blocked_send_cycle_locked(
            tcbs,
            exact,
            SendCycleEnding::TimedOut { deadline: d, now: d }
        ))));
        assert_eq!(status(&k, SENDER), Some(TaskStatus::Runnable));
        assert_eq!(deadline(&k, SENDER), None);
        assert_eq!(completion(&k, SENDER), Some((KernelState::SEND_COMPLETION_TIMED_OUT, 1)));
    }

    // ── 9: repeated send/block cycles (S6) ───────────────────────────────────────────────────

    /// S6 — many cycles in a row, each ended by a timeout after a receiver took its waiter, each
    /// followed by the delayed receiver wake of the PREVIOUS cycle. No delayed operation ever
    /// affects a newer cycle, and a legitimate receive of the last cycle still completes it.
    #[test]
    fn s6_delayed_wakes_of_older_cycles_never_reach_a_newer_one() {
        let k = fresh();
        task(&k, SENDER, SENDER, 5);
        let mut eps = alloc::vec::Vec::new();
        for _ in 0..6 {
            eps.push(endpoint(&k, SENDER));
        }
        let mut stale: alloc::vec::Vec<SenderWakeTarget> = alloc::vec::Vec::new();
        for (round, (ep, cap)) in eps.iter().copied().take(5).enumerate() {
            let d = block_send(&k, SENDER, cap, b"x", 1);
            let (_m, target) = take(&k, ep);
            assert_eq!(target.send_generation, round as u64 + 1);
            timeout(&k, CpuId((round % 2) as u8), d + 1);
            k.with(|s| {
                s.with_tcbs_mut(|tcbs| {
                    crate::kernel::task::take_blocked_syscall_completion_for(
                        tcbs,
                        SENDER,
                        Some(Asid(5)),
                        crate::kernel::task::BlockedSyscallClass::IpcSend,
                    )
                })
            });
            stale.push(target);
        }
        let (last_ep, last_cap) = eps[5];
        let d = block_send(&k, SENDER, last_cap, b"last", 1000);
        for (i, target) in stale.iter().enumerate() {
            assert_eq!(wake(&k, SEAMS[i % 3], CpuId((i % 2) as u8), *target), Ok(()));
        }
        assert!(matches!(status(&k, SENDER), Some(TaskStatus::Blocked(WaitReason::EndpointSend(_)))));
        assert_eq!(deadline(&k, SENDER), Some(d));
        assert_eq!(live_waiters(&k, last_ep), alloc::vec![(SENDER, 6)]);
        assert_eq!(queued(&k, SENDER), 0);
        let (m, target) = take(&k, last_ep);
        assert_eq!(m.as_slice(), b"last");
        assert_eq!(wake(&k, Seam::Broad, CpuId(0), target), Ok(()));
        assert_eq!(completion(&k, SENDER), Some((KernelState::SEND_COMPLETION_OK, 6)));
        assert_eq!(queued(&k, SENDER), 1);
    }

    // ── 10: queue order survives a removal ───────────────────────────────────────────────────

    /// Three senders park on one endpoint; the first times out; a fourth parks. A receiver must
    /// take them in arrival order — B, C, D — so the removal may not leave a hole that the
    /// newcomer fills ahead of the older waiters.
    #[test]
    fn arrival_order_survives_a_timed_out_waiter() {
        let k = fresh();
        const A: u64 = 20;
        const B: u64 = 21;
        const C: u64 = 22;
        const D: u64 = 23;
        for (i, t) in [A, B, C, D].into_iter().enumerate() {
            task(&k, t, t, 10 + i as u16);
        }
        let (ep, cap) = endpoint(&k, A);
        // Each sender holds its own delegated SEND capability to the one endpoint.
        let cap_of = |t: u64| {
            k.with(|s| s.grant_capability_task_to_task_with_rights(A, cap, t, crate::kernel::capabilities::CapRights::SEND))
                .expect("grant")
        };
        let (cb, cc, cd) = (cap_of(B), cap_of(C), cap_of(D));
        let da = block_send(&k, A, cap, b"a", 1);
        block_send(&k, B, cb, b"b", 1000);
        block_send(&k, C, cc, b"c", 1000);
        timeout(&k, CpuId(0), da + 1);
        assert!(!live_waiters(&k, ep).iter().any(|w| w.0 == A), "A's waiter was removed");
        assert_eq!(waiters(&k, ep)[0].0, B, "the queue stays dense");
        // A, woken by its timeout, parks elsewhere so D can run and send.
        let (_elsewhere, elsewhere_cap) = endpoint(&k, A);
        block_send(&k, A, elsewhere_cap, b"a2", 1000);
        block_send(&k, D, cd, b"d", 1000);
        let order: alloc::vec::Vec<alloc::vec::Vec<u8>> = (0..3)
            .map(|_| take(&k, ep).0.as_slice().to_vec())
            .collect();
        assert_eq!(order, alloc::vec![b"b".to_vec(), b"c".to_vec(), b"d".to_vec()]);
    }

    // ── 1 / 5 / 6: task-scoped and cycle-scoped removal at the owner ─────────────────────────

    /// INVARIANT TEST (state injected). The task-scoped removal takes EVERY waiter carrying the
    /// TID — whatever ASID or generation it recorded, since a TID names one TCB — and no other
    /// task's, compacts every queue it touched, and reports exactly the cap-bearing ones.
    #[test]
    fn the_task_scoped_removal_takes_every_waiter_of_that_tcb_and_nothing_else() {
        let k = fresh();
        let plain = Message::new(SENDER, b"p").expect("m");
        let carrying = Message::with_header(SENDER, 1, Message::FLAG_CAP_TRANSFER, Some(0x0001_0003), b"c")
            .expect("m");
        let w = |tid: u64, asid: u16, generation: u64, msg: Message| SenderWaiter {
            tid: ThreadId(tid),
            msg,
            asid: Some(Asid(asid)),
            send_generation: generation,
        };
        let mut orphaned: [Option<(SenderWaiter, usize)>; MAX_ENDPOINT_SENDER_WAITERS] = [None; MAX_ENDPOINT_SENDER_WAITERS];
        let (n, q0, q1) = k.with(|s| {
            s.with_ipc_state_mut(|ipc| {
                ipc.endpoint_sender_waiters[0][0] = Some(w(PEER, 9, 4, plain));
                ipc.endpoint_sender_waiters[0][1] = Some(w(SENDER, 5, 1, carrying));
                ipc.endpoint_sender_waiters[0][2] = Some(w(PEER + 1, 9, 2, plain));
                ipc.endpoint_sender_waiters[1][0] = Some(w(SENDER, 4, 3, plain));
                ipc.endpoint_sender_waiters[1][1] = Some(w(PEER, 9, 5, plain));
                let n = remove_sender_waiters_for_task_locked(ipc, ThreadId(SENDER), &mut orphaned);
                let ids = |q: &[Option<SenderWaiter>; MAX_ENDPOINT_SENDER_WAITERS]| {
                    q.iter().map(|w| w.map(|w| w.tid.0)).collect::<alloc::vec::Vec<_>>()
                };
                (n, ids(&ipc.endpoint_sender_waiters[0]), ids(&ipc.endpoint_sender_waiters[1]))
            })
        });
        assert_eq!(n, 1, "only the cap-bearing waiter is reported");
        assert_eq!(orphaned[0].map(|(w, ep)| (w.tid.0, w.send_generation, ep)), Some((SENDER, 1, 0)));
        assert_eq!(&q0[..3], &[Some(PEER), Some(PEER + 1), None], "dense, in order");
        assert_eq!(&q1[..2], &[Some(PEER), None], "a stale ASID of the same TCB goes too");
    }

    /// INVARIANT TEST (state injected). The cycle-scoped removal matches the whole
    /// `{tid, asid, send_generation}` triple, leaves everything else, and compacts.
    #[test]
    fn the_cycle_removal_matches_the_whole_triple() {
        let k = fresh();
        let m = Message::new(SENDER, b"p").expect("m");
        let exact = SenderWakeTarget { tid: ThreadId(SENDER), asid: Some(Asid(5)), send_generation: 3 };
        k.with(|s| {
            s.with_ipc_state_mut(|ipc| {
                ipc.endpoint_sender_waiters[2][0] = Some(SenderWaiter { tid: ThreadId(PEER), msg: m, asid: Some(Asid(9)), send_generation: 1 });
                ipc.endpoint_sender_waiters[2][1] = Some(SenderWaiter { tid: ThreadId(SENDER), msg: m, asid: Some(Asid(5)), send_generation: 3 });
                ipc.endpoint_sender_waiters[2][2] = Some(SenderWaiter { tid: ThreadId(PEER + 1), msg: m, asid: Some(Asid(9)), send_generation: 1 });
                for wrong in [
                    SenderWakeTarget { asid: Some(Asid(6)), ..exact },
                    SenderWakeTarget { asid: None, ..exact },
                    SenderWakeTarget { send_generation: 2, ..exact },
                    SenderWakeTarget { send_generation: 4, ..exact },
                ] {
                    assert!(remove_sender_waiter_for_cycle_locked(ipc, wrong).is_none(), "{wrong:?}");
                }
                let (removed, ep) = remove_sender_waiter_for_cycle_locked(ipc, exact).expect("exact");
                assert_eq!((removed.wake_target(), ep), (exact, 2));
                let ids: alloc::vec::Vec<_> = ipc.endpoint_sender_waiters[2].iter().map(|w| w.map(|w| w.tid.0)).collect();
                assert_eq!(&ids[..3], &[Some(PEER), Some(PEER + 1), None]);
                assert!(remove_sender_waiter_for_cycle_locked(ipc, exact).is_none(), "once");
            })
        });
    }

    // ── 1 / 5 (S1): a TID is never recycled, so exit/reap by TID is exact ─────────────────────

    /// S1 — registration cannot recycle a TID: registering a TID that a dead TCB still carries is
    /// a no-op, and the only production writer that removes a TCB from the table is the
    /// reservation owner (a reservation or an undone thread spawn — never a task that ran).
    /// So no replacement incarnation can ever share a dying task's numeric TID, and the
    /// task-scoped removal's TID predicate names exactly that TCB.
    #[test]
    fn s1_a_tid_is_never_recycled_so_task_scoped_removal_is_exact() {
        let k = fresh();
        task(&k, SENDER, SENDER, 5);
        let (ep, cap) = endpoint(&k, SENDER);
        block_send(&k, SENDER, cap, b"x", 1000);
        let tcbs_before = k.with(|s| s.with_tcbs(|t| t.iter().flatten().count()));
        // Death clears the waiter by TID ...
        k.with(|s| s.mark_task_dead(SENDER)).expect("dead");
        assert!(live_waiters(&k, ep).is_empty());
        // ... and the TID cannot come back as another task.
        k.with(|s| s.register_task(SENDER)).expect("a no-op, not an error");
        assert_eq!(status(&k, SENDER), Some(TaskStatus::Dead), "the dead TCB keeps its TID");
        assert_eq!(k.with(|s| s.with_tcbs(|t| t.iter().flatten().count())), tcbs_before);
        // Structural half: one TCB-slot remover, and it is the reservation owner.
        let removers: alloc::vec::Vec<_> = crate::kernel::boot::tests::stage199d_wa2a_ownership_boundary::production_sources()
            .into_iter()
            .filter(|(_, src)| src.contains("tcbs[index] = None;") || src.contains("tcbs[idx] = None;"))
            .map(|(rel, _)| rel)
            .collect();
        assert_eq!(removers, alloc::vec![alloc::string::String::from("src/kernel/spawn_reservation.rs")]);
    }

    // ── 4 (S4): a sender that died after its waiter was taken ────────────────────────────────

    /// S4 — INVARIANT TEST: a blocked sender cannot exit itself and no production path kills a
    /// blocked task, so this injects the death. The receiver already holds the message; its
    /// wake of a dead sender must wake nothing, publish nothing and not fail the receive.
    #[test]
    fn s4_a_receivers_wake_of_a_dead_sender_is_refused_quietly() {
        for seam in SEAMS {
            let k = fresh();
            task(&k, SENDER, SENDER, 5);
            let (ep, cap) = endpoint(&k, SENDER);
            block_send(&k, SENDER, cap, b"x", 1000);
            let (_m, target) = take(&k, ep);
            k.with(|s| s.mark_task_dead(SENDER)).expect("dead");
            assert_eq!(wake(&k, seam, CpuId(0), target), Ok(()), "{seam:?}");
            assert_eq!(status(&k, SENDER), Some(TaskStatus::Dead), "{seam:?}");
            assert_eq!(completion(&k, SENDER), None, "{seam:?}");
            assert_eq!(queued(&k, SENDER), 0, "{seam:?}");
        }
    }

    // ── an ASID-less sender: woken as before, never by another incarnation's target ──────────

    /// An ASID-less sender (a kernel sender) is a cycle with `asid: None`: its own target wakes it
    /// without a completion, exactly as before U6; an ASID-bearing target never reaches it, and
    /// its target never reaches an ASID-bearing task.
    #[test]
    fn an_asid_less_cycle_is_its_own_and_nobody_elses() {
        let k = fresh();
        k.with(|s| s.register_task(SENDER)).expect("task");
        let (ep, cap) = endpoint(&k, SENDER);
        make_current(&k, SENDER);
        k.with(|s| {
            assert_eq!(
                s.ipc_send_with_deadline(cap, Message::new(SENDER, b"k").expect("m"), 1000),
                Err(KernelError::WouldBlock)
            )
        });
        let (_m, target) = take(&k, ep);
        assert_eq!(target.asid, None);
        assert_eq!(wake(&k, Seam::Broad, CpuId(0), SenderWakeTarget { asid: Some(Asid(1)), ..target }), Ok(()));
        assert!(matches!(status(&k, SENDER), Some(TaskStatus::Blocked(_))), "not by an ASID-bearing target");
        assert_eq!(wake(&k, Seam::Broad, CpuId(0), target), Ok(()));
        assert_eq!(status(&k, SENDER), Some(TaskStatus::Runnable));
        assert_eq!(completion(&k, SENDER), None, "no incarnation, no completion");
        assert_eq!(queued(&k, SENDER), 1);
    }
}
