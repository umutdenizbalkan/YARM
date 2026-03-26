#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
use core::arch::global_asm;

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
global_asm!(
    r#"
    .section .bss.bootstack,"aw",@nobits
    .align 16
boot_stack_aarch64:
    .skip 16384
boot_stack_aarch64_end:

    .section .text.boot,"ax",@progbits
    .weak _start
    .type _start,%function
_start:
    adrp x0, boot_stack_aarch64_end
    add x0, x0, :lo12:boot_stack_aarch64_end
    mov sp, x0
    .weak yarm_kernel_main
    bl yarm_kernel_main
1:
    wfe
    b 1b
    "#
);
