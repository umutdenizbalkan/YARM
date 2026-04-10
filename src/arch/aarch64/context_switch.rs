// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use crate::kernel::task::ArchSwitchContext;
#[cfg(test)]
use core::sync::atomic::{AtomicUsize, Ordering};

#[inline]
pub fn switch_frames(
    prev: &mut ArchSwitchContext,
    next: &ArchSwitchContext,
    next_kernel_stack_top: Option<u64>,
) {
    prev.set_stack_ptr(next.stack_ptr());
    prev.set_instruction_ptr(next.instruction_ptr());
    if let Some(stack_top) = next_kernel_stack_top {
        prev.set_stack_ptr(stack_top as usize);
    }
    #[cfg(test)]
    {
        SWITCH_CALLS.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
static SWITCH_CALLS: AtomicUsize = AtomicUsize::new(0);

#[cfg(test)]
pub fn reset_switch_call_count_for_test() {
    SWITCH_CALLS.store(0, Ordering::Relaxed);
}

#[cfg(test)]
pub fn switch_call_count_for_test() -> usize {
    SWITCH_CALLS.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switch_frames_applies_next_stack_and_instruction() {
        let mut prev = ArchSwitchContext::default();
        let mut next = ArchSwitchContext::default();
        next.set_stack_ptr(0x1000);
        next.set_instruction_ptr(0x2000);

        switch_frames(&mut prev, &next, None);

        assert_eq!(prev.stack_ptr(), 0x1000);
        assert_eq!(prev.instruction_ptr(), 0x2000);
    }

    #[test]
    fn switch_frames_prefers_explicit_kernel_stack_top() {
        let mut prev = ArchSwitchContext::default();
        let mut next = ArchSwitchContext::default();
        next.set_stack_ptr(0x1111);
        next.set_instruction_ptr(0x2222);

        switch_frames(&mut prev, &next, Some(0x3333));

        assert_eq!(prev.stack_ptr(), 0x3333);
        assert_eq!(prev.instruction_ptr(), 0x2222);
    }
}
