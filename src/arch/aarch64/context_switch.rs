// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use crate::kernel::task::ArchSwitchContext;
#[cfg(test)]
use core::sync::atomic::{AtomicUsize, Ordering};

#[cfg(all(not(test), target_arch = "aarch64"))]
unsafe extern "C" {
    fn yarm_aarch64_switch_frame(prev: *mut ArchSwitchContext, next: *const ArchSwitchContext);
}

#[cfg(all(not(test), target_arch = "aarch64"))]
#[unsafe(no_mangle)]
extern "C" fn yarm_aarch64_ctxsw_marker_0() {
    crate::arch::aarch64::console::write_line("YARM_AARCH64_CTXSW C0");
}

#[cfg(all(not(test), target_arch = "aarch64"))]
core::arch::global_asm!(
    r#"
    .section .text, "ax", @progbits
    .global yarm_aarch64_switch_frame
    .type yarm_aarch64_switch_frame, %function
yarm_aarch64_switch_frame:
    sub sp, sp, #16
    stp x0, x1, [sp, #0]
    bl yarm_aarch64_ctxsw_marker_0
    ldp x0, x1, [sp, #0]
    add sp, sp, #16

    // Save callee-saved SIMD q8..q15 into ArchSwitchContext::fxsave area.
    stp q8, q9, [x0, #128]
    stp q10, q11, [x0, #160]
    stp q12, q13, [x0, #192]
    stp q14, q15, [x0, #224]

    // Save callee-saved GPRs x19..x30.
    str x25, [x0, #64]
    str x26, [x0, #72]
    str x27, [x0, #80]
    str x28, [x0, #88]
    str x29, [x0, #96]
    str x30, [x0, #104]
    str x19, [x0, #16]
    str x20, [x0, #24]
    str x21, [x0, #32]
    str x22, [x0, #40]
    str x23, [x0, #48]
    str x24, [x0, #56]

    // Save current stack pointer and continuation PC.
    mov x2, sp
    str x2, [x0, #0]
    str x30, [x0, #8]

    // Restore callee-saved SIMD q8..q15 from next frame.
    ldp q8, q9, [x1, #128]
    ldp q10, q11, [x1, #160]
    ldp q12, q13, [x1, #192]
    ldp q14, q15, [x1, #224]

    // Restore callee-saved GPRs x19..x30.
    ldr x25, [x1, #64]
    ldr x26, [x1, #72]
    ldr x27, [x1, #80]
    ldr x28, [x1, #88]
    ldr x29, [x1, #96]
    ldr x30, [x1, #104]
    ldr x19, [x1, #16]
    ldr x20, [x1, #24]
    ldr x21, [x1, #32]
    ldr x22, [x1, #40]
    ldr x23, [x1, #48]
    ldr x24, [x1, #56]

    // Switch to next stack and branch to next continuation.
    ldr x2, [x1, #0]
    mov sp, x2
    ldr x2, [x1, #8]
    br x2
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

    #[cfg(all(not(test), target_arch = "aarch64"))]
    unsafe {
        if let Some(stack_top) = next_kernel_stack_top {
            let mut next_override = *next;
            next_override.set_stack_ptr(stack_top as usize);
            yarm_aarch64_switch_frame(prev as *mut _, &next_override as *const _);
        } else {
            yarm_aarch64_switch_frame(prev as *mut _, next as *const _);
        }
    }

    #[cfg(any(test, not(target_arch = "aarch64")))]
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
