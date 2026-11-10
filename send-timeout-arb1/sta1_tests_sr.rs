            /// SEND-TIMEOUT-ARB1 — a pinned shared-region transfer committed through the split
            /// blocking-send route, raced by a timeout inside the receiver's take→claim window.
            mod send_timeout_arb1_envelope {
                use super::*;

                /// R5 — the receiver took the transfer-bearing waiter; a timeout on CPU 1 lands
                /// before the receiver's claim. The receiver owns the delivery, so the sender
                /// sees success, and the envelope and its single pin stay with the receiver.
                #[test]
                fn r5_a_timeout_after_the_take_leaves_delivery_and_envelope_with_the_receiver() {
                    let cpu = CpuId(0);
                    reset_u6_latches(0);
                    reset_u6_latches(1);
                    let f = sr_fixture(EndpointMode::Synchronous, 1);
                    publish_and_commit_msg(&f.k, cpu, f.send_cap, f.msg, 1);
                    let cycle = send_generation(&f.k, SENDER);
                    let (msg, target) =
                        f.k.with(|s| s.ipc_recv_endpoint_take(f.eidx)).expect("take");
                    assert_eq!(msg.and_then(|m| m.transferred_cap()).map(|c| c.0), Some(f.handle));
                    let target = target.expect("the blocked sender");
                    let now = 1_000;
                    f.k.collect_due_ipc_timeout_work(now, now, now, CpuId(1));
                    f.k.drain_send_timeout_post_work(CpuId(1), now);
                    let r = f
                        .k
                        .with_cpu(cpu, |kernel| kernel.apply_blocked_sender_wake(Some(target)))
                        .expect("broad phase");
                    assert_eq!(r, Ok(()));
                    let done = parked_completion(&f.k, SENDER).expect("completion");
                    assert_eq!(done.result, KernelState::SEND_COMPLETION_OK, "delivered");
                    assert_eq!(done.blocked_generation, cycle);
                    assert!(envelope_live(&f.k, f.handle), "the receiver still owns the envelope");
                    assert_eq!(pin(&f.k, f.object_id), 1, "and its one pin");
                    reset_u6_latches(0);
                    reset_u6_latches(1);
                }
            

                /// R9 — the claim after a withdrawal fails only when the sender is no longer
                /// blocked in that cycle. INVARIANT TEST (the death is injected between the
                /// drain's withdrawal and its claim): the drain still cancels the transfer it
                /// withdrew exactly once, publishes nothing and wakes nothing, and the dying
                /// task's own sweep finds nothing left to settle.
                #[test]
                fn r9_a_failed_claim_after_withdrawal_strands_nothing_and_leaks_nothing() {
                    let cpu = CpuId(0);
                    reset_u6_latches(0);
                    reset_u6_latches(1);
                    let f: &'static SrFixture =
                        alloc::boxed::Box::leak(alloc::boxed::Box::new(sr_fixture(
                            EndpointMode::Synchronous,
                            1,
                        )));
                    publish_and_commit_msg(&f.k, cpu, f.send_cap, f.msg, 1);
                    assert_eq!(pin(&f.k, f.object_id), 1);
                    let now = 1_000;
                    f.k.collect_due_ipc_timeout_work(now, now, now, CpuId(1));
                    crate::kernel::boot::sender_waiter::send_arb_phase_hook::arm_between_drain_phases(
                        alloc::boxed::Box::new(move || {
                            f.k.with(|s| s.mark_task_dead(SENDER)).expect("dies");
                        }),
                    );
                    f.k.drain_send_timeout_post_work(CpuId(1), now);
                    assert!(sender_waiters(&f.k, f.eidx).is_empty(), "withdrawn");
                    assert!(!envelope_live(&f.k, f.handle), "the withdrawn transfer is cancelled");
                    // The dead sender's process teardown may reclaim the object; reclaim refuses
                    // while a pin is held, so "gone" also proves the pin was released.
                    assert!(
                        refcounts(&f.k, f.object_id).is_none_or(|r| r.2 == 0),
                        "its pin released exactly once"
                    );
                    assert!(parked_completion(&f.k, SENDER).is_none(), "nothing published");
                    assert_eq!(status_of(&f.k, SENDER), Some(TaskStatus::Dead), "not resurrected");
                    f.k.with_cpu(cpu, |kernel| kernel.clear_ipc_waiters_for_tid(SENDER))
                        .expect("broad phase");
                    assert!(
                        refcounts(&f.k, f.object_id).is_none_or(|r| r.2 == 0),
                        "and never twice"
                    );
                    reset_u6_latches(0);
                    reset_u6_latches(1);
                }
            }
