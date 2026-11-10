
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
