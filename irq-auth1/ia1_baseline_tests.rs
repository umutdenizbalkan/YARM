
/// IRQ-AUTH1 baseline reproducers, written against the CURRENT binding API
/// (`bind_irq_notification(raw_line, notification_cap)`). Each asserts the corrected contract; on
/// `41403e1` the defects make them fail. A test-only hook between the notification resolve and
/// the route write drives the B5/B6/B10 interleavings.
mod irq_auth1_baseline {
    use super::*;
    use crate::kernel::boot::ipc_state::irq_bind_phase_hook;
    use crate::kernel::capabilities::CapRights;

    const R1: u64 = 1;
    const R2: u64 = 2;

    fn run_as(state: &mut KernelState, tid: u64) {
        if state.current_tid() == Some(tid) {
            return;
        }
        if !state.task_present_in_any_runqueue(tid) {
            state.enqueue_current_cpu(tid).expect("enqueue");
        }
        for _ in 0..32 {
            if state.current_tid() == Some(tid) {
                return;
            }
            if state.current_tid().is_none() {
                let _ = state.dispatch_next_task();
            } else {
                let _ = state.yield_current();
            }
        }
        assert_eq!(state.current_tid(), Some(tid));
    }

    fn route(state: &KernelState, line: u16) -> Option<usize> {
        state.with_ipc_state(|ipc| ipc.irq_routes[line as usize])
    }

    fn generation(state: &KernelState, idx: usize) -> u64 {
        state.with_ipc_state(|ipc| ipc.notification_generations[idx])
    }

    fn revoke_all(state: &mut KernelState, caps: &[(u64, CapId)]) {
        for (tid, cap) in caps {
            let cnode = state.task_cnode(*tid).expect("cnode");
            let _ = state.revoke_capability_in_cnode(cnode, *cap);
        }
    }

    /// B1 — SIGNAL on a Notification and no IRQ capability at all.
    #[test]
    fn b1_binding_requires_irq_authority() {
        let mut state = Bootstrap::init().expect("init");
        let (_idx, sig, _recv) = state.create_notification(4).expect("n");
        let got = state.bind_irq_notification(9, sig);
        assert!(got.is_err(), "a Notification capability alone bound line 9: {got:?}");
        assert_eq!(route(&state, 9), None);
    }

    /// B2 — authority for line X, binding line Y.
    #[test]
    fn b2_authority_for_one_line_does_not_bind_another() {
        let mut state = Bootstrap::init().expect("init");
        let _irq_x = state.mint_irq_cap(9).expect("irq 9");
        let (_idx, sig, _recv) = state.create_notification(4).expect("n");
        let got = state.bind_irq_notification(10, sig);
        assert!(got.is_err(), "authority for line 9 bound line 10: {got:?}");
        assert_eq!(route(&state, 10), None);
    }

    /// B3 — an occupied line is redirected by another task.
    #[test]
    fn b3_an_occupied_line_is_not_redirected() {
        let mut state = Bootstrap::init().expect("init");
        state.register_task(R2).expect("r2");
        let (n1, sig1, _r1) = state.create_notification(4).expect("n1");
        state.bind_irq_notification(9, sig1).expect("bind n1");
        run_as(&mut state, R2);
        let (n2, sig2, _r2) = state.create_notification(4).expect("n2");
        assert_ne!(n1, n2);
        let got = state.bind_irq_notification(9, sig2);
        assert!(got.is_err(), "line 9 was redirected: {got:?}");
        assert_eq!(route(&state, 9), Some(n1), "the N1 route is unchanged");
    }

    /// B4 — the same binding repeated is idempotent.
    #[test]
    fn b4_rebinding_the_same_incarnation_is_idempotent() {
        let mut state = Bootstrap::init().expect("init");
        let (n1, sig1, _r1) = state.create_notification(4).expect("n1");
        state.bind_irq_notification(9, sig1).expect("bind");
        state.bind_irq_notification(9, sig1).expect("rebind");
        assert_eq!(route(&state, 9), Some(n1));
    }

