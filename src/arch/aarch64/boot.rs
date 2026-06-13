// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
use core::arch::global_asm;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
use core::fmt::Write;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
global_asm!(
    r#"
    .section .bss.bootstack,"aw",@nobits
    .align 16
boot_stack_aarch64:
    .skip 0x01000000
boot_stack_aarch64_end:
    .align 16
exc_stack_aarch64:
    .skip 65536
exc_stack_aarch64_end:
    .align 16
secondary_boot_stacks:
    .skip 0x00100000
secondary_boot_stacks_end:

    .section .text.boot,"ax",@progbits
    .weak _start
    .type _start,%function
_start:
    mov x20, x0
    mov x21, x1
    mov x22, x2
    mov x23, x3
    adrp x9, boot_stack_aarch64_end
    add x9, x9, :lo12:boot_stack_aarch64_end
    mov sp, x9
    bl yarm_aarch64_marker_raw_entry
    bl yarm_aarch64_marker_raw_after_marker
    bl yarm_aarch64_marker_dtb_x0
    bl yarm_aarch64_marker_bss_clear_begin
    adrp x1, __bss_start
    add x1, x1, :lo12:__bss_start
    adrp x2, __bss_end
    add x2, x2, :lo12:__bss_end
    sub x2, x2, x1
    cbz x2, 2f
1:
    str xzr, [x1], #8
    subs x2, x2, #8
    b.gt 1b
2:
    bl yarm_aarch64_marker_bss_clear_done
    adrp x9, boot_stack_aarch64_end
    add x9, x9, :lo12:boot_stack_aarch64_end
    mov sp, x9
    bl yarm_aarch64_marker_stack_ready
    bl yarm_aarch64_marker_before_el1
    bl yarm_aarch64_enter_el1_if_needed
    bl yarm_aarch64_marker_after_el1
    bl yarm_aarch64_enable_fp_simd
    bl yarm_aarch64_marker_before_rust
    mov x0, x20
    mov x1, x21
    mov x2, x22
    mov x3, x23
    bl yarm_aarch64_select_early_console
    bl yarm_aarch64_boot_breadcrumb_b0
    bl yarm_aarch64_boot_marker_start
    bl yarm_aarch64_boot_breadcrumb_b1
    bl yarm_aarch64_boot_breadcrumb_b2
    bl yarm_aarch64_boot_breadcrumb_b3
    mov x0, x20
    .weak yarm_kernel_main
    bl yarm_kernel_main
3:
    wfe
    b 3b

    .global yarm_aarch64_enter_el1_if_needed
    .type yarm_aarch64_enter_el1_if_needed,%function
yarm_aarch64_enter_el1_if_needed:
    mrs x0, CurrentEL
    lsr x0, x0, #2
    cmp x0, #0x2
    b.ne 2f
    // Do not inherit EL2 trap/control bits from reset/firmware state.
    // Program a known baseline: EL1 runs AArch64 (RW=1), everything else clear.
    mov x1, #(1 << 31)
    msr HCR_EL2, x1
    // Clear common EL2 trap controls so EL1 sysreg/MMU setup does not trap
    // unexpectedly during early bring-up.
    msr CPTR_EL2, xzr
    msr HSTR_EL2, xzr
    msr MDCR_EL2, xzr
    mrs x1, CNTHCTL_EL2
    orr x1, x1, #3
    msr CNTHCTL_EL2, x1
    msr CNTVOFF_EL2, xzr
    mov x1, #(3 << 20)
    msr CPACR_EL1, x1
    ldr x1, =0x30D00800
    msr SCTLR_EL1, x1
    isb
    mov x2, sp
    msr SP_EL1, x2
    adr x1, 1f
    msr ELR_EL2, x1
    mov x1, #0x3C5
    msr SPSR_EL2, x1
    isb
    eret
1:
2:
    ret
    "#
);

