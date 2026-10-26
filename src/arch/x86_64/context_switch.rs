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
71:
    fxsave [rdi + 64]
72:
    // KERNEL-NOFP1: audited exception record (scripts/check-kernel-nofp.py).
    .pushsection .yarm_nofp_map, "", @progbits
    .balign 8
    .quad 0x3150464F4E4D5259
    .quad 71b
    .quad 72b
    .asciz "X86_SWITCH_SAVE"
    .balign 8
    .popsection

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

73:
    fxrstor [rsi + 64]
74:
    // KERNEL-NOFP1: audited exception record (scripts/check-kernel-nofp.py).
    .pushsection .yarm_nofp_map, "", @progbits
    .balign 8
    .quad 0x3150464F4E4D5259
    .quad 73b
    .quad 74b
    .asciz "X86_SWITCH_RESTORE"
    .balign 8
    .popsection

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
pub fn switch_frames(
    prev: &mut ArchSwitchContext,
    next: &ArchSwitchContext,
    next_kernel_stack_top: Option<u64>,
) {
    #[cfg(test)]
    {
        SWITCH_CALLS.fetch_add(1, Ordering::Relaxed);
    }
    #[cfg(target_arch = "x86_64")]
    if let Some(rsp0) = next_kernel_stack_top {
        crate::arch::x86_64::descriptor_tables::refresh_boot_tss_rsp0(rsp0);
    }

    #[cfg(all(not(test), target_arch = "x86_64"))]
    unsafe {
        yarm_x86_switch_frame(prev as *mut _, next as *const _);
    }

    #[cfg(any(test, not(target_arch = "x86_64")))]
    {
        let _ = (prev, next, next_kernel_stack_top);
    }
}

/// KERNEL-NOFP1: an audited exception (`X86_SWITCH_FRAME_INIT`); never inlined, so the image
/// carries exactly one instance.
#[inline(never)]
pub fn initialize_frame_fpu_state(frame: &mut ArchSwitchContext) {
    #[cfg(all(not(test), target_arch = "x86_64"))]
    unsafe {
        core::arch::asm!(
            "72:",
            "fninit",
            "fxsave [{}]",
            "73:",
            crate::arch::nofp_map_record!("72b", "73b", "X86_SWITCH_FRAME_INIT"),
            in(reg) ((frame as *mut ArchSwitchContext as *mut u8).add(64)),
            options(nostack)
        );
    }

    #[cfg(any(test, not(target_arch = "x86_64")))]
    {
        let _ = frame;
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
            ArchSwitchContext::WORDS * 8 + ArchSwitchContext::FXSAVE_BYTES
        );
        frame.set_stack_ptr(0xAAA0);
        frame.set_instruction_ptr(0xBBB0);
        assert_eq!(frame.stack_ptr(), 0xAAA0);
        assert_eq!(frame.instruction_ptr(), 0xBBB0);
    }
}
