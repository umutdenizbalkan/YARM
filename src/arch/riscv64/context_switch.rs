// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use crate::kernel::task::ArchSwitchContext;
#[cfg(test)]
use core::sync::atomic::{AtomicUsize, Ordering};

#[inline]
pub fn switch_frames(_prev: &mut ArchSwitchContext, _next: &ArchSwitchContext) {
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
