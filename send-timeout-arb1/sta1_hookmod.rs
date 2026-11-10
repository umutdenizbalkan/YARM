
/// SEND-TIMEOUT-ARB1: test-only phase handshakes at the two windows a concurrent CPU can land in —
/// between a broad block's task-block (Phase B) and its waiter publication (Phase C), and between
/// the send-timeout drain's two domain claims.
#[cfg(test)]
pub(crate) mod send_arb_phase_hook {
    extern crate std;
    use core::cell::RefCell;
    type BlockHook = alloc::boxed::Box<dyn FnMut(&mut crate::kernel::boot::KernelState)>;
    type DrainHook = alloc::boxed::Box<dyn FnMut()>;
    std::thread_local! {
        static BETWEEN_BLOCK_AND_PUBLISH: RefCell<Option<BlockHook>> = const { RefCell::new(None) };
        static BETWEEN_DRAIN_PHASES: RefCell<Option<DrainHook>> = const { RefCell::new(None) };
    }
    pub(crate) fn arm_between_block_and_publish(hook: BlockHook) {
        BETWEEN_BLOCK_AND_PUBLISH.with(|h| *h.borrow_mut() = Some(hook));
    }
    pub(crate) fn run_between_block_and_publish(state: &mut crate::kernel::boot::KernelState) {
        let hook = BETWEEN_BLOCK_AND_PUBLISH.with(|h| h.borrow_mut().take());
        if let Some(mut hook) = hook {
            hook(state);
        }
    }
    pub(crate) fn arm_between_drain_phases(hook: DrainHook) {
        BETWEEN_DRAIN_PHASES.with(|h| *h.borrow_mut() = Some(hook));
    }
    pub(crate) fn run_between_drain_phases() {
        let hook = BETWEEN_DRAIN_PHASES.with(|h| h.borrow_mut().take());
        if let Some(mut hook) = hook {
            hook();
        }
    }
}
