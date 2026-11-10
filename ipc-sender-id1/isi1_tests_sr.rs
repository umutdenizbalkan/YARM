
            /// IPC-SENDER-ID1 — a transfer-bearing (shared-region, pinned) blocked sender across
            /// the receive/timeout/exit competitions. The fixture is the real envelope stash and
            /// the real off-lock blocking-send commit; the take, the timeout pipeline and the
            /// wake seams are production functions run in a two-CPU order.
            mod ipc_sender_id1_envelope {
                use super::*;

                fn take_target(f: &SrFixture) -> crate::kernel::ipc::SenderWakeTarget {
                    let (msg, target) =
                        f.k.with(|s| s.ipc_recv_endpoint_take(f.eidx)).expect("take");
                    assert_eq!(
                        msg.and_then(|m| m.transferred_cap()).map(|c| c.0),
                        Some(f.handle),
                        "the receiver now holds the transfer-bearing message"
                    );
                    target.expect("the blocked sender")
                }

                /// S5 — the receiver took the waiter (and with it the envelope); a timeout on
                /// CPU 1 then claims the cycle. The timeout finds no waiter, so it settles
                /// nothing: the envelope and its pin stay with the receiver. The receiver's wake
                /// is refused quietly — its receive does not fail, so the envelope it is about to
                /// materialize is not dropped.
                #[test]
                fn a_timeout_after_the_take_leaves_the_receivers_envelope_and_pin() {
                    let cpu = CpuId(0);
                    reset_u6_latches(0);
                    reset_u6_latches(1);
                    let f = sr_fixture(EndpointMode::Synchronous, 1);
                    publish_and_commit_msg(&f.k, cpu, f.send_cap, f.msg, 1);
                    let target = take_target(&f);
                    let d = f.k.with(|s| {
                        s.with_tcbs(|t| {
                            t.iter()
                                .flatten()
                                .find(|t| t.tid.0 == SENDER)
                                .and_then(|t| t.ipc_timeout_deadline)
                        })
                    });
                    let now = d.expect("armed") + 1;
                    f.k.collect_due_ipc_timeout_work(now, now, now, CpuId(1));
                    f.k.drain_send_timeout_post_work(CpuId(1), now);
                    assert!(envelope_live(&f.k, f.handle), "not settled by the timeout");
                    assert_eq!(pin(&f.k, f.object_id), 1, "the pin rides to the receiver");
                    let r = f
                        .k
                        .with_cpu(cpu, |kernel| kernel.apply_blocked_sender_wake(Some(target)))
                        .expect("broad phase");
                    assert_eq!(r, Ok(()), "the receive that holds the envelope does not fail");
                    assert!(envelope_live(&f.k, f.handle));
                    assert_eq!(pin(&f.k, f.object_id), 1, "still exactly one pin");
                    reset_u6_latches(0);
                    reset_u6_latches(1);
                }

                /// S5 — exit after a receiver took the waiter: the exit sweep finds no waiter of
                /// the sender, so it settles nothing that the receiver now owns.
                #[test]
                fn an_exit_after_the_take_does_not_settle_the_receivers_envelope() {
                    let cpu = CpuId(0);
                    reset_u6_latches(0);
                    let f = sr_fixture(EndpointMode::Synchronous, 1);
                    publish_and_commit_msg(&f.k, cpu, f.send_cap, f.msg, 5_000);
                    let _target = take_target(&f);
                    f.k.with_cpu(cpu, |kernel| kernel.clear_ipc_waiters_for_tid(SENDER))
                        .expect("broad phase");
                    assert!(envelope_live(&f.k, f.handle));
                    assert_eq!(pin(&f.k, f.object_id), 1);
                    reset_u6_latches(0);
                }

                /// S5 / 7 — the timeout wins while the waiter is still queued: the envelope and
                /// pin are settled exactly once, and every later removal (exit sweep, a second
                /// timeout pass, a late receiver take) finds nothing more to settle.
                #[test]
                fn a_queued_envelope_is_settled_once_whatever_runs_after_the_timeout() {
                    let cpu = CpuId(0);
                    reset_u6_latches(0);
                    reset_u6_latches(1);
                    let f = sr_fixture(EndpointMode::Synchronous, 1);
                    publish_and_commit_msg(&f.k, cpu, f.send_cap, f.msg, 1);
                    let now = 1_000;
                    f.k.collect_due_ipc_timeout_work(now, now, now, CpuId(1));
                    f.k.drain_send_timeout_post_work(CpuId(1), now);
                    assert!(!envelope_live(&f.k, f.handle));
                    assert_eq!(pin(&f.k, f.object_id), 0, "released once");
                    f.k.with_cpu(cpu, |kernel| kernel.clear_ipc_waiters_for_tid(SENDER))
                        .expect("broad phase");
                    f.k.collect_due_ipc_timeout_work(now, now, now, CpuId(0));
                    f.k.drain_send_timeout_post_work(CpuId(0), now);
                    assert_eq!(
                        f.k.with(|s| s.ipc_recv_endpoint_take(f.eidx)),
                        Ok((None, None)),
                        "no waiter is left to take"
                    );
                    assert_eq!(pin(&f.k, f.object_id), 0, "never below zero, never twice");
                    reset_u6_latches(0);
                    reset_u6_latches(1);
                }
            }