#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-stage1"
))]
global_asm!(
    r#"
    .section .text.boot,"ax",@progbits
    .macro rpi5_fixed_marker name, label
    .global \name
    .type \name,%function
\name:
    stp x9, x30, [sp, #-16]!
    adr x9, \label
    bl .Lrpi5_emergency_write_x9
    ldp x9, x30, [sp], #16
    ret
    .endm

    rpi5_fixed_marker yarm_aarch64_marker_raw_entry, .Lrpi5_raw_entry
    rpi5_fixed_marker yarm_aarch64_marker_raw_after_marker, .Lrpi5_raw_after_marker
    rpi5_fixed_marker yarm_aarch64_marker_bss_clear_begin, .Lrpi5_bss_clear_begin
    rpi5_fixed_marker yarm_aarch64_marker_bss_clear_done, .Lrpi5_bss_clear_done
    rpi5_fixed_marker yarm_aarch64_marker_stack_ready, .Lrpi5_stack_ready
    rpi5_fixed_marker yarm_aarch64_marker_before_el1, .Lrpi5_before_el1
    rpi5_fixed_marker yarm_aarch64_marker_after_el1, .Lrpi5_after_el1
    rpi5_fixed_marker yarm_aarch64_marker_before_rust, .Lrpi5_before_rust

    .type yarm_aarch64_marker_dtb_x0,%function
yarm_aarch64_marker_dtb_x0:
    stp x9, x10, [sp, #-16]!
    stp x11, x12, [sp, #-16]!
    stp x13, x14, [sp, #-16]!
    stp x15, x30, [sp, #-16]!
    adr x9, .Lrpi5_dtb_x0_prefix
    bl .Lrpi5_emergency_write_x9
    mov x10, x20
    mov x11, #60
.Lrpi5_dtb_x0_hex:
    lsr x12, x10, x11
    and x12, x12, #0xf
    cmp x12, #10
    add x13, x12, #'0'
    add x14, x12, #('a' - 10)
    csel x15, x13, x14, lo
    bl .Lrpi5_emergency_write_byte_x15
    subs x11, x11, #4
    b.ge .Lrpi5_dtb_x0_hex
    mov x15, #'\r'
    bl .Lrpi5_emergency_write_byte_x15
    mov x15, #'\n'
    bl .Lrpi5_emergency_write_byte_x15
    ldp x15, x30, [sp], #16
    ldp x13, x14, [sp], #16
    ldp x11, x12, [sp], #16
    ldp x9, x10, [sp], #16
    ret

    .global yarm_aarch64_rpi5_emergency_write
    .type yarm_aarch64_rpi5_emergency_write,%function
yarm_aarch64_rpi5_emergency_write:
    stp x9, x10, [sp, #-16]!
    stp x11, x12, [sp, #-16]!
    stp x13, x15, [sp, #-16]!
    stp x0, x30, [sp, #-16]!
    mov x9, x0
    bl .Lrpi5_emergency_write_x9
    ldp x0, x30, [sp], #16
    ldp x13, x15, [sp], #16
    ldp x11, x12, [sp], #16
    ldp x9, x10, [sp], #16
    ret

    .global yarm_aarch64_rpi5_emergency_write_hex
    .type yarm_aarch64_rpi5_emergency_write_hex,%function
yarm_aarch64_rpi5_emergency_write_hex:
    stp x0, x1, [sp, #-16]!
    stp x9, x10, [sp, #-16]!
    stp x11, x12, [sp, #-16]!
    stp x13, x14, [sp, #-16]!
    stp x15, x30, [sp, #-16]!
    mov x9, x0
    bl .Lrpi5_emergency_write_x9
    mov x10, x1
    mov x11, #60
.Lrpi5_emergency_hex:
    lsr x12, x10, x11
    and x12, x12, #0xf
    cmp x12, #10
    add x13, x12, #'0'
    add x14, x12, #('a' - 10)
    csel x15, x13, x14, lo
    bl .Lrpi5_emergency_write_byte_x15
    subs x11, x11, #4
    b.ge .Lrpi5_emergency_hex
    mov x15, #'\r'
    bl .Lrpi5_emergency_write_byte_x15
    mov x15, #'\n'
    bl .Lrpi5_emergency_write_byte_x15
    ldp x15, x30, [sp], #16
    ldp x13, x14, [sp], #16
    ldp x11, x12, [sp], #16
    ldp x9, x10, [sp], #16
    ldp x0, x1, [sp], #16
    ret

.Lrpi5_emergency_write_x9:
    stp x10, x11, [sp, #-16]!
    stp x12, x13, [sp, #-16]!
    // The Pi 5 Stage 1 DTB parser translates the preferred
    // /soc@107c000000/serial@7d001000 PL011 to 0x107d001000.  This raw
    // pre-BSS path deliberately uses that same physical base; it does not
    // consult console globals or attempt to configure clocks/divisors.
    movz x10, #0x1000
    movk x10, #0x7d00, lsl #16
    movk x10, #0x0010, lsl #32
.Lrpi5_emergency_write_next:
    ldrb w11, [x9], #1
    cbz w11, .Lrpi5_raw_entry_done
    movz x12, #0x0010, lsl #16
.Lrpi5_raw_entry_wait:
    ldr w13, [x10, #0x18]
    tbz w13, #5, .Lrpi5_raw_entry_send
    subs x12, x12, #1
    b.ne .Lrpi5_raw_entry_wait
    b .Lrpi5_raw_entry_done
.Lrpi5_raw_entry_send:
    dsb sy
    str w11, [x10]
    dsb sy
    b .Lrpi5_emergency_write_next
.Lrpi5_raw_entry_done:
    ldp x12, x13, [sp], #16
    ldp x10, x11, [sp], #16
    ret

// Writes x15 while preserving the pointer/value registers used by callers.
.Lrpi5_emergency_write_byte_x15:
    stp x10, x12, [sp, #-16]!
    stp x13, x30, [sp, #-16]!
    movz x10, #0x1000
    movk x10, #0x7d00, lsl #16
    movk x10, #0x0010, lsl #32
    movz x12, #0x0010, lsl #16
1:
    ldr w13, [x10, #0x18]
    tbz w13, #5, 2f
    subs x12, x12, #1
    b.ne 1b
    b 3f
2:
    dsb sy
    str w15, [x10]
    dsb sy
3:
    ldp x13, x30, [sp], #16
    ldp x10, x12, [sp], #16
    ret

    .section .rodata.rpi5_raw_entry,"a",@progbits
    .balign 8
.Lrpi5_raw_entry:
    .asciz "RPI5_RAW_ENTRY\r\n"
.Lrpi5_raw_after_marker:
    .asciz "RPI5_RAW_AFTER_MARKER\r\n"
.Lrpi5_dtb_x0_prefix:
    .asciz "RPI5_DTB_X0 value=0x"
.Lrpi5_bss_clear_begin:
    .asciz "RPI5_BSS_CLEAR_BEGIN\r\n"
.Lrpi5_bss_clear_done:
    .asciz "RPI5_BSS_CLEAR_DONE\r\n"
.Lrpi5_stack_ready:
    .asciz "RPI5_STACK_READY\r\n"
.Lrpi5_before_el1:
    .asciz "RPI5_BEFORE_EL1\r\n"
.Lrpi5_after_el1:
    .asciz "RPI5_AFTER_EL1\r\n"
.Lrpi5_before_rust:
    .asciz "RPI5_BEFORE_RUST\r\n"
    "#
);

#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    not(feature = "rpi5-stage1")
))]
global_asm!(
    r#"
    .section .text.boot,"ax",@progbits
    .macro rpi5_noop_marker name
    .global \name
    .type \name,%function
\name:
    ret
    .endm
    rpi5_noop_marker yarm_aarch64_marker_raw_entry
    rpi5_noop_marker yarm_aarch64_marker_raw_after_marker
    rpi5_noop_marker yarm_aarch64_marker_dtb_x0
    rpi5_noop_marker yarm_aarch64_marker_bss_clear_begin
    rpi5_noop_marker yarm_aarch64_marker_bss_clear_done
    rpi5_noop_marker yarm_aarch64_marker_stack_ready
    rpi5_noop_marker yarm_aarch64_marker_before_el1
    rpi5_noop_marker yarm_aarch64_marker_after_el1
    rpi5_noop_marker yarm_aarch64_marker_before_rust
    "#
);

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
global_asm!(
    r#"
    .section .text.boot,"ax",@progbits
    .global yarm_aarch64_secondary_entry
    .type yarm_aarch64_secondary_entry,%function
yarm_aarch64_secondary_entry:
    mrs x0, MPIDR_EL1
    and x0, x0, #0xff
    adrp x1, secondary_boot_stacks_end
    add x1, x1, :lo12:secondary_boot_stacks_end
    mov x2, #0x4000
    mul x3, x0, x2
    sub sp, x1, x3
    bl yarm_aarch64_secondary_cpu_boot
1:
    wfe
    b 1b
    "#
);

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
global_asm!(
    r#"
    .section .text.boot,"ax",@progbits
    .global yarm_aarch64_enter_user_mode_eret
    .type yarm_aarch64_enter_user_mode_eret,%function
yarm_aarch64_enter_user_mode_eret:
    sub sp, sp, #64
    stp x0, x1, [sp, #0]
    stp x2, x3, [sp, #16]
    stp x4, x5, [sp, #32]
    stp x6, x7, [sp, #48]
    bl yarm_aarch64_user_entry_marker_0
    ldp x9, x10, [sp, #0]
    ldp x11, x12, [sp, #16]
    ldp x13, x14, [sp, #32]
    ldp x15, x16, [sp, #48]
    add sp, sp, #64
    mov x19, x9
    mov x20, x10
    mov x21, x11
    mov x22, x12
    mov x23, x13
    mov x24, x14
    mov x25, x15
    mov x26, x16
    bl yarm_aarch64_user_entry_marker_before_sp_el0
    msr sp_el0, x20
    bl yarm_aarch64_user_entry_marker_before_elr
    mov x0, x19
    bl yarm_aarch64_write_elr_marker
    msr tpidr_el0, x26
    msr elr_el1, x19
    msr spsr_el1, xzr
    bl yarm_aarch64_user_entry_marker_1
    mov x0, x19
    mov x1, x20
    bl yarm_aarch64_before_eret_marker
    bl yarm_aarch64_user_entry_marker_before_eret
    isb
    mov x0, x21
    mov x1, x22
    mov x2, x23
    mov x3, x24
    mov x4, x25
    eret
    "#
);

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
const AARCH64_PTE_VALID: u64 = 1 << 0;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
const AARCH64_PTE_TABLE: u64 = 1 << 1;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
const AARCH64_PTE_AF: u64 = 1 << 10;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
const AARCH64_PTE_SH_INNER: u64 = 0b11 << 8;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
const AARCH64_PTE_ATTR_SHIFT: u64 = 2;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
const AARCH64_ATTRIDX_NORMAL_WB: u64 = 0;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
const AARCH64_ATTRIDX_DEVICE_NGNRE: u64 = 3;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
const AARCH64_BLOCK_2M: u64 = 2 * 1024 * 1024;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
const AARCH64_BLOCK_1G: u64 = 1024 * 1024 * 1024;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
const AARCH64_UART_MMIO_BASE: u64 = 0x0900_0000;

const AARCH64_TRAP_TRACE: bool = false;

#[inline(always)]
fn trap_trace_line(msg: &str) {
    if AARCH64_TRAP_TRACE {
        crate::arch::aarch64::console::write_line(msg);
    }
}
#[inline(always)]
fn trap_trace_log(args: core::fmt::Arguments) {
    if AARCH64_TRAP_TRACE {
        crate::yarm_log!("{}", args);
    }
}
macro_rules! boot_trace { ($($arg:tt)*) => { trap_trace_log(format_args!($($arg)*)) }; }

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
#[repr(C, align(4096))]
struct AlignedL2([u64; 512]);

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
#[repr(C, align(4096))]
struct AlignedL1([u64; 512]);

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
static mut BOOT_L1_TABLE: AlignedL1 = AlignedL1([0; 512]);
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
static mut BOOT_L2_TABLE: AlignedL2 = AlignedL2([0; 512]);

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
global_asm!(
    r#"
    .section .text.boot,"ax",@progbits
    .align 11
    .global yarm_aarch64_vector_table_el1
    .type yarm_aarch64_vector_table_el1,%function
yarm_aarch64_vector_table_el1:
    // Current EL with SP0 (offsets 0x000..0x180)
    .balign 128
    b yarm_aarch64_vector_sync_lower_a64_sp0
    .balign 128
    b yarm_aarch64_vector_irq_lower_a64_sp0
    .balign 128
    b yarm_aarch64_vector_fiq_lower_a64_sp0
    .balign 128
    b yarm_aarch64_vector_serror_lower_a64_sp0
    // Current EL with SPx (offsets 0x200..0x380)
    .balign 128
    b yarm_aarch64_vector_sync_current
    .balign 128
    b yarm_aarch64_vector_irq_current
    .balign 128
    b yarm_aarch64_vector_fiq_current
    .balign 128
    b yarm_aarch64_vector_serror_current
    // Lower EL using AArch64 (offsets 0x400..0x580)
    .balign 128
    b yarm_aarch64_vector_sync_lower_a64
    .balign 128
    b yarm_aarch64_vector_irq_lower_a64
    .balign 128
    b yarm_aarch64_vector_fiq_lower_a64
    .balign 128
    b yarm_aarch64_vector_serror_lower_a64
    // Lower EL using AArch32 (offsets 0x600..0x780)
    .balign 128
    b yarm_aarch64_vector_sync_lower_a32
    .balign 128
    b yarm_aarch64_vector_irq_lower_a32
    .balign 128
    b yarm_aarch64_vector_fiq_lower_a32
    .balign 128
    b yarm_aarch64_vector_serror_lower_a32
    .balign 128

    .macro YARM_AARCH64_VECTOR_STUB name kind
    .global \name
    .type \name,%function
\name:
    sub sp, sp, #16
    str x0, [sp, #0]
    mov x0, #\kind
    b yarm_aarch64_vector_dispatch
    .endm

    YARM_AARCH64_VECTOR_STUB yarm_aarch64_vector_sync_lower_a64_sp0, 1
    YARM_AARCH64_VECTOR_STUB yarm_aarch64_vector_irq_lower_a64_sp0, 2
    YARM_AARCH64_VECTOR_STUB yarm_aarch64_vector_fiq_lower_a64_sp0, 3
    YARM_AARCH64_VECTOR_STUB yarm_aarch64_vector_serror_lower_a64_sp0, 4
    YARM_AARCH64_VECTOR_STUB yarm_aarch64_vector_sync_current, 5
    YARM_AARCH64_VECTOR_STUB yarm_aarch64_vector_irq_current, 6
    YARM_AARCH64_VECTOR_STUB yarm_aarch64_vector_fiq_current, 7
    YARM_AARCH64_VECTOR_STUB yarm_aarch64_vector_serror_current, 8
    YARM_AARCH64_VECTOR_STUB yarm_aarch64_vector_sync_lower_a64, 9
    YARM_AARCH64_VECTOR_STUB yarm_aarch64_vector_irq_lower_a64, 10
    YARM_AARCH64_VECTOR_STUB yarm_aarch64_vector_fiq_lower_a64, 11
    YARM_AARCH64_VECTOR_STUB yarm_aarch64_vector_serror_lower_a64, 12
    YARM_AARCH64_VECTOR_STUB yarm_aarch64_vector_sync_lower_a32, 13
    YARM_AARCH64_VECTOR_STUB yarm_aarch64_vector_irq_lower_a32, 14
    YARM_AARCH64_VECTOR_STUB yarm_aarch64_vector_fiq_lower_a32, 15
    YARM_AARCH64_VECTOR_STUB yarm_aarch64_vector_serror_lower_a32, 16

    .global yarm_aarch64_vector_dispatch
    .type yarm_aarch64_vector_dispatch,%function
yarm_aarch64_vector_dispatch:
    // Per-task EL1 stack frame layout (800 bytes total):
    //   0x000..0x0F7 : x0..x30 (31 x 8 bytes)
    //   0x0F8        : SP_EL0
    //   0x100        : ELR_EL1
    //   0x108        : SPSR_EL1
    //   0x110        : ESR_EL1
    //   0x118        : FAR_EL1
    //   0x120..0x31F : q0..q31 (32 x 16 bytes)
    sub sp, sp, #800
    stp x0, x1, [sp, #0]
    stp x2, x3, [sp, #16]
    stp x4, x5, [sp, #32]
    stp x6, x7, [sp, #48]
    stp x8, x9, [sp, #64]
    stp x10, x11, [sp, #80]
    stp x12, x13, [sp, #96]
    stp x14, x15, [sp, #112]
    stp x16, x17, [sp, #128]
    stp x18, x19, [sp, #144]
    stp x20, x21, [sp, #160]
    stp x22, x23, [sp, #176]
    stp x24, x25, [sp, #192]
    stp x26, x27, [sp, #208]
    stp x28, x29, [sp, #224]
    str x30, [sp, #240]
    ldr x9, [sp, #800]
    str x9, [sp, #0]
    mrs x9, sp_el0
    str x9, [sp, #248]
    mrs x9, elr_el1
    mov x10, x9
    mov x0, x10
    bl yarm_aarch64_vector_elr_marker
    mov x9, x10
    str x9, [sp, #256]
    mrs x9, spsr_el1
    str x9, [sp, #264]
    mrs x9, esr_el1
    str x9, [sp, #272]
    mrs x9, far_el1
    str x9, [sp, #280]
    stp q0, q1, [sp, #288]
    stp q2, q3, [sp, #320]
    stp q4, q5, [sp, #352]
    stp q6, q7, [sp, #384]
    stp q8, q9, [sp, #416]
    stp q10, q11, [sp, #448]
    stp q12, q13, [sp, #480]
    stp q14, q15, [sp, #512]
    stp q16, q17, [sp, #544]
    stp q18, q19, [sp, #576]
    stp q20, q21, [sp, #608]
    stp q22, q23, [sp, #640]
    stp q24, q25, [sp, #672]
    stp q26, q27, [sp, #704]
    stp q28, q29, [sp, #736]
    stp q30, q31, [sp, #768]
    bl yarm_aarch64_vector_first_marker
    msr daifset, #0xf
    mov x1, sp
    bl yarm_aarch64_vector_entry
    ldp q0, q1, [sp, #288]
    ldp q2, q3, [sp, #320]
    ldp q4, q5, [sp, #352]
    ldp q6, q7, [sp, #384]
    ldp q8, q9, [sp, #416]
    ldp q10, q11, [sp, #448]
    ldp q12, q13, [sp, #480]
    ldp q14, q15, [sp, #512]
    ldp q16, q17, [sp, #544]
    ldp q18, q19, [sp, #576]
    ldp q20, q21, [sp, #608]
    ldp q22, q23, [sp, #640]
    ldp q24, q25, [sp, #672]
    ldp q26, q27, [sp, #704]
    ldp q28, q29, [sp, #736]
    ldp q30, q31, [sp, #768]
    // Set all system registers before any debug marker calls.
    ldr x9, [sp, #248]
    msr sp_el0, x9
    ldr x9, [sp, #256]
    msr elr_el1, x9
    ldr x9, [sp, #264]
    msr spsr_el1, x9
    // All debug marker calls happen here, before any user GPR is restored.
    // Every bl clobbers x0..x18 and x30 (caller-saved per AAPCS), but the
    // vector frame memory at [sp+0..sp+247] is not touched by callees since
    // they allocate their own frames below sp.
    ldr x0, [sp, #256]
    bl yarm_aarch64_return_to_user_elr_marker
    mrs x0, elr_el1
    bl yarm_aarch64_write_return_elr_marker
    mrs x0, elr_el1
    bl yarm_aarch64_final_elr_reg_marker
    ldr x0, [sp, #0]
    bl yarm_aarch64_return_to_user_x0_marker
    // Restore user GPRs.  No function calls after this point.
    ldr x30, [sp, #240]
    ldp x28, x29, [sp, #224]
    ldp x26, x27, [sp, #208]
    ldp x24, x25, [sp, #192]
    ldp x22, x23, [sp, #176]
    ldp x20, x21, [sp, #160]
    ldp x18, x19, [sp, #144]
    ldp x16, x17, [sp, #128]
    ldp x14, x15, [sp, #112]
    ldp x12, x13, [sp, #96]
    ldp x10, x11, [sp, #80]
    ldp x8, x9, [sp, #64]
    ldp x6, x7, [sp, #48]
    ldp x4, x5, [sp, #32]
    ldp x2, x3, [sp, #16]
    ldp x0, x1, [sp, #0]
    add sp, sp, #816
    eret
    "#
);

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
fn halt_stage1() -> ! {
    loop {
        unsafe {
            core::arch::asm!("wfe", options(nomem, nostack, preserves_flags));
        }
    }
}

#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-stage1"
))]
#[inline(always)]
pub(super) fn rpi5_emergency_marker(marker: &'static [u8]) {
    unsafe extern "C" {
        fn yarm_aarch64_rpi5_emergency_write(marker: *const u8);
    }
    unsafe {
        yarm_aarch64_rpi5_emergency_write(marker.as_ptr());
    }
}

#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-stage1"
))]
#[inline(always)]
pub(super) fn rpi5_emergency_hex(prefix: &'static [u8], value: u64) {
    unsafe extern "C" {
        fn yarm_aarch64_rpi5_emergency_write_hex(prefix: *const u8, value: u64);
    }
    unsafe {
        yarm_aarch64_rpi5_emergency_write_hex(prefix.as_ptr(), value);
    }
}

#[cfg(any(
    feature = "hosted-dev",
    not(target_arch = "aarch64"),
    not(feature = "rpi5-stage1")
))]
#[inline(always)]
fn rpi5_emergency_marker(_marker: &'static [u8]) {}

#[cfg(any(
    feature = "hosted-dev",
    not(target_arch = "aarch64"),
    not(feature = "rpi5-stage1")
))]
#[inline(always)]
fn rpi5_emergency_hex(_prefix: &'static [u8], _value: u64) {}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
#[unsafe(no_mangle)]
extern "C" fn yarm_aarch64_select_early_console(start_info_ptr: usize) {
    use crate::arch::aarch64_boot_policy::DetectedPlatform;
    use crate::kernel::boot_command_line::{BootPhase, PlatformOption, parse_yarm_boot_options};

    const RPI5_EMERGENCY_UART_BASE: u64 = 0x10_7d00_1000;

    rpi5_emergency_marker(b"RPI5_RUST_ENTRY\r\n\0");
    rpi5_emergency_marker(b"RPI5_DTB_PARSE_BEGIN\r\n\0");
    let Some(dtb) = dtb_slice_from_start_info(start_info_ptr) else {
        // Preserve the direct-kernel QEMU fallback when no firmware DTB can be read.
        crate::arch::aarch64::console::init_early_mmio_base(0x0900_0000);
        return;
    };
    let Some(info) = crate::arch::aarch64_boot_policy::parse_platform_dtb(dtb) else {
        return;
    };
    rpi5_emergency_marker(b"RPI5_DTB_PARSE_DONE\r\n\0");
    rpi5_emergency_marker(b"RPI5_BOOT_OPTIONS_BEGIN\r\n\0");
    let raw = crate::arch::fdt::chosen_bootargs(dtb).unwrap_or(&[]);
    let options = parse_yarm_boot_options(raw);
    rpi5_emergency_marker(b"RPI5_BOOT_OPTIONS_DONE\r\n\0");
    rpi5_emergency_marker(b"RPI5_AFTER_BOOT_OPTIONS\r\n\0");
    let selected = match options.platform {
        PlatformOption::Auto => info.platform,
        PlatformOption::QemuVirt => DetectedPlatform::QemuVirt,
        PlatformOption::Rpi5 => DetectedPlatform::Rpi5Bcm2712,
    };
    match selected {
        DetectedPlatform::QemuVirt => {
            crate::arch::aarch64::console::init_early_mmio_base(0x0900_0000);
        }
        DetectedPlatform::Rpi5Bcm2712 => {
            rpi5_emergency_marker(b"RPI5_CONSOLE_SELECT_BEGIN\r\n\0");
            let Some(serial) = info.serial else {
                rpi5_emergency_marker(b"RPI5_UART_TRANSLATION_FAILED\r\n\0");
                halt_stage1();
            };
            rpi5_emergency_hex(b"RPI5_SELECTED_UART_BASE value=0x\0", serial.base);
            if serial.base != RPI5_EMERGENCY_UART_BASE {
                rpi5_emergency_marker(b"RPI5_SELECTED_UART_BASE_MISMATCH\r\n\0");
                halt_stage1();
            }
            if !crate::arch::aarch64::console::init_dtb_pl011(serial.base as usize) {
                halt_stage1();
            }
            rpi5_emergency_marker(b"RPI5_CONSOLE_SELECT_DONE\r\n\0");
            rpi5_emergency_marker(b"RPI5_CONSOLE_WRITE_BEGIN\r\n\0");
            rpi5_emergency_marker(b"RPI5_BOOT_00_ENTRY\r\n\0");
            if !crate::arch::aarch64::console::try_write_line("") {
                rpi5_emergency_marker(b"RPI5_CONSOLE_WRITE_TIMEOUT\r\n\0");
                halt_stage1();
            }
            rpi5_emergency_marker(b"RPI5_CONSOLE_WRITE_DONE\r\n\0");
            rpi5_emergency_marker(b"RPI5_AFTER_CONSOLE_WRITE\r\n\0");
            if options.boot_phase == BootPhase::Entry {
                halt_stage1();
            }
            rpi5_emergency_marker(b"RPI5_BEFORE_BOOT01\r\n\0");
            rpi5_emergency_marker(b"RPI5_BOOT_01_DTB_PTR\r\n\0");
            rpi5_emergency_hex(b"RPI5_BOOT_01_DTB_PTR value=0x\0", start_info_ptr as u64);
            #[cfg(not(feature = "rpi5-stage1"))]
            crate::yarm_log!("RPI5_BOOT_01_DTB_PTR value=0x{:x}", start_info_ptr as u64);
            rpi5_emergency_marker(b"RPI5_AFTER_BOOT01\r\n\0");

            rpi5_emergency_marker(b"RPI5_BEFORE_BOOT02\r\n\0");
            rpi5_emergency_marker(b"RPI5_BOOT_02_UART_SELECTED\r\n\0");
            rpi5_emergency_hex(b"RPI5_BOOT_02_UART_SELECTED base=0x\0", serial.base);
            #[cfg(not(feature = "rpi5-stage1"))]
            crate::yarm_log!(
                "RPI5_BOOT_02_UART_SELECTED path={} base=0x{:x}",
                serial.path.as_str(),
                serial.base
            );
            rpi5_emergency_marker(b"RPI5_AFTER_BOOT02\r\n\0");

            rpi5_emergency_marker(b"RPI5_BEFORE_BOOT03\r\n\0");
            rpi5_emergency_marker(b"RPI5_BOOT_03_UART_OK\r\n\0");
            #[cfg(not(feature = "rpi5-stage1"))]
            crate::arch::aarch64::console::write_line("RPI5_BOOT_03_UART_OK");
            rpi5_emergency_marker(b"RPI5_AFTER_BOOT03\r\n\0");
            if options.boot_phase == BootPhase::Uart {
                halt_stage1();
            }
            #[cfg(feature = "rpi5-stage1")]
            if options.boot_phase == BootPhase::Dtb {
                rpi5_stage1_dtb_diagnostics(dtb);
            }
            #[cfg(feature = "rpi5-stage1")]
            if matches!(options.boot_phase, BootPhase::Mmu | BootPhase::Kernel) {
                rpi5_stage1_kernel_core_diagnostics(dtb);
            }
        }
        DetectedPlatform::Unknown => halt_stage1(),
    }
}

#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-stage1"
))]
fn rpi5_stage1_dtb_diagnostics(dtb: &[u8]) -> ! {
    use crate::arch::aarch64_boot_policy::{DiagnosticPsciConduit, parse_platform_dtb_diagnostics};
    use core::fmt::Write;

    struct Line {
        bytes: [u8; 384],
        len: usize,
        truncated: bool,
    }
    impl Line {
        const fn new() -> Self {
            Self {
                bytes: [0; 384],
                len: 0,
                truncated: false,
            }
        }
        fn emit(&self) -> bool {
            let text =
                core::str::from_utf8(&self.bytes[..self.len]).unwrap_or("RPI5_DTB_FORMAT_ERR");
            crate::arch::aarch64::console::try_write_line(text)
        }
    }
    impl Write for Line {
        fn write_str(&mut self, value: &str) -> core::fmt::Result {
            let remaining = self.bytes.len().saturating_sub(self.len);
            let copied = remaining.min(value.len());
            self.bytes[self.len..self.len + copied].copy_from_slice(&value.as_bytes()[..copied]);
            self.len += copied;
            self.truncated |= copied != value.len();
            if copied == value.len() {
                Ok(())
            } else {
                Err(core::fmt::Error)
            }
        }
    }
    macro_rules! diag {
        ($($arg:tt)*) => {{
            let mut line = Line::new();
            let _ = write!(&mut line, $($arg)*);
            if line.truncated || !line.emit() {
                rpi5_emergency_marker(b"RPI5_DTB_DIAG_OUTPUT_FAILED\r\n\0");
                halt_stage1();
            }
        }};
    }

    rpi5_emergency_marker(b"RPI5_DTB_DIAG_BEGIN\r\n\0");
    let Some(info) = parse_platform_dtb_diagnostics(dtb) else {
        rpi5_emergency_marker(b"RPI5_DTB_DIAG_PARSE_FAILED\r\n\0");
        halt_stage1();
    };
    for (index, range) in info.memory_ranges[..info.memory_range_count]
        .iter()
        .enumerate()
    {
        diag!(
            "RPI5_DTB_MEMORY_RANGE index={} start=0x{:016x} size=0x{:016x}",
            index,
            range.start,
            range.size
        );
    }
    if info.memory_ranges_truncated {
        diag!("RPI5_DTB_MEMORY_RANGE_TRUNCATED");
    }
    for (index, range) in info.reserved_ranges[..info.reserved_range_count]
        .iter()
        .enumerate()
    {
        diag!(
            "RPI5_DTB_RESERVED_RANGE index={} start=0x{:016x} size=0x{:016x} no_map={}",
            index,
            range.start,
            range.size,
            range.no_map as u8
        );
    }
    if info.reserved_ranges_truncated {
        diag!("RPI5_DTB_RESERVED_RANGE_TRUNCATED");
    }
    diag!(
        "RPI5_DTB_INITRD present={} start=0x{:016x} end=0x{:016x}",
        matches!((info.initrd_start, info.initrd_end), (Some(start), Some(end)) if end > start)
            as u8,
        info.initrd_start.unwrap_or(0),
        info.initrd_end.unwrap_or(0)
    );
    diag!(
        "RPI5_DTB_BOOTARGS len={} truncated={}",
        info.bootargs_len,
        info.bootargs_truncated as u8
    );
    if !info.interrupt_controller_path.is_empty() {
        diag!(
            "RPI5_DTB_IRQC path={} base=0x{:016x} compatible={}",
            info.interrupt_controller_path.as_str(),
            info.interrupt_controller_base.unwrap_or(0),
            info.interrupt_controller_compatible.as_str()
        );
    } else {
        diag!("RPI5_DTB_IRQC_MISSING");
    }
    if !info.l2_interrupt_controller_path.is_empty() {
        if let Some(base) = info.l2_interrupt_controller_base {
            diag!(
                "RPI5_DTB_IRQC_L2 path={} base=0x{:016x} compatible={}",
                info.l2_interrupt_controller_path.as_str(),
                base,
                info.l2_interrupt_controller_compatible.as_str()
            );
        } else {
            diag!(
                "RPI5_DTB_IRQC_L2_BASE_MISSING path={} compatible={}",
                info.l2_interrupt_controller_path.as_str(),
                info.l2_interrupt_controller_compatible.as_str()
            );
        }
    }
    if let Some(base) = info.gic_dist_base {
        diag!("RPI5_DTB_GIC_DIST base=0x{:016x}", base);
        if let Some(base) = info.gic_redist_base {
            diag!("RPI5_DTB_GIC_REDIST base=0x{:016x}", base);
        }
    } else {
        diag!("RPI5_DTB_GIC_MISSING");
    }
    let conduit = match info.psci_conduit {
        DiagnosticPsciConduit::Hvc => "hvc",
        DiagnosticPsciConduit::Smc => "smc",
        DiagnosticPsciConduit::None => "none",
    };
    diag!("RPI5_DTB_PSCI conduit={}", conduit);
    let max_cpus = 1usize;
    let effective_bitmap = info.cpu_bitmap & 1;
    diag!(
        "RPI5_DTB_CPU_BITMAP value=0x{:016x} count={} max_cpus={} effective={}",
        info.cpu_bitmap,
        info.cpu_bitmap.count_ones(),
        max_cpus,
        effective_bitmap.count_ones()
    );
    for (index, controller) in info.pcie_controllers[..info.pcie_controller_count]
        .iter()
        .enumerate()
    {
        if let Some(base) = controller.base {
            diag!(
                "RPI5_DTB_PCIE_CONTROLLER index={} path={} base=0x{:016x}",
                index,
                controller.path.as_str(),
                base
            );
        } else {
            diag!(
                "RPI5_DTB_PCIE_CONTROLLER_BASE_MISSING index={} path={}",
                index,
                controller.path.as_str()
            );
        }
    }
    if info.pcie_controller_count == 0 {
        diag!("RPI5_DTB_PCIE_CONTROLLER_MISSING");
    }
    if info.pcie_controllers_truncated {
        diag!("RPI5_DTB_PCIE_CONTROLLER_TRUNCATED");
    }
    if let Some(index) = info.rp1_controller_index {
        diag!("RPI5_DTB_RP1_PCIE present=1 controller_index={}", index);
    } else {
        diag!("RPI5_DTB_RP1_PCIE present=0");
    }
    if !info.rp1_node_path.is_empty() {
        diag!("RPI5_DTB_RP1_NODE path={}", info.rp1_node_path.as_str());
    }
    rpi5_emergency_marker(b"RPI5_DTB_DIAG_DONE\r\n\0");
    halt_stage1();
}

#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-stage1"
))]
fn rpi5_stage1_kernel_core_diagnostics(dtb: &[u8]) -> ! {
    use crate::arch::aarch64_boot_policy::{
        RPI5_STAGE1_GICR_FRAME_STRIDE, RPI5_STAGE1_GICR_SCAN_FRAMES, Stage1KernelRange,
        Stage1MmuMemoryType, build_rpi5_stage1_kernel_bootstrap_record,
        parse_platform_dtb_diagnostics, plan_rpi5_stage1_allocator_handoff,
        plan_rpi5_stage1_identity_map, plan_rpi5_stage1_kernel_memory, plan_rpi5_stage2a_initrd,
        rpi5_stage1_gicr_typer_plausible, rpi5_stage1_timer_delta, rpi5_stage2a_cpio_first_name,
    };
    use core::fmt::Write;

    struct Line {
        bytes: [u8; 192],
        len: usize,
        truncated: bool,
    }
    impl Line {
        const fn new() -> Self {
            Self {
                bytes: [0; 192],
                len: 0,
                truncated: false,
            }
        }
        fn emit(&self) -> bool {
            let text =
                core::str::from_utf8(&self.bytes[..self.len]).unwrap_or("RPI5_KERNEL_FORMAT_ERR");
            crate::arch::aarch64::console::try_write_line(text)
        }
    }
    impl Write for Line {
        fn write_str(&mut self, value: &str) -> core::fmt::Result {
            let remaining = self.bytes.len().saturating_sub(self.len);
            let copied = remaining.min(value.len());
            self.bytes[self.len..self.len + copied].copy_from_slice(&value.as_bytes()[..copied]);
            self.len += copied;
            self.truncated |= copied != value.len();
            if copied == value.len() {
                Ok(())
            } else {
                Err(core::fmt::Error)
            }
        }
    }
    macro_rules! kernel_diag {
        ($($arg:tt)*) => {{
            let mut line = Line::new();
            let _ = write!(&mut line, $($arg)*);
            if line.truncated || !line.emit() {
                rpi5_emergency_marker(b"RPI5_KERNEL_OUTPUT_FAILED\r\n\0");
                halt_stage1();
            }
        }};
    }

    rpi5_emergency_marker(b"RPI5_KERNEL_PLAN_BEGIN\r\n\0");
    let Some(info) = parse_platform_dtb_diagnostics(dtb) else {
        kernel_diag!("RPI5_KERNEL_PLAN_FAILED reason=dtb_parse_failed");
        halt_stage1();
    };
    let kernel = Stage1KernelRange::new(
        core::ptr::addr_of!(__kernel_start) as u64,
        core::ptr::addr_of!(__kernel_end) as u64,
    );
    let Some(dtb_end) = (dtb.as_ptr() as u64).checked_add(dtb.len() as u64) else {
        kernel_diag!("RPI5_KERNEL_PLAN_FAILED reason=dtb_address_overflow");
        halt_stage1();
    };
    let dtb_range = Stage1KernelRange::new(dtb.as_ptr() as u64, dtb_end);
    kernel_diag!(
        "RPI5_KERNEL_IMAGE_RANGE start=0x{:016x} end=0x{:016x}",
        kernel.start,
        kernel.end
    );
    kernel_diag!(
        "RPI5_KERNEL_DTB_RANGE start=0x{:016x} end=0x{:016x}",
        dtb_range.start,
        dtb_range.end
    );
    let plan = match plan_rpi5_stage1_kernel_memory(&info, kernel, dtb_range) {
        Ok(plan) => plan,
        Err(reason) => {
            kernel_diag!("RPI5_KERNEL_PLAN_FAILED reason={}", reason.label());
            halt_stage1();
        }
    };
    for (index, range) in plan.reserved_ranges[..plan.reserved_range_count]
        .iter()
        .enumerate()
    {
        kernel_diag!(
            "RPI5_KERNEL_RESERVED_RANGE index={} start=0x{:016x} end=0x{:016x}",
            index,
            range.start,
            range.end
        );
    }
    for index in &plan.zero_reserved_skipped[..plan.zero_reserved_skipped_count] {
        kernel_diag!("RPI5_KERNEL_RESERVED_ZERO_SKIPPED index={}", index);
    }
    for (index, range) in plan.usable_ranges[..plan.usable_range_count]
        .iter()
        .enumerate()
    {
        kernel_diag!(
            "RPI5_KERNEL_USABLE_RANGE index={} start=0x{:016x} end=0x{:016x}",
            index,
            range.start,
            range.end
        );
    }
    kernel_diag!(
        "RPI5_KERNEL_PT_POOL start=0x{:016x} end=0x{:016x}",
        plan.page_table_pool.start,
        plan.page_table_pool.end
    );
    kernel_diag!(
        "RPI5_KERNEL_EARLY_HEAP start=0x{:016x} end=0x{:016x}",
        plan.early_heap.start,
        plan.early_heap.end
    );
    rpi5_emergency_marker(b"RPI5_KERNEL_PLAN_DONE\r\n\0");

    rpi5_emergency_marker(b"RPI5_MMU_PLAN_BEGIN\r\n\0");
    let stack_pointer: u64;
    unsafe {
        core::arch::asm!(
            "mov {0}, sp",
            out(reg) stack_pointer,
            options(nomem, nostack, preserves_flags)
        );
    }
    let stack_page_start = stack_pointer & !(RPI5_STAGE1_PAGE_SIZE - 1);
    let stack_page =
        Stage1KernelRange::new(stack_page_start, stack_page_start + RPI5_STAGE1_PAGE_SIZE);
    let mmu_plan = match plan_rpi5_stage1_identity_map(
        &info,
        &plan,
        kernel,
        stack_page,
        dtb_range,
        RPI5_STAGE1_UART_BASE,
    ) {
        Ok(plan) => plan,
        Err(reason) => {
            kernel_diag!("RPI5_MMU_PLAN_FAILED reason={}", reason.label());
            halt_stage1();
        }
    };
    for mapping in &mmu_plan.mappings[..mmu_plan.mapping_count] {
        match mapping.memory_type {
            Stage1MmuMemoryType::Normal => kernel_diag!(
                "RPI5_MMU_MAP_NORMAL start=0x{:016x} end=0x{:016x}",
                mapping.range.start,
                mapping.range.end
            ),
            Stage1MmuMemoryType::DeviceNgnre => kernel_diag!(
                "RPI5_MMU_MAP_DEVICE start=0x{:016x} end=0x{:016x}",
                mapping.range.start,
                mapping.range.end
            ),
        }
    }
    let root = match unsafe { rpi5_stage1_build_identity_tables(&mmu_plan) } {
        Ok(root) => root,
        Err(reason) => {
            kernel_diag!("RPI5_MMU_PLAN_FAILED reason={}", reason);
            halt_stage1();
        }
    };
    kernel_diag!("RPI5_MMU_PT_ROOT base=0x{:016x}", root);
    rpi5_emergency_marker(b"RPI5_MMU_PLAN_DONE\r\n\0");
    rpi5_emergency_marker(b"RPI5_MMU_ENABLE_BEGIN\r\n\0");
    if let Err(reason) = unsafe { rpi5_stage1_enable_identity_mmu(root, mmu_plan.pt_pool) } {
        kernel_diag!("RPI5_MMU_ENABLE_FAILED reason={}", reason);
        halt_stage1();
    }
    rpi5_emergency_marker(b"RPI5_MMU_ENABLE_DONE\r\n\0");
    if !crate::arch::aarch64::console::try_write_line("RPI5_UART_AFTER_MMU_OK") {
        rpi5_emergency_marker(b"RPI5_MMU_ENABLE_FAILED reason=uart_after_mmu_timeout\r\n\0");
        halt_stage1();
    }
    rpi5_emergency_marker(b"RPI5_KERNEL_CORE_DONE\r\n\0");

    rpi5_emergency_marker(b"RPI5_ALLOC_PLAN_BEGIN\r\n\0");
    let allocator_plan = match plan_rpi5_stage1_allocator_handoff(&info, &plan) {
        Ok(plan) => plan,
        Err(reason) => {
            kernel_diag!("RPI5_ALLOC_PLAN_FAILED reason={}", reason.label());
            halt_stage1();
        }
    };
    for reserved in &allocator_plan.reserved[..allocator_plan.reserved_count] {
        kernel_diag!(
            "RPI5_ALLOC_RESERVED start=0x{:016x} end=0x{:016x} reason={}",
            reserved.range.start,
            reserved.range.end,
            reserved.reason.label()
        );
    }
    for usable in &allocator_plan.usable[..allocator_plan.usable_count] {
        kernel_diag!(
            "RPI5_ALLOC_USABLE start=0x{:016x} end=0x{:016x}",
            usable.start,
            usable.end
        );
    }
    kernel_diag!(
        "RPI5_EARLY_HEAP_READY start=0x{:016x} end=0x{:016x}",
        plan.early_heap.start,
        plan.early_heap.end
    );

    use crate::kernel::frame_allocator::{MemoryRegion, PhysicalFrameAllocator};
    if core::mem::size_of::<PhysicalFrameAllocator>()
        > (plan.early_heap.end - plan.early_heap.start) as usize
        || plan.early_heap.start % core::mem::align_of::<PhysicalFrameAllocator>() as u64 != 0
    {
        kernel_diag!("RPI5_ALLOC_PLAN_FAILED reason=early_heap_metadata_fit");
        halt_stage1();
    }
    let mut regions = [MemoryRegion {
        start: 0,
        len: 0,
        usable: false,
    }; crate::arch::aarch64_boot_policy::MAX_STAGE1_ALLOC_USABLE_RANGES];
    for (slot, usable) in regions
        .iter_mut()
        .zip(&allocator_plan.usable[..allocator_plan.usable_count])
    {
        *slot = MemoryRegion {
            start: usable.start,
            len: usable.end - usable.start,
            usable: true,
        };
    }
    let allocator_ptr = plan.early_heap.start as *mut PhysicalFrameAllocator;
    unsafe {
        core::ptr::write(allocator_ptr, PhysicalFrameAllocator::new_uninit());
    }
    let allocator = unsafe { &mut *allocator_ptr };
    if allocator
        .init_from_memory_map(&regions[..allocator_plan.usable_count])
        .is_err()
    {
        kernel_diag!("RPI5_ALLOC_PLAN_FAILED reason=frame_allocator_init");
        halt_stage1();
    }
    kernel_diag!(
        "RPI5_FRAME_ALLOC_READY total_pages={} free_pages={}",
        allocator.total_frames(),
        allocator.free_frames()
    );
    if allocator.total_frames() as u64 != allocator_plan.total_pages
        || allocator.free_frames() != allocator.total_frames()
    {
        kernel_diag!("RPI5_ALLOC_PLAN_FAILED reason=frame_count_mismatch");
        halt_stage1();
    }
    rpi5_emergency_marker(b"RPI5_FRAME_ALLOC_TEST_BEGIN\r\n\0");
    let initial_free = allocator.free_frames();
    let test_frame = match allocator.alloc_frame() {
        Ok(frame) => frame,
        Err(_) => {
            kernel_diag!("RPI5_ALLOC_PLAN_FAILED reason=frame_alloc_test");
            halt_stage1();
        }
    };
    kernel_diag!("RPI5_FRAME_ALLOC_TEST_PAGE phys=0x{:016x}", test_frame);
    let test_range = Stage1KernelRange::new(test_frame, test_frame + RPI5_STAGE1_PAGE_SIZE);
    if test_frame % RPI5_STAGE1_PAGE_SIZE != 0
        || !allocator_plan.usable[..allocator_plan.usable_count]
            .iter()
            .any(|usable| usable.start <= test_range.start && usable.end >= test_range.end)
        || allocator_plan.reserved[..allocator_plan.reserved_count]
            .iter()
            .any(|reserved| test_range.overlaps(reserved.range))
    {
        kernel_diag!("RPI5_ALLOC_PLAN_FAILED reason=frame_alloc_test_range");
        halt_stage1();
    }
    if allocator.free_frame(test_frame).is_err() || allocator.free_frames() != initial_free {
        kernel_diag!("RPI5_ALLOC_PLAN_FAILED reason=frame_free_test");
        halt_stage1();
    }
    rpi5_emergency_marker(b"RPI5_FRAME_ALLOC_TEST_DONE\r\n\0");
    rpi5_emergency_marker(b"RPI5_ALLOC_PLAN_DONE\r\n\0");
    rpi5_emergency_marker(b"RPI5_KERNEL_ALLOCATOR_READY\r\n\0");
    rpi5_emergency_marker(b"RPI5_KERNEL_CORE_ALLOC_DONE\r\n\0");

    rpi5_emergency_marker(b"RPI5_IRQTIMER_DIAG_BEGIN\r\n\0");
    let timer_frequency: u64;
    let counter_begin: u64;
    let counter_end: u64;
    unsafe {
        core::arch::asm!(
            "mrs {0}, CNTFRQ_EL0",
            out(reg) timer_frequency,
            options(nomem, nostack, preserves_flags)
        );
        core::arch::asm!(
            "isb",
            "mrs {0}, CNTPCT_EL0",
            out(reg) counter_begin,
            options(nomem, nostack, preserves_flags)
        );
    }
    kernel_diag!("RPI5_TIMER_CNTFRQ value=0x{:016x}", timer_frequency);
    if timer_frequency == 0 {
        kernel_diag!("RPI5_IRQTIMER_DIAG_FAILED reason=counter_frequency_zero");
        halt_stage1();
    }
    kernel_diag!("RPI5_TIMER_CNTPCT_BEGIN value=0x{:016x}", counter_begin);
    for _ in 0..4096 {
        core::hint::spin_loop();
    }
    unsafe {
        core::arch::asm!(
            "isb",
            "mrs {0}, CNTPCT_EL0",
            out(reg) counter_end,
            options(nomem, nostack, preserves_flags)
        );
    }
    kernel_diag!("RPI5_TIMER_CNTPCT_END value=0x{:016x}", counter_end);
    let Some(counter_delta) = rpi5_stage1_timer_delta(counter_begin, counter_end) else {
        kernel_diag!("RPI5_IRQTIMER_DIAG_FAILED reason=counter_not_incrementing");
        halt_stage1();
    };
    kernel_diag!("RPI5_TIMER_CNTPCT_DELTA value=0x{:016x}", counter_delta);
    rpi5_emergency_marker(b"RPI5_TIMER_COUNTER_OK\r\n\0");
    kernel_diag!("RPI5_PSCI_CONDUIT value={}", info.psci_conduit.label());

    let Some(gicd_base) = info.gic_dist_base else {
        kernel_diag!("RPI5_IRQTIMER_DIAG_FAILED reason=gicd_missing");
        halt_stage1();
    };
    kernel_diag!("RPI5_GICD_PROBE_BEGIN base=0x{:016x}", gicd_base);
    let gicd_typer = unsafe { core::ptr::read_volatile((gicd_base + 0x004) as *const u32) };
    let gicd_iidr = unsafe { core::ptr::read_volatile((gicd_base + 0x008) as *const u32) };
    kernel_diag!("RPI5_GICD_TYPER value=0x{:08x}", gicd_typer);
    kernel_diag!("RPI5_GICD_IIDR value=0x{:08x}", gicd_iidr);
    rpi5_emergency_marker(b"RPI5_GICD_PROBE_DONE\r\n\0");

    let Some(gicr_base) = info.gic_redist_base else {
        kernel_diag!("RPI5_IRQTIMER_DIAG_FAILED reason=gicr_missing");
        halt_stage1();
    };
    kernel_diag!("RPI5_GICR_PROBE_BEGIN base=0x{:016x}", gicr_base);
    let gicr_typer = unsafe { core::ptr::read_volatile((gicr_base + 0x008) as *const u64) };
    kernel_diag!("RPI5_GICR_TYPER value=0x{:016x}", gicr_typer);
    rpi5_emergency_marker(b"RPI5_GICR_PROBE_DONE\r\n\0");

    // No reviewed read-only register definition for bcm7271-l2-intc is available
    // in this tree. Do not infer one from production drivers or probe an offset.
    kernel_diag!("RPI5_L2_INTC_PROBE_DEFERRED reason=no_reviewed_read_only_offset");
    rpi5_emergency_marker(b"RPI5_IRQTIMER_DIAG_DONE\r\n\0");
    rpi5_emergency_marker(b"RPI5_KERNEL_IRQTIMER_READY\r\n\0");

    rpi5_emergency_marker(b"RPI5_IRQ_INIT_BEGIN\r\n\0");
    kernel_diag!("RPI5_GICR_VALIDATE_BEGIN base=0x{:016x}", gicr_base);
    let mut validated_gicr = None;
    for index in 0..RPI5_STAGE1_GICR_SCAN_FRAMES {
        let Some(frame_base) =
            gicr_base.checked_add((index as u64) * RPI5_STAGE1_GICR_FRAME_STRIDE)
        else {
            kernel_diag!("RPI5_IRQ_INIT_FAILED reason=gicr_scan_address_overflow");
            halt_stage1();
        };
        let typer = unsafe { core::ptr::read_volatile((frame_base + 0x008) as *const u64) };
        kernel_diag!(
            "RPI5_GICR_FRAME index={} base=0x{:016x} typer=0x{:016x}",
            index,
            frame_base,
            typer
        );
        if validated_gicr.is_none() && rpi5_stage1_gicr_typer_plausible(typer) {
            validated_gicr = Some(frame_base);
        }
    }
    if validated_gicr.is_some() {
        rpi5_emergency_marker(b"RPI5_GICR_VALIDATE_DONE\r\n\0");
        kernel_diag!("RPI5_IRQ_INIT_DEFERRED reason=gic_init_sequence_not_reviewed");
    } else {
        kernel_diag!("RPI5_GICR_VALIDATE_FAILED reason=no_valid_frame");
        kernel_diag!("RPI5_IRQ_INIT_DEFERRED reason=gicr_unvalidated");
    }

    rpi5_emergency_marker(b"RPI5_TIMER_INIT_BEGIN\r\n\0");
    let timer_ctl_before: u64;
    unsafe {
        core::arch::asm!(
            "mrs {0}, CNTP_CTL_EL0",
            out(reg) timer_ctl_before,
            options(nomem, nostack, preserves_flags)
        );
    }
    kernel_diag!("RPI5_TIMER_CTL_BEFORE value=0x{:016x}", timer_ctl_before);
    let timer_tval = (timer_frequency / 100).clamp(1, u32::MAX as u64);
    unsafe {
        core::arch::asm!(
            "msr CNTP_TVAL_EL0, {0}",
            in(reg) timer_tval,
            options(nomem, nostack, preserves_flags)
        );
        core::arch::asm!(
            "msr CNTP_CTL_EL0, {0}",
            "isb",
            in(reg) 3u64,
            options(nomem, nostack, preserves_flags)
        );
    }
    kernel_diag!("RPI5_TIMER_TVAL_SET value=0x{:016x}", timer_tval);
    let timer_ctl_after: u64;
    unsafe {
        core::arch::asm!(
            "mrs {0}, CNTP_CTL_EL0",
            out(reg) timer_ctl_after,
            options(nomem, nostack, preserves_flags)
        );
    }
    kernel_diag!("RPI5_TIMER_CTL_AFTER value=0x{:016x}", timer_ctl_after);
    if timer_ctl_after & 0x3 != 0x3 {
        kernel_diag!("RPI5_IRQ_INIT_FAILED reason=timer_masked_enable_readback");
        halt_stage1();
    }
    kernel_diag!("RPI5_TIMER_INIT_DONE masked=1");
    rpi5_emergency_marker(b"RPI5_KERNEL_BOOT_PREP_DONE\r\n\0");

    rpi5_emergency_marker(b"RPI5_KERNEL_BOOT_BEGIN\r\n\0");
    unsafe extern "C" {
        static yarm_aarch64_vector_table_el1: u8;
    }
    let trap_vector_base = core::ptr::addr_of!(yarm_aarch64_vector_table_el1) as u64;
    let record = match build_rpi5_stage1_kernel_bootstrap_record(
        &info,
        &allocator_plan,
        RPI5_STAGE1_UART_BASE,
        allocator.total_frames() as u64,
        allocator.free_frames() as u64,
        trap_vector_base,
    ) {
        Some(record) => record,
        None => {
            kernel_diag!("RPI5_KERNEL_BOOT_FAILED reason=platform_record_invalid");
            halt_stage1();
        }
    };
    kernel_diag!(
        "RPI5_KERNEL_PLATFORM_READY uart=0x{:016x} psci={} gicd=0x{:016x} gicr=0x{:016x}",
        record.uart_base,
        record.psci_conduit.label(),
        record.gic_dist_base.unwrap_or(0),
        record.gic_redist_base.unwrap_or(0)
    );
    kernel_diag!(
        "RPI5_KERNEL_MEMORY_READY ranges={} reserved={} total_pages={} free_pages={}",
        record.memory_range_count,
        record.reserved_range_count,
        record.frame_total_pages,
        record.frame_free_pages
    );
    kernel_diag!(
        "RPI5_KERNEL_CPU0_READY bitmap=0x{:016x} effective={}",
        record.cpu_bitmap,
        record.effective_cpu_count
    );
    unsafe {
        core::arch::asm!(
            "msr daifset, #0xf",
            "msr VBAR_EL1, {0}",
            "isb",
            in(reg) record.trap_vector_base,
            options(nomem, nostack, preserves_flags)
        );
    }
    let installed_vbar: u64;
    unsafe {
        core::arch::asm!(
            "mrs {0}, VBAR_EL1",
            out(reg) installed_vbar,
            options(nomem, nostack, preserves_flags)
        );
    }
    if installed_vbar != record.trap_vector_base {
        kernel_diag!("RPI5_KERNEL_BOOT_FAILED reason=trap_vector_readback");
        halt_stage1();
    }
    rpi5_emergency_marker(b"RPI5_KERNEL_TRAP_READY\r\n\0");
    rpi5_emergency_marker(b"RPI5_KERNEL_STATE_BEGIN\r\n\0");
    if allocator.total_frames() as u64 != record.frame_total_pages
        || allocator.free_frames() as u64 != record.frame_free_pages
    {
        kernel_diag!("RPI5_KERNEL_BOOT_FAILED reason=allocator_handoff_changed");
        halt_stage1();
    }
    rpi5_emergency_marker(b"RPI5_KERNEL_STATE_READY\r\n\0");
    kernel_diag!("RPI5_KERNEL_IRQ_DEFERRED reason=gic_init_sequence_not_reviewed");
    if !record.initrd_present {
        kernel_diag!("RPI5_KERNEL_BOOTSTRAP_NO_USERSPACE reason=no_initrd");
    }
    rpi5_emergency_marker(b"RPI5_KERNEL_BOOT_OK\r\n\0");

    rpi5_emergency_marker(b"RPI5_INITRD_DETECT_BEGIN\r\n\0");
    let (Some(initrd_start), Some(initrd_end)) = (info.initrd_start, info.initrd_end) else {
        rpi5_emergency_marker(b"RPI5_INITRD_MISSING\r\n\0");
        kernel_diag!("RPI5_STAGE2A_DEFERRED reason=no_initrd");
        halt_stage1();
    };
    kernel_diag!(
        "RPI5_INITRD_DTB_PROPS start=0x{:016x} end=0x{:016x}",
        initrd_start,
        initrd_end
    );
    let initrd_plan = match plan_rpi5_stage2a_initrd(&info, &allocator_plan) {
        Ok(plan) => plan,
        Err(reason) => {
            kernel_diag!("RPI5_INITRD_INVALID reason={}", reason.label());
            kernel_diag!("RPI5_STAGE2A_DEFERRED reason=invalid_initrd");
            halt_stage1();
        }
    };
    kernel_diag!(
        "RPI5_INITRD_RANGE start=0x{:016x} end=0x{:016x} size=0x{:016x}",
        initrd_plan.byte_range.start,
        initrd_plan.byte_range.end,
        initrd_plan.byte_range.end - initrd_plan.byte_range.start
    );
    kernel_diag!(
        "RPI5_INITRD_RESERVED start=0x{:016x} end=0x{:016x}",
        initrd_plan.reservation.start,
        initrd_plan.reservation.end
    );
    rpi5_emergency_marker(b"RPI5_INITRD_CPIO_CHECK_BEGIN\r\n\0");
    let initrd_size = initrd_plan.byte_range.end - initrd_plan.byte_range.start;
    let Ok(initrd_size) = usize::try_from(initrd_size) else {
        kernel_diag!("RPI5_INITRD_CPIO_INVALID reason=size_not_addressable");
        kernel_diag!("RPI5_STAGE2A_DEFERRED reason=invalid_cpio");
        halt_stage1();
    };
    let initrd = unsafe {
        core::slice::from_raw_parts(initrd_plan.byte_range.start as *const u8, initrd_size)
    };
    let first_name = match rpi5_stage2a_cpio_first_name(initrd) {
        Ok(name) => name,
        Err(reason) => {
            kernel_diag!("RPI5_INITRD_CPIO_INVALID reason={}", reason.label());
            kernel_diag!("RPI5_STAGE2A_DEFERRED reason=invalid_cpio");
            halt_stage1();
        }
    };
    rpi5_emergency_marker(b"RPI5_INITRD_CPIO_MAGIC_OK\r\n\0");
    let first_name = core::str::from_utf8(first_name).unwrap_or("<non-utf8>");
    kernel_diag!("RPI5_INITRD_CPIO_FIRST_ENTRY name={}", first_name);
    kernel_diag!(
        "RPI5_INITRD_READY start=0x{:016x} end=0x{:016x}",
        initrd_plan.byte_range.start,
        initrd_plan.byte_range.end
    );
    rpi5_emergency_marker(b"RPI5_STAGE2A_DONE\r\n\0");
    halt_stage1();
}

#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-stage1"
))]
const RPI5_STAGE1_UART_BASE: u64 = 0x10_7d00_1000;
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-stage1"
))]
const RPI5_STAGE1_PAGE_SIZE: u64 = 4096;
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-stage1"
))]
const RPI5_STAGE1_L2_BLOCK_SIZE: u64 = 2 * 1024 * 1024;
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-stage1"
))]
const RPI5_STAGE1_L1_BLOCK_SIZE: u64 = 1024 * 1024 * 1024;
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-stage1"
))]
const RPI5_STAGE1_PTE_VALID: u64 = 1 << 0;
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-stage1"
))]
const RPI5_STAGE1_PTE_TABLE_OR_PAGE: u64 = 1 << 1;
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-stage1"
))]
const RPI5_STAGE1_PTE_AF: u64 = 1 << 10;
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-stage1"
))]
const RPI5_STAGE1_PTE_PXN: u64 = 1 << 53;
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-stage1"
))]
const RPI5_STAGE1_PTE_UXN: u64 = 1 << 54;
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-stage1"
))]
const RPI5_STAGE1_PTE_ADDR_MASK: u64 = 0x0000_ffff_ffff_f000;
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-stage1"
))]
const RPI5_STAGE1_NORMAL_FLAGS: u64 =
    RPI5_STAGE1_PTE_VALID | RPI5_STAGE1_PTE_AF | (0b11 << 8) | RPI5_STAGE1_PTE_UXN;
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-stage1"
))]
const RPI5_STAGE1_DEVICE_FLAGS: u64 = RPI5_STAGE1_PTE_VALID
    | RPI5_STAGE1_PTE_AF
    | (0b10 << 8)
    | (1 << 2)
    | RPI5_STAGE1_PTE_PXN
    | RPI5_STAGE1_PTE_UXN;
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-stage1"
))]
const RPI5_STAGE1_MAIR_EL1: u64 = 0x04ff;
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-stage1"
))]
const RPI5_STAGE1_TCR_EL1: u64 =
    25 | (1 << 8) | (1 << 10) | (0b11 << 12) | (1 << 23) | (0b010 << 32);

#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-stage1"
))]
struct Rpi5Stage1TableAllocator {
    next: u64,
    end: u64,
}

#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-stage1"
))]
impl Rpi5Stage1TableAllocator {
    unsafe fn allocate(&mut self) -> Result<u64, &'static str> {
        let Some(next) = self.next.checked_add(RPI5_STAGE1_PAGE_SIZE) else {
            return Err("pt_pool_overflow");
        };
        if next > self.end {
            return Err("pt_pool_exhausted");
        }
        let page = self.next;
        self.next = next;
        unsafe {
            core::ptr::write_bytes(page as *mut u8, 0, RPI5_STAGE1_PAGE_SIZE as usize);
        }
        Ok(page)
    }
}

#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-stage1"
))]
unsafe fn rpi5_stage1_build_identity_tables(
    plan: &crate::arch::aarch64_boot_policy::Stage1MmuPlan,
) -> Result<u64, &'static str> {
    let mut allocator = Rpi5Stage1TableAllocator {
        next: plan.pt_pool.start,
        end: plan.pt_pool.end,
    };
    let root = unsafe { allocator.allocate()? };
    for mapping in &plan.mappings[..plan.mapping_count] {
        let flags = match mapping.memory_type {
            crate::arch::aarch64_boot_policy::Stage1MmuMemoryType::Normal => {
                RPI5_STAGE1_NORMAL_FLAGS
            }
            crate::arch::aarch64_boot_policy::Stage1MmuMemoryType::DeviceNgnre => {
                RPI5_STAGE1_DEVICE_FLAGS
            }
        };
        unsafe {
            rpi5_stage1_map_identity_range(
                root,
                &mut allocator,
                mapping.range.start,
                mapping.range.end,
                flags,
            )?;
        }
    }
    Ok(root)
}

#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-stage1"
))]
unsafe fn rpi5_stage1_map_identity_range(
    root: u64,
    allocator: &mut Rpi5Stage1TableAllocator,
    mut start: u64,
    end: u64,
    flags: u64,
) -> Result<(), &'static str> {
    if start % RPI5_STAGE1_PAGE_SIZE != 0 || end % RPI5_STAGE1_PAGE_SIZE != 0 || start >= end {
        return Err("unaligned_mapping");
    }
    while start < end {
        if start % RPI5_STAGE1_L1_BLOCK_SIZE == 0 && end - start >= RPI5_STAGE1_L1_BLOCK_SIZE {
            let l1_index = ((start >> 30) & 0x1ff) as usize;
            unsafe {
                rpi5_stage1_write_empty_entry(root, l1_index, start | flags)?;
            }
            start += RPI5_STAGE1_L1_BLOCK_SIZE;
            continue;
        }
        let l1_index = ((start >> 30) & 0x1ff) as usize;
        let l2 = unsafe { rpi5_stage1_ensure_table(root, l1_index, allocator)? };
        if start % RPI5_STAGE1_L2_BLOCK_SIZE == 0 && end - start >= RPI5_STAGE1_L2_BLOCK_SIZE {
            let l2_index = ((start >> 21) & 0x1ff) as usize;
            unsafe {
                rpi5_stage1_write_empty_entry(l2, l2_index, start | flags)?;
            }
            start += RPI5_STAGE1_L2_BLOCK_SIZE;
            continue;
        }
        let l2_index = ((start >> 21) & 0x1ff) as usize;
        let l3 = unsafe { rpi5_stage1_ensure_table(l2, l2_index, allocator)? };
        let l3_index = ((start >> 12) & 0x1ff) as usize;
        unsafe {
            rpi5_stage1_write_empty_entry(
                l3,
                l3_index,
                start | flags | RPI5_STAGE1_PTE_TABLE_OR_PAGE,
            )?;
        }
        start += RPI5_STAGE1_PAGE_SIZE;
    }
    Ok(())
}

#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-stage1"
))]
unsafe fn rpi5_stage1_ensure_table(
    table: u64,
    index: usize,
    allocator: &mut Rpi5Stage1TableAllocator,
) -> Result<u64, &'static str> {
    let entry = unsafe { core::ptr::read_volatile((table as *const u64).add(index)) };
    if entry == 0 {
        let child = unsafe { allocator.allocate()? };
        unsafe {
            core::ptr::write_volatile(
                (table as *mut u64).add(index),
                child | RPI5_STAGE1_PTE_VALID | RPI5_STAGE1_PTE_TABLE_OR_PAGE,
            );
        }
        return Ok(child);
    }
    if entry & (RPI5_STAGE1_PTE_VALID | RPI5_STAGE1_PTE_TABLE_OR_PAGE)
        == (RPI5_STAGE1_PTE_VALID | RPI5_STAGE1_PTE_TABLE_OR_PAGE)
    {
        Ok(entry & RPI5_STAGE1_PTE_ADDR_MASK)
    } else {
        Err("mapping_conflict")
    }
}

#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-stage1"
))]
unsafe fn rpi5_stage1_write_empty_entry(
    table: u64,
    index: usize,
    value: u64,
) -> Result<(), &'static str> {
    let slot = unsafe { (table as *mut u64).add(index) };
    if unsafe { core::ptr::read_volatile(slot) } != 0 {
        return Err("mapping_conflict");
    }
    unsafe {
        core::ptr::write_volatile(slot, value);
    }
    Ok(())
}

#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-stage1"
))]
unsafe fn rpi5_stage1_enable_identity_mmu(
    root: u64,
    pt_pool: crate::arch::aarch64_boot_policy::Stage1KernelRange,
) -> Result<(), &'static str> {
    let mut sctlr: u64;
    unsafe {
        core::arch::asm!("mrs {0}, SCTLR_EL1", out(reg) sctlr, options(nostack, preserves_flags));
    }
    if sctlr & 1 != 0 {
        return Err("already_enabled");
    }
    let mut line = pt_pool.start;
    while line < pt_pool.end {
        unsafe {
            core::arch::asm!("dc cvac, {0}", in(reg) line, options(nostack, preserves_flags));
        }
        line += 64;
    }
    unsafe {
        core::arch::asm!(
            "dsb ish",
            "msr MAIR_EL1, {mair}",
            "msr TCR_EL1, {tcr}",
            "msr TTBR0_EL1, {root}",
            "msr TTBR1_EL1, xzr",
            "dsb ishst",
            "tlbi vmalle1",
            "dsb ish",
            "isb",
            "ic iallu",
            "dsb nsh",
            "isb",
            mair = in(reg) RPI5_STAGE1_MAIR_EL1,
            tcr = in(reg) RPI5_STAGE1_TCR_EL1,
            root = in(reg) root,
            options(nostack, preserves_flags)
        );
        sctlr |= (1 << 0) | (1 << 2) | (1 << 12);
        core::arch::asm!(
            "msr SCTLR_EL1, {0}",
            "isb",
            in(reg) sctlr,
            options(nostack, preserves_flags)
        );
        core::arch::asm!("mrs {0}, SCTLR_EL1", out(reg) sctlr, options(nostack, preserves_flags));
    }
    if sctlr & 1 == 0 {
        Err("sctlr_m_not_set")
    } else {
        Ok(())
    }
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
#[unsafe(no_mangle)]
extern "C" fn yarm_aarch64_boot_marker_start() {
    crate::arch::aarch64::console::write_line("YARM_AARCH64_BOOT_MARKER stage=_start");
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
#[unsafe(no_mangle)]
extern "C" fn yarm_aarch64_boot_breadcrumb_b0() {
    crate::arch::aarch64::console::write_line("YARM_AARCH64_BREADCRUMB B0");
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
#[unsafe(no_mangle)]
extern "C" fn yarm_aarch64_boot_breadcrumb_b1() {
    crate::arch::aarch64::console::write_line("YARM_AARCH64_BREADCRUMB B1");
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
#[unsafe(no_mangle)]
extern "C" fn yarm_aarch64_boot_breadcrumb_b2() {
    crate::arch::aarch64::console::write_line("YARM_AARCH64_BREADCRUMB B2");
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
#[unsafe(no_mangle)]
extern "C" fn yarm_aarch64_boot_breadcrumb_b3() {
    crate::arch::aarch64::console::write_line("YARM_AARCH64_BREADCRUMB B3");
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
#[unsafe(no_mangle)]
extern "C" fn yarm_aarch64_user_entry_marker_0() {
    crate::arch::aarch64::console::write_line("YARM_AARCH64_USER_ENTRY U0");
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
#[unsafe(no_mangle)]
extern "C" fn yarm_aarch64_user_entry_marker_1() {
    crate::arch::aarch64::console::write_line("YARM_AARCH64_USER_ENTRY U1");
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
#[unsafe(no_mangle)]
extern "C" fn yarm_aarch64_user_entry_marker_before_sp_el0() {
    crate::arch::aarch64::console::write_line("YARM_AARCH64_USER_ENTRY U_SP");
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
#[unsafe(no_mangle)]
extern "C" fn yarm_aarch64_user_entry_marker_before_elr() {
    crate::arch::aarch64::console::write_line("YARM_AARCH64_USER_ENTRY U_ELR");
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
#[unsafe(no_mangle)]
extern "C" fn yarm_aarch64_user_entry_marker_before_eret() {
    crate::arch::aarch64::console::write_line("YARM_AARCH64_USER_ENTRY U_ERET");
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
#[unsafe(no_mangle)]
extern "C" fn yarm_aarch64_write_elr_marker(elr: u64) {
    crate::yarm_log!("AARCH64_MSR_ELR_ACTUAL value=0x{:016x}", elr);
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
#[unsafe(no_mangle)]
extern "C" fn yarm_aarch64_before_eret_marker(elr: u64, sp: u64) {
    crate::yarm_log!(
        "YARM_AARCH64_BEFORE_ERET elr=0x{:016x} sp=0x{:016x}",
        elr,
        sp
    );
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
#[unsafe(no_mangle)]
extern "C" fn yarm_aarch64_vector_first_marker() {
    trap_trace_line("YARM_AARCH64_VECTOR_FIRST");
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
#[unsafe(no_mangle)]
extern "C" fn yarm_aarch64_vector_elr_marker(elr: u64) {
    LAST_VECTOR_RAW_ELR.store(elr, Ordering::Relaxed);
    boot_trace!("YARM_AARCH64_VECTOR_ELR_RAW elr=0x{:016x}", elr);
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
pub(crate) fn last_vector_raw_elr() -> u64 {
    LAST_VECTOR_RAW_ELR.load(Ordering::Relaxed)
}

#[cfg(any(feature = "hosted-dev", not(target_arch = "aarch64")))]
pub(crate) fn last_vector_raw_elr() -> u64 {
    0
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
#[unsafe(no_mangle)]
extern "C" fn yarm_aarch64_return_to_user_elr_marker(elr: u64) {
    boot_trace!("AARCH64_RETURN_TO_USER_ELR value=0x{:016x}", elr);
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
#[unsafe(no_mangle)]
extern "C" fn yarm_aarch64_return_to_user_x0_marker(x0: u64) {
    boot_trace!("AARCH64_RETURN_TO_USER_X0 value=0x{:016x}", x0);
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
#[unsafe(no_mangle)]
extern "C" fn yarm_aarch64_write_return_elr_marker(elr: u64) {
    boot_trace!("AARCH64_WRITE_RETURN_ELR value=0x{:016x}", elr);
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
#[unsafe(no_mangle)]
extern "C" fn yarm_aarch64_final_elr_reg_marker(elr: u64) {
    boot_trace!("AARCH64_FINAL_ELR_REG value=0x{:016x}", elr);
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
#[unsafe(no_mangle)]
extern "C" fn yarm_aarch64_enable_fp_simd() {
    unsafe {
        let mut cpacr_el1: u64;
        core::arch::asm!("mrs {0}, CPACR_EL1", out(reg) cpacr_el1, options(nomem, preserves_flags));
        cpacr_el1 |= (0b11u64) << 20;
        core::arch::asm!("msr CPACR_EL1, {0}", in(reg) cpacr_el1, options(nomem, preserves_flags));
        core::arch::asm!("isb", options(nomem, preserves_flags));
    }
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
static TRAP_KERNEL_STATE_PTR: core::sync::atomic::AtomicPtr<crate::kernel::boot::KernelState> =
    core::sync::atomic::AtomicPtr::new(core::ptr::null_mut());
static TRAP_SHARED_KERNEL_PTR: core::sync::atomic::AtomicPtr<crate::runtime::SharedKernel> =
    core::sync::atomic::AtomicPtr::new(core::ptr::null_mut());
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
static BSP_RELEASED_SECONDARIES: AtomicBool = AtomicBool::new(false);
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
static BSP_RELEASE_LOGGED: AtomicBool = AtomicBool::new(false);
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
static SECONDARY_ONLINE_LOGGED_MASK: AtomicU64 = AtomicU64::new(0);
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
static SECONDARY_READY_LOGGED_MASK: AtomicU64 = AtomicU64::new(0);
const AARCH64_LOCK_SPLIT_TRACE: bool = false;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
static SECONDARY_JOINED_LOGGED_MASK: AtomicU64 = AtomicU64::new(0);
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
static LAST_VECTOR_RAW_ELR: AtomicU64 = AtomicU64::new(0);
// One-shot: emitted once on the first AArch64 trap that takes the shared path.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
static STAGE2N_FIRST_TRAP_LOGGED: AtomicBool = AtomicBool::new(false);

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PsciConduit {
    Unknown = 0,
    Smc = 1,
    Hvc = 2,
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
static PSCI_CONDUIT: AtomicU8 = AtomicU8::new(PsciConduit::Unknown as u8);

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
fn install_trap_kernel_state(kernel: &mut crate::kernel::boot::KernelState) {
    TRAP_KERNEL_STATE_PTR.store(kernel as *mut _, core::sync::atomic::Ordering::SeqCst);
}

#[allow(dead_code)]
fn install_trap_shared_kernel(shared: &'static crate::runtime::SharedKernel) {
    TRAP_SHARED_KERNEL_PTR.store(
        shared as *const _ as *mut _,
        core::sync::atomic::Ordering::SeqCst,
    );
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
fn trap_kernel_state_mut() -> Option<&'static mut crate::kernel::boot::KernelState> {
    let ptr = TRAP_KERNEL_STATE_PTR.load(core::sync::atomic::Ordering::SeqCst);
    if ptr.is_null() {
        None
    } else {
        Some(unsafe { &mut *ptr })
    }
}

#[allow(dead_code)]
fn trap_shared_kernel() -> Option<&'static crate::runtime::SharedKernel> {
    let ptr = TRAP_SHARED_KERNEL_PTR.load(core::sync::atomic::Ordering::SeqCst);
    if ptr.is_null() {
        None
    } else {
        Some(unsafe { &*ptr })
    }
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
#[repr(C, align(16))]
struct Aarch64VectorFrame {
    gprs: [u64; 31],
    sp_el0: u64,
    elr_el1: u64,
    spsr_el1: u64,
    esr_el1: u64,
    far_el1: u64,
    neon: [[u8; 16]; 32],
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
const _: () = assert!(core::mem::size_of::<Aarch64VectorFrame>() == 800);
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
const _: () = assert!(core::mem::offset_of!(Aarch64VectorFrame, sp_el0) == 248);
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
const _: () = assert!(core::mem::offset_of!(Aarch64VectorFrame, elr_el1) == 256);
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
const _: () = assert!(core::mem::offset_of!(Aarch64VectorFrame, spsr_el1) == 264);
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
const _: () = assert!(core::mem::offset_of!(Aarch64VectorFrame, esr_el1) == 272);
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
const _: () = assert!(core::mem::offset_of!(Aarch64VectorFrame, far_el1) == 280);

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
fn write_trapframe_back_to_vector_frame(
    frame: &mut Aarch64VectorFrame,
    trap_frame: &crate::kernel::trapframe::TrapFrame,
) {
    for idx in 0..31 {
        frame.gprs[idx] = trap_frame.user_gpr(idx) as u64;
    }
    frame.sp_el0 = trap_frame.saved_sp() as u64;
    frame.elr_el1 = trap_frame.saved_pc() as u64;
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
#[unsafe(no_mangle)]
extern "C" fn yarm_aarch64_vector_entry(kind: u64, frame: *mut Aarch64VectorFrame) {
    // Stage 30 / Review C1: in debug builds, assert no boot raw-borrow window is
    // live. A trap/timer reaching with_cpu during that window would alias the boot
    // &mut KernelState (UB). Compiles to nothing in release; zero vector overhead.
    #[cfg(any(debug_assertions, test))]
    debug_assert!(
        !crate::runtime::boot_raw_borrow_is_active(),
        "aarch64 trap/timer vector fired during boot raw-borrow window — aliasing &mut KernelState risk"
    );
    trap_trace_line("YARM_AARCH64_VECTOR_ENTRY");
    trap_trace_line("YARM_AARCH64_BOOT_MARKER stage=exception");
    let Some(frame) = (unsafe { frame.as_mut() }) else {
        crate::arch::aarch64::console::write_line("YARM_AARCH64_EXCEPTION_FRAME missing");
        return;
    };
    struct FixedBufWriter<'a> {
        buf: &'a mut [u8],
        len: usize,
    }
    impl Write for FixedBufWriter<'_> {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            let bytes = s.as_bytes();
            let remaining = self.buf.len().saturating_sub(self.len);
            let copy_len = remaining.min(bytes.len());
            self.buf[self.len..self.len + copy_len].copy_from_slice(&bytes[..copy_len]);
            self.len += copy_len;
            Ok(())
        }
    }
    if AARCH64_TRAP_TRACE {
        let mut line = [0u8; 160];
        let mut writer = FixedBufWriter {
            buf: &mut line,
            len: 0,
        };
        let _ = write!(
            writer,
            "YARM_AARCH64_EXCEPTION_REGS esr_el1=0x{:016x} far_el1=0x{:016x} elr_el1=0x{:016x} spsr_el1=0x{:016x}",
            frame.esr_el1,
            frame.far_el1,
            crate::arch::aarch64::boot::last_vector_raw_elr(),
            frame.spsr_el1
        );
        let line_len = writer.len;
        if let Ok(msg) = core::str::from_utf8(&line[..line_len]) {
            crate::arch::aarch64::console::write_line(msg);
        }
    }
    let is_irq_kind = matches!(kind, 2 | 6 | 10 | 14);
    let trap_cpu =
        crate::kernel::scheduler::CpuId((crate::arch::aarch64::read_mpidr_el1() & 0xff) as u8);
    if let Some(shared) = trap_shared_kernel() {
        if AARCH64_LOCK_SPLIT_TRACE {
            crate::yarm_log!("YARM_LOCK_SPLIT_STAGE2N path=aarch64_shared_trap_entry");
        }
        if STAGE2N_FIRST_TRAP_LOGGED
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
        {
            crate::yarm_log!("YARM_LOCK_SPLIT_STAGE2N_FIRST_SHARED_TRAP arch=aarch64");
        }
        let mut trap_frame = crate::kernel::trapframe::TrapFrame::zeroed();
        trap_frame.set_saved_pc(frame.elr_el1 as usize);
        trap_frame.set_saved_sp(frame.sp_el0 as usize);
        for idx in 0..31 {
            trap_frame.set_user_gpr(idx, frame.gprs[idx] as usize);
        }
        let context = crate::arch::aarch64::trap::Aarch64TrapContext {
            esr_el1: frame.esr_el1 as u32,
            far_el1: frame.far_el1,
            irq_line: None,
            is_timer_irq: is_irq_kind,
        };
        if crate::arch::trap_entry::dispatch_trap_entry_with_shared_kernel(
            shared,
            trap_cpu,
            context,
            Some(&mut trap_frame),
        )
        .is_ok()
        {
            write_trapframe_back_to_vector_frame(frame, &trap_frame);
            if AARCH64_TRAP_TRACE {
                let log_tid = shared.current_tid_split_read(trap_cpu).unwrap_or(0);
                boot_trace!(
                    "AARCH64_VECTOR_FRAME_FINAL tid={} x0={} x1={} x2={}",
                    log_tid,
                    frame.gprs[0],
                    frame.gprs[1],
                    frame.gprs[2],
                );
            }
        } else {
            crate::arch::aarch64::console::write_line("YARM_AARCH64_TRAP_HANDLE failed");
            crate::arch::aarch64::console::write_line("YARM_AARCH64_TRAP_HANDLE halting");
            loop {
                unsafe {
                    core::arch::asm!("wfe", options(nomem, nostack, preserves_flags));
                }
            }
        }
    } else if let Some(kernel) = trap_kernel_state_mut() {
        if AARCH64_LOCK_SPLIT_TRACE {
            crate::yarm_log!("YARM_LOCK_SPLIT_STAGE2N path=aarch64_shared_trap_entry fallback=1");
        }
        crate::yarm_log!("YARM_LOCK_SPLIT_STAGE2N_FALLBACK arch=aarch64 reason=no_shared_kernel");
        let current_tid = kernel.current_tid();
        if current_tid == Some(1) {
            boot_trace!(
                "AARCH64_HANDOFF_TRAP cpu={} tid={} ESR_EL1=0x{:016x} ELR_EL1=0x{:016x} FAR_EL1=0x{:016x} SPSR_EL1=0x{:016x}",
                trap_cpu.0,
                current_tid.unwrap_or(0),
                frame.esr_el1,
                crate::arch::aarch64::boot::last_vector_raw_elr(),
                frame.far_el1,
                frame.spsr_el1
            );
        }
        let mut trap_frame = crate::kernel::trapframe::TrapFrame::zeroed();
        trap_frame.set_saved_pc(frame.elr_el1 as usize);
        trap_frame.set_saved_sp(frame.sp_el0 as usize);
        for idx in 0..31 {
            trap_frame.set_user_gpr(idx, frame.gprs[idx] as usize);
        }
        let context = crate::arch::aarch64::trap::Aarch64TrapContext {
            esr_el1: frame.esr_el1 as u32,
            far_el1: frame.far_el1,
            irq_line: None,
            is_timer_irq: is_irq_kind,
        };
        if crate::arch::aarch64::trap::handle_trap_entry(
            kernel,
            trap_cpu,
            context,
            Some(&mut trap_frame),
        )
        .is_ok()
        {
            write_trapframe_back_to_vector_frame(frame, &trap_frame);
            boot_trace!(
                "AARCH64_VECTOR_FRAME_FINAL tid={} x0={} x1={} x2={}",
                kernel.current_tid().unwrap_or(0),
                frame.gprs[0],
                frame.gprs[1],
                frame.gprs[2],
            );
        } else {
            crate::arch::aarch64::console::write_line("YARM_AARCH64_TRAP_HANDLE failed");
            crate::arch::aarch64::console::write_line("YARM_AARCH64_TRAP_HANDLE halting");
            loop {
                unsafe {
                    core::arch::asm!("wfe", options(nomem, nostack, preserves_flags));
                }
            }
        }
    } else {
        crate::arch::aarch64::console::write_line("YARM_AARCH64_TRAP_HANDLE no_kernel_state");
    }
    match kind {
        1 => crate::arch::aarch64::console::write_line(
            "YARM_AARCH64_EXCEPTION_KIND sync_current_sp0",
        ),
        2 => {
            crate::arch::aarch64::console::write_line("YARM_AARCH64_EXCEPTION_KIND irq_current_sp0")
        }
        3 => {
            crate::arch::aarch64::console::write_line("YARM_AARCH64_EXCEPTION_KIND fiq_current_sp0")
        }
        4 => crate::arch::aarch64::console::write_line(
            "YARM_AARCH64_EXCEPTION_KIND serr_current_sp0",
        ),
        5 => crate::arch::aarch64::console::write_line(
            "YARM_AARCH64_EXCEPTION_KIND sync_current_spx",
        ),
        6 => {
            crate::arch::aarch64::console::write_line("YARM_AARCH64_EXCEPTION_KIND irq_current_spx")
        }
        7 => {
            crate::arch::aarch64::console::write_line("YARM_AARCH64_EXCEPTION_KIND fiq_current_spx")
        }
        8 => crate::arch::aarch64::console::write_line(
            "YARM_AARCH64_EXCEPTION_KIND serr_current_spx",
        ),
        9 => {
            crate::arch::aarch64::console::write_line("YARM_AARCH64_EXCEPTION_KIND sync_lower_a64")
        }
        10 => {
            crate::arch::aarch64::console::write_line("YARM_AARCH64_EXCEPTION_KIND irq_lower_a64")
        }
        11 => {
            crate::arch::aarch64::console::write_line("YARM_AARCH64_EXCEPTION_KIND fiq_lower_a64")
        }
        12 => {
            crate::arch::aarch64::console::write_line("YARM_AARCH64_EXCEPTION_KIND serr_lower_a64")
        }
        13 => {
            crate::arch::aarch64::console::write_line("YARM_AARCH64_EXCEPTION_KIND sync_lower_a32")
        }
        14 => {
            crate::arch::aarch64::console::write_line("YARM_AARCH64_EXCEPTION_KIND irq_lower_a32")
        }
        15 => {
            crate::arch::aarch64::console::write_line("YARM_AARCH64_EXCEPTION_KIND fiq_lower_a32")
        }
        16 => {
            crate::arch::aarch64::console::write_line("YARM_AARCH64_EXCEPTION_KIND serr_lower_a32")
        }
        _ => crate::arch::aarch64::console::write_line("YARM_AARCH64_EXCEPTION_KIND unknown"),
    }
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
const RING3_INIT_SERVER_TID: u64 = 1;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
const RING3_SUPERVISOR_TID: u64 = 2;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
const RING3_PM_SERVER_TID: u64 = 3;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
const INITRAMFS_HELLO_WORLD_IMAGE_ID: u64 = 0x494E_4954_4845_4C4F; // "INITHELO"

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
fn initramfs_static_hello_world_elf() -> [u8; 256] {
    let mut image = [0u8; 256];
    // ELF header.
    image[..4].copy_from_slice(b"\x7FELF");
    image[4] = 2; // ELFCLASS64
    image[5] = 1; // little-endian
    image[6] = 1; // EV_CURRENT
    image[7] = 0; // SYSV ABI
    image[16..18].copy_from_slice(&2u16.to_le_bytes()); // ET_EXEC
    image[18..20].copy_from_slice(&0xB7u16.to_le_bytes()); // EM_AARCH64
    image[20..24].copy_from_slice(&1u32.to_le_bytes()); // EV_CURRENT
    let entry = 0x0040_1000u64;
    image[24..32].copy_from_slice(&entry.to_le_bytes());
    image[32..40].copy_from_slice(&64u64.to_le_bytes()); // e_phoff
    image[52..54].copy_from_slice(&(64u16).to_le_bytes()); // e_ehsize
    image[54..56].copy_from_slice(&(56u16).to_le_bytes()); // e_phentsize
    image[56..58].copy_from_slice(&(1u16).to_le_bytes()); // e_phnum

    // Single PT_LOAD segment.
    let ph = 64usize;
    image[ph..ph + 4].copy_from_slice(&1u32.to_le_bytes()); // PT_LOAD
    image[ph + 4..ph + 8].copy_from_slice(&5u32.to_le_bytes()); // RX
    image[ph + 8..ph + 16].copy_from_slice(&128u64.to_le_bytes()); // p_offset
    image[ph + 16..ph + 24].copy_from_slice(&(entry & !0xFFF).to_le_bytes()); // p_vaddr
    image[ph + 24..ph + 32].copy_from_slice(&0u64.to_le_bytes()); // p_paddr
    image[ph + 32..ph + 40].copy_from_slice(&20u64.to_le_bytes()); // p_filesz
    image[ph + 40..ph + 48].copy_from_slice(&20u64.to_le_bytes()); // p_memsz
    image[ph + 48..ph + 56].copy_from_slice(&0x1000u64.to_le_bytes()); // p_align

    // Minimal first-user diagnostic stub:
    // movz x8,#0 ; movz x0,#0x1234 ; movz x1,#0xBEEF ; svc #0 ; b .
    image[128..148].copy_from_slice(&[
        0x08, 0x00, 0x80, 0xD2, // movz x8, #0
        0x80, 0x46, 0x82, 0xD2, // movz x0, #0x1234
        0xE1, 0xDD, 0x97, 0xD2, // movz x1, #0xBEEF
        0x01, 0x00, 0x00, 0xD4, // svc #0
        0x00, 0x00, 0x00, 0x14, // b .
    ]);
    image
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
const INITRD_INIT_ELF_MAX_SIZE: usize = 16 * 1024 * 1024;

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
fn load_init_elf_from_initramfs_vfs() -> Option<alloc::vec::Vec<u8>> {
    let bytes = crate::kernel::boot::Bootstrap::boot_initrd_bytes()?;
    let entry = yarm_srv_common::cpio::CpioArchive::new(bytes)
        .find("/init")
        .ok()
        .flatten()
        .or_else(|| {
            yarm_srv_common::cpio::CpioArchive::new(bytes)
                .find("init")
                .ok()
                .flatten()
        })?;
    let file_data = entry.file_data();
    crate::yarm_log!("YARM_INITRD_INIT_FOUND len={}", file_data.len());
    if file_data.len() > INITRD_INIT_ELF_MAX_SIZE {
        crate::yarm_log!(
            "YARM_INITRD_INIT_TOO_LARGE len={} cap={}",
            file_data.len(),
            INITRD_INIT_ELF_MAX_SIZE
        );
        return None;
    }
    Some(alloc::vec::Vec::from(file_data))
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
fn load_supervisor_elf_from_initramfs_vfs() -> Option<alloc::vec::Vec<u8>> {
    let bytes = crate::kernel::boot::Bootstrap::boot_initrd_bytes()?;
    let entry = yarm_srv_common::cpio::CpioArchive::new(bytes)
        .find("sbin/supervisor")
        .ok()
        .flatten()?;
    let file_data = entry.file_data();
    if file_data.len() > INITRD_INIT_ELF_MAX_SIZE {
        return None;
    }
    Some(alloc::vec::Vec::from(file_data))
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
fn load_pm_elf_from_initramfs_vfs() -> Option<alloc::vec::Vec<u8>> {
    let bytes = crate::kernel::boot::Bootstrap::boot_initrd_bytes()?;
    let entry = yarm_srv_common::cpio::CpioArchive::new(bytes)
        .find("sbin/process_manager")
        .ok()
        .flatten()?;
    let file_data = entry.file_data();
    if file_data.len() > INITRD_INIT_ELF_MAX_SIZE {
        return None;
    }
    Some(alloc::vec::Vec::from(file_data))
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
pub fn bootstrap_first_user_task(
    kernel: &mut crate::kernel::boot::KernelState,
) -> Result<(), crate::kernel::boot::KernelError> {
    use crate::kernel::boot::UserImageSpec;
    use crate::kernel::task::TaskClass;

    if kernel.task_asid(RING3_INIT_SERVER_TID).is_some() {
        return Ok(());
    }

    // Load ELFs and create address spaces.
    let (init_asid, _) = kernel.create_user_address_space().map_err(|e| {
        crate::yarm_log!("YARM_FIRST_USER_FAIL step=create_init_asid err={:?}", e);
        e
    })?;
    let init_image = load_init_elf_from_initramfs_vfs();
    let init_fallback = initramfs_static_hello_world_elf();
    let (init_bytes, init_source): (&[u8], &str) = match init_image.as_deref() {
        Some(img) => (img, "initrd"),
        None => (&init_fallback, "synthetic"),
    };
    let init_elf_info = yarm_srv_common::elf::ElfImageInfo::parse(0, init_bytes).map_err(|e| {
        crate::yarm_log!(
            "YARM_FIRST_USER_FAIL step=parse_init_elf_header err={:?}",
            e
        );
        crate::kernel::boot::KernelError::WrongObject
    })?;
    let (_, init_first_pt_load, init_heap) = kernel
        .load_elf_pt_load_segments(init_asid, init_bytes)
        .map_err(|e| {
            crate::yarm_log!("YARM_FIRST_USER_FAIL step=load_init_elf err={:?}", e);
            e
        })?;
    let init_entry = init_elf_info.entry as usize;
    crate::yarm_log!("INIT_ELF_HEADER_ENTRY value={:#x}", init_elf_info.entry);
    crate::yarm_log!("INIT_FIRST_PT_LOAD_VADDR value={:#x}", init_first_pt_load);
    crate::yarm_log!("INIT_SELECTED_ENTRY value={:#x}", init_entry);
    if init_entry == init_first_pt_load {
        crate::yarm_log!(
            "INIT_ENTRY_EQUALS_FIRST_PT_LOAD_WARN: ELF e_entry matches first PT_LOAD base; entry may be wrong"
        );
    }
    crate::yarm_log!(
        "YARM_INITRD_INIT_ELF_SELECTED entry={:#x} source={}",
        init_entry,
        init_source
    );

    let supervisor_image = load_supervisor_elf_from_initramfs_vfs();
    let supervisor_aei: Option<(_, usize, usize)> = if let Some(ref sup_bytes) = supervisor_image {
        let sup_elf_info =
            yarm_srv_common::elf::ElfImageInfo::parse(1, sup_bytes).map_err(|e| {
                crate::yarm_log!(
                    "YARM_FIRST_USER_FAIL step=parse_supervisor_elf_header err={:?}",
                    e
                );
                crate::kernel::boot::KernelError::WrongObject
            })?;
        let (sup_asid, _) = kernel.create_user_address_space().map_err(|e| {
            crate::yarm_log!(
                "YARM_FIRST_USER_FAIL step=create_supervisor_asid err={:?}",
                e
            );
            e
        })?;
        let (_, sup_first_pt_load, sup_heap) = kernel
            .load_elf_pt_load_segments(sup_asid, sup_bytes)
            .map_err(|e| {
                crate::yarm_log!("YARM_FIRST_USER_FAIL step=load_supervisor_elf err={:?}", e);
                e
            })?;
        let sup_entry = sup_elf_info.entry as usize;
        crate::yarm_log!(
            "SUPERVISOR_ELF_HEADER_ENTRY value={:#x}",
            sup_elf_info.entry
        );
        crate::yarm_log!(
            "SUPERVISOR_FIRST_PT_LOAD_VADDR value={:#x}",
            sup_first_pt_load
        );
        crate::yarm_log!("SUPERVISOR_SELECTED_ENTRY value={:#x}", sup_entry);
        if sup_entry == sup_first_pt_load {
            crate::yarm_log!(
                "SUPERVISOR_ENTRY_EQUALS_FIRST_PT_LOAD_WARN: ELF e_entry matches first PT_LOAD base; entry may be wrong"
            );
        }
        Some((sup_asid, sup_entry, sup_heap))
    } else {
        crate::yarm_log!("YARM_SUPERVISOR_ELF_MISSING path=sbin/supervisor");
        return Err(crate::kernel::boot::KernelError::MemoryObjectMissing);
    };

    let pm_image = load_pm_elf_from_initramfs_vfs();
    let pm_aei: Option<(_, usize, usize)> = if let Some(ref pm_bytes) = pm_image {
        let pm_elf_info = yarm_srv_common::elf::ElfImageInfo::parse(2, pm_bytes).map_err(|e| {
            crate::yarm_log!("YARM_FIRST_USER_FAIL step=parse_pm_elf_header err={:?}", e);
            crate::kernel::boot::KernelError::WrongObject
        })?;
        let (pm_asid, _) = kernel.create_user_address_space().map_err(|e| {
            crate::yarm_log!("YARM_FIRST_USER_FAIL step=create_pm_asid err={:?}", e);
            e
        })?;
        let (_, pm_first_pt_load, pm_heap) = kernel
            .load_elf_pt_load_segments(pm_asid, pm_bytes)
            .map_err(|e| {
                crate::yarm_log!("YARM_FIRST_USER_FAIL step=load_pm_elf err={:?}", e);
                e
            })?;
        let pm_entry = pm_elf_info.entry as usize;
        crate::yarm_log!("PM_ELF_HEADER_ENTRY value={:#x}", pm_elf_info.entry);
        crate::yarm_log!("PM_FIRST_PT_LOAD_VADDR value={:#x}", pm_first_pt_load);
        crate::yarm_log!("PM_SELECTED_ENTRY value={:#x}", pm_entry);
        if pm_entry == pm_first_pt_load {
            crate::yarm_log!(
                "PM_ENTRY_EQUALS_FIRST_PT_LOAD_WARN: ELF e_entry matches first PT_LOAD base; entry may be wrong"
            );
        }
        Some((pm_asid, pm_entry, pm_heap))
    } else {
        crate::yarm_log!("YARM_PM_ELF_MISSING path=sbin/process_manager");
        return Err(crate::kernel::boot::KernelError::MemoryObjectMissing);
    };

    // Pre-register all tasks so cap grants work.
    if supervisor_aei.is_some() {
        kernel
            .register_task_with_class(RING3_SUPERVISOR_TID, TaskClass::SystemServer)
            .map_err(|e| {
                crate::yarm_log!("YARM_FIRST_USER_FAIL step=register_supervisor err={:?}", e);
                e
            })?;
    }
    if pm_aei.is_some() {
        kernel
            .register_task_with_class(RING3_PM_SERVER_TID, TaskClass::SystemServer)
            .map_err(|e| {
                crate::yarm_log!("YARM_FIRST_USER_FAIL step=register_pm err={:?}", e);
                e
            })?;
    }
    kernel
        .register_task_with_class(RING3_INIT_SERVER_TID, TaskClass::SystemServer)
        .map_err(|e| {
            crate::yarm_log!("YARM_FIRST_USER_FAIL step=register_init err={:?}", e);
            e
        })?;

    // Create endpoints and grant capabilities per boot topology.
    // EP1: PM-inbound — PM gets RECV (slot 17), init gets SEND (slot 1).
    let (_, pm_inbound_send_root, pm_inbound_recv_root) =
        kernel.create_endpoint(16).map_err(|e| {
            crate::yarm_log!("YARM_FIRST_USER_FAIL step=create_pm_inbound_ep err={:?}", e);
            e
        })?;
    let pm_inbound_send_init = kernel
        .grant_capability_task_to_task_with_rights(
            0,
            pm_inbound_send_root,
            RING3_INIT_SERVER_TID,
            crate::kernel::capabilities::CapRights::SEND,
        )
        .map_err(|e| {
            crate::yarm_log!("YARM_GRANT_FAIL cap=pm_inbound_send_init err={:?}", e);
            e
        })?;
    crate::yarm_log!(
        "CAP_GRANT_BOOT dst_tid={} slot=1 cap={} rights=SEND result=ok",
        RING3_INIT_SERVER_TID,
        pm_inbound_send_init.0
    );
    let pm_inbound_send_sup = if supervisor_aei.is_some() {
        let c = kernel
            .grant_capability_task_to_task_with_rights(
                0,
                pm_inbound_send_root,
                RING3_SUPERVISOR_TID,
                crate::kernel::capabilities::CapRights::SEND,
            )
            .map_err(|e| {
                crate::yarm_log!("YARM_GRANT_FAIL cap=pm_inbound_send_sup err={:?}", e);
                e
            })?;
        crate::yarm_log!(
            "CAP_GRANT_BOOT dst_tid={} slot=1 cap={} rights=SEND result=ok",
            RING3_SUPERVISOR_TID,
            c.0
        );
        Some(c)
    } else {
        None
    };
    let pm_inbound_recv_pm = if pm_aei.is_some() {
        let c = kernel
            .grant_capability_task_to_task_with_rights(
                0,
                pm_inbound_recv_root,
                RING3_PM_SERVER_TID,
                crate::kernel::capabilities::CapRights::RECEIVE,
            )
            .map_err(|e| {
                crate::yarm_log!("YARM_GRANT_FAIL cap=pm_inbound_recv_pm err={:?}", e);
                e
            })?;
        crate::yarm_log!(
            "CAP_GRANT_BOOT dst_tid={} slot=17 cap={} rights=RECEIVE result=ok",
            RING3_PM_SERVER_TID,
            c.0
        );
        Some(c)
    } else {
        None
    };

    // EP2: Init-reply — init gets RECV (slot 2).
    let (_, _, init_reply_recv_root) = kernel.create_endpoint(8).map_err(|e| {
        crate::yarm_log!("YARM_FIRST_USER_FAIL step=create_init_reply_ep err={:?}", e);
        e
    })?;
    let init_reply_recv_init = kernel
        .grant_capability_task_to_task_with_rights(
            0,
            init_reply_recv_root,
            RING3_INIT_SERVER_TID,
            crate::kernel::capabilities::CapRights::RECEIVE,
        )
        .map_err(|e| {
            crate::yarm_log!("YARM_GRANT_FAIL cap=init_reply_recv_init err={:?}", e);
            e
        })?;
    crate::yarm_log!(
        "CAP_GRANT_BOOT dst_tid={} slot=2 cap={} rights=RECEIVE result=ok",
        RING3_INIT_SERVER_TID,
        init_reply_recv_init.0
    );

    // EP2b: PM outbound reply endpoint — PM gets a dedicated RECV (slot 2).
    let (_, _, pm_outbound_reply_recv_root) = kernel.create_endpoint(8).map_err(|e| {
        crate::yarm_log!(
            "YARM_FIRST_USER_FAIL step=create_pm_outbound_reply_ep err={:?}",
            e
        );
        e
    })?;
    let pm_outbound_reply_recv_pm = if pm_aei.is_some() {
        let c = kernel
            .grant_capability_task_to_task_with_rights(
                0,
                pm_outbound_reply_recv_root,
                RING3_PM_SERVER_TID,
                crate::kernel::capabilities::CapRights::RECEIVE,
            )
            .map_err(|e| {
                crate::yarm_log!("YARM_GRANT_FAIL cap=pm_outbound_reply_recv_pm err={:?}", e);
                e
            })?;
        crate::yarm_log!(
            "CAP_GRANT_BOOT dst_tid={} slot=2 cap={} rights=RECEIVE result=ok",
            RING3_PM_SERVER_TID,
            c.0
        );
        Some(c)
    } else {
        None
    };

    // EP3: Supervisor fault — supervisor gets RECV (slot 3).
    let (_, _, sup_fault_recv_root) = kernel.create_endpoint(8).map_err(|e| {
        crate::yarm_log!("YARM_FIRST_USER_FAIL step=create_sup_fault_ep err={:?}", e);
        e
    })?;
    let sup_fault_recv_sup = if supervisor_aei.is_some() {
        let c = kernel
            .grant_capability_task_to_task_with_rights(
                0,
                sup_fault_recv_root,
                RING3_SUPERVISOR_TID,
                crate::kernel::capabilities::CapRights::RECEIVE,
            )
            .map_err(|e| {
                crate::yarm_log!("YARM_GRANT_FAIL cap=sup_fault_recv_sup err={:?}", e);
                e
            })?;
        crate::yarm_log!(
            "CAP_GRANT_BOOT dst_tid={} slot=3 cap={} rights=RECEIVE result=ok",
            RING3_SUPERVISOR_TID,
            c.0
        );
        Some(c)
    } else {
        None
    };

    // EP4: Supervisor control — supervisor SEND (slot 4), supervisor RECV (slot 5).
    let (_, sup_ctrl_send_root, sup_ctrl_recv_root) = kernel.create_endpoint(8).map_err(|e| {
        crate::yarm_log!("YARM_FIRST_USER_FAIL step=create_sup_ctrl_ep err={:?}", e);
        e
    })?;
    let sup_ctrl_send_sup = if supervisor_aei.is_some() {
        let c = kernel
            .grant_capability_task_to_task_with_rights(
                0,
                sup_ctrl_send_root,
                RING3_SUPERVISOR_TID,
                crate::kernel::capabilities::CapRights::SEND,
            )
            .map_err(|e| {
                crate::yarm_log!("YARM_GRANT_FAIL cap=sup_ctrl_send_sup err={:?}", e);
                e
            })?;
        crate::yarm_log!(
            "CAP_GRANT_BOOT dst_tid={} slot=4 cap={} rights=SEND result=ok",
            RING3_SUPERVISOR_TID,
            c.0
        );
        Some(c)
    } else {
        None
    };
    let sup_ctrl_recv_sup = if supervisor_aei.is_some() {
        let c = kernel
            .grant_capability_task_to_task_with_rights(
                0,
                sup_ctrl_recv_root,
                RING3_SUPERVISOR_TID,
                crate::kernel::capabilities::CapRights::RECEIVE,
            )
            .map_err(|e| {
                crate::yarm_log!("YARM_GRANT_FAIL cap=sup_ctrl_recv_sup err={:?}", e);
                e
            })?;
        crate::yarm_log!(
            "CAP_GRANT_BOOT dst_tid={} slot=5 cap={} rights=RECEIVE result=ok",
            RING3_SUPERVISOR_TID,
            c.0
        );
        Some(c)
    } else {
        None
    };

    // EP5: Supervisor PM reply — supervisor gets RECV (slot 2); distinct from init's EP2.
    let (_, _, sup_pm_reply_recv_root) = kernel.create_endpoint(8).map_err(|e| {
        crate::yarm_log!(
            "YARM_FIRST_USER_FAIL step=create_sup_pm_reply_ep err={:?}",
            e
        );
        e
    })?;
    let sup_pm_reply_recv_sup = if supervisor_aei.is_some() {
        let c = kernel
            .grant_capability_task_to_task_with_rights(
                0,
                sup_pm_reply_recv_root,
                RING3_SUPERVISOR_TID,
                crate::kernel::capabilities::CapRights::RECEIVE,
            )
            .map_err(|e| {
                crate::yarm_log!("YARM_GRANT_FAIL cap=sup_pm_reply_recv_sup err={:?}", e);
                e
            })?;
        crate::yarm_log!(
            "CAP_GRANT_BOOT dst_tid={} slot=2 cap={} rights=RECEIVE result=ok",
            RING3_SUPERVISOR_TID,
            c.0
        );
        Some(c)
    } else {
        None
    };

    // Register supervisor as the kernel fault handler for its own TID.
    if let Some(fault_cap) = sup_fault_recv_sup {
        kernel
            .set_supervisor_endpoint_for_task(RING3_SUPERVISOR_TID, fault_cap)
            .map_err(|e| {
                crate::yarm_log!(
                    "YARM_FIRST_USER_FAIL step=set_supervisor_endpoint err={:?}",
                    e
                );
                e
            })?;
    }

    // Spawn supervisor (TID 2) first.
    if let Some((sup_asid, sup_entry, sup_heap)) = supervisor_aei {
        let mut sup_args = UserImageSpec::DEFAULT_STARTUP_ARGS;
        sup_args[0] = RING3_SUPERVISOR_TID;
        if let Some(c) = pm_inbound_send_sup {
            sup_args[1] = c.0;
        }
        if let Some(c) = sup_pm_reply_recv_sup {
            sup_args[2] = c.0;
        }
        if let Some(c) = sup_fault_recv_sup {
            sup_args[3] = c.0;
        }
        if let Some(c) = sup_ctrl_send_sup {
            sup_args[4] = c.0;
        }
        if let Some(c) = sup_ctrl_recv_sup {
            sup_args[5] = c.0;
        }
        sup_args[8] = RING3_INIT_SERVER_TID;
        for n in 0..10usize {
            crate::yarm_log!("SUP_STARTUP_SLOT slot={} value={}", n, sup_args[n]);
        }
        kernel
            .spawn_user_task_from_image(UserImageSpec {
                tid: RING3_SUPERVISOR_TID,
                entry: sup_entry,
                asid: Some(sup_asid),
                class: TaskClass::SystemServer,
                startup_args: sup_args,
                ..Default::default()
            })
            .map_err(|e| {
                crate::yarm_log!("YARM_FIRST_USER_FAIL step=spawn_supervisor err={:?}", e);
                e
            })?;
        kernel
            .set_task_brk_bounds(RING3_SUPERVISOR_TID, sup_heap, sup_heap)
            .map_err(|e| {
                crate::yarm_log!("YARM_FIRST_USER_FAIL step=set_supervisor_brk err={:?}", e);
                e
            })?;
        crate::yarm_log!("YARM_SUPERVISOR_TID2_SPAWNED tid={}", RING3_SUPERVISOR_TID);
    }

    // Spawn PM (TID 3) second.
    if let Some((pm_asid, pm_entry, pm_heap)) = pm_aei {
        let mut pm_args = UserImageSpec::DEFAULT_STARTUP_ARGS;
        pm_args[0] = RING3_PM_SERVER_TID;
        if let Some(c) = pm_outbound_reply_recv_pm {
            pm_args[2] = c.0;
        }
        if let Some(c) = pm_inbound_recv_pm {
            pm_args[17] = c.0;
        }
        kernel
            .spawn_user_task_from_image(UserImageSpec {
                tid: RING3_PM_SERVER_TID,
                entry: pm_entry,
                asid: Some(pm_asid),
                class: TaskClass::SystemServer,
                startup_args: pm_args,
                ..Default::default()
            })
            .map_err(|e| {
                crate::yarm_log!("YARM_FIRST_USER_FAIL step=spawn_pm err={:?}", e);
                e
            })?;
        kernel
            .set_task_brk_bounds(RING3_PM_SERVER_TID, pm_heap, pm_heap)
            .map_err(|e| {
                crate::yarm_log!("YARM_FIRST_USER_FAIL step=set_pm_brk err={:?}", e);
                e
            })?;
        crate::yarm_log!("YARM_PM_TID3_SPAWNED tid={}", RING3_PM_SERVER_TID);
    }

    // Spawn init (TID 1) third.
    let mut init_args = UserImageSpec::DEFAULT_STARTUP_ARGS;
    init_args[0] = RING3_INIT_SERVER_TID;
    init_args[1] = pm_inbound_send_init.0;
    init_args[2] = init_reply_recv_init.0;
    init_args[9] = RING3_SUPERVISOR_TID;
    crate::yarm_log!(
        "YARM_FIRST_USER_STARTUP_ARGS tid={} arg0={} arg1={} arg2={} arg3={}",
        RING3_INIT_SERVER_TID,
        init_args[0],
        init_args[1],
        init_args[2],
        init_args[3]
    );
    crate::yarm_log!("YARM_FIRST_USER_SPAWN_BEGIN tid={}", RING3_INIT_SERVER_TID);
    kernel
        .spawn_user_task_from_image(UserImageSpec {
            tid: RING3_INIT_SERVER_TID,
            entry: init_entry,
            asid: Some(init_asid),
            class: TaskClass::SystemServer,
            startup_args: init_args,
            ..Default::default()
        })
        .map_err(|e| {
            crate::yarm_log!("YARM_FIRST_USER_FAIL step=spawn_init err={:?}", e);
            e
        })?;
    kernel
        .set_task_brk_bounds(RING3_INIT_SERVER_TID, init_heap, init_heap)
        .map_err(|e| {
            crate::yarm_log!("YARM_FIRST_USER_FAIL step=set_init_brk err={:?}", e);
            e
        })?;
    crate::yarm_log!(
        "YARM_INIT_DONE arch=aarch64 phase={} image_id=0x{:x} seeded=0 initramfs_handled=1 devfs_handled=0",
        if init_source == "initrd" {
            "initrd_init_elf"
        } else {
            "kernel_static_init_elf"
        },
        INITRAMFS_HELLO_WORLD_IMAGE_ID
    );
    Ok(())
}

#[cfg(any(feature = "hosted-dev", not(target_arch = "aarch64")))]
pub fn bootstrap_first_user_task(
    _kernel: &mut crate::kernel::boot::KernelState,
) -> Result<(), crate::kernel::boot::KernelError> {
    Ok(())
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
pub fn release_secondary_cpus_after_bootstrap() {
    BSP_RELEASED_SECONDARIES.store(true, Ordering::Release);
    if !BSP_RELEASE_LOGGED.swap(true, Ordering::AcqRel) {
        crate::yarm_log!("YARM_AARCH64_SMP_RELEASE cpu=0 released=1 src=boot_release_hook");
    }
    unsafe {
        core::arch::asm!("sev", options(nomem, nostack, preserves_flags));
    }
}

#[cfg(any(feature = "hosted-dev", not(target_arch = "aarch64")))]
pub fn release_secondary_cpus_after_bootstrap() {}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
pub fn enter_dispatched_user_task_if_available(
    kernel: &crate::kernel::boot::KernelState,
    dispatched_tid: Option<u64>,
) {
    if let Some(tid) = dispatched_tid
        && let Some(context) = kernel.thread_user_context(tid)
        && context.instruction_ptr.0 != 0
        && context.stack_ptr.0 != 0
    {
        let tls = kernel.thread_tls_base(tid).unwrap_or(0) as u64;
        crate::yarm_log!(
            "YARM_AARCH64_RING3_INIT_TASK tid={} entry=0x{:x} stack_top=0x{:x} tls=0x{:x}",
            tid,
            context.instruction_ptr.0,
            context.stack_ptr.0,
            tls
        );
        if kernel.current_cpu().0 == crate::arch::platform_constants::BOOTSTRAP_CPU_ID {
            let ttbr0_el1: u64 = {
                let value: u64;
                unsafe {
                    core::arch::asm!(
                        "mrs {0}, ttbr0_el1",
                        out(reg) value,
                        options(nomem, preserves_flags)
                    );
                }
                value
            };
            crate::yarm_log!(
                "BSP_BEFORE_ENTER_RING3 tid={} entry=0x{:x} stack_top=0x{:x}",
                tid,
                context.instruction_ptr.0,
                context.stack_ptr.0
            );
            crate::yarm_log!(
                "CTX3 inside enter_dispatched_user_task_if_available tid={}",
                tid
            );
            crate::yarm_log!(
                "BSP_CONTEXT_RESTORE_DUMP tid={} elr=0x{:x} sp=0x{:x} spsr=0x{:x} ttbr0=0x{:x} arg0=0x{:x} arg1=0x{:x} arg2=0x{:x} arg3=0x{:x} arg4=0x{:x} arg5=0x{:x}",
                tid,
                context.instruction_ptr.0,
                context.stack_ptr.0,
                0u64,
                ttbr0_el1,
                context.arg0 as u64,
                context.arg1 as u64,
                context.arg2 as u64,
                context.arg3 as u64,
                context.arg4 as u64,
                context.arg5 as u64
            );
        }
        unsafe {
            unsafe extern "C" {
                fn yarm_aarch64_enter_user_mode_eret(
                    entry: u64,
                    stack_top: u64,
                    arg0: u64,
                    arg1: u64,
                    arg2: u64,
                    arg3: u64,
                    arg4: u64,
                    tls: u64,
                ) -> !;
            }
            yarm_aarch64_enter_user_mode_eret(
                context.instruction_ptr.0,
                context.stack_ptr.0,
                context.arg0 as u64,
                context.arg1 as u64,
                context.arg2 as u64,
                context.arg3 as u64,
                context.arg4 as u64,
                tls,
            );
        }
        #[allow(unreachable_code)]
        if kernel.current_cpu().0 == crate::arch::platform_constants::BOOTSTRAP_CPU_ID {
            crate::yarm_log!("BSP_ENTER_RING3_DONE tid={}", tid);
        }
    }
}

#[cfg(any(feature = "hosted-dev", not(target_arch = "aarch64")))]
pub fn enter_dispatched_user_task_if_available(
    _kernel: &crate::kernel::boot::KernelState,
    _dispatched_tid: Option<u64>,
) {
}

pub fn run_with_prepared_kernel(run: fn(&mut crate::kernel::boot::KernelState)) {
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
    crate::arch::aarch64::console::write_line(
        "YARM_AARCH64_BOOT_MARKER stage=run_with_prepared_kernel",
    );
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
    let saved_ttbr0: u64 = {
        let ttbr0: u64;
        unsafe {
            core::arch::asm!(
                "mrs {0}, ttbr0_el1",
                out(reg) ttbr0,
                options(nostack, preserves_flags)
            );
        }
        ttbr0
    };
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
    crate::arch::aarch64::console::write_line(
        "YARM_AARCH64_BOOT_MARKER stage=bootstrap_init_begin",
    );
    // Hardware AArch64: own KernelState through SharedKernel (Stage-2N shared trap path).
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
    let shared = crate::kernel::boot::Bootstrap::init_shared_static().expect("kernel init");
    // SAFETY: single-CPU boot; no trap handler can race before install_trap_shared_kernel
    // stores the pointer; the reference must not be used after the ERET to user space.
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
    let kernel: &mut crate::kernel::boot::KernelState = unsafe { shared.borrow_kernel_for_boot() };
    #[cfg(any(feature = "hosted-dev", not(target_arch = "aarch64")))]
    let kernel = crate::kernel::boot::Bootstrap::init_static().expect("kernel init");
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
    install_trap_shared_kernel(shared);
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
    crate::yarm_log!("YARM_LOCK_SPLIT_STAGE2N_INSTALLED arch=aarch64 shared=1 raw=0");
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
    crate::arch::aarch64::console::write_line("YARM_AARCH64_BOOT_MARKER stage=bootstrap_init_done");
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
    BSP_RELEASED_SECONDARIES.store(false, Ordering::Release);
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
    let started_secondary = start_secondary_cpus(kernel);
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
    if saved_ttbr0 != 0 {
        unsafe {
            core::arch::asm!(
                "msr ttbr0_el1, {0}",
                "isb",
                in(reg) saved_ttbr0,
                options(nostack, preserves_flags)
            );
        }
    }
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
    {
        // Do not touch EL1 physical timer registers during early bootstrap.
        // On some boot paths (e.g. direct EL1 entry), CNTHCTL_EL2 may still
        // trap EL1 timer sysreg accesses, which raises a synchronous exception
        // right after this stage marker. Timer/IRQ bring-up is deferred to the
        // normal init path once platform control is fully established.
    }
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
    crate::yarm_log!(
        "YARM_SMP_STARTUP started_secondary={} online_cpus={} present_cpus={}",
        started_secondary,
        kernel.online_cpu_count(),
        kernel.present_cpu_count()
    );
    crate::yarm_log!(
        "YARM_BOOT_OK present_cpus={} present_bitmap=0x{:x} online_cpus={}",
        kernel.present_cpu_count(),
        kernel.present_cpu_bitmap(),
        kernel.online_cpu_count()
    );
    run(kernel);
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
fn start_secondary_cpus(kernel: &mut crate::kernel::boot::KernelState) -> usize {
    let mut started = 0usize;
    let secondary_entry = yarm_aarch64_secondary_entry as *const () as usize as u64;
    let conduit = match PSCI_CONDUIT.load(Ordering::Acquire) {
        x if x == PsciConduit::Smc as u8 => PsciConduit::Smc,
        x if x == PsciConduit::Hvc as u8 => PsciConduit::Hvc,
        _ => PsciConduit::Unknown,
    };
    if matches!(conduit, PsciConduit::Unknown) {
        crate::yarm_log!("YARM_SMP_PSCI unavailable_conduit");
        return 0;
    }
    let present = kernel.present_cpu_bitmap();
    for cpu_id in 0..64u8 {
        if cpu_id == crate::arch::platform_constants::BOOTSTRAP_CPU_ID {
            continue;
        }
        let mask = 1u64 << cpu_id;
        if (present & mask) == 0 {
            continue;
        }
        let cpu = crate::kernel::scheduler::CpuId(cpu_id);
        let psci_ret = psci_cpu_on(conduit, cpu.0 as u64, secondary_entry, cpu.0 as u64);
        crate::yarm_log!("YARM_SMP_PSCI cpu={} ret={}", cpu.0, psci_ret);
        if psci_ret == 0 && kernel.bring_up_cpu(cpu).is_ok() {
            started += 1;
        }
    }
    started
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
unsafe extern "C" {
    fn yarm_aarch64_secondary_entry() -> !;
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
const PSCI_CPU_ON_FID: u64 = 0xC400_0003;

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
fn psci_cpu_on(conduit: PsciConduit, target_cpu: u64, entry_point: u64, context_id: u64) -> i64 {
    let mut x0 = PSCI_CPU_ON_FID;
    let x1 = target_cpu;
    let x2 = entry_point;
    let x3 = context_id;
    unsafe {
        match conduit {
            PsciConduit::Smc => core::arch::asm!(
                "smc #0",
                inout("x0") x0,
                in("x1") x1,
                in("x2") x2,
                in("x3") x3,
                options(nostack)
            ),
            PsciConduit::Hvc => core::arch::asm!(
                "hvc #0",
                inout("x0") x0,
                in("x1") x1,
                in("x2") x2,
                in("x3") x3,
                options(nostack)
            ),
            PsciConduit::Unknown => return -1,
        }
    }
    x0 as i64
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
#[unsafe(no_mangle)]
extern "C" fn yarm_aarch64_secondary_cpu_boot(cpu_id: u64) -> ! {
    let cpu_bit = 1u64.checked_shl(cpu_id as u32).unwrap_or(0);
    if cpu_bit != 0
        && (SECONDARY_ONLINE_LOGGED_MASK.fetch_or(cpu_bit, Ordering::AcqRel) & cpu_bit) == 0
    {
        crate::arch::aarch64::console::write_line("YARM_AARCH64_SMP_SECONDARY_ONLINE");
    }
    crate::yarm_log!("YARM_AARCH64_SMP_SECONDARY cpu={}", cpu_id);
    let cpu = crate::kernel::scheduler::CpuId(cpu_id as u8);
    unsafe {
        unsafe extern "C" {
            static yarm_aarch64_vector_table_el1: u8;
        }
        let vector_base = (&yarm_aarch64_vector_table_el1 as *const u8) as u64;
        core::arch::asm!("msr VBAR_EL1, {0}", in(reg) vector_base, options(nomem, preserves_flags));
        core::arch::asm!("isb", options(nomem, preserves_flags));
    }

    let kernel = loop {
        if let Some(kernel) = trap_kernel_state_mut() {
            break kernel;
        }
        unsafe {
            core::arch::asm!("wfe", options(nomem, nostack, preserves_flags));
        }
    };
    crate::yarm_log!(
        "YARM_AARCH64_SMP_WAIT cpu={} state=waiting_for_bsp_release",
        cpu_id
    );
    while !BSP_RELEASED_SECONDARIES.load(Ordering::Acquire) {
        unsafe {
            core::arch::asm!("wfe", options(nomem, nostack, preserves_flags));
        }
    }
    crate::yarm_log!("YARM_AARCH64_SMP_WAIT cpu={} state=released", cpu_id);
    let _ = kernel.set_current_cpu(cpu);
    let observed_cpu = kernel.current_cpu();
    if observed_cpu.0 != cpu.0 {
        crate::yarm_log!(
            "AP_CPU_IDENTITY_VIOLATION assigned_cpu={} observed_cpu={} mpidr=0x{:x}",
            cpu.0,
            observed_cpu.0,
            crate::arch::aarch64::read_mpidr_el1()
        );
    }
    assert_eq!(observed_cpu.0, cpu.0);
    let _ = kernel.process_cross_cpu_work_for_cpu(cpu);
    kernel.program_timer_deadline_current_cpu(
        crate::arch::platform_layout::BOOTSTRAP_TIMER_DEADLINE_TICKS,
    );
    if cpu_bit != 0
        && (SECONDARY_READY_LOGGED_MASK.fetch_or(cpu_bit, Ordering::AcqRel) & cpu_bit) == 0
    {
        crate::yarm_log!(
            "YARM_AARCH64_SMP_SECONDARY_READY cpu={} state=local_scheduler_initialized",
            cpu_id
        );
    }
    if cpu_bit != 0
        && (SECONDARY_JOINED_LOGGED_MASK.fetch_or(cpu_bit, Ordering::AcqRel) & cpu_bit) == 0
    {
        crate::yarm_log!("YARM_AARCH64_SMP_SECONDARY_JOINED cpu={}", cpu_id);
    }

    loop {
        let _ = kernel.process_cross_cpu_work_for_cpu(cpu);
        unsafe {
            core::arch::asm!("wfe", options(nomem, nostack, preserves_flags));
        }
    }
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
unsafe extern "C" {
    static __kernel_start: u8;
    static __kernel_end: u8;
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
const MAX_PT_ALLOCATOR_REGIONS: usize = 8;

pub fn prepare_arch_boot(_start_info_ptr: usize) {
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
    {
        let start_info_ptr = _start_info_ptr;
        crate::arch::aarch64::console::write_line("YARM_AARCH64_BREADCRUMB P0");
        crate::yarm_log!(
            "YARM_AARCH64_START_INFO_PTR value=0x{:x}",
            start_info_ptr as u64
        );
        crate::arch::aarch64::console::write_line(
            "YARM_AARCH64_BOOT_MARKER stage=prepare_arch_boot",
        );
        unsafe {
            unsafe extern "C" {
                static yarm_aarch64_vector_table_el1: u8;
            }
            let vector_base = (&yarm_aarch64_vector_table_el1 as *const u8) as u64;
            core::arch::asm!("msr VBAR_EL1, {0}", in(reg) vector_base, options(nomem, preserves_flags));
            core::arch::asm!("isb", options(nomem, preserves_flags));
        }
        crate::arch::aarch64::console::write_line("YARM_AARCH64_BOOT_MARKER stage=vbar_el1_ready");

        if let Some(dtb) = dtb_slice_from_start_info(start_info_ptr) {
            crate::arch::aarch64::console::write_line("YARM_AARCH64_BREADCRUMB P1");
            let captured = crate::kernel::boot_command_line::set_raw_cmdline_from_bytes(
                crate::arch::fdt::chosen_bootargs(dtb).unwrap_or(&[]),
            );
            crate::yarm_log!(
                "YARM_BOOT_CMDLINE_CAPTURE arch=aarch64 len={} truncated={}",
                captured.raw_cmdline().len(),
                captured.cmdline_was_truncated() as u8
            );
            let options =
                crate::kernel::boot_command_line::parse_yarm_boot_options(captured.raw_cmdline());
            if let Some(info) = crate::arch::aarch64_boot_policy::parse_platform_dtb(dtb) {
                use crate::arch::aarch64_boot_policy::DetectedPlatform;
                use crate::kernel::boot_command_line::{BootPhase, PlatformOption};
                let selected = match options.platform {
                    PlatformOption::Auto => info.platform,
                    PlatformOption::QemuVirt => DetectedPlatform::QemuVirt,
                    PlatformOption::Rpi5 => DetectedPlatform::Rpi5Bcm2712,
                };
                crate::yarm_log!(
                    "YARM_AARCH64_PLATFORM detected={} selected={} phase={:?} max_cpus={}",
                    info.platform.label(),
                    selected.label(),
                    options.boot_phase,
                    options.max_cpus.unwrap_or(0),
                );
                if selected == DetectedPlatform::Rpi5Bcm2712 {
                    crate::yarm_log!(
                        "RPI5_BOOT_DTB memory_start=0x{:x} memory_len=0x{:x} reserved_count={} reserved_first=0x{:x} reserved_first_len=0x{:x}",
                        info.memory_start.unwrap_or(0),
                        info.memory_len.unwrap_or(0),
                        info.reserved_count,
                        info.first_reserved_start.unwrap_or(0),
                        info.first_reserved_len.unwrap_or(0),
                    );
                    crate::yarm_log!(
                        "RPI5_BOOT_GIC path={} base=0x{:x}",
                        info.interrupt_controller_path.as_str(),
                        info.interrupt_controller_base.unwrap_or(0),
                    );
                    crate::yarm_log!(
                        "RPI5_BOOT_INITRD present={} start=0x{:x} end=0x{:x}",
                        info.has_initrd() as u8,
                        info.initrd_start.unwrap_or(0),
                        info.initrd_end.unwrap_or(0),
                    );
                    if matches!(options.boot_phase, BootPhase::Dtb | BootPhase::Mmu) {
                        crate::yarm_log!("RPI5_BOOT_STOP phase={:?} stage1=1", options.boot_phase);
                        halt_stage1();
                    }
                    if options.boot_phase == BootPhase::Kernel && !info.has_initrd() {
                        crate::arch::aarch64::console::write_line(
                            "RPI5_BOOT_KERNEL_REFUSED reason=missing_initrd",
                        );
                        halt_stage1();
                    }
                    // Stage 1 never enters the existing userspace boot chain on Raspberry Pi 5.
                    crate::arch::aarch64::console::write_line(
                        "RPI5_BOOT_KERNEL_REFUSED reason=stage1_uart_only",
                    );
                    halt_stage1();
                }
            }
            if let Some(parsed) = crate::arch::aarch64::dtb::parse_boot_dtb(dtb) {
                crate::yarm_log!(
                    "YARM_AARCH64_DTB memory_start=0x{:x} memory_len=0x{:x} initrd_start=0x{:x} initrd_end=0x{:x} gic_cpu_if_base=0x{:x}",
                    parsed.memory_start.unwrap_or(0),
                    parsed.memory_len.unwrap_or(0),
                    parsed.initrd_start.unwrap_or(0),
                    parsed.initrd_end.unwrap_or(0),
                    parsed.gic_cpu_if_base.unwrap_or(0),
                );
                if let Some(bitmap) = parsed.present_cpu_bitmap {
                    let max_cpus = options
                        .max_cpus
                        .unwrap_or(u64::BITS as usize)
                        .min(u64::BITS as usize);
                    let cpu_mask = if max_cpus == u64::BITS as usize {
                        u64::MAX
                    } else {
                        (1u64 << max_cpus) - 1
                    };
                    let bitmap = bitmap & cpu_mask;
                    let _ = crate::arch::boot_entry::stage_present_cpu_bitmap_for_bootstrap(bitmap);
                    crate::yarm_log!(
                        "YARM_AARCH64_DTB_CPU_BITMAP value=0x{:x} count={}",
                        bitmap,
                        bitmap.count_ones()
                    );
                }
                let conduit = match parsed.psci_conduit {
                    crate::arch::aarch64::dtb::PsciConduit::Smc => PsciConduit::Smc,
                    crate::arch::aarch64::dtb::PsciConduit::Hvc => PsciConduit::Hvc,
                    crate::arch::aarch64::dtb::PsciConduit::Unknown => PsciConduit::Unknown,
                };
                PSCI_CONDUIT.store(conduit as u8, Ordering::Release);
                crate::yarm_log!("YARM_AARCH64_DTB_PSCI conduit={:?}", conduit);
                if let (Some(start), Some(len)) = (parsed.memory_start, parsed.memory_len) {
                    let _ = crate::arch::boot_entry::stage_detected_ram_for_bootstrap(&[
                        crate::kernel::frame_allocator::MemoryRegion {
                            start,
                            len,
                            usable: true,
                        },
                    ]);
                    let mut reserved: [(u64, u64); 5] = [(0, 0); 5];
                    let mut reserved_len = 0usize;
                    let page = crate::kernel::vm::PAGE_SIZE as u64;
                    let kernel_start = (core::ptr::addr_of!(__kernel_start) as u64) & !(page - 1);
                    let kernel_end = align_up_u64(core::ptr::addr_of!(__kernel_end) as u64, page)
                        .unwrap_or(kernel_start);
                    if kernel_end > kernel_start {
                        reserved[reserved_len] = (kernel_start, kernel_end);
                        reserved_len += 1;
                        crate::yarm_log!(
                            "PMEM_RESERVE_KERNEL start=0x{:x} end=0x{:x}",
                            kernel_start,
                            kernel_end
                        );
                    }
                    // extra_reserved_start marks the first non-kernel range; we store these
                    // in the Bootstrap statics so init_static() can include them.
                    let extra_reserved_start = reserved_len;
                    let l1_start = (core::ptr::addr_of!(BOOT_L1_TABLE) as u64) & !(page - 1);
                    let l1_end_raw = (core::ptr::addr_of!(BOOT_L1_TABLE) as u64)
                        .checked_add(core::mem::size_of::<AlignedL1>() as u64);
                    let l1_end = l1_end_raw
                        .and_then(|value| align_up_u64(value, page))
                        .unwrap_or(l1_start);
                    if l1_end > l1_start {
                        reserved[reserved_len] = (l1_start, l1_end);
                        reserved_len += 1;
                    }
                    let l2_start = (core::ptr::addr_of!(BOOT_L2_TABLE) as u64) & !(page - 1);
                    let l2_end_raw = (core::ptr::addr_of!(BOOT_L2_TABLE) as u64)
                        .checked_add(core::mem::size_of::<AlignedL2>() as u64);
                    let l2_end = l2_end_raw
                        .and_then(|value| align_up_u64(value, page))
                        .unwrap_or(l2_start);
                    if l2_end > l2_start {
                        reserved[reserved_len] = (l2_start, l2_end);
                        reserved_len += 1;
                    }
                    let dtb_start = dtb.as_ptr() as u64;
                    let dtb_end = dtb_start.checked_add(dtb.len() as u64).unwrap_or(dtb_start);
                    if dtb_end > dtb_start {
                        let dtb_pa_start = dtb_start & !(page - 1);
                        let dtb_reserved_end = align_up_u64(dtb_end, page).unwrap_or(dtb_start);
                        reserved[reserved_len] = (dtb_pa_start, dtb_reserved_end);
                        reserved_len += 1;
                        crate::yarm_log!(
                            "PMEM_RESERVE_DTB start=0x{:x} end=0x{:x}",
                            dtb_pa_start,
                            dtb_reserved_end
                        );
                    }
                    if let (Some(initrd_start), Some(initrd_end)) =
                        (parsed.initrd_start, parsed.initrd_end)
                        && initrd_end > initrd_start
                        && reserved_len < reserved.len()
                    {
                        let initrd_len = initrd_end.saturating_sub(initrd_start) as usize;
                        if initrd_len > 0 {
                            // SAFETY: DTB-provided initrd physical window is immutable boot memory.
                            let bytes = unsafe {
                                core::slice::from_raw_parts(initrd_start as *const u8, initrd_len)
                            };
                            crate::kernel::boot::Bootstrap::install_boot_initrd_bytes(bytes);
                        }
                        let initrd_pa_start = initrd_start & !(page - 1);
                        let initrd_pa_end = align_up_u64(initrd_end, page).unwrap_or(initrd_start);
                        reserved[reserved_len] = (initrd_pa_start, initrd_pa_end);
                        reserved_len += 1;
                        crate::yarm_log!(
                            "PMEM_RESERVE_INITRD start=0x{:x} end=0x{:x}",
                            initrd_pa_start,
                            initrd_pa_end
                        );
                    }
                    // Persist all non-kernel reserved ranges so Bootstrap::init_static()
                    // (called later from run_with_prepared_kernel) includes them when
                    // building the main frame allocator's exclusion set.
                    crate::kernel::boot::Bootstrap::install_boot_extra_reserved_ranges(
                        &reserved[extra_reserved_start..reserved_len],
                    );
                    let (alloc_regions, alloc_regions_len) = build_allocator_regions_from_ram(
                        start,
                        len,
                        crate::arch::platform_layout::NEXT_ANON_PHYS_BASE,
                        &reserved[..reserved_len],
                    );
                    if alloc_regions_len > 0 {
                        let _ = crate::kernel::frame_allocator::init_pt_frame_allocator(
                            &alloc_regions[..alloc_regions_len],
                        );
                    } else {
                        crate::yarm_log!(
                            "YARM_AARCH64_DTB_ALLOCATOR no_usable_regions ram_start=0x{:x} ram_len=0x{:x} alloc_start=0x{:x}",
                            start,
                            len,
                            crate::arch::platform_layout::NEXT_ANON_PHYS_BASE
                        );
                    }
                }
                if let Some(gic_base) = parsed.gic_cpu_if_base {
                    let mut desc = [0u8; 40];
                    if let Some(desc_len) = encode_irq_desc_gic_cpu_if_base(gic_base, &mut desc) {
                        let _ = crate::arch::boot_entry::stage_irq_controller_description_for_boot(
                            &desc[..desc_len],
                        );
                    }
                }
            }
        }
        crate::arch::aarch64::console::write_line("YARM_AARCH64_BREADCRUMB P2");
        setup_bootstrap_mmu();
        crate::arch::aarch64::console::write_line("YARM_AARCH64_BREADCRUMB P3");
    }
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
fn build_allocator_regions_from_ram(
    ram_base: u64,
    ram_size: u64,
    alloc_start: u64,
    reserved: &[(u64, u64)],
) -> (
    [crate::kernel::frame_allocator::MemoryRegion; MAX_PT_ALLOCATOR_REGIONS],
    usize,
) {
    let mut out = [crate::kernel::frame_allocator::MemoryRegion {
        start: 0,
        len: 0,
        usable: false,
    }; MAX_PT_ALLOCATOR_REGIONS];
    let page = crate::kernel::vm::PAGE_SIZE as u64;
    let Some(ram_end) = ram_base.checked_add(ram_size) else {
        return (out, 0);
    };
    let Some(start) = align_up_u64(ram_base.max(alloc_start), page) else {
        return (out, 0);
    };
    let end = align_down_u64(ram_end, page);
    if end <= start {
        return (out, 0);
    }

    let mut segments = [(0u64, 0u64); MAX_PT_ALLOCATOR_REGIONS];
    let mut seg_len = 1usize;
    segments[0] = (start, end);

    for &(res_start, res_end) in reserved {
        if res_end <= res_start {
            continue;
        }
        let res_start = align_down_u64(res_start, page).max(start);
        let Some(res_end_aligned) = align_up_u64(res_end, page) else {
            continue;
        };
        let res_end = res_end_aligned.min(end);
        if res_end <= res_start {
            continue;
        }
        let mut next = [(0u64, 0u64); MAX_PT_ALLOCATOR_REGIONS];
        let mut next_len = 0usize;
        for &(seg_start, seg_end) in segments.iter().take(seg_len) {
            if res_end <= seg_start || res_start >= seg_end {
                if next_len < MAX_PT_ALLOCATOR_REGIONS {
                    next[next_len] = (seg_start, seg_end);
                    next_len += 1;
                }
                continue;
            }
            if seg_start < res_start && next_len < MAX_PT_ALLOCATOR_REGIONS {
                next[next_len] = (seg_start, res_start);
                next_len += 1;
            }
            if res_end < seg_end && next_len < MAX_PT_ALLOCATOR_REGIONS {
                next[next_len] = (res_end, seg_end);
                next_len += 1;
            }
        }
        segments = next;
        seg_len = next_len;
        if seg_len == 0 {
            break;
        }
    }

    let mut out_len = 0usize;
    for &(seg_start, seg_end) in segments.iter().take(seg_len) {
        if seg_end <= seg_start {
            continue;
        }
        out[out_len] = crate::kernel::frame_allocator::MemoryRegion {
            start: seg_start,
            len: seg_end - seg_start,
            usable: true,
        };
        out_len += 1;
    }
    (out, out_len)
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
const fn align_down_u64(value: u64, align: u64) -> u64 {
    value & !(align - 1)
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
const fn align_up_u64(value: u64, align: u64) -> Option<u64> {
    match value.checked_add(align - 1) {
        Some(added) => Some(added & !(align - 1)),
        None => None,
    }
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
fn setup_bootstrap_mmu() {
    crate::arch::aarch64::console::write_line("YARM_AARCH64_BREADCRUMB M0");
    unsafe {
        let l1_addr = core::ptr::addr_of!(BOOT_L1_TABLE) as u64;
        let l2_addr = core::ptr::addr_of!(BOOT_L2_TABLE) as u64;
        let l1_ptr = core::ptr::addr_of_mut!(BOOT_L1_TABLE.0) as *mut u64;
        let l2_ptr = core::ptr::addr_of_mut!(BOOT_L2_TABLE.0) as *mut u64;

        for idx in 0..512 {
            core::ptr::write(l1_ptr.add(idx), 0);
            core::ptr::write(l2_ptr.add(idx), 0);
        }

        core::ptr::write(
            l1_ptr,
            (l2_addr & !0xFFF) | AARCH64_PTE_VALID | AARCH64_PTE_TABLE,
        );
        for idx in 1..512 {
            let base = (idx as u64) * AARCH64_BLOCK_1G;
            core::ptr::write(
                l1_ptr.add(idx),
                base | AARCH64_PTE_VALID
                    | AARCH64_PTE_AF
                    | AARCH64_PTE_SH_INNER
                    | (AARCH64_ATTRIDX_NORMAL_WB << AARCH64_PTE_ATTR_SHIFT),
            );
        }

        for idx in 0..512 {
            let base = (idx as u64) * AARCH64_BLOCK_2M;
            core::ptr::write(
                l2_ptr.add(idx),
                base | AARCH64_PTE_VALID
                    | AARCH64_PTE_AF
                    | AARCH64_PTE_SH_INNER
                    | (AARCH64_ATTRIDX_NORMAL_WB << AARCH64_PTE_ATTR_SHIFT),
            );
        }

        let device_block = |base: u64| -> u64 {
            base | AARCH64_PTE_VALID
                | AARCH64_PTE_AF
                | (AARCH64_ATTRIDX_DEVICE_NGNRE << AARCH64_PTE_ATTR_SHIFT)
        };
        let gic_l2_index = (0x0800_0000u64 / AARCH64_BLOCK_2M) as usize;
        core::ptr::write(l2_ptr.add(gic_l2_index), device_block(0x0800_0000));
        let uart_l2_index = (AARCH64_UART_MMIO_BASE / AARCH64_BLOCK_2M) as usize;
        core::ptr::write(
            l2_ptr.add(uart_l2_index),
            device_block(AARCH64_UART_MMIO_BASE),
        );

        // AttrIdx0 = normal WB/WA cacheable (0xff).
        // AttrIdx1 = normal WT cacheable (0xbb).
        // AttrIdx2 = normal non-cacheable (0x44).
        // AttrIdx3 = device nGnRE (0x04).
        let mair: u64 = 0xff | (0xbb << 8) | (0x44 << 16) | (0x04 << 24);
        let tcr: u64 = 25u64
            | (1 << 8)
            | (1 << 10)
            | (0b11 << 12)
            | (25u64 << 16)
            | (1u64 << 23)
            | (0b10 << 30)
            | (0b010 << 32);

        core::arch::asm!("msr MAIR_EL1, {0}", in(reg) mair, options(nostack, preserves_flags));
        core::arch::asm!("msr TCR_EL1, {0}", in(reg) tcr, options(nostack, preserves_flags));
        core::arch::asm!("msr TTBR0_EL1, {0}", in(reg) l1_addr, options(nostack, preserves_flags));
        core::arch::asm!("msr TTBR1_EL1, xzr", options(nostack, preserves_flags));
        crate::arch::aarch64::console::write_line("YARM_AARCH64_BREADCRUMB M1");
        crate::arch::aarch64::console::write_line("YARM_AARCH64_BREADCRUMB M1A");
        core::arch::asm!("dsb ish", options(nostack, preserves_flags));
        crate::arch::aarch64::console::write_line("YARM_AARCH64_BREADCRUMB M1B");
        core::arch::asm!("isb", options(nostack, preserves_flags));
        crate::arch::aarch64::console::write_line("YARM_AARCH64_BREADCRUMB M1C");
        core::arch::asm!("tlbi vmalle1", options(nostack, preserves_flags));
        crate::arch::aarch64::console::write_line("YARM_AARCH64_BREADCRUMB M1D");
        // Keep the bootstrap sequence conservative: some emulated CPU configs can
        // fault on IC invalidate-all during early bring-up. We can safely defer this
        // for now because MMU/caches are just being enabled.
        crate::arch::aarch64::console::write_line("YARM_AARCH64_BREADCRUMB M1E");
        core::arch::asm!("dsb ish", options(nostack, preserves_flags));
        crate::arch::aarch64::console::write_line("YARM_AARCH64_BREADCRUMB M1F");
        core::arch::asm!("isb", options(nostack, preserves_flags));
        crate::arch::aarch64::console::write_line("YARM_AARCH64_BREADCRUMB M1G");

        crate::arch::aarch64::console::write_line("YARM_AARCH64_BREADCRUMB M1H");
        // Preserve implementation-defined/reserved state, and only enable M/C/I.
        // This is safer across CPU models than forcing a fixed bit mask.
        let mut sctlr: u64;
        core::arch::asm!("mrs {0}, SCTLR_EL1", out(reg) sctlr, options(nostack, preserves_flags));
        sctlr |= (1 << 0) | (1 << 2) | (1 << 12);
        crate::arch::aarch64::console::write_line("YARM_AARCH64_BREADCRUMB M1I");
        core::arch::asm!("ic iallu", options(nostack, preserves_flags));
        core::arch::asm!("dsb nsh", options(nostack, preserves_flags));
        core::arch::asm!("isb", options(nostack, preserves_flags));
        core::arch::asm!("msr SCTLR_EL1, {0}", in(reg) sctlr, options(nostack, preserves_flags));
        crate::arch::aarch64::console::write_line("YARM_AARCH64_BREADCRUMB M1J");
        core::arch::asm!("isb", options(nostack, preserves_flags));
        crate::arch::aarch64::console::write_line("YARM_AARCH64_BREADCRUMB M1K");
    }
    crate::arch::aarch64::console::write_line("YARM_AARCH64_BREADCRUMB M2");
    crate::arch::aarch64::console::write_line("YARM_AARCH64_BOOT_MARKER stage=mmu_enabled");
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
fn dtb_slice_from_start_info(start_info_ptr: usize) -> Option<&'static [u8]> {
    let mut candidate_ptr = start_info_ptr;
    if candidate_ptr == 0 {
        crate::yarm_log!("YARM_AARCH64_DTB_STATUS missing_start_info_ptr");
        candidate_ptr = probe_qemu_virt_dtb_pointer().unwrap_or(0);
        if candidate_ptr != 0 {
            crate::yarm_log!(
                "YARM_AARCH64_DTB_STATUS recovered_ptr ptr=0x{:x}",
                candidate_ptr as u64
            );
        }
    }
    if candidate_ptr == 0 {
        return None;
    }
    let magic_be = unsafe { core::ptr::read_unaligned(candidate_ptr as *const u32) };
    if u32::from_be(magic_be) != 0xd00dfeed {
        crate::yarm_log!(
            "YARM_AARCH64_DTB_STATUS bad_magic value=0x{:x} ptr=0x{:x}",
            u32::from_be(magic_be),
            candidate_ptr as u64
        );
        return None;
    }
    let total_size_be = unsafe { core::ptr::read_unaligned((candidate_ptr + 4) as *const u32) };
    let total_size = u32::from_be(total_size_be) as usize;
    if total_size < 40 || total_size > 2 * 1024 * 1024 {
        crate::yarm_log!(
            "YARM_AARCH64_DTB_STATUS bad_size size={} ptr=0x{:x}",
            total_size,
            candidate_ptr as u64
        );
        return None;
    }
    Some(unsafe { core::slice::from_raw_parts(candidate_ptr as *const u8, total_size) })
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
fn probe_qemu_virt_dtb_pointer() -> Option<usize> {
    const FDT_MAGIC: u32 = 0xd00dfeed;
    const PROBE_START: u64 = 0x4000_0000;
    // Probe within the first 512 MiB from RAM base. Without firmware-provided
    // RAM size metadata, probing past absent RAM can fault before kernel state
    // is installed (observed at FAR=0x6000_0000 on 512 MiB guests).
    const PROBE_BYTES: u64 = 512 * 1024 * 1024;
    const PROBE_STEP_PAGE: u64 = 0x1000;
    const PROBE_INTRA_PAGE_SCAN_BYTES: u64 = 128;
    const PROBE_INTRA_PAGE_STEP: u64 = 8;

    let candidate_is_valid = |addr: u64| -> bool {
        let raw_magic = unsafe { core::ptr::read_unaligned(addr as *const u32) };
        if u32::from_be(raw_magic) != FDT_MAGIC {
            return false;
        }
        let total_size_be = unsafe { core::ptr::read_unaligned((addr + 4) as *const u32) };
        let total_size = u32::from_be(total_size_be) as usize;
        (40..=2 * 1024 * 1024).contains(&total_size)
    };

    let mut addr = PROBE_START;
    let end = PROBE_START + PROBE_BYTES;
    while addr < end {
        if candidate_is_valid(addr) {
            return Some(addr as usize);
        }
        let page_scan_end = (addr + PROBE_INTRA_PAGE_SCAN_BYTES).min(end);
        let mut probe = addr + PROBE_INTRA_PAGE_STEP;
        while probe < page_scan_end {
            if candidate_is_valid(probe) {
                return Some(probe as usize);
            }
            probe += PROBE_INTRA_PAGE_STEP;
        }
        addr += PROBE_STEP_PAGE;
    }
    None
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
fn encode_irq_desc_gic_cpu_if_base(base: usize, out: &mut [u8]) -> Option<usize> {
    let prefix = b"gic_cpu_if_base=0x";
    if out.len() < prefix.len() + 16 {
        return None;
    }
    out[..prefix.len()].copy_from_slice(prefix);
    let mut cursor = prefix.len();
    let nybbles = core::mem::size_of::<usize>() * 2;
    let mut emitted = false;
    for shift in (0..nybbles).rev() {
        let nibble = ((base >> (shift * 4)) & 0xF) as u8;
        if nibble != 0 || emitted || shift == 0 {
            out[cursor] = if nibble < 10 {
                b'0' + nibble
            } else {
                b'a' + (nibble - 10)
            };
            cursor += 1;
            emitted = true;
        }
    }
    Some(cursor)
}

pub fn emit_panic(info: &core::panic::PanicInfo<'_>) {
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
    {
        struct PanicSerialWriter;
        impl Write for PanicSerialWriter {
            fn write_str(&mut self, s: &str) -> core::fmt::Result {
                for line in s.split('\n') {
                    if !line.is_empty() {
                        crate::arch::aarch64::console::write_line(line);
                    }
                }
                Ok(())
            }
        }

        crate::arch::aarch64::console::write_line("YARM_AARCH64_BOOT_MARKER stage=panic");
        let mut writer = PanicSerialWriter;
        let _ = writer.write_str("PANIC ");
        if let Some(location) = info.location() {
            let _ = write!(
                writer,
                "{}:{}:{}",
                location.file(),
                location.line(),
                location.column()
            );
        } else {
            let _ = writer.write_str("<unknown>");
        }
        let _ = writer.write_str(": ");
        let _ = write!(writer, "{}", info.message());
        let _ = writer.write_str("\n");
    }
    #[cfg(any(feature = "hosted-dev", not(target_arch = "aarch64")))]
    let _ = info;
}
