// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

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
    .weak _start
    .type _start,@function
_start:
    la sp, boot_stack_riscv64_end
    .weak yarm_kernel_main
    call yarm_kernel_main
1:
    wfi
    j 1b
    "#
);