    /// B5 — the binder's incarnation is destroyed and its slot reused between the resolve and
    /// the route write: the newer object must not be bound.
    #[test]
    fn b5_a_stale_notification_does_not_bind_the_newer_object() {
        let mut state = Bootstrap::init().expect("init");
        let (n1, sig1, recv1) = state.create_notification(4).expect("n1");
        let g1 = generation(&state, n1);
        irq_bind_phase_hook::arm(alloc::boxed::Box::new(move |s: &mut KernelState| {
            revoke_all(s, &[(0, sig1), (0, recv1)]);
            let (n2, _s2, _r2) = s.create_notification(4).expect("n2");
            assert_eq!(n2, n1, "the slot is reused");
        }));
        let got = state.bind_irq_notification(9, sig1);
        assert_ne!(generation(&state, n1), g1, "a newer incarnation occupies the slot");
        assert_eq!(route(&state, 9), None, "the newer object was bound by a stale cap: {got:?}");
    }

    /// B6 — destruction interleaves with the bind: the result is a live binding or none.
    #[test]
    fn b6_concurrent_teardown_leaves_no_stale_route() {
        let mut state = Bootstrap::init().expect("init");
        let (n1, sig1, recv1) = state.create_notification(4).expect("n1");
        irq_bind_phase_hook::arm(alloc::boxed::Box::new(move |s: &mut KernelState| {
            revoke_all(s, &[(0, sig1), (0, recv1)]);
        }));
        let _ = state.bind_irq_notification(9, sig1);
        let live = state.with_ipc_state(|ipc| ipc.notifications[n1].is_some());
        assert!(!live);
        assert_eq!(route(&state, 9), None, "a route names a destroyed incarnation");
    }

    /// B7 — a kernel-owned line (x86_64: line 0 decodes as the LAPIC timer vector).
    #[test]
    fn b7_a_kernel_owned_line_cannot_be_bound() {
        let mut state = Bootstrap::init().expect("init");
        let _irq0 = state.mint_irq_cap(0).expect("irq 0");
        let (_idx, sig, _recv) = state.create_notification(4).expect("n");
        let got = state.bind_irq_notification(0, sig);
        assert!(got.is_err(), "the timer line was bound: {got:?}");
        assert_eq!(route(&state, 0), None);
    }

    /// B8 — revoking the binding authority ends the binding.
    #[test]
    fn b8_revoking_the_binding_authority_clears_the_route() {
        let mut state = Bootstrap::init().expect("init");
        let irq = state.mint_irq_cap(9).expect("irq 9");
        let (n1, sig, _recv) = state.create_notification(4).expect("n");
        state.bind_irq_notification(9, sig).expect("bind");
        assert_eq!(route(&state, 9), Some(n1));
        revoke_all(&mut state, &[(0, irq)]);
        assert_eq!(route(&state, 9), None, "the route outlived the authority that made it");
    }

    /// B9 — a failed bind leaves the existing route exactly as it was.
    #[test]
    fn b9_a_failed_bind_changes_nothing() {
        let mut state = Bootstrap::init().expect("init");
        let (n1, sig1, _r1) = state.create_notification(4).expect("n1");
        state.bind_irq_notification(9, sig1).expect("bind n1");
        let (_e, _send, ep_recv) = state.create_endpoint(2).expect("ep");
        assert!(state.bind_irq_notification(9, ep_recv).is_err());
        assert_eq!(route(&state, 9), Some(n1));
        let (_n2, sig2, _r2) = state.create_notification(4).expect("n2");
        let got = state.bind_irq_notification(9, sig2);
        assert!(got.is_err(), "the occupied bind succeeded: {got:?}");
        assert_eq!(route(&state, 9), Some(n1), "the route changed");
    }

    /// B10 — a replacement incarnation never inherits the previous one's route.
    #[test]
    fn b10_a_reused_slot_inherits_no_route() {
        let mut state = Bootstrap::init().expect("init");
        let (n1, sig1, recv1) = state.create_notification(4).expect("n1");
        irq_bind_phase_hook::arm(alloc::boxed::Box::new(move |s: &mut KernelState| {
            revoke_all(s, &[(0, sig1), (0, recv1)]);
        }));
        let _ = state.bind_irq_notification(9, sig1);
        let (n2, _sig2, recv2) = state.create_notification(4).expect("n2");
        assert_eq!(n2, n1, "the slot is reused");
        state.route_external_irq(9).expect("irq");
        assert!(
            state.try_ipc_recv(recv2).expect("probe").is_none(),
            "the replacement received the old binding's interrupt"
        );
    }
}
