#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
use core::arch::global_asm;

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
global_asm!(
    r#"
    .section .bss.bootstack,"aw",@nobits
    .align 16
boot_stack_riscv64:
    .skip 16384
boot_stack_riscv64_end:

    .section .text.boot,"ax",@progbits
    .globl _start
    .type _start,@function
_start:
    la sp, boot_stack_riscv64_end
    call yarm_kernel_main_riscv64
1:
    wfi
    j 1b
    "#
);

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
unsafe extern "C" {
    fn yarm_kernel_main() -> !;
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
#[unsafe(no_mangle)]
pub extern "C" fn yarm_kernel_main_riscv64() -> ! {
    unsafe { yarm_kernel_main() }
}
