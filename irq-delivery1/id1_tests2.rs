
    /// Completions for one bridge settlement, through both completion tokens: the single-step
    /// seam (x86_64 LAPIC EOI / AArch64 inert) and a RISC-V PLIC claim, counted at the one owner
    /// and in the claim model.
    fn settle_both(kernel: &SharedKernel, line: u16) -> (Option<Result<(), TrapHandleError>>, u32, u32) {
        use crate::arch::external_irq_claim::model;
        let _ = completion_count_for_test::take();
        model::reset();
        let a = settle_external_interrupt_at_bridge(kernel, CpuId(0), u32::from(line), single_step(line));
        let single = completion_count_for_test::take();
        let b = settle_external_interrupt_at_bridge(
            kernel,
            CpuId(0),
            u32::from(line),
            InterruptCompletion::PlicClaim {
                source: u32::from(line),
                base: 0x0C00_0000,
                context_index: 1,
            },
        );
        let _ = completion_count_for_test::take();
        let claims = model::completions();
        model::reset();
        assert_eq!(a, b, "both tokens settle the same way");
        (a, single, claims)
    }

    /// §5/§9.12 — exactly one controller completion for every ending: normal, saturated,
    /// repeated under saturation, unrouted, revoked binding, destroyed Notification, stale route.
    #[test]
    fn every_ending_is_completed_exactly_once_on_both_tokens() {
        let (kernel, idx, sig, recv) = fixture(1);
        let _armed = ArmedInjection::arm();
        assert_eq!(settle_both(&kernel, LINE), (Some(Ok(())), 1, 1), "normal, then saturated");
        for _ in 0..3 {
            assert_eq!(settle_both(&kernel, LINE), (Some(Ok(())), 1, 1), "repeated under saturation");
        }
        assert_eq!(settle_both(&kernel, 40), (Some(Ok(())), 1, 1), "unrouted");
        // A stale route naming the incarnation before this one.
        let gen = kernel.with(|s| s.with_ipc_state(|ipc| ipc.notification_generations[idx]));
        kernel.with(|s| {
            s.with_ipc_state_mut(|ipc| {
                ipc.irq_routes[41] = Some(crate::kernel::boot::irq_binding::forced_route_for_test(
                    idx,
                    gen.wrapping_sub(1),
                ))
            })
        });
        assert_eq!(settle_both(&kernel, 41), (Some(Ok(())), 1, 1), "stale route");
        // Revoke LINE2's line authority (its owner): the binding ends.
        kernel.with(|s| {
            let route = s.with_ipc_state(|ipc| ipc.irq_routes[usize::from(LINE2)]).expect("bound");
            let _ = s.revoke_capability_in_cnode(route.owner.cnode, route.owner.cap);
            assert!(s.with_ipc_state(|ipc| ipc.irq_routes[usize::from(LINE2)].is_none()));
        });
        assert_eq!(settle_both(&kernel, LINE2), (Some(Ok(())), 1, 1), "revoked binding");
        // Destroy the Notification.
        kernel.with(|s| {
            let cnode = s.current_task_cnode().expect("cnode");
            let _ = s.revoke_capability_in_cnode(cnode, sig);
            let _ = s.revoke_capability_in_cnode(cnode, recv);
        });
        assert_eq!(settle_both(&kernel, LINE), (Some(Ok(())), 1, 1), "destroyed Notification");
    }

    /// §8 I4 — the receiver consumes a pending line while another interrupt of that line
    /// arrives. The two are totally ordered at rank 3; in neither order is the new one erased.
    #[test]
    fn i4_consuming_a_pending_line_cannot_erase_a_new_interrupt() {
        // (a) the new interrupt lands after the consumption: it is observable afterwards.
        let (kernel, _idx, _sig, recv) = fixture(1);
        saturate(&kernel, 1);
        let _ = kernel.deliver_external_irq_split(CpuId(0), LINE2);
        let first: alloc::vec::Vec<u16> = drain(&kernel, recv);
        assert_eq!(first, [LINE, LINE2]);
        let after = kernel.deliver_external_irq_split(CpuId(0), LINE2);
        assert_eq!(after.marker(), "delivered", "the queue has room again: {after:?}");
        assert_eq!(drain(&kernel, recv), [LINE2], "the new interrupt was not erased");
        // (b) it lands before: it coalesces into the bit the consumption then reports once.
        let (kernel, idx, _sig, recv) = fixture(1);
        saturate(&kernel, 1);
        let _ = kernel.deliver_external_irq_split(CpuId(0), LINE2);
        let again = kernel.deliver_external_irq_split(CpuId(1), LINE2);
        assert!(matches!(again, crate::kernel::boot::IrqDeliveryOutcome::Coalesced { newly_pending: false, .. }));
        kernel.with(|s| {
            s.with_ipc_state(|ipc| {
                assert_eq!(
                    ipc.notifications[idx].as_ref().expect("live").overflow_pending,
                    1u64 << LINE2
                )
            })
        });
        assert_eq!(drain(&kernel, recv), [LINE, LINE2]);
        kernel.with(|s| {
            s.with_ipc_state(|ipc| {
                assert_eq!(ipc.notifications[idx].as_ref().expect("live").overflow_pending, 0)
            })
        });
    }

    /// §8 I2/I3 — two interrupts on one line from two CPUs, and two different lines: one pending
    /// bit per line, distinct lines stay distinct. (Hosted and serial: the two deliveries are
    /// ordered by the rank-3 acquisition each takes, which is the production linearization point;
    /// this does not prove parallel execution.)
    #[test]
    fn i2_i3_same_line_coalesces_across_cpus_and_lines_stay_distinct() {
        let (kernel, idx, _sig, recv) = fixture(1);
        saturate(&kernel, 1);
        let a = kernel.deliver_external_irq_split(CpuId(0), LINE2);
        let b = kernel.deliver_external_irq_split(CpuId(1), LINE2);
        let c = kernel.deliver_external_irq_split(CpuId(1), LINE);
        assert!(matches!(a, crate::kernel::boot::IrqDeliveryOutcome::Coalesced { newly_pending: true, .. }));
        assert!(matches!(b, crate::kernel::boot::IrqDeliveryOutcome::Coalesced { newly_pending: false, .. }));
        assert!(matches!(c, crate::kernel::boot::IrqDeliveryOutcome::Coalesced { newly_pending: true, .. }));
        kernel.with(|s| {
            s.with_ipc_state(|ipc| {
                let n = ipc.notifications[idx].as_ref().expect("live");
                assert_eq!(n.len, 1, "the queued event is intact");
                assert_eq!(n.overflow_pending, (1u64 << LINE) | (1u64 << LINE2));
            })
        });
        assert_eq!(drain(&kernel, recv), [LINE, LINE, LINE2], "queued first, then lowest line first");
    }

    /// While any line is pending the object stays in coalescing mode, so a busy line cannot
    /// refill the queue ahead of a line that overflowed.
    #[test]
    fn a_busy_line_cannot_starve_a_pending_one() {
        let (kernel, _idx, _sig, recv) = fixture(2);
        saturate(&kernel, 2);
        let _ = kernel.deliver_external_irq_split(CpuId(0), LINE2);
        // The receiver takes one queued event; the queue has room, but LINE2 is pending.
        let first = kernel.with(|s| s.try_ipc_recv(recv).expect("recv").expect("event").opcode);
        assert_eq!(first, LINE);
        for _ in 0..5 {
            let o = kernel.deliver_external_irq_split(CpuId(0), LINE);
            assert_eq!(o.marker(), "coalesced", "{o:?}");
        }
        assert_eq!(drain(&kernel, recv), [LINE, LINE, LINE2], "LINE2 is reached, once, after the queue");
    }

    /// §8 I8 / §9.14 — a receiver that blocks while only a pending indication remains gets it at
    /// once (it is not parked); a parked receiver is woken exactly once and its record consumed.
    #[test]
    fn i8_pending_work_is_taken_by_a_blocking_receive_and_waiters_are_woken_once() {
        let (kernel, idx, _sig, recv) = fixture(1);
        saturate(&kernel, 1);
        let _ = kernel.deliver_external_irq_split(CpuId(0), LINE2);
        kernel.with(|s| {
            assert_eq!(s.try_ipc_recv(recv).expect("recv").map(|m| m.opcode), Some(LINE));
            let got = s.ipc_recv(recv).expect("blocking receive");
            assert_eq!(got.map(|m| m.opcode), Some(LINE2), "the pending line, not a park");
            assert!(s.with_ipc_state(|ipc| ipc.notification_waiters[idx].is_none()));
            // Now empty: a blocking receive parks, and one interrupt wakes it exactly once.
            s.register_task(9).expect("receiver task");
        });
        kernel.with(|s| {
            let r9 = s.grant_capability_task_to_task_with_rights(0, recv, 9, crate::kernel::capabilities::CapRights::RECEIVE).expect("r9");
            s.enqueue_current_cpu(9).expect("enqueue");
            let _ = s.yield_current();
            assert_eq!(s.current_tid(), Some(9));
            assert_eq!(s.ipc_recv(r9).expect("park"), None);
            assert!(s.task_is_blocked(9));
        });
        let woke = kernel.deliver_external_irq_split(CpuId(0), LINE);
        assert!(matches!(woke, crate::kernel::boot::IrqDeliveryOutcome::DeliveredAndWoke { waiter_tid: 9, .. }), "{woke:?}");
        let again = kernel.deliver_external_irq_split(CpuId(0), LINE);
        assert_eq!(again.marker(), "delivered", "no record left to wake twice: {again:?}");
        kernel.with(|s| assert!(!s.task_is_blocked(9)));
    }

    /// §8 I5/I6/I7 — saturation interleaved with route revocation and with destruction and
    /// reuse: a delivery after the route is gone is `NoRoute`; the live Notification keeps its
    /// pending state; a stale delivery never touches the replacement's state.
    #[test]
    fn i5_i6_i7_revocation_destruction_and_reuse_around_saturation() {
        let (kernel, idx, sig, recv) = fixture(1);
        saturate(&kernel, 1);
        let _ = kernel.deliver_external_irq_split(CpuId(0), LINE2);
        // I6: revoke LINE2's owner; a further LINE2 interrupt has no route, the pending bit stays.
        kernel.with(|s| {
            let route = s.with_ipc_state(|ipc| ipc.irq_routes[usize::from(LINE2)]).expect("bound");
            let _ = s.revoke_capability_in_cnode(route.owner.cnode, route.owner.cap);
        });
        assert_eq!(kernel.deliver_external_irq_split(CpuId(0), LINE2).marker(), "no_route");
        kernel.with(|s| {
            s.with_ipc_state(|ipc| {
                assert_eq!(ipc.notifications[idx].as_ref().expect("live").overflow_pending, 1u64 << LINE2)
            })
        });
        // I5/I7: destroy, reuse, and force the old incarnation's route back: the stale delivery
        // reaches nothing of the replacement.
        let old = kernel.with(|s| s.with_ipc_state(|ipc| ipc.notification_generations[idx]));
        kernel.with(|s| {
            let cnode = s.current_task_cnode().expect("cnode");
            let _ = s.revoke_capability_in_cnode(cnode, sig);
            let _ = s.revoke_capability_in_cnode(cnode, recv);
        });
        let (idx2, _s2, recv2) = kernel.with(|s| s.create_notification(1).expect("replacement"));
        assert_eq!(idx2, idx);
        kernel.with(|s| {
            s.with_ipc_state_mut(|ipc| {
                ipc.irq_routes[usize::from(LINE)] =
                    Some(crate::kernel::boot::irq_binding::forced_route_for_test(idx, old))
            })
        });
        assert_eq!(kernel.deliver_external_irq_split(CpuId(0), LINE).marker(), "target_gone");
        kernel.with(|s| {
            s.with_ipc_state(|ipc| {
                let n = ipc.notifications[idx2].as_ref().expect("replacement");
                assert_eq!((n.len, n.overflow_pending), (0, 0), "the replacement is untouched");
            })
        });
        assert_eq!(drain(&kernel, recv2), []);
    }

    /// The broad producer coalesces the same way: `route_external_irq` returns `Ok` on a
    /// saturated route and the line stays observable.
    #[test]
    fn the_broad_route_coalesces_too() {
        let (kernel, _idx, _sig, recv) = fixture(1);
        kernel.with(|s| {
            for _ in 0..3 {
                s.route_external_irq(LINE).expect("never an error");
            }
            s.route_external_irq(LINE2).expect("never an error");
        });
        assert_eq!(drain(&kernel, recv), [LINE, LINE, LINE2]);
    }

    /// Owner level: the receive clears only the bit it reports; a signal never refuses for want
    /// of space; a line no route can carry is the only refusal (an invariant, never saturation).
    #[test]
    fn the_object_owner_coalesces_reports_and_clears_exactly() {
        use crate::kernel::boot::{IrqSignal, NotificationObject};
        let mut n = NotificationObject::new(1).expect("depth 1");
        assert_eq!(n.send_irq(3), Ok(IrqSignal::Queued));
        assert_eq!(n.send_irq(5), Ok(IrqSignal::Coalesced { newly_pending: true }));
        assert_eq!(n.send_irq(7), Ok(IrqSignal::Coalesced { newly_pending: true }));
        assert_eq!(n.send_irq(5), Ok(IrqSignal::Coalesced { newly_pending: false }));
        assert_eq!(n.recv().map(|m| m.opcode), Some(3));
        assert_eq!(n.send_irq(3), Ok(IrqSignal::Coalesced { newly_pending: true }), "coalescing mode");
        assert_eq!(n.recv().map(|m| m.opcode), Some(3));
        assert_eq!(n.overflow_pending, (1 << 5) | (1 << 7), "only bit 3 was cleared");
        assert_eq!(n.recv().map(|m| m.opcode), Some(5));
        assert_eq!(n.recv().map(|m| m.opcode), Some(7));
        assert!(n.recv().is_none() && !n.has_event());
        assert_eq!(n.send_irq(9), Ok(IrqSignal::Queued), "back to queuing once drained");
        let past = crate::arch::platform_constants::MAX_IRQ_LINES as u16;
        assert_eq!(n.send_irq(past), Err(KernelError::WrongObject));
    }
}
