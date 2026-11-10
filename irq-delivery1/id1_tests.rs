
/// IRQ-DELIVERY1 — a saturated Notification IRQ queue. Everything runs through the real owners:
/// `deliver_external_irq_split`, the bridge `settle_external_interrupt_at_bridge`, the shared
/// trap dispatch `dispatch_trap_entry_with_shared_kernel`, the broad `route_external_irq`, and
/// the real receive (`try_ipc_recv`). Completions are counted at the one completion owner.
mod irq_delivery1 {
    use super::*;
    use crate::arch::hal_adapters::{InterruptCompletion, completion_count_for_test};
    use crate::kernel::syscall_split::settle_external_interrupt_at_bridge;
    use crate::runtime::SharedKernel;

    const LINE: u16 = 27;
    const LINE2: u16 = 28;

    struct ArmedInjection;
    impl ArmedInjection {
        fn arm() -> Self {
            crate::kernel::boot::set_irq1_hosted_bridge_injection(true);
            let _ = completion_count_for_test::take();
            Self
        }
    }
    impl Drop for ArmedInjection {
        fn drop(&mut self) {
            crate::kernel::boot::set_irq1_hosted_bridge_injection(false);
        }
    }

    /// A Notification of `depth`, bound to LINE (and LINE2) through line authority.
    fn fixture(depth: usize) -> (SharedKernel, usize, CapId, CapId) {
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        let (idx, sig, recv) = kernel.with(|s| {
            let (idx, sig, recv) = s.create_notification(depth).expect("notification");
            s.bind_irq_line_for_test(LINE, sig).expect("bind LINE");
            s.bind_irq_line_for_test(LINE2, sig).expect("bind LINE2");
            (idx, sig, recv)
        });
        (kernel, idx, sig, recv)
    }

    fn single_step(line: u16) -> InterruptCompletion {
        InterruptCompletion::ArchSingleStep { line }
    }

    /// Fill the queue to its depth through the real split delivery.
    fn saturate(kernel: &SharedKernel, depth: usize) {
        for _ in 0..depth {
            let outcome = kernel.deliver_external_irq_split(CpuId(0), LINE);
            assert_eq!(outcome.marker(), "delivered", "filling: {outcome:?}");
        }
    }

    /// Every line the receiver observes, in order, through the real non-blocking receive.
    fn drain(kernel: &SharedKernel, recv: CapId) -> alloc::vec::Vec<u16> {
        let mut seen = alloc::vec::Vec::new();
        kernel.with(|s| {
            while let Some(msg) = s.try_ipc_recv(recv).expect("receive") {
                seen.push(msg.opcode);
                assert!(seen.len() <= 64, "bounded");
            }
        });
        seen
    }

    fn x86_external_vector(line: u16) -> u8 {
        0x20 + line as u8
    }

    /// Q1 — one IRQ past a full queue: not an error, and the line's pending work survives.
    #[test]
    fn q1_one_irq_past_a_full_queue_is_nonfatal_and_not_lost() {
        let (kernel, _idx, _sig, recv) = fixture(2);
        saturate(&kernel, 2);
        let extra = kernel.deliver_external_irq_split(CpuId(0), LINE);
        assert_eq!(extra.propagated_error(), None, "saturation is not an error: {extra:?}");
        assert_ne!(extra.marker(), "failed");
        assert_eq!(
            drain(&kernel, recv),
            [LINE, LINE, LINE],
            "the two queued events, then the line's retained pending indication"
        );
    }

    /// Q2 — a user-origin interrupt on a saturated route, through the real shared dispatch:
    /// `Ok`, the interrupted frame untouched, exactly one completion.
    #[test]
    fn q2_a_user_origin_interrupt_on_a_full_queue_returns_ok_and_leaves_the_frame() {
        let (kernel, _idx, _sig, recv) = fixture(1);
        saturate(&kernel, 1);
        let _armed = ArmedInjection::arm();
        let mut frame = TrapFrame::new(0x55, [11, 22, 33, 44, 55, 66]);
        let before = frame.clone();
        let ctx = crate::arch::trap_entry::ArchTrapContext {
            vector: x86_external_vector(LINE),
            error_code: 0,
            fault_addr: 0,
            entered_from_user: true,
        };
        let result = crate::arch::trap_entry::dispatch_trap_entry_with_shared_kernel(
            &kernel,
            CpuId(0),
            ctx,
            Some(&mut frame),
        );
        assert_eq!(result, Ok(()), "the port's fatal channel must not be entered");
        assert_eq!(frame, before, "no error encoded into the interrupted frame");
        assert_eq!(completion_count_for_test::take(), 1, "exactly one completion");
        assert_eq!(drain(&kernel, recv), [LINE, LINE]);
    }

