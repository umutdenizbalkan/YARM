use crate::kernel::task::KernelSwitchFrame;

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
unsafe extern "C" {
    fn yarm_x86_switch_frame(prev: *mut KernelSwitchFrame, next: *const KernelSwitchFrame);
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
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
pub fn switch_frames(prev: &mut KernelSwitchFrame, next: &KernelSwitchFrame) {
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
    unsafe {
        yarm_x86_switch_frame(prev as *mut _, next as *const _);
    }

    #[cfg(any(feature = "hosted-dev", not(target_arch = "x86_64")))]
    {
        let _ = (prev, next);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernel_switch_frame_layout_matches_asm_offsets() {
        let frame = KernelSwitchFrame::default();
        let base = &frame as *const _ as usize;
        assert_eq!(core::mem::offset_of!(KernelSwitchFrame, stack_ptr), 0);
        assert_eq!(core::mem::offset_of!(KernelSwitchFrame, instruction_ptr), 8);
        assert_eq!(core::mem::offset_of!(KernelSwitchFrame, rbx), 16);
        assert_eq!(core::mem::offset_of!(KernelSwitchFrame, rbp), 24);
        assert_eq!(core::mem::offset_of!(KernelSwitchFrame, r12), 32);
        assert_eq!(core::mem::offset_of!(KernelSwitchFrame, r13), 40);
        assert_eq!(core::mem::offset_of!(KernelSwitchFrame, r14), 48);
        assert_eq!(core::mem::offset_of!(KernelSwitchFrame, r15), 56);
        let end = base + core::mem::size_of::<KernelSwitchFrame>();
        assert_eq!(end - base, 64);
    }
}
