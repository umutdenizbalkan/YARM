
    /// SEND-TIMEOUT-ARB1 — exact arbitration between blocked-send delivery, timeout and
    /// publication.
    ///
    /// The contract under test: a blocking-send cycle's message is consumed (or made deliverable)
    /// only by the receiver that wins the cycle, and a timeout wins only a cycle whose message it
    /// withdrew — so the sender's result always agrees with what happened to its message. Every
    /// interleaving is the production phases run in an order two CPUs can run them; the two
    /// windows a production function does not return across (the broad block's Phase B→C and the
    /// timeout drain's two claims) are entered through test-only phase hooks.
    mod send_timeout_arb1 {
        use super::*;
        use crate::kernel::boot::sender_waiter::send_arb_phase_hook as hook;

        fn leak(k: SharedKernel) -> &'static SharedKernel {
            alloc::boxed::Box::leak(alloc::boxed::Box::new(k))
        }

        /// The sender resumes: it consumes its parked completion, exactly as the resume boundary.
        fn resume(k: &SharedKernel, tid: u64, asid: u16) -> Option<u64> {
            k.with(|s| {
                s.with_tcbs_mut(|tcbs| {
                    crate::kernel::task::take_blocked_syscall_completion_for(
                        tcbs,
                        tid,
                        Some(Asid(asid)),
                        crate::kernel::task::BlockedSyscallClass::IpcSend,
                    )
                })
            })
            .map(|c| c.result)
        }

        /// A waiter exists for `tid` while `tid` is not blocked in a send: a deliverable message
        /// attached to a sender whose cycle is already over.
        fn orphan_waiters(k: &SharedKernel, tid: u64) -> usize {
            let blocked =
                matches!(status(k, tid), Some(TaskStatus::Blocked(WaitReason::EndpointSend(_))));
            let n = k.with(|s| {
                s.with_ipc_state(|ipc| {
                    ipc.endpoint_sender_waiters
                        .iter()
                        .flatten()
                        .flatten()
                        .filter(|w| w.tid.0 == tid)
                        .count()
                })
            });
            if blocked { 0 } else { n }
        }

        // ── R1 / R4: the receiver took the message; a timeout lands before its claim ─────────

        /// R1 — the receiver removed the waiter (and with it the message); before its claim, the
        /// timeout drain runs on another CPU. The cycle belongs to the receiver: the sender must
        /// see success, on every receiver seam.
        #[test]
        fn r1_a_timeout_after_the_take_cannot_report_a_delivered_message_as_timed_out() {
            for seam in SEAMS {
                let k = fresh();
                task(&k, SENDER, SENDER, 5);
                let (ep, cap) = endpoint(&k, SENDER);
                let d = block_send(&k, SENDER, cap, b"one", 1);
                let (msg, target) = take(&k, ep);
                assert_eq!(msg.as_slice(), b"one", "the receiver holds the message");
                timeout(&k, CpuId(1), d + 1);
                assert_eq!(wake(&k, seam, CpuId(0), target), Ok(()), "{seam:?}");
                assert_eq!(
                    completion(&k, SENDER),
                    Some((KernelState::SEND_COMPLETION_OK, 1)),
                    "{seam:?}: a delivered message is a successful send"
                );
                assert_eq!(queued(&k, SENDER), 1, "{seam:?}: woken exactly once");
                assert_eq!(deadline(&k, SENDER), None, "{seam:?}: the deadline is disarmed");
            }
        }

        /// R4 — a buffered endpoint is full and a sender waits for capacity. The receiver
        /// dequeues one message and refills the sender's into the freed slot; a timeout then
        /// lands before the receiver's claim. The refilled message is deliverable, so the send
        /// succeeded.
        #[test]
        fn r4_a_timeout_after_a_refill_cannot_report_the_refilled_message_as_timed_out() {
            let k = fresh();
            task(&k, SENDER, SENDER, 5);
            make_current(&k, SENDER);
            let (ep, cap) = k.with(|s| {
                let (ep, cap, _r) = s.create_endpoint(1).expect("buffered");
                s.ipc_send(cap, Message::new(SENDER, b"head").expect("m")).expect("fill");
                (ep, cap)
            });
            let d = block_send(&k, SENDER, cap, b"tail", 1);
            let (head, target) = take(&k, ep);
            assert_eq!(head.as_slice(), b"head");
            timeout(&k, CpuId(1), d + 1);
            assert_eq!(wake(&k, Seam::Broad, CpuId(0), target), Ok(()));
            assert_eq!(completion(&k, SENDER), Some((KernelState::SEND_COMPLETION_OK, 1)));
            let (tail, none) = k.with(|s| s.ipc_recv_endpoint_take(ep)).expect("take");
            assert_eq!(tail.map(|m| m.as_slice().to_vec()), Some(b"tail".to_vec()));
            assert!(none.is_none(), "the refilled message is queued, owned by nobody");
        }

        // ── R2: the timeout owns the cycle; a receiver lands inside the drain ────────────────

        /// R2 — the timeout drain has begun settling a due cycle when a receiver on another CPU
        /// tries to take it. Whichever wins, the sender's result must agree with the message:
        /// a receiver that obtained the message means success; `TimedOut` means no receiver
        /// obtained it.
        #[test]
        fn r2_a_receiver_inside_the_timeout_settle_cannot_obtain_a_timed_out_message() {
            let k = leak(fresh());
            task(k, SENDER, SENDER, 5);
            let (ep, cap) = endpoint(k, SENDER);
            let d = block_send(k, SENDER, cap, b"one", 1);
            k.collect_due_ipc_timeout_work(d + 1, d + 1, d + 1, CpuId(1));
            let got = alloc::rc::Rc::new(core::cell::Cell::new(None::<SenderWakeTarget>));
            let seen = got.clone();
            hook::arm_between_drain_phases(alloc::boxed::Box::new(move || {
                let (msg, target) = k.with(|s| s.ipc_recv_endpoint_take(ep)).expect("take");
                if msg.is_some() {
                    seen.set(target);
                }
            }));
            k.drain_send_timeout_post_work(CpuId(1), d + 1);
            if let Some(target) = got.get() {
                let _ = wake(k, Seam::Broad, CpuId(0), target);
            }
            let result = completion(k, SENDER).map(|c| c.0);
            match got.get() {
                Some(_) => assert_eq!(
                    result,
                    Some(KernelState::SEND_COMPLETION_OK),
                    "a receiver obtained the message, so the send succeeded"
                ),
                None => assert_eq!(result, Some(KernelState::SEND_COMPLETION_TIMED_OUT)),
            }
            assert_eq!(queued(k, SENDER), 1, "woken exactly once");
        }

        // ── R3: a timeout between the task block and the waiter's publication ────────────────

        /// R3 — the sender is `Blocked(EndpointSend)` with its deadline armed, but its waiter is
        /// not yet published, when the timeout runs on another CPU. No orphan may result: there
        /// is never a deliverable waiter of a sender whose cycle is over, and the cycle still
        /// ends exactly once.
        #[test]
        fn r3_a_timeout_before_publication_leaves_no_orphan_waiter() {
            let k = leak(fresh());
            task(k, SENDER, SENDER, 5);
            let (ep, cap) = endpoint(k, SENDER);
            hook::arm_between_block_and_publish(alloc::boxed::Box::new(move |s| {
                let d = s
                    .with_tcbs(|t| {
                        t.iter()
                            .flatten()
                            .find(|t| t.tid.0 == SENDER)
                            .and_then(|t| t.ipc_timeout_deadline)
                    })
                    .expect("Phase B armed the deadline");
                k.collect_due_ipc_timeout_work(d + 1, d + 1, d + 1, CpuId(1));
                k.drain_send_timeout_post_work(CpuId(1), d + 1);
            }));
            make_current(k, SENDER);
            let r = k.with(|s| s.ipc_send_with_deadline(cap, Message::new(SENDER, b"x").expect("m"), 1));
            assert_eq!(r, Err(KernelError::WouldBlock));
            assert_eq!(orphan_waiters(k, SENDER), 0, "no waiter outlives its cycle");
            // Whatever the drain did, the cycle ends exactly once and agrees with the message.
            let d = deadline(k, SENDER).unwrap_or(2);
            timeout(k, CpuId(1), d + 1);
            assert_eq!(orphan_waiters(k, SENDER), 0);
            assert_eq!(
                completion(k, SENDER).map(|c| c.0),
                Some(KernelState::SEND_COMPLETION_TIMED_OUT),
                "the undelivered message timed out"
            );
            assert!(
                k.with(|s| s.ipc_recv_endpoint_take(ep)).expect("take").0.is_none(),
                "and no receiver can deliver it afterwards"
            );
            assert_eq!(queued(k, SENDER), 1, "woken exactly once");
        }

        // ── R5: a capability-bearing send through the real IpcSend syscall ──────────────────

        /// R5 — an in-lock blocking send of a transferred capability (the route production uses
        /// whenever more than one CPU dispatches, and always under a hosted trap). While the
        /// sender is parked, its waiter's message names the transfer envelope, so the envelope
        /// must still be live: the cycle's eventual winner — a receiver that delivers it or a
        /// timeout that cancels it — owns it.
        #[test]
        fn r5_a_parked_capability_send_still_owns_its_transfer_envelope() {
            let mut state = Bootstrap::init().expect("init");
            state.set_timer_for_test(Timer::new(1));
            let (eidx, send_cap, _recv) = state
                .create_endpoint_with_mode(1, EndpointMode::Synchronous)
                .expect("ep");
            let (_e2, carried, _r2) = state.create_endpoint(2).expect("carried");
            let tid = state.current_tid().expect("current");
            let mut frame = TrapFrame::new(
                crate::kernel::syscall::Syscall::IpcSend as usize,
                [send_cap.0 as usize, 0, 0, 0, 100, carried.0 as usize],
            );
            state.handle_trap(Trap::Syscall, Some(&mut frame)).expect("send");
            assert!(matches!(
                state.task_status(tid),
                Some(TaskStatus::Blocked(WaitReason::EndpointSend(_)))
            ));
            let handle = state
                .with_ipc_state(|ipc| ipc.endpoint_sender_waiters[eidx][0])
                .and_then(|w| w.msg.transferred_cap())
                .expect("the parked waiter carries the transfer");
            let idx = (handle.0 & 0xFFFF) as usize;
            assert!(
                state.with_ipc_state(|ipc| ipc.transfer_envelopes[idx].is_some()
                    && ipc.transfer_envelope_generations[idx] == (handle.0 >> 16)),
                "the envelope the waiter names is live"
            );
        }

        // ── R6: the deadline boundary ───────────────────────────────────────────────────────

        /// R6 — no wall-clock tie-break. Before the deadline a timeout refuses with no mutation;
        /// at the deadline (and after it) the timeout is eligible, and the winner is whichever of
        /// the receiver's take and the timeout's withdrawal reaches the waiter first.
        #[test]
        fn r6_the_deadline_makes_a_timeout_eligible_and_the_waiter_decides() {
            // just before: refused, nothing moves
            let k = fresh();
            task(&k, SENDER, SENDER, 5);
            let (ep, cap) = endpoint(&k, SENDER);
            let d = block_send(&k, SENDER, cap, b"x", 3);
            let before = (status(&k, SENDER), live_waiters(&k, ep), deadline(&k, SENDER));
            k.collect_due_ipc_timeout_work(d, d, d, CpuId(1));
            k.drain_send_timeout_post_work(CpuId(1), d - 1);
            assert_eq!((status(&k, SENDER), live_waiters(&k, ep), deadline(&k, SENDER)), before);
            // exactly at the deadline, the receiver first: the receiver wins
            let (_m, target) = take(&k, ep);
            timeout(&k, CpuId(1), d);
            assert_eq!(wake(&k, Seam::Broad, CpuId(0), target), Ok(()));
            assert_eq!(completion(&k, SENDER), Some((KernelState::SEND_COMPLETION_OK, 1)));
            // exactly at the deadline, the timeout first: the timeout wins and withdraws it
            let k = fresh();
            task(&k, SENDER, SENDER, 5);
            let (ep, cap) = endpoint(&k, SENDER);
            let d = block_send(&k, SENDER, cap, b"x", 3);
            timeout(&k, CpuId(1), d);
            assert_eq!(completion(&k, SENDER), Some((KernelState::SEND_COMPLETION_TIMED_OUT, 1)));
            assert!(k.with(|s| s.ipc_recv_endpoint_take(ep)).expect("take").0.is_none());
        }

        // ── R7: delayed work of an older cycle ──────────────────────────────────────────────

        /// R7 — timeout work collected for cycle 1 is still queued when a receiver ends cycle 1
        /// and the sender blocks again (cycle 2). The stale work must not touch cycle 2.
        #[test]
        fn r7_stale_timeout_work_never_reaches_the_next_cycle() {
            let k = fresh();
            task(&k, SENDER, SENDER, 5);
            let (ep, cap) = endpoint(&k, SENDER);
            let (ep2, cap2) = endpoint(&k, SENDER);
            let d = block_send(&k, SENDER, cap, b"one", 1);
            k.collect_due_ipc_timeout_work(d + 1, d + 1, d + 1, CpuId(1));
            let (_m, target) = take(&k, ep);
            assert_eq!(wake(&k, Seam::Broad, CpuId(0), target), Ok(()));
            assert_eq!(resume(&k, SENDER, 5), Some(KernelState::SEND_COMPLETION_OK));
            let d2 = block_send(&k, SENDER, cap2, b"two", 1000);
            k.drain_send_timeout_post_work(CpuId(1), d + 1);
            assert!(matches!(status(&k, SENDER), Some(TaskStatus::Blocked(WaitReason::EndpointSend(_)))));
            assert_eq!(deadline(&k, SENDER), Some(d2));
            assert_eq!(live_waiters(&k, ep2), alloc::vec![(SENDER, 2)]);
            assert_eq!(completion(&k, SENDER), None);
        }

        // ── R8: the sender dies while a timeout and a receiver are pending ──────────────────

        /// R8 — INVARIANT TEST (the death is injected: a blocked sender cannot exit itself). A
        /// receiver took the waiter and timeout work is queued when the sender dies. Neither
        /// path may publish into the dead task, wake it, or settle anything twice.
        #[test]
        fn r8_a_dead_sender_receives_no_publication_and_no_wake() {
            let k = fresh();
            task(&k, SENDER, SENDER, 5);
            let (ep, cap) = endpoint(&k, SENDER);
            let d = block_send(&k, SENDER, cap, b"x", 1);
            k.collect_due_ipc_timeout_work(d + 1, d + 1, d + 1, CpuId(1));
            let (_m, target) = take(&k, ep);
            k.with(|s| s.mark_task_dead(SENDER)).expect("dead");
            k.drain_send_timeout_post_work(CpuId(1), d + 1);
            assert_eq!(wake(&k, Seam::OffLock, CpuId(0), target), Ok(()));
            assert_eq!(status(&k, SENDER), Some(TaskStatus::Dead));
            assert_eq!(completion(&k, SENDER), None);
            assert_eq!(queued(&k, SENDER), 0);
        }

        // ── dependencies of the contract: every removal of a waiter ends its cycle ──────────

        /// The waiter queue is full, so the broad block cannot publish its waiter. The cycle then
        /// has no waiter and never will, so nothing else can end it: the blocker must, and the
        /// sender may not be left `Blocked(EndpointSend)` with nothing to wake it.
        #[test]
        fn a_refused_publication_never_strands_the_blocked_sender() {
            let k = fresh();
            const BASE: u64 = 40;
            let n = MAX_ENDPOINT_SENDER_WAITERS as u64;
            for i in 0..=n {
                task(&k, BASE + i, BASE + i, 30 + i as u16);
            }
            let (ep, cap) = endpoint(&k, BASE);
            let caps: alloc::vec::Vec<CapId> = (0..=n)
                .map(|i| {
                    if i == 0 {
                        cap
                    } else {
                        k.with(|s| {
                            s.grant_capability_task_to_task_with_rights(
                                BASE,
                                cap,
                                BASE + i,
                                crate::kernel::capabilities::CapRights::SEND,
                            )
                        })
                        .expect("grant")
                    }
                })
                .collect();
            for i in 0..n {
                block_send(&k, BASE + i, caps[i as usize], b"w", 1000);
            }
            assert_eq!(live_waiters(&k, ep).len(), n as usize, "the waiter queue is full");
            let last = BASE + n;
            make_current(&k, last);
            let r = k.with(|s| {
                s.ipc_send_with_deadline(caps[n as usize], Message::new(last, b"z").expect("m"), 1000)
            });
            assert!(r.is_err(), "the refused send reports an error");
            assert!(
                !matches!(status(&k, last), Some(TaskStatus::Blocked(WaitReason::EndpointSend(_)))),
                "a sender with no waiter is not left blocked"
            );
            assert_eq!(live_waiters(&k, ep).len(), n as usize, "the queue is unchanged");
        }

        /// The endpoint a sender is parked on is destroyed. Its waiter is removed with it, so the
        /// remover owns the cycle and must end it: the sender is woken once with an error, never
        /// left blocked waiting for a waiter that no longer exists.
        #[test]
        fn endpoint_destruction_ends_the_cycles_it_removes() {
            let k = fresh();
            task(&k, SENDER, SENDER, 5);
            let (ep, cap) = endpoint(&k, SENDER);
            block_send(&k, SENDER, cap, b"x", 1000);
            k.with(|s| s.destroy_endpoint(ep)).expect("destroy");
            assert_eq!(status(&k, SENDER), Some(TaskStatus::Runnable));
            let (result, generation) = completion(&k, SENDER).expect("a completion");
            assert_eq!(generation, 1);
            assert_ne!(result, KernelState::SEND_COMPLETION_OK, "an undelivered send failed");
            assert_ne!(result, KernelState::SEND_COMPLETION_TIMED_OUT, "and did not time out");
            assert_eq!(queued(&k, SENDER), 1);
        }
    }