    /// Q3 — the same from idle (no interrupted user frame).
    #[test]
    fn q3_an_idle_origin_interrupt_on_a_full_queue_returns_ok() {
        let (kernel, _idx, _sig, recv) = fixture(1);
        saturate(&kernel, 1);
        let _armed = ArmedInjection::arm();
        let ctx = crate::arch::trap_entry::ArchTrapContext {
            vector: x86_external_vector(LINE),
            error_code: 0,
            fault_addr: 0,
            entered_from_user: false,
        };
        let result = crate::arch::trap_entry::dispatch_trap_entry_with_shared_kernel(
            &kernel,
            CpuId(0),
            ctx,
            None,
        );
        assert_eq!(result, Ok(()), "the idle fatal channel must not be entered");
        assert_eq!(completion_count_for_test::take(), 1);
        assert_eq!(drain(&kernel, recv), [LINE, LINE]);
    }

    /// Q4 — the bridge body every kernel-thread port calls: a settled `Ok`, so the kernel-thread
    /// arm has nothing to report, and one completion.
    #[test]
    fn q4_the_bridge_settles_a_saturated_delivery_as_ok() {
        let (kernel, _idx, _sig, _recv) = fixture(1);
        saturate(&kernel, 1);
        let _armed = ArmedInjection::arm();
        assert_eq!(
            settle_external_interrupt_at_bridge(&kernel, CpuId(0), u32::from(LINE), single_step(LINE)),
            Some(Ok(()))
        );
        assert_eq!(completion_count_for_test::take(), 1);
    }

    /// Q5 — ordinary delivery is unchanged: delivered, queued, received, one completion.
    #[test]
    fn q5_ordinary_delivery_is_unchanged() {
        let (kernel, _idx, _sig, recv) = fixture(4);
        let _armed = ArmedInjection::arm();
        assert_eq!(
            settle_external_interrupt_at_bridge(&kernel, CpuId(0), u32::from(LINE), single_step(LINE)),
            Some(Ok(()))
        );
        assert_eq!(completion_count_for_test::take(), 1);
        assert_eq!(drain(&kernel, recv), [LINE]);
        assert_eq!(drain(&kernel, recv), [], "nothing duplicated");
    }

    /// Q6 — two lines on one Notification: after saturation the receiver still observes the
    /// line whose interrupt did not fit, and the two stay distinct.
    #[test]
    fn q6_two_saturated_lines_both_remain_observable() {
        let (kernel, _idx, _sig, recv) = fixture(1);
        saturate(&kernel, 1);
        for line in [LINE2, LINE, LINE2, LINE] {
            let o = kernel.deliver_external_irq_split(CpuId(0), line);
            assert_eq!(o.propagated_error(), None, "{o:?}");
        }
        let seen = drain(&kernel, recv);
        assert_eq!(seen.first(), Some(&LINE), "the queued event first");
        assert!(seen.contains(&LINE2), "the overflowed line is still observable: {seen:?}");
        assert_eq!(
            seen.iter().filter(|&&l| l == LINE2).count(),
            1,
            "repeated saturation of one line coalesces: {seen:?}"
        );
    }

    /// Q7 — the exact incarnation is destroyed while saturated: its queued and pending events go
    /// with it, and the replacement in the same slot starts with none.
    #[test]
    fn q7_destruction_retires_queued_and_pending_events() {
        let (kernel, idx, sig, recv) = fixture(1);
        saturate(&kernel, 1);
        let _ = kernel.deliver_external_irq_split(CpuId(0), LINE2);
        kernel.with(|s| {
            let cnode = s.current_task_cnode().expect("cnode");
            let _ = s.revoke_capability_in_cnode(cnode, sig);
            let _ = s.revoke_capability_in_cnode(cnode, recv);
            assert!(s.with_ipc_state(|ipc| ipc.notifications[idx].is_none()), "destroyed");
        });
        let (idx2, _sig2, recv2) = kernel.with(|s| s.create_notification(1).expect("replacement"));
        assert_eq!(idx2, idx, "the slot is reused");
        assert_eq!(drain(&kernel, recv2), [], "the replacement inherits no event");
        let o = kernel.deliver_external_irq_split(CpuId(0), LINE2);
        assert_eq!(o.marker(), "no_route", "and no route: {o:?}");
    }
}
