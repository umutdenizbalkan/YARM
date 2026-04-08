// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use crate::kernel::task::ArchSwitchContext;
#[cfg(test)]
use core::sync::atomic::{AtomicUsize, Ordering};

#[cfg(all(not(test), target_arch = "x86_64"))]
unsafe extern "C" {
    fn yarm_x86_switch_frame(prev: *mut ArchSwitchContext, next: *const ArchSwitchContext);
}

#[cfg(all(not(test), target_arch = "x86_64"))]
core::arch::global_asm!(
    r#"
    .section .text, "ax", @progbits
    .global yarm_x86_switch_frame
    .type yarm_x86_switch_frame, @function
yarm_x86_switch_frame:
    mov [rdi + 16], rbx
    mov [rdi + 24], rbp
    mov [rdi + 32], r12
    mov [rdi + 40], r13
    mov [rdi + 48], r14
    mov [rdi + 56], r15

    mov rax, [rsp]
    mov [rdi + 8], rax
    lea rax, [rsp + 8]
    mov [rdi + 0], rax

    mov rbx, [rsi + 16]
    mov rbp, [rsi + 24]
    mov r12, [rsi + 32]
    mov r13, [rsi + 40]
    mov r14, [rsi + 48]
    mov r15, [rsi + 56]
    mov rsp, [rsi + 0]
    jmp [rsi + 8]
"#
);

#[inline]
pub fn switch_frames(prev: &mut ArchSwitchContext, next: &ArchSwitchContext) {
    #[cfg(test)]
    {
        SWITCH_CALLS.fetch_add(1, Ordering::Relaxed);
    }
    #[cfg(all(not(test), target_arch = "x86_64"))]
    unsafe {
        yarm_x86_switch_frame(prev as *mut _, next as *const _);
    }

    #[cfg(any(test, not(target_arch = "x86_64")))]
    {
        let _ = (prev, next);
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
    fn kernel_switch_frame_layout_matches_asm_offsets() {
        let mut frame = ArchSwitchContext::default();
        assert_eq!(
            core::mem::size_of::<ArchSwitchContext>(),
            ArchSwitchContext::WORDS * 8
        );
        frame.set_stack_ptr(0xAAA0);
        frame.set_instruction_ptr(0xBBB0);
        assert_eq!(frame.stack_ptr(), 0xAAA0);
        assert_eq!(frame.instruction_ptr(), 0xBBB0);
    }
}
