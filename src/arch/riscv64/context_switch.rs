// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use crate::kernel::task::ArchSwitchContext;
#[cfg(test)]
use core::sync::atomic::{AtomicUsize, Ordering};

#[cfg(all(not(test), target_arch = "riscv64"))]
unsafe extern "C" {
    fn yarm_riscv64_switch_frame(prev: *mut ArchSwitchContext, next: *const ArchSwitchContext);
}

#[cfg(all(not(test), target_arch = "riscv64"))]
core::arch::global_asm!(
    r#"
    .section .text, "ax", @progbits
    .global yarm_riscv64_switch_frame
    .type yarm_riscv64_switch_frame, @function
yarm_riscv64_switch_frame:
    // Save callee-saved integer registers.
    sd s0, 16(a0)
    sd s1, 24(a0)
    sd s2, 32(a0)
    sd s3, 40(a0)
    sd s4, 48(a0)
    sd s5, 56(a0)
    sd s6, 64(a0)
    sd s7, 72(a0)
    sd s8, 80(a0)
    sd s9, 88(a0)
    sd s10, 96(a0)
    sd s11, 104(a0)
    sd ra, 8(a0)
    sd sp, 0(a0)

    // Restore callee-saved integer registers.
    ld s0, 16(a1)
    ld s1, 24(a1)
    ld s2, 32(a1)
    ld s3, 40(a1)
    ld s4, 48(a1)
    ld s5, 56(a1)
    ld s6, 64(a1)
    ld s7, 72(a1)
    ld s8, 80(a1)
    ld s9, 88(a1)
    ld s10, 96(a1)
    ld s11, 104(a1)
    ld ra, 8(a1)
    ld sp, 0(a1)
    jr ra
"#
);

#[inline]
pub fn switch_frames(
    prev: &mut ArchSwitchContext,
    next: &ArchSwitchContext,
    next_kernel_stack_top: Option<u64>,
) {
    #[cfg(test)]
    {
        SWITCH_CALLS.fetch_add(1, Ordering::Relaxed);
    }

    #[cfg(all(not(test), target_arch = "riscv64"))]
    unsafe {
        if let Some(stack_top) = next_kernel_stack_top {
            let mut next_override = *next;
            next_override.set_stack_ptr(stack_top as usize);
            yarm_riscv64_switch_frame(prev as *mut _, &next_override as *const _);
        } else {
            yarm_riscv64_switch_frame(prev as *mut _, next as *const _);
        }
    }

    #[cfg(any(test, not(target_arch = "riscv64")))]
    {
        prev.set_stack_ptr(next.stack_ptr());
        prev.set_instruction_ptr(next.instruction_ptr());
        if let Some(stack_top) = next_kernel_stack_top {
            prev.set_stack_ptr(stack_top as usize);
        }
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
