
mod sta1_explore {
    use super::*;
    use crate::kernel::task::{TaskStatus, WaitReason};

    /// R5 probe: an in-lock blocking send carrying a transferred capability, through the real
    /// IpcSend syscall. Does the parked waiter still own a live envelope?
    #[test]
    fn explore_inlock_cap_bearing_blocking_send_envelope() {
        let mut state = Bootstrap::init().expect("init");
        state.set_timer_for_test(Timer::new(1));
        let (eidx, send_cap, _recv) = state
            .create_endpoint_with_mode(1, EndpointMode::Synchronous)
            .expect("ep");
        let (_e2, carried_cap, _r2) = state.create_endpoint(2).expect("ep2");
        let tid = state.current_tid().expect("current");
        std::println!("current tid={tid} asid={:?}", state.task_asid(tid));
        let mut frame = TrapFrame::new(
            crate::kernel::syscall::Syscall::IpcSend as usize,
            [send_cap.0 as usize, 0, 0, 0, 100, carried_cap.0 as usize],
        );
        let r = state.handle_trap(Trap::Syscall, Some(&mut frame));
        std::println!("handle_trap={r:?} frame={:?}", frame);
        std::println!("status={:?}", state.task_status(tid));
        let waiter = state.with_ipc_state(|ipc| ipc.endpoint_sender_waiters[eidx][0]);
        std::println!("waiter={:?}", waiter.map(|w| (w.tid.0, w.send_generation, w.msg.transferred_cap())));
        if let Some(handle) = waiter.and_then(|w| w.msg.transferred_cap()) {
            let idx = (handle.0 & 0xFFFF) as usize;
            let live = state.with_ipc_state(|ipc| {
                ipc.transfer_envelopes[idx].is_some()
                    && ipc.transfer_envelope_generations[idx] == (handle.0 >> 16)
            });
            std::println!("envelope_live={live}");
        }
        let _ = (TaskStatus::Runnable, WaitReason::Futex(VirtAddr(0)));
    }
}
