// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
use core::arch::global_asm;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
use core::fmt::Write;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};

#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    not(feature = "rpi5-highhalf")
))]
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
    feature = "rpi5-highhalf"
))]
global_asm!(
    r#"
    /*
     * HH-2 is a transition diagnostic, not the final user-entry path.  It
     * builds distinct low-identity and high-kernel roots from linker-reserved
     * pages, keeps TTBR0 on the low root, enables TTBR1, branches to the high
     * alias of this code, proves VBAR/UART high aliases, and halts.  It never
     * installs the Stage2C root and never executes an EL0 ERET.
     */
    .equ HH_UART_PHYS,       0x107d001000
    .equ HH_UART_VIRT,       0xffffff907d001000
    .equ HH_VA_OFFSET,       0xffffff8000000000
    .equ HH_RAM_LIMIT,       0x80000000
    .equ HH_MAIR,            0x04ff
    .equ HH_TCR,             0x00000002b5193519
    .equ HH_NORMAL_BLOCK,    0x701
    .equ HH_DEVICE_PAGE,     0x60000000000707
    .equ HH_TABLE,           0x3

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
    .global _start
    .type _start,%function
_start:
    mov x20, x0
    ldr x9, =boot_stack_aarch64_end
    mov sp, x9
    bl .Lhh_enter_el1
    ldr x19, =HH_UART_PHYS
    adr x0, .Lhh_low_entry
    bl .Lhh_write_cstr

    adr x0, .Lhh_plan_begin
    bl .Lhh_write_cstr
    mrs x0, SCTLR_EL1
    tbnz x0, #0, .Lhh_fail_already_enabled
    ldr x21, =__hh_ttbr0_root
    ldr x22, =__hh_ttbr1_root
    cmp x21, x22
    b.eq .Lhh_fail_roots
    tst x21, #0xfff
    b.ne .Lhh_fail_roots
    tst x22, #0xfff
    b.ne .Lhh_fail_roots

    ldr x0, =__hh_pt_pool_start
    ldr x1, =__hh_pt_pool_end
.Lhh_zero_pt:
    cmp x0, x1
    b.hs .Lhh_zero_pt_done
    stp xzr, xzr, [x0], #16
    b .Lhh_zero_pt
.Lhh_zero_pt_done:

    // TTBR0 retains a bounded 0..2 GiB identity map during the transition.
    ldr x0, =HH_NORMAL_BLOCK
    str x0, [x21]
    ldr x1, =0x40000000
    orr x1, x1, x0
    str x1, [x21, #8]

    // TTBR1 maps the same RAM at VA = PA + HH_VA_OFFSET.
    str x0, [x22]
    str x1, [x22, #8]

    // UART is outside RAM. Keep its physical alias in TTBR0 until the branch.
    ldr x23, =__hh_uart0_l2
    orr x0, x23, #HH_TABLE
    str x0, [x21, #(65 * 8)]
    ldr x24, =__hh_uart0_l3
    orr x0, x24, #HH_TABLE
    mov x1, #0xf40
    str x0, [x23, x1]
    ldr x0, =HH_UART_PHYS
    ldr x1, =HH_DEVICE_PAGE
    orr x0, x0, x1
    mov x1, #8
    str x0, [x24, x1]

    // TTBR1 receives the equivalent dedicated device-nGnRE high alias.
    ldr x23, =__hh_uart1_l2
    orr x0, x23, #HH_TABLE
    str x0, [x22, #(65 * 8)]
    ldr x24, =__hh_uart1_l3
    orr x0, x24, #HH_TABLE
    mov x1, #0xf40
    str x0, [x23, x1]
    ldr x0, =HH_UART_PHYS
    ldr x1, =HH_DEVICE_PAGE
    orr x0, x0, x1
    mov x1, #8
    str x0, [x24, x1]

    // All required high aliases are contained by the reviewed 0..2 GiB map.
    ldr x25, =__kernel_phys_start
    ldr x26, =__kernel_phys_end
    cmp x26, x25
    b.ls .Lhh_fail_range
    ldr x0, =HH_RAM_LIMIT
    cmp x26, x0
    b.hi .Lhh_fail_range
    cbz x20, .Lhh_fail_dtb
    cmp x20, x0
    b.hs .Lhh_fail_dtb
    ldr w1, [x20, #4]
    rev w1, w1
    add x27, x20, x1
    cmp x27, x20
    b.ls .Lhh_fail_dtb
    cmp x27, x0
    b.hi .Lhh_fail_dtb

    adr x0, .Lhh_map_kernel
    bl .Lhh_write_cstr
    mov x0, x25
    bl .Lhh_write_hex
    adr x0, .Lhh_virt_sep
    bl .Lhh_write_cstr
    ldr x1, =HH_VA_OFFSET
    add x0, x25, x1
    bl .Lhh_write_hex
    adr x0, .Lhh_size_sep
    bl .Lhh_write_cstr
    sub x0, x26, x25
    bl .Lhh_write_hex_line

    adr x0, .Lhh_map_stack
    bl .Lhh_write_cstr
    ldr x25, =boot_stack_aarch64
    ldr x26, =boot_stack_aarch64_end
    mov x0, x25
    bl .Lhh_write_hex
    adr x0, .Lhh_virt_sep
    bl .Lhh_write_cstr
    ldr x1, =HH_VA_OFFSET
    add x0, x25, x1
    bl .Lhh_write_hex
    adr x0, .Lhh_size_sep
    bl .Lhh_write_cstr
    sub x0, x26, x25
    bl .Lhh_write_hex_line

    adr x0, .Lhh_map_dtb
    bl .Lhh_write_cstr
    mov x0, x20
    bl .Lhh_write_hex
    adr x0, .Lhh_virt_sep
    bl .Lhh_write_cstr
    ldr x1, =HH_VA_OFFSET
    add x0, x20, x1
    bl .Lhh_write_hex
    adr x0, .Lhh_size_sep
    bl .Lhh_write_cstr
    sub x0, x27, x20
    bl .Lhh_write_hex_line

    adr x0, .Lhh_map_heap
    bl .Lhh_write_cstr
    ldr x25, =__hh_heap_start
    ldr x26, =__hh_heap_end
    mov x0, x25
    bl .Lhh_write_hex
    adr x0, .Lhh_virt_sep
    bl .Lhh_write_cstr
    ldr x1, =HH_VA_OFFSET
    add x0, x25, x1
    bl .Lhh_write_hex
    adr x0, .Lhh_size_sep
    bl .Lhh_write_cstr
    sub x0, x26, x25
    bl .Lhh_write_hex_line

    adr x0, .Lhh_map_uart
    bl .Lhh_write_cstr
    adr x0, .Lhh_ttbr0
    bl .Lhh_write_cstr
    mov x0, x21
    bl .Lhh_write_hex_line
    adr x0, .Lhh_ttbr1
    bl .Lhh_write_cstr
    mov x0, x22
    bl .Lhh_write_hex_line
    adr x0, .Lhh_tcr
    bl .Lhh_write_cstr
    ldr x0, =HH_TCR
    bl .Lhh_write_hex_line
    adr x0, .Lhh_plan_done
    bl .Lhh_write_cstr

    adr x0, .Lhh_enable_begin
    bl .Lhh_write_cstr
    ldr x0, =__hh_pt_pool_start
    ldr x1, =__hh_pt_pool_end
.Lhh_clean_pt:
    cmp x0, x1
    b.hs .Lhh_clean_pt_done
    dc cvac, x0
    add x0, x0, #64
    b .Lhh_clean_pt
.Lhh_clean_pt_done:
    dsb sy
    ldr x0, =HH_MAIR
    msr MAIR_EL1, x0
    ldr x0, =HH_TCR
    msr TCR_EL1, x0
    msr TTBR0_EL1, x21
    msr TTBR1_EL1, x22
    dsb ishst
    tlbi vmalle1
    dsb ish
    isb
    ic iallu
    dsb nsh
    isb
    mrs x0, SCTLR_EL1
    orr x0, x0, #(1 << 0)
    orr x0, x0, #(1 << 2)
    orr x0, x0, #(1 << 12)
    msr SCTLR_EL1, x0
    isb
    mrs x0, SCTLR_EL1
    tbz x0, #0, .Lhh_fail_enable
    adr x0, .Lhh_enable_done
    bl .Lhh_write_cstr
    adr x0, .Lhh_jump_high
    bl .Lhh_write_cstr

    adr x0, .Lhh_high_entry
    ldr x1, =HH_VA_OFFSET
    add x0, x0, x1
    br x0

.Lhh_high_entry:
    adr x0, .Lhh_high_entry
    ldr x1, =HH_VA_OFFSET
    cmp x0, x1
    b.hs 1f
    adr x0, .Lhh_high_failed_pc
    b .Lhh_high_fail
1:
    mov x0, sp
    ldr x1, =HH_VA_OFFSET
    add x0, x0, x1
    mov sp, x0
    adr x0, .Lhh_vectors
    msr VBAR_EL1, x0
    isb
    mrs x1, VBAR_EL1
    cmp x0, x1
    b.ne .Lhh_high_failed_vbar
    ldr x19, =HH_UART_VIRT
    adr x0, .Lhh_high_ok
    bl .Lhh_write_cstr
    adr x0, .Lhh_vbar_ok
    bl .Lhh_write_cstr
    ldr x0, =yarm_rpi5_hh_rust_continue
    br x0

.Lhh_fail_roots:
    adr x0, .Lhh_failed_roots
    b .Lhh_low_fail
.Lhh_fail_already_enabled:
    adr x0, .Lhh_enable_already
    b .Lhh_low_fail
.Lhh_fail_range:
    adr x0, .Lhh_failed_range
    b .Lhh_low_fail
.Lhh_fail_dtb:
    adr x0, .Lhh_failed_dtb
    b .Lhh_low_fail
.Lhh_fail_enable:
    adr x0, .Lhh_enable_failed
.Lhh_low_fail:
    bl .Lhh_write_cstr
    b .Lhh_halt
.Lhh_high_failed_vbar:
    adr x0, .Lhh_high_failed_vbar_msg
.Lhh_high_fail:
    bl .Lhh_write_cstr
.Lhh_halt:
    wfe
    b .Lhh_halt

.Lhh_enter_el1:
    mrs x0, CurrentEL
    lsr x0, x0, #2
    cmp x0, #2
    b.ne 1f
    mov x0, #(1 << 31)
    msr HCR_EL2, x0
    msr CPTR_EL2, xzr
    msr HSTR_EL2, xzr
    msr MDCR_EL2, xzr
    mov x0, sp
    msr SP_EL1, x0
    adr x0, 1f
    msr ELR_EL2, x0
    mov x0, #0x3c5
    msr SPSR_EL2, x0
    isb
    eret
1:
    ret

// x0 = NUL-terminated bytes, x19 = selected low/high UART alias.
.Lhh_write_cstr:
    stp x1, x2, [sp, #-16]!
    stp x3, x30, [sp, #-16]!
1:
    ldrb w1, [x0], #1
    cbz w1, 3f
    mov x2, #0x10000
2:
    ldr w3, [x19, #0x18]
    tbz w3, #5, 4f
    subs x2, x2, #1
    b.ne 2b
    b 3f
4:
    str w1, [x19]
    b 1b
3:
    ldp x3, x30, [sp], #16
    ldp x1, x2, [sp], #16
    ret

// x0 = value. Writes exactly 16 lower-case hexadecimal digits.
.Lhh_write_hex:
    stp x1, x2, [sp, #-16]!
    stp x3, x4, [sp, #-16]!
    stp x5, x30, [sp, #-16]!
    mov x5, x0
    mov x4, #60
1:
    lsr x1, x5, x4
    and x1, x1, #0xf
    cmp x1, #10
    add x2, x1, #'0'
    add x3, x1, #('a' - 10)
    csel x1, x2, x3, lo
    mov x2, #0x10000
2:
    ldr w3, [x19, #0x18]
    tbz w3, #5, 3f
    subs x2, x2, #1
    b.ne 2b
    b 4f
3:
    str w1, [x19]
4:
    subs x4, x4, #4
    b.ge 1b
    ldp x5, x30, [sp], #16
    ldp x3, x4, [sp], #16
    ldp x1, x2, [sp], #16
    ret

.Lhh_write_hex_line:
    stp x0, x30, [sp, #-16]!
    bl .Lhh_write_hex
    adr x0, .Lhh_crlf
    bl .Lhh_write_cstr
    ldp x0, x30, [sp], #16
    ret

    .balign 2048
.Lhh_vectors:
    .rept 16
    b .Lhh_halt
    .space 124
    .endr

    .section .rodata.rpi5_raw_entry,"a",@progbits
.Lhh_low_entry:       .asciz "RPI5_HH_LOW_ENTRY\r\n"
.Lhh_plan_begin:      .asciz "RPI5_HH_PLAN_BEGIN\r\n"
.Lhh_map_kernel:      .asciz "RPI5_HH_MAP_KERNEL phys=0x"
.Lhh_map_stack:       .asciz "RPI5_HH_MAP_STACK phys=0x"
.Lhh_map_dtb:         .asciz "RPI5_HH_MAP_DTB phys=0x"
.Lhh_map_heap:        .asciz "RPI5_HH_MAP_HEAP phys=0x"
.Lhh_virt_sep:        .asciz " virt=0x"
.Lhh_size_sep:        .asciz " size=0x"
.Lhh_map_uart:        .asciz "RPI5_HH_MAP_UART phys=0x000000107d001000 virt=0xffffff907d001000\r\n"
.Lhh_ttbr0:           .asciz "RPI5_HH_TTBR0_ROOT base=0x"
.Lhh_ttbr1:           .asciz "RPI5_HH_TTBR1_ROOT base=0x"
.Lhh_tcr:             .asciz "RPI5_HH_TCR value=0x"
.Lhh_plan_done:       .asciz "RPI5_HH_PLAN_DONE\r\n"
.Lhh_enable_begin:    .asciz "RPI5_HH_ENABLE_BEGIN\r\n"
.Lhh_enable_done:     .asciz "RPI5_HH_ENABLE_DONE\r\n"
.Lhh_jump_high:       .asciz "RPI5_HH_JUMP_HIGH\r\n"
.Lhh_high_ok:         .asciz "RPI5_HH_HIGH_ENTRY_OK\r\n"
.Lhh_vbar_ok:         .asciz "RPI5_HH_VBAR_HIGH_OK\r\n"
.Lhh_failed_roots:    .asciz "RPI5_HH_PLAN_FAILED reason=invalid_or_shared_roots\r\n"
.Lhh_failed_range:    .asciz "RPI5_HH_PLAN_FAILED reason=range_outside_high_map\r\n"
.Lhh_failed_dtb:      .asciz "RPI5_HH_PLAN_FAILED reason=invalid_dtb_range\r\n"
.Lhh_enable_already:  .asciz "RPI5_HH_ENABLE_FAILED reason=mmu_already_enabled\r\n"
.Lhh_enable_failed:   .asciz "RPI5_HH_ENABLE_FAILED reason=sctlr_m_not_set\r\n"
.Lhh_high_failed_pc:  .asciz "RPI5_HH_HIGH_ENTRY_FAILED reason=pc_not_high\r\n"
.Lhh_high_failed_vbar_msg: .asciz "RPI5_HH_HIGH_ENTRY_FAILED reason=vbar_readback\r\n"
.Lhh_crlf:            .asciz "\r\n"
    "#
);

#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
const RPI5_HH_UART_VIRT: usize = 0xffff_ff90_7d00_1000;
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
const RPI5_HH_VA_OFFSET: u64 = 0xffff_ff80_0000_0000;
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
const RPI5_HH_TCR_EL1: u64 = 0x0000_0002_b519_3519;

#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
macro_rules! rpi5_hh_retained_marker {
    ($name:ident, $value:literal) => {
        #[used]
        #[unsafe(link_section = ".rodata.rpi5_hh_markers")]
        static $name: [u8; $value.len()] = *$value;
    };
}

#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_HH_RUST_ENTRY_MARKER, b"RPI5_HH_RUST_ENTRY");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_HH_RUST_AFTER_ENTRY_MARKER, b"RPI5_HH_RUST_AFTER_ENTRY");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_HH_READ_PC_BEGIN_MARKER, b"RPI5_HH_READ_PC_BEGIN");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_HH_READ_PC_CAPTURED_MARKER, b"RPI5_HH_READ_PC_CAPTURED");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH_READ_PC_PRINT_BEGIN_MARKER,
    b"RPI5_HH_READ_PC_PRINT_BEGIN"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_HH_HEX_BEGIN_MARKER, b"RPI5_HH_HEX_BEGIN");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_HH_HEX_DIGIT_BEGIN_MARKER, b"RPI5_HH_HEX_DIGIT_BEGIN");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_HH_HEX_DIGIT_DONE_MARKER, b"RPI5_HH_HEX_DIGIT_DONE");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_HH_HEX_DONE_MARKER, b"RPI5_HH_HEX_DONE");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_HH_HEX_FAILED_MARKER, b"RPI5_HH_HEX_FAILED reason=");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH3_HEX_FAULT_BOUNDARY_MARKER,
    b"RPI5_HH3_FAULT_BOUNDARY reason=hex_output"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH_READ_PC_DONE_MARKER,
    b"RPI5_HH_READ_PC_DONE value=0x"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH_READ_PC_FAILED_MARKER,
    b"RPI5_HH_READ_PC_FAILED reason="
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH3_PC_FAULT_BOUNDARY_MARKER,
    b"RPI5_HH3_FAULT_BOUNDARY reason=pc_read_or_print"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_HH_READ_SP_BEGIN_MARKER, b"RPI5_HH_READ_SP_BEGIN");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_HH_READ_SP_CAPTURED_MARKER, b"RPI5_HH_READ_SP_CAPTURED");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_HH_SP_HEX_BEGIN_MARKER, b"RPI5_HH_SP_HEX_BEGIN");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH_SP_HEX_DIGIT_BEGIN_MARKER,
    b"RPI5_HH_SP_HEX_DIGIT_BEGIN"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH_SP_HEX_DIGIT_DONE_MARKER,
    b"RPI5_HH_SP_HEX_DIGIT_DONE"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_HH_SP_HEX_DONE_MARKER, b"RPI5_HH_SP_HEX_DONE");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH_SP_HEX_FAILED_MARKER,
    b"RPI5_HH_SP_HEX_FAILED reason="
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH3_SP_HEX_FAULT_BOUNDARY_MARKER,
    b"RPI5_HH3_FAULT_BOUNDARY reason=sp_hex_output"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH_READ_SP_DONE_MARKER,
    b"RPI5_HH_READ_SP_DONE value=0x"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_HH_READ_VBAR_BEGIN_MARKER, b"RPI5_HH_READ_VBAR_BEGIN");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH_READ_VBAR_CAPTURED_MARKER,
    b"RPI5_HH_READ_VBAR_CAPTURED"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_HH_VBAR_HEX_BEGIN_MARKER, b"RPI5_HH_VBAR_HEX_BEGIN");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH_VBAR_HEX_DIGIT_BEGIN_MARKER,
    b"RPI5_HH_VBAR_HEX_DIGIT_BEGIN"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH_VBAR_HEX_DIGIT_DONE_MARKER,
    b"RPI5_HH_VBAR_HEX_DIGIT_DONE"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_HH_VBAR_HEX_DONE_MARKER, b"RPI5_HH_VBAR_HEX_DONE");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH_VBAR_HEX_FAILED_MARKER,
    b"RPI5_HH_VBAR_HEX_FAILED reason="
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH3_VBAR_HEX_FAULT_BOUNDARY_MARKER,
    b"RPI5_HH3_FAULT_BOUNDARY reason=vbar_hex_output"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH_READ_VBAR_DONE_MARKER,
    b"RPI5_HH_READ_VBAR_DONE value=0x"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_HH_READ_TTBR_BEGIN_MARKER, b"RPI5_HH_READ_TTBR_BEGIN");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_HH_READ_TTBR_DONE_MARKER, b"RPI5_HH_READ_TTBR_DONE");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_HH_PRINT_REGS_BEGIN_MARKER, b"RPI5_HH_PRINT_REGS_BEGIN");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH_PRINT_REGS_FIRST_BEGIN_MARKER,
    b"RPI5_HH_PRINT_REGS_FIRST_BEGIN"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH_PRINT_REGS_FIRST_HEX_BEGIN_MARKER,
    b"RPI5_HH_PRINT_REGS_FIRST_HEX_BEGIN"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH_PRINT_REGS_FIRST_HEX_DIGIT_BEGIN_MARKER,
    b"RPI5_HH_PRINT_REGS_FIRST_HEX_DIGIT_BEGIN"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH_PRINT_REGS_FIRST_HEX_DIGIT_DONE_MARKER,
    b"RPI5_HH_PRINT_REGS_FIRST_HEX_DIGIT_DONE"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH_PRINT_REGS_FIRST_HEX_DONE_MARKER,
    b"RPI5_HH_PRINT_REGS_FIRST_HEX_DONE"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH_PRINT_REGS_FIRST_HEX_FAILED_MARKER,
    b"RPI5_HH_PRINT_REGS_FIRST_HEX_FAILED reason="
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH3_PRINT_REGS_FIRST_HEX_FAULT_BOUNDARY_MARKER,
    b"RPI5_HH3_FAULT_BOUNDARY reason=print_regs_first_hex_output"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH_PRINT_REGS_FIRST_DONE_MARKER,
    b"RPI5_HH_PRINT_REGS_FIRST_DONE value=0x"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH_PRINT_REGS_SP_BEGIN_MARKER,
    b"RPI5_HH_PRINT_REGS_SP_BEGIN"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH_PRINT_REGS_SP_HEX_BEGIN_MARKER,
    b"RPI5_HH_PRINT_REGS_SP_HEX_BEGIN"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH_PRINT_REGS_SP_HEX_DIGIT_BEGIN_MARKER,
    b"RPI5_HH_PRINT_REGS_SP_HEX_DIGIT_BEGIN"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH_PRINT_REGS_SP_HEX_DIGIT_DONE_MARKER,
    b"RPI5_HH_PRINT_REGS_SP_HEX_DIGIT_DONE"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH_PRINT_REGS_SP_HEX_DONE_MARKER,
    b"RPI5_HH_PRINT_REGS_SP_HEX_DONE"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH_PRINT_REGS_SP_HEX_FAILED_MARKER,
    b"RPI5_HH_PRINT_REGS_SP_HEX_FAILED reason="
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH3_PRINT_REGS_SP_HEX_FAULT_BOUNDARY_MARKER,
    b"RPI5_HH3_FAULT_BOUNDARY reason=print_regs_sp_hex_output"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH_PRINT_REGS_SP_DONE_MARKER,
    b"RPI5_HH_PRINT_REGS_SP_DONE value=0x"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH_PRINT_REGS_BYPASS_FOR_HH3_PROOF_MARKER,
    b"RPI5_HH_PRINT_REGS_BYPASS_FOR_HH3_PROOF"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_HH_PRINT_REGS_DONE_MARKER, b"RPI5_HH_PRINT_REGS_DONE");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_HH3_PRECHECK_DONE_MARKER, b"RPI5_HH3_PRECHECK_DONE");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_HH3_FAILED_MARKER, b"RPI5_HH3_FAILED reason=");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH3_FAULT_BOUNDARY_MARKER,
    b"RPI5_HH3_FAULT_BOUNDARY reason="
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_HH_REGISTERS_OK_MARKER, b"RPI5_HH_REGISTERS_OK");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_HH_RUST_UART_OK_MARKER, b"RPI5_HH_RUST_UART_OK");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_HH3_DONE_MARKER, b"RPI5_HH3_DONE");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_HH4_BEGIN_MARKER, b"RPI5_HH4_BEGIN");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_HH4_DTB_PTR_BEGIN_MARKER, b"RPI5_HH4_DTB_PTR_BEGIN");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_HH4_DTB_PTR_OK_MARKER, b"RPI5_HH4_DTB_PTR_OK value=0x");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH4_DTB_VIRT_OK_MARKER,
    b"RPI5_HH4_DTB_VIRT_OK value=0x"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH4_DTB_PTR_FAILED_MARKER,
    b"RPI5_HH4_DTB_PTR_FAILED reason="
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_HH4_UART_STILL_OK_MARKER, b"RPI5_HH4_UART_STILL_OK");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH4_FAULT_BOUNDARY_MARKER,
    b"RPI5_HH4_FAULT_BOUNDARY reason="
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_HH4_PRECHECK_OK_MARKER, b"RPI5_HH4_PRECHECK_OK");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH4_EMPTY_TTBR0_ROOT_MARKER,
    b"RPI5_HH4_EMPTY_TTBR0_ROOT base=0x"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH4_TTBR0_REPLACE_BEGIN_MARKER,
    b"RPI5_HH4_TTBR0_REPLACE_BEGIN old=0x"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_HH4_NEW_TTBR0_SEPARATOR, b" new=0x");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH4_TTBR0_REPLACE_DONE_MARKER,
    b"RPI5_HH4_TTBR0_REPLACE_DONE"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_HH4_PC_HIGH_OK_MARKER, b"RPI5_HH4_PC_HIGH_OK value=0x");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_HH4_SP_HIGH_OK_MARKER, b"RPI5_HH4_SP_HIGH_OK value=0x");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH4_VBAR_HIGH_OK_MARKER,
    b"RPI5_HH4_VBAR_HIGH_OK value=0x"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH4_UART_AFTER_TTBR0_OK_MARKER,
    b"RPI5_HH4_UART_AFTER_TTBR0_OK"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_HH4_DONE_MARKER, b"RPI5_HH4_DONE");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_HH4_FAILED_MARKER, b"RPI5_HH4_FAILED reason=");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_HH5_BEGIN_MARKER, b"RPI5_HH5_BEGIN");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_DTB_CHOSEN_BEGIN_MARKER,
    b"RPI5_HH5_DTB_CHOSEN_BEGIN"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_HH5_DTB_CHOSEN_OK_MARKER, b"RPI5_HH5_DTB_CHOSEN_OK");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_FDT_HEADER_BEGIN_MARKER,
    b"RPI5_HH5_FDT_HEADER_BEGIN"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_HH5_FDT_HEADER_OK_MARKER, b"RPI5_HH5_FDT_HEADER_OK");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_FDT_BLOCKS_BEGIN_MARKER,
    b"RPI5_HH5_FDT_BLOCKS_BEGIN"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_HH5_FDT_BLOCKS_OK_MARKER, b"RPI5_HH5_FDT_BLOCKS_OK");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_FDT_CHOSEN_SCAN_BEGIN_MARKER,
    b"RPI5_HH5_FDT_CHOSEN_SCAN_BEGIN"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_FDT_CHOSEN_FOUND_MARKER,
    b"RPI5_HH5_FDT_CHOSEN_FOUND"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_FDT_CHOSEN_SCAN_DONE_MARKER,
    b"RPI5_HH5_FDT_CHOSEN_SCAN_DONE"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_FDT_INITRD_PROPS_BEGIN_MARKER,
    b"RPI5_HH5_FDT_INITRD_PROPS_BEGIN"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_FDT_INITRD_PROPS_DONE_MARKER,
    b"RPI5_HH5_FDT_INITRD_PROPS_DONE"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_DTB_WALK_FAILED_MARKER,
    b"RPI5_HH5_DTB_WALK_FAILED reason="
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_HH5_INITRD_BEGIN_MARKER, b"RPI5_HH5_INITRD_BEGIN");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_INITRD_RANGE_MARKER,
    b"RPI5_HH5_INITRD_RANGE phys_start=0x"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_HH5_INITRD_PHYS_END_SEP_MARKER, b" phys_end=0x");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_INITRD_VIRT_MARKER,
    b"RPI5_HH5_INITRD_VIRT virt_start=0x"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_HH5_INITRD_VIRT_END_SEP_MARKER, b" virt_end=0x");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_HH5_INITRD_OK_MARKER, b"RPI5_HH5_INITRD_OK");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_INITRD_FAILED_MARKER,
    b"RPI5_HH5_INITRD_FAILED reason="
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_ALLOC_BRIDGE_BEGIN_MARKER,
    b"RPI5_HH5_ALLOC_BRIDGE_BEGIN"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_ALLOC_BRIDGE_RANGE_MARKER,
    b"RPI5_HH5_ALLOC_BRIDGE_RANGE phys=0x"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_HH5_ALLOC_BRIDGE_VIRT_SEP_MARKER, b" virt=0x");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_HH5_ALLOC_BRIDGE_SIZE_SEP_MARKER, b" size=0x");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_HH5_ALLOC_BRIDGE_OK_MARKER, b"RPI5_HH5_ALLOC_BRIDGE_OK");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_ALLOC_BRIDGE_FAILED_MARKER,
    b"RPI5_HH5_ALLOC_BRIDGE_FAILED reason="
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_HH5_HANDOFF_BEGIN_MARKER, b"RPI5_HH5_HANDOFF_BEGIN");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_HH5_HANDOFF_OK_MARKER, b"RPI5_HH5_HANDOFF_OK virt=0x");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_FAULT_BOUNDARY_MARKER,
    b"RPI5_HH5_FAULT_BOUNDARY reason="
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_ENTER_KERNEL_BEGIN_MARKER,
    b"RPI5_HH5_ENTER_KERNEL_BEGIN"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_NORMAL_BOOT_AUDIT_BEGIN_MARKER,
    b"RPI5_HH5_NORMAL_BOOT_AUDIT_BEGIN"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_NORMAL_BOOT_AUDIT_DONE_MARKER,
    b"RPI5_HH5_NORMAL_BOOT_AUDIT_DONE"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_BOOT_INPUT_OK_MARKER,
    b"RPI5_HH5_BOOT_INPUT_OK virt=0x"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_ALLOC_ADAPTER_BEGIN_MARKER,
    b"RPI5_HH5_ALLOC_ADAPTER_BEGIN"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_ALLOC_ADAPTER_LAYOUT_BEGIN_MARKER,
    b"RPI5_HH5_ALLOC_ADAPTER_LAYOUT_BEGIN"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_ALLOC_ADAPTER_LAYOUT_OK_MARKER,
    b"RPI5_HH5_ALLOC_ADAPTER_LAYOUT_OK"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_ALLOC_ADAPTER_STORAGE_BEGIN_MARKER,
    b"RPI5_HH5_ALLOC_ADAPTER_STORAGE_BEGIN"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_ALLOC_ADAPTER_STORAGE_OK_MARKER,
    b"RPI5_HH5_ALLOC_ADAPTER_STORAGE_OK virt=0x"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_ALLOC_ADAPTER_ZERO_BEGIN_MARKER,
    b"RPI5_HH5_ALLOC_ADAPTER_ZERO_BEGIN"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_ALLOC_ADAPTER_ZERO_DONE_MARKER,
    b"RPI5_HH5_ALLOC_ADAPTER_ZERO_DONE"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_ALLOC_ADAPTER_INIT_BEGIN_MARKER,
    b"RPI5_HH5_ALLOC_ADAPTER_INIT_BEGIN"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_ALLOC_ADAPTER_INIT_DONE_MARKER,
    b"RPI5_HH5_ALLOC_ADAPTER_INIT_DONE"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_ALLOC_ADAPTER_INIT_RANGE_BEGIN_MARKER,
    b"RPI5_HH5_ALLOC_ADAPTER_INIT_RANGE_BEGIN"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_ALLOC_ADAPTER_INIT_RANGE_OK_MARKER,
    b"RPI5_HH5_ALLOC_ADAPTER_INIT_RANGE_OK pages=0x"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_ALLOC_ADAPTER_INIT_CAPACITY_OK_MARKER,
    b"RPI5_HH5_ALLOC_ADAPTER_INIT_CAPACITY_OK capacity=0x"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_ALLOC_ADAPTER_INIT_CALL_BEGIN_MARKER,
    b"RPI5_HH5_ALLOC_ADAPTER_INIT_CALL_BEGIN"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_ALLOC_ADAPTER_INIT_CALL_DONE_MARKER,
    b"RPI5_HH5_ALLOC_ADAPTER_INIT_CALL_DONE"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_ALLOC_ADAPTER_PROBE_ALLOC_BEGIN_MARKER,
    b"RPI5_HH5_ALLOC_ADAPTER_PROBE_ALLOC_BEGIN"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_ALLOC_ADAPTER_PROBE_ALLOC_CALL_BEGIN_MARKER,
    b"RPI5_HH5_ALLOC_ADAPTER_PROBE_ALLOC_CALL_BEGIN"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_ALLOC_ADAPTER_PROBE_ALLOC_CALL_DONE_MARKER,
    b"RPI5_HH5_ALLOC_ADAPTER_PROBE_ALLOC_CALL_DONE"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_ALLOC_ADAPTER_PROBE_ALLOC_VALIDATE_BEGIN_MARKER,
    b"RPI5_HH5_ALLOC_ADAPTER_PROBE_ALLOC_VALIDATE_BEGIN"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_ALLOC_ADAPTER_PROBE_ALLOC_VALIDATE_OK_MARKER,
    b"RPI5_HH5_ALLOC_ADAPTER_PROBE_ALLOC_VALIDATE_OK"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_ALLOC_ADAPTER_PROBE_ALLOC_OK_MARKER,
    b"RPI5_HH5_ALLOC_ADAPTER_PROBE_ALLOC_OK frame=0x"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_ALLOC_ADAPTER_PROBE_FREE_BEGIN_MARKER,
    b"RPI5_HH5_ALLOC_ADAPTER_PROBE_FREE_BEGIN"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_ALLOC_ADAPTER_PROBE_FREE_CALL_BEGIN_MARKER,
    b"RPI5_HH5_ALLOC_ADAPTER_PROBE_FREE_CALL_BEGIN"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_ALLOC_ADAPTER_PROBE_FREE_CALL_DONE_MARKER,
    b"RPI5_HH5_ALLOC_ADAPTER_PROBE_FREE_CALL_DONE"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_ALLOC_ADAPTER_PROBE_FREE_OK_MARKER,
    b"RPI5_HH5_ALLOC_ADAPTER_PROBE_FREE_OK"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_ALLOC_ADAPTER_RANGE_MARKER,
    b"RPI5_HH5_ALLOC_ADAPTER_RANGE usable_start=0x"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_ALLOC_ADAPTER_USABLE_END_SEP_MARKER,
    b" usable_end=0x"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_ALLOC_ADAPTER_OK_MARKER,
    b"RPI5_HH5_ALLOC_ADAPTER_OK"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_ALLOC_ADAPTER_FAILED_MARKER,
    b"RPI5_HH5_ALLOC_ADAPTER_FAILED reason="
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_KERNEL_ENTRY_BEGIN_MARKER, b"RPI5_KERNEL_ENTRY_BEGIN");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_KERNEL_DTB_PARSE_BEGIN_MARKER,
    b"RPI5_KERNEL_DTB_PARSE_BEGIN"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_KERNEL_DTB_PARSE_OK_MARKER, b"RPI5_KERNEL_DTB_PARSE_OK");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_KERNEL_INITRD_OK_MARKER, b"RPI5_KERNEL_INITRD_OK");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_KERNEL_PMEM_BEGIN_MARKER, b"RPI5_KERNEL_PMEM_BEGIN");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_KERNEL_PMEM_OK_MARKER,
    b"RPI5_KERNEL_PMEM_OK free_pages=0x"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_KERNEL_BOOTINFO_OK_MARKER, b"RPI5_KERNEL_BOOTINFO_OK");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_BOOT4_GLOBAL_HEAP_AUDIT_BEGIN_MARKER,
    b"RPI5_BOOT4_GLOBAL_HEAP_AUDIT_BEGIN"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_BOOT4_GLOBAL_HEAP_AUDIT_DONE_MARKER,
    b"RPI5_BOOT4_GLOBAL_HEAP_AUDIT_DONE"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_BOOT4_FAULT_BOUNDARY_MARKER,
    b"RPI5_BOOT4_FAULT_BOUNDARY reason="
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_KERNEL_GLOBAL_HEAP_BEGIN_MARKER,
    b"RPI5_KERNEL_GLOBAL_HEAP_BEGIN"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_KERNEL_GLOBAL_HEAP_RANGE_MARKER,
    b"RPI5_KERNEL_GLOBAL_HEAP_RANGE virt=0x"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_KERNEL_GLOBAL_HEAP_SIZE_SEP_MARKER, b" size=0x");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_KERNEL_GLOBAL_HEAP_OK_MARKER,
    b"RPI5_KERNEL_GLOBAL_HEAP_OK"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_KERNEL_GLOBAL_HEAP_FAILED_MARKER,
    b"RPI5_KERNEL_GLOBAL_HEAP_FAILED reason="
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_KERNEL_VM_BEGIN_MARKER, b"RPI5_KERNEL_VM_BEGIN");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_KERNEL_VM_LAYOUT_OK_MARKER, b"RPI5_KERNEL_VM_LAYOUT_OK");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_KERNEL_VM_OK_MARKER, b"RPI5_KERNEL_VM_OK");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_KERNEL_VM_FAILED_MARKER,
    b"RPI5_KERNEL_VM_FAILED reason="
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_BOOT4_PHYSMAP_AUDIT_BEGIN_MARKER,
    b"RPI5_BOOT4_PHYSMAP_AUDIT_BEGIN"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_BOOT4_PHYSMAP_AUDIT_DONE_MARKER,
    b"RPI5_BOOT4_PHYSMAP_AUDIT_DONE"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_KERNEL_GLOBAL_ALLOCATOR_BEGIN_MARKER,
    b"RPI5_KERNEL_GLOBAL_ALLOCATOR_BEGIN"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_KERNEL_GLOBAL_ALLOCATOR_HEAP_RANGE_MARKER,
    b"RPI5_KERNEL_GLOBAL_ALLOCATOR_HEAP_RANGE phys=0x"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_KERNEL_PHYSMAP_SWITCH_BEGIN_MARKER,
    b"RPI5_KERNEL_PHYSMAP_SWITCH_BEGIN"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_KERNEL_PHYSMAP_SWITCH_OK_MARKER,
    b"RPI5_KERNEL_PHYSMAP_SWITCH_OK offset=0x"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_KERNEL_GLOBAL_ALLOCATOR_PHYSMAP_OK_MARKER,
    b"RPI5_KERNEL_GLOBAL_ALLOCATOR_PHYSMAP_OK"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_KERNEL_GLOBAL_ALLOCATOR_PROBE_BEGIN_MARKER,
    b"RPI5_KERNEL_GLOBAL_ALLOCATOR_PROBE_BEGIN"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_KERNEL_GLOBAL_ALLOCATOR_PROBE_OK_MARKER,
    b"RPI5_KERNEL_GLOBAL_ALLOCATOR_PROBE_OK ptr=0x"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_KERNEL_GLOBAL_ALLOCATOR_HIGHMAP_OK_MARKER,
    b"RPI5_KERNEL_GLOBAL_ALLOCATOR_HIGHMAP_OK"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_KERNEL_GLOBAL_ALLOCATOR_FAILED_MARKER,
    b"RPI5_KERNEL_GLOBAL_ALLOCATOR_FAILED reason="
);
// Devicetree reference names compared via raw-pointer reads (no anonymous
// literals, no slice iterators) so the lookup stays on the proven HH path.
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_HH5_NAME_CHOSEN, b"chosen");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_HH5_NAME_INITRD_START, b"linux,initrd-start");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_HH5_NAME_INITRD_END, b"linux,initrd-end");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_HH5_NAME_BOOTARGS, b"bootargs");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_INITRD_READY_MARKER,
    b"RPI5_HH5_INITRD_READY start=0x"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_INIT_LOOKUP_OK_MARKER,
    b"RPI5_HH5_INIT_LOOKUP_OK offset=0x"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_INIT_ELF_OK_MARKER,
    b"RPI5_HH5_INIT_ELF_OK entry=0x00000000004023d8"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_USER_ROOT_READY_MARKER,
    b"RPI5_HH5_USER_ROOT_READY base=0x"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_INIT_STACK_READY_MARKER,
    b"RPI5_HH5_INIT_STACK_READY sp=0x000000003fe00000"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_TRAP_FRAME_READY_MARKER,
    b"RPI5_HH5_TRAP_FRAME_READY entry=0x00000000004023d8 sp=0x000000003fe00000"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_ENTER_USER_PRECHECK_OK_MARKER,
    b"RPI5_HH5_ENTER_USER_PRECHECK_OK"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_TTBR0_USER_INSTALL_MARKER,
    b"RPI5_HH5_TTBR0_USER_INSTALL root=0x"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_ENTER_USER_ATTEMPT_MARKER,
    b"RPI5_HH5_ENTER_USER_ATTEMPT tid=1 entry=0x00000000004023d8 sp=0x000000003fe00000"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_HH5_DEFERRED_MARKER, b"RPI5_HH5_DEFERRED reason=");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_DONE_DEFERRED_MARKER,
    b"RPI5_HH5_DONE status=deferred"
);
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(RPI5_HH5_FAILED_MARKER, b"RPI5_HH5_FAILED reason=");
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
rpi5_hh_retained_marker!(
    RPI5_HH5_ENTER_USER_FAILED_MARKER,
    b"RPI5_HH5_ENTER_USER_FAILED reason="
);

#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
fn rpi5_hh_write_bytes(bytes: &[u8]) -> bool {
    const TX_READY_POLL_LIMIT: usize = 0x1_0000;
    let data = RPI5_HH_UART_VIRT as *mut u32;
    let flags = (RPI5_HH_UART_VIRT + 0x18) as *const u32;
    for &byte in bytes {
        let mut ready = false;
        for _ in 0..TX_READY_POLL_LIMIT {
            if unsafe { core::ptr::read_volatile(flags) } & (1 << 5) == 0 {
                ready = true;
                break;
            }
        }
        if !ready {
            return false;
        }
        unsafe {
            core::ptr::write_volatile(data, u32::from(byte));
        }
    }
    true
}

#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
fn rpi5_hh_write_line(line: &[u8]) -> bool {
    rpi5_hh_write_bytes(line) && rpi5_hh_write_bytes(b"\r\n")
}

#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
// Retained for reference only; the HH paths now emit hex via the proven inline
// raw-pointer technique because this slice-based helper stalls on hardware.
#[allow(dead_code)]
fn rpi5_hh_write_hex_line(prefix: &[u8], value: u64) -> bool {
    let mut digits = [0u8; 16];
    rpi5_hh_hex_digits(value, &mut digits);
    rpi5_hh_write_bytes(prefix) && rpi5_hh_write_bytes(&digits) && rpi5_hh_write_bytes(b"\r\n")
}

#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
#[allow(dead_code)]
fn rpi5_hh_write_two_hex_line(prefix: &[u8], first: u64, separator: &[u8], second: u64) -> bool {
    let mut first_digits = [0u8; 16];
    let mut second_digits = [0u8; 16];
    rpi5_hh_hex_digits(first, &mut first_digits);
    rpi5_hh_hex_digits(second, &mut second_digits);
    rpi5_hh_write_bytes(prefix)
        && rpi5_hh_write_bytes(&first_digits)
        && rpi5_hh_write_bytes(separator)
        && rpi5_hh_write_bytes(&second_digits)
        && rpi5_hh_write_bytes(b"\r\n")
}

#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
fn rpi5_hh_fail(reason: &[u8]) -> ! {
    let _ = rpi5_hh_write_bytes(b"RPI5_HH_REGISTER_MISMATCH reason=");
    let _ = rpi5_hh_write_line(reason);
    let _ = rpi5_hh_write_bytes(&RPI5_HH3_FAULT_BOUNDARY_MARKER);
    let _ = rpi5_hh_write_line(reason);
    let _ = rpi5_hh_write_bytes(&RPI5_HH3_FAILED_MARKER);
    let _ = rpi5_hh_write_line(reason);
    rpi5_hh_halt()
}

#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
fn rpi5_hh_pc_fail(reason: &[u8]) -> ! {
    let _ = rpi5_hh_write_bytes(&RPI5_HH_READ_PC_FAILED_MARKER);
    let _ = rpi5_hh_write_line(reason);
    let _ = rpi5_hh_write_line(&RPI5_HH3_PC_FAULT_BOUNDARY_MARKER);
    rpi5_hh_halt()
}

#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
fn rpi5_hh_hex_fail(reason: &[u8]) -> ! {
    let _ = rpi5_hh_write_bytes(&RPI5_HH_HEX_FAILED_MARKER);
    let _ = rpi5_hh_write_line(reason);
    let _ = rpi5_hh_write_line(&RPI5_HH3_HEX_FAULT_BOUNDARY_MARKER);
    rpi5_hh_halt()
}

#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
fn rpi5_hh_sp_hex_fail(reason: &[u8]) -> ! {
    let _ = rpi5_hh_write_bytes(&RPI5_HH_SP_HEX_FAILED_MARKER);
    let _ = rpi5_hh_write_line(reason);
    let _ = rpi5_hh_write_line(&RPI5_HH3_SP_HEX_FAULT_BOUNDARY_MARKER);
    rpi5_hh_halt()
}

#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
fn rpi5_hh_vbar_hex_fail(reason: &[u8]) -> ! {
    let _ = rpi5_hh_write_bytes(&RPI5_HH_VBAR_HEX_FAILED_MARKER);
    let _ = rpi5_hh_write_line(reason);
    let _ = rpi5_hh_write_line(&RPI5_HH3_VBAR_HEX_FAULT_BOUNDARY_MARKER);
    rpi5_hh_halt()
}

#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
fn rpi5_hh_print_regs_first_hex_fail(reason: &[u8]) -> ! {
    let _ = rpi5_hh_write_bytes(&RPI5_HH_PRINT_REGS_FIRST_HEX_FAILED_MARKER);
    let _ = rpi5_hh_write_line(reason);
    let _ = rpi5_hh_write_line(&RPI5_HH3_PRINT_REGS_FIRST_HEX_FAULT_BOUNDARY_MARKER);
    rpi5_hh_halt()
}

#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
fn rpi5_hh_print_regs_sp_hex_fail(reason: &[u8]) -> ! {
    let _ = rpi5_hh_write_bytes(&RPI5_HH_PRINT_REGS_SP_HEX_FAILED_MARKER);
    let _ = rpi5_hh_write_line(reason);
    let _ = rpi5_hh_write_line(&RPI5_HH3_PRINT_REGS_SP_HEX_FAULT_BOUNDARY_MARKER);
    rpi5_hh_halt()
}

#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
fn rpi5_hh_halt() -> ! {
    loop {
        unsafe {
            core::arch::asm!("wfe", options(nomem, nostack, preserves_flags));
        }
    }
}

#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
fn rpi5_hh4_fail(reason: &[u8]) -> ! {
    let _ = rpi5_hh_write_bytes(&RPI5_HH4_FAULT_BOUNDARY_MARKER);
    let _ = rpi5_hh_write_line(reason);
    let _ = rpi5_hh_write_bytes(&RPI5_HH4_FAILED_MARKER);
    let _ = rpi5_hh_write_line(reason);
    rpi5_hh_halt()
}

#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
struct Rpi5Hh4Ready {
    empty_ttbr0_root: u64,
    ttbr1_root: u64,
    dtb_phys: u64,
    dtb_virt: u64,
}

#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
fn rpi5_hh4_retire_low_ttbr0(dtb_phys: u64) -> Rpi5Hh4Ready {
    unsafe extern "C" {
        static __hh_ttbr0_root: u8;
        static __hh_ttbr1_root: u8;
        static __hh_empty_ttbr0_root: u8;
    }

    if !rpi5_hh_write_line(&RPI5_HH4_BEGIN_MARKER) {
        rpi5_hh4_fail(b"uart_timeout");
    }

    /*
     * Inline, bounded, raw-pointer high-UART output. This duplicates the proven
     * HH-3 inline hex technique rather than calling the helper-based
     * `rpi5_hh_write_hex_line`, whose slice-iterator hex path has never produced
     * output on hardware. Per the ultra-early-boot policy we prefer this
     * duplication over a shared abstraction. Plain line markers still use the
     * proven `rpi5_hh_write_line`, which is confirmed working on hardware.
     */
    const HH4_TX_POLL_LIMIT: usize = 0x1_0000;
    let hh4_uart_data = RPI5_HH_UART_VIRT as *mut u32;
    let hh4_uart_flags = (RPI5_HH_UART_VIRT + 0x18) as *const u32;
    macro_rules! hh4_write_byte {
        ($byte:expr, $reason:literal) => {{
            let mut poll = 0usize;
            while poll < HH4_TX_POLL_LIMIT {
                if unsafe { core::ptr::read_volatile(hh4_uart_flags) } & (1 << 5) == 0 {
                    break;
                }
                poll += 1;
            }
            if poll == HH4_TX_POLL_LIMIT {
                rpi5_hh4_fail($reason);
            }
            unsafe {
                core::ptr::write_volatile(hh4_uart_data, $byte as u32);
            }
        }};
    }
    macro_rules! hh4_emit_marker {
        ($marker:expr, $reason:literal) => {{
            let mut idx = 0usize;
            let ptr = core::ptr::addr_of!($marker).cast::<u8>();
            while idx < $marker.len() {
                let byte = unsafe { core::ptr::read(ptr.add(idx)) };
                hh4_write_byte!(byte, $reason);
                idx += 1;
            }
        }};
    }
    macro_rules! hh4_emit_hex {
        ($value:expr, $reason:literal) => {{
            let mut nib = 0usize;
            while nib < 16 {
                let shift = 60 - nib * 4;
                let nibble = (($value >> shift) & 0xf) as u8;
                let digit = if nibble < 10 {
                    b'0' + nibble
                } else {
                    b'a' + nibble - 10
                };
                hh4_write_byte!(digit, $reason);
                nib += 1;
            }
        }};
    }
    macro_rules! hh4_emit_crlf {
        ($reason:literal) => {{
            hh4_write_byte!(b'\r', $reason);
            hh4_write_byte!(b'\n', $reason);
        }};
    }
    macro_rules! hh4_hex_line {
        ($marker:expr, $value:expr, $reason:literal) => {{
            hh4_emit_marker!($marker, $reason);
            hh4_emit_hex!($value, $reason);
            hh4_emit_crlf!($reason);
        }};
    }

    /*
     * Part C — prove the firmware DTB pointer survived the low->high handoff.
     *
     * `dtb_phys` was captured from x20 at the top of the continuation. TTBR1
     * maps 0..2 GiB normal RAM at VA = PA + HH_VA_OFFSET, so when the pointer is
     * inside that window its high virtual alias is dtb_phys + HH_VA_OFFSET. We
     * validate the pointer, then read the FDT magic (0xd00dfeed, big-endian)
     * through the high alias to prove both the pointer and the high mapping.
     */
    if !rpi5_hh_write_line(&RPI5_HH4_DTB_PTR_BEGIN_MARKER) {
        rpi5_hh4_fail(b"dtb_ptr_begin_uart_timeout");
    }
    macro_rules! hh4_dtb_fail {
        ($reason:literal) => {{
            let _ = rpi5_hh_write_bytes(&RPI5_HH4_DTB_PTR_FAILED_MARKER);
            let _ = rpi5_hh_write_line($reason);
            rpi5_hh4_fail($reason);
        }};
    }
    if dtb_phys == 0 {
        hh4_dtb_fail!(b"dtb_null");
    }
    if dtb_phys & 0x3 != 0 {
        hh4_dtb_fail!(b"dtb_misaligned");
    }
    // 0x8000_0000 == HH_RAM_LIMIT, the high (TTBR1) identity window upper bound.
    if dtb_phys >= 0x8000_0000 {
        hh4_dtb_fail!(b"dtb_out_of_high_window");
    }
    let dtb_virt = dtb_phys + RPI5_HH_VA_OFFSET;
    let dtb_magic = unsafe { core::ptr::read_volatile(dtb_virt as *const u32) };
    if u32::from_be(dtb_magic) != 0xd00d_feed {
        hh4_dtb_fail!(b"dtb_magic");
    }
    hh4_hex_line!(
        RPI5_HH4_DTB_PTR_OK_MARKER,
        dtb_phys,
        b"dtb_ptr_ok_uart_timeout"
    );
    hh4_hex_line!(
        RPI5_HH4_DTB_VIRT_OK_MARKER,
        dtb_virt,
        b"dtb_virt_ok_uart_timeout"
    );
    if !rpi5_hh_write_line(&RPI5_HH4_UART_STILL_OK_MARKER) {
        rpi5_hh4_fail(b"uart_still_ok_uart_timeout");
    }

    let pc: u64;
    let sp: u64;
    let vbar: u64;
    let old_ttbr0: u64;
    let ttbr1: u64;
    unsafe {
        core::arch::asm!(
            "adr {pc}, .",
            "mov {sp}, sp",
            "mrs {vbar}, VBAR_EL1",
            "mrs {ttbr0}, TTBR0_EL1",
            "mrs {ttbr1}, TTBR1_EL1",
            pc = out(reg) pc,
            sp = out(reg) sp,
            vbar = out(reg) vbar,
            ttbr0 = out(reg) old_ttbr0,
            ttbr1 = out(reg) ttbr1,
            options(nomem, nostack, preserves_flags)
        );
    }

    let root_mask = !0xfffu64;
    let expected_old_ttbr0 = core::ptr::addr_of!(__hh_ttbr0_root) as u64;
    let expected_ttbr1 = core::ptr::addr_of!(__hh_ttbr1_root) as u64;
    let empty_ttbr0_root = core::ptr::addr_of!(__hh_empty_ttbr0_root) as u64;
    if pc < RPI5_HH_VA_OFFSET {
        rpi5_hh4_fail(b"pc_not_high");
    }
    if sp < RPI5_HH_VA_OFFSET {
        rpi5_hh4_fail(b"sp_not_high");
    }
    if vbar < RPI5_HH_VA_OFFSET || vbar & 0x7ff != 0 {
        rpi5_hh4_fail(b"vbar_not_high_aligned");
    }
    if old_ttbr0 & root_mask != expected_old_ttbr0 {
        rpi5_hh4_fail(b"old_ttbr0_mismatch");
    }
    if ttbr1 & root_mask != expected_ttbr1 {
        rpi5_hh4_fail(b"ttbr1_root_mismatch");
    }
    if empty_ttbr0_root == 0
        || empty_ttbr0_root & 0xfff != 0
        || empty_ttbr0_root == expected_old_ttbr0
        || empty_ttbr0_root == expected_ttbr1
    {
        rpi5_hh4_fail(b"empty_ttbr0_invalid");
    }
    for address in [
        core::ptr::addr_of!(RPI5_HH4_TTBR0_REPLACE_DONE_MARKER) as u64,
        core::ptr::addr_of!(RPI5_HH4_PC_HIGH_OK_MARKER) as u64,
        core::ptr::addr_of!(RPI5_HH4_SP_HIGH_OK_MARKER) as u64,
        core::ptr::addr_of!(RPI5_HH4_VBAR_HIGH_OK_MARKER) as u64,
        core::ptr::addr_of!(RPI5_HH4_UART_AFTER_TTBR0_OK_MARKER) as u64,
        core::ptr::addr_of!(RPI5_HH4_DONE_MARKER) as u64,
        rpi5_hh4_retire_low_ttbr0 as usize as u64,
        rpi5_hh5_bridge as usize as u64,
    ] {
        if address < RPI5_HH_VA_OFFSET {
            rpi5_hh4_fail(b"post_replace_pointer_not_high");
        }
    }
    if !rpi5_hh_write_line(&RPI5_HH4_PRECHECK_OK_MARKER) {
        rpi5_hh4_fail(b"uart_timeout");
    }
    hh4_hex_line!(
        RPI5_HH4_EMPTY_TTBR0_ROOT_MARKER,
        empty_ttbr0_root,
        b"empty_ttbr0_root_uart_timeout"
    );
    hh4_emit_marker!(
        RPI5_HH4_TTBR0_REPLACE_BEGIN_MARKER,
        b"ttbr0_replace_begin_uart_timeout"
    );
    hh4_emit_hex!(old_ttbr0, b"ttbr0_replace_old_uart_timeout");
    hh4_emit_marker!(
        RPI5_HH4_NEW_TTBR0_SEPARATOR,
        b"ttbr0_replace_sep_uart_timeout"
    );
    hh4_emit_hex!(empty_ttbr0_root, b"ttbr0_replace_new_uart_timeout");
    hh4_emit_crlf!(b"ttbr0_replace_crlf_uart_timeout");

    let empty_root = empty_ttbr0_root as *mut u64;
    for index in 0..512 {
        unsafe {
            core::ptr::write_volatile(empty_root.add(index), 0);
        }
    }
    for offset in (0..4096).step_by(64) {
        let address = empty_ttbr0_root + offset;
        unsafe {
            core::arch::asm!("dc cvac, {address}", address = in(reg) address, options(nostack));
        }
    }

    /*
     * This is the HH-4 architectural boundary. Every instruction, stack
     * access, literal, vector, and UART access after the TTBR write uses a
     * TTBR1 high address. The empty root deliberately maps no low addresses.
     */
    unsafe {
        core::arch::asm!(
            "dsb ishst",
            "msr TTBR0_EL1, {root}",
            "isb",
            "tlbi vmalle1",
            "dsb ish",
            "isb",
            root = in(reg) empty_ttbr0_root,
            options(nostack, preserves_flags)
        );
    }

    let replaced_ttbr0: u64;
    let post_pc: u64;
    let post_sp: u64;
    let post_vbar: u64;
    unsafe {
        core::arch::asm!(
            "mrs {ttbr0}, TTBR0_EL1",
            "adr {pc}, .",
            "mov {sp}, sp",
            "mrs {vbar}, VBAR_EL1",
            ttbr0 = out(reg) replaced_ttbr0,
            pc = out(reg) post_pc,
            sp = out(reg) post_sp,
            vbar = out(reg) post_vbar,
            options(nomem, nostack, preserves_flags)
        );
    }
    if replaced_ttbr0 & root_mask != empty_ttbr0_root {
        rpi5_hh4_fail(b"ttbr0_replace_readback");
    }
    if post_pc < RPI5_HH_VA_OFFSET {
        rpi5_hh4_fail(b"post_replace_pc_not_high");
    }
    if post_sp < RPI5_HH_VA_OFFSET {
        rpi5_hh4_fail(b"post_replace_sp_not_high");
    }
    if post_vbar < RPI5_HH_VA_OFFSET || post_vbar & 0x7ff != 0 {
        rpi5_hh4_fail(b"post_replace_vbar_not_high");
    }
    if !rpi5_hh_write_line(&RPI5_HH4_TTBR0_REPLACE_DONE_MARKER) {
        rpi5_hh4_fail(b"uart_timeout");
    }
    hh4_hex_line!(
        RPI5_HH4_PC_HIGH_OK_MARKER,
        post_pc,
        b"pc_high_ok_uart_timeout"
    );
    hh4_hex_line!(
        RPI5_HH4_SP_HIGH_OK_MARKER,
        post_sp,
        b"sp_high_ok_uart_timeout"
    );
    hh4_hex_line!(
        RPI5_HH4_VBAR_HIGH_OK_MARKER,
        post_vbar,
        b"vbar_high_ok_uart_timeout"
    );
    if !rpi5_hh_write_line(&RPI5_HH4_UART_AFTER_TTBR0_OK_MARKER)
        || !rpi5_hh_write_line(&RPI5_HH4_DONE_MARKER)
    {
        rpi5_hh4_fail(b"uart_timeout");
    }

    Rpi5Hh4Ready {
        empty_ttbr0_root,
        ttbr1_root: expected_ttbr1,
        dtb_phys,
        dtb_virt,
    }
}

#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
#[allow(dead_code)]
fn rpi5_hh_hex_digits(value: u64, digits: &mut [u8; 16]) {
    for (index, digit) in digits.iter_mut().enumerate() {
        let nibble = ((value >> (60 - index * 4)) & 0xf) as u8;
        *digit = if nibble < 10 {
            b'0' + nibble
        } else {
            b'a' + nibble - 10
        };
    }
}

/// Read a big-endian u32 from a high virtual address (DTB content lives in the
/// TTBR1-mapped 0..2 GiB window). Uses an unaligned read so it is valid for any
/// 4-byte-aligned structure offset regardless of pointer provenance.
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
unsafe fn hh5_be32(va: u64) -> u32 {
    u32::from_be(unsafe { core::ptr::read_unaligned(va as *const u32) })
}

#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
unsafe fn hh5_be64(va: u64) -> u64 {
    let hi = unsafe { hh5_be32(va) } as u64;
    let lo = unsafe { hh5_be32(va + 4) } as u64;
    (hi << 32) | lo
}

/// Bounded NUL-terminated string length over a high virtual address.
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
#[allow(dead_code)]
unsafe fn hh5_cstr_len(va: u64, max: usize) -> usize {
    let mut i = 0usize;
    while i < max {
        let byte = unsafe { core::ptr::read_volatile((va + i as u64) as *const u8) };
        if byte == 0 {
            break;
        }
        i += 1;
    }
    i
}

/// Exact comparison of a NUL-terminated devicetree name at `name_va` against a
/// retained reference byte string. Both sides are read with raw pointers (no
/// slice iterators, no anonymous literals) to stay on the proven HH path.
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
unsafe fn hh5_name_eq(name_va: u64, ref_va: u64, ref_len: usize) -> bool {
    let mut i = 0usize;
    while i < ref_len {
        let a = unsafe { core::ptr::read_volatile((name_va + i as u64) as *const u8) };
        let b = unsafe { core::ptr::read((ref_va + i as u64) as *const u8) };
        if a != b {
            return false;
        }
        i += 1;
    }
    let term = unsafe { core::ptr::read_volatile((name_va + ref_len as u64) as *const u8) };
    term == 0
}

#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
#[derive(Clone, Copy)]
struct Hh5Chosen {
    walk_ok: bool,
    chosen_found: bool,
    initrd_present: bool,
    bad_cell_width: bool,
    initrd_start: u64,
    initrd_end: u64,
    bootargs_va: u64,
    bootargs_len: u64,
}

/// Minimal, bounded, high-alias-only flattened-devicetree walker that extracts
/// the `/chosen` `linux,initrd-start` / `linux,initrd-end` / `bootargs`
/// properties. It never dereferences a low physical address: every read targets
/// `dtb_virt` (the TTBR1 high alias of the firmware DTB). FDT layout per the
/// Devicetree Specification chapter 5 (all header/token words big-endian).
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
unsafe fn hh5_parse_chosen(dtb_virt: u64) -> Hh5Chosen {
    let mut out = Hh5Chosen {
        walk_ok: false,
        chosen_found: false,
        initrd_present: false,
        bad_cell_width: false,
        initrd_start: 0,
        initrd_end: 0,
        bootargs_va: 0,
        bootargs_len: 0,
    };

    // Precise structural failure: emit the specific reason then return not-ok.
    // `out` is returned so the caller can still emit the generic fault boundary.
    macro_rules! walk_fail {
        ($reason:literal) => {{
            let _ = rpi5_hh_write_bytes(&RPI5_HH5_DTB_WALK_FAILED_MARKER);
            let _ = rpi5_hh_write_line($reason);
            return out;
        }};
    }

    // --- Task B(1,2): header fields (all big-endian u32) and block bounds. ---
    let _ = rpi5_hh_write_line(&RPI5_HH5_FDT_HEADER_BEGIN_MARKER);
    let totalsize = unsafe { hh5_be32(dtb_virt + 0x04) } as u64;
    let off_struct = unsafe { hh5_be32(dtb_virt + 0x08) } as u64;
    let off_strings = unsafe { hh5_be32(dtb_virt + 0x0c) } as u64;
    let version = unsafe { hh5_be32(dtb_virt + 0x14) };
    let size_strings = unsafe { hh5_be32(dtb_virt + 0x20) } as u64;
    let size_struct = unsafe { hh5_be32(dtb_virt + 0x24) } as u64;
    // A sane firmware DTB is far below 1 MiB; size_dt_struct/strings need v17.
    if totalsize < 0x40 || totalsize > 0x0010_0000 {
        walk_fail!(b"header");
    }
    if version < 17 {
        walk_fail!(b"header");
    }
    let _ = rpi5_hh_write_line(&RPI5_HH5_FDT_HEADER_OK_MARKER);

    let _ = rpi5_hh_write_line(&RPI5_HH5_FDT_BLOCKS_BEGIN_MARKER);
    if off_struct < 0x28 || (off_struct & 0x3) != 0 || off_struct > totalsize {
        walk_fail!(b"blocks");
    }
    if size_struct == 0 || off_struct + size_struct > totalsize {
        walk_fail!(b"blocks");
    }
    if off_strings > totalsize || off_strings + size_strings > totalsize {
        walk_fail!(b"blocks");
    }
    let _ = rpi5_hh_write_line(&RPI5_HH5_FDT_BLOCKS_OK_MARKER);

    let mut p = dtb_virt + off_struct;
    let struct_end = dtb_virt + off_struct + size_struct;
    let strings_base = dtb_virt + off_strings;

    let chosen_ref = core::ptr::addr_of!(RPI5_HH5_NAME_CHOSEN) as u64;
    let start_ref = core::ptr::addr_of!(RPI5_HH5_NAME_INITRD_START) as u64;
    let end_ref = core::ptr::addr_of!(RPI5_HH5_NAME_INITRD_END) as u64;
    let bootargs_ref = core::ptr::addr_of!(RPI5_HH5_NAME_BOOTARGS) as u64;

    let mut depth: i32 = 0;
    let mut chosen_depth: i32 = -1;
    let mut have_start = false;
    let mut have_end = false;
    let mut ended = false;
    let mut guard: u32 = 0;
    const GUARD_MAX: u32 = 1 << 20;
    const DEPTH_MAX: i32 = 64;

    let _ = rpi5_hh_write_line(&RPI5_HH5_FDT_CHOSEN_SCAN_BEGIN_MARKER);

    while guard < GUARD_MAX {
        guard += 1;
        // Task B: every token read is bounds-checked (4-byte aligned).
        if p + 4 > struct_end {
            walk_fail!(b"token_bounds");
        }
        let token = unsafe { hh5_be32(p) };
        p += 4;
        if token == 0x1 {
            // FDT_BEGIN_NODE: unit name follows, NUL-terminated, padded to 4.
            // The root node has an empty name (immediate NUL), which is valid.
            let name_va = p;
            depth += 1;
            if depth > DEPTH_MAX {
                walk_fail!(b"depth_overflow");
            }
            let avail = struct_end - p;
            let mut nl = 0u64;
            let mut terminated = false;
            while nl < avail && nl < 256 {
                if unsafe { core::ptr::read_volatile((name_va + nl) as *const u8) } == 0 {
                    terminated = true;
                    break;
                }
                nl += 1;
            }
            if !terminated {
                walk_fail!(b"node_name_bounds");
            }
            if chosen_depth < 0
                && unsafe { hh5_name_eq(name_va, chosen_ref, RPI5_HH5_NAME_CHOSEN.len()) }
            {
                chosen_depth = depth;
                out.chosen_found = true;
                let _ = rpi5_hh_write_line(&RPI5_HH5_FDT_CHOSEN_FOUND_MARKER);
                let _ = rpi5_hh_write_line(&RPI5_HH5_FDT_INITRD_PROPS_BEGIN_MARKER);
            }
            let adv = ((nl + 1) + 3) & !3;
            if p + adv > struct_end {
                walk_fail!(b"node_name_bounds");
            }
            p += adv;
        } else if token == 0x2 {
            // FDT_END_NODE
            if depth <= 0 {
                walk_fail!(b"bad_token");
            }
            if chosen_depth == depth {
                chosen_depth = -1;
                let _ = rpi5_hh_write_line(&RPI5_HH5_FDT_INITRD_PROPS_DONE_MARKER);
            }
            depth -= 1;
        } else if token == 0x3 {
            // FDT_PROP: len(u32), nameoff(u32), value[len], padded to 4.
            if p + 8 > struct_end {
                walk_fail!(b"prop_bounds");
            }
            let prop_len = unsafe { hh5_be32(p) } as u64;
            let nameoff = unsafe { hh5_be32(p + 4) } as u64;
            let val_va = p + 8;
            let val_adv = (prop_len + 3) & !3;
            if val_va + val_adv > struct_end {
                walk_fail!(b"prop_bounds");
            }
            if nameoff >= size_strings {
                walk_fail!(b"string_bounds");
            }
            if chosen_depth >= 0 && depth == chosen_depth {
                let name_va = strings_base + nameoff;
                if unsafe { hh5_name_eq(name_va, start_ref, RPI5_HH5_NAME_INITRD_START.len()) } {
                    // Task D: accept 4- or 8-byte cells; flag any other width.
                    if prop_len == 4 {
                        out.initrd_start = unsafe { hh5_be32(val_va) } as u64;
                        have_start = true;
                    } else if prop_len == 8 {
                        out.initrd_start = unsafe { hh5_be64(val_va) };
                        have_start = true;
                    } else {
                        out.bad_cell_width = true;
                    }
                } else if unsafe { hh5_name_eq(name_va, end_ref, RPI5_HH5_NAME_INITRD_END.len()) } {
                    if prop_len == 4 {
                        out.initrd_end = unsafe { hh5_be32(val_va) } as u64;
                        have_end = true;
                    } else if prop_len == 8 {
                        out.initrd_end = unsafe { hh5_be64(val_va) };
                        have_end = true;
                    } else {
                        out.bad_cell_width = true;
                    }
                } else if unsafe {
                    hh5_name_eq(name_va, bootargs_ref, RPI5_HH5_NAME_BOOTARGS.len())
                } {
                    out.bootargs_va = val_va;
                    out.bootargs_len = prop_len;
                }
            }
            // Advance past the 8-byte property header AND the padded value.
            p = val_va + val_adv;
        } else if token == 0x4 {
            // FDT_NOP: no extra data.
        } else if token == 0x9 {
            // FDT_END
            ended = true;
            break;
        } else {
            walk_fail!(b"bad_token");
        }
    }

    if !ended {
        walk_fail!(b"token_bounds");
    }
    let _ = rpi5_hh_write_line(&RPI5_HH5_FDT_CHOSEN_SCAN_DONE_MARKER);
    if !out.chosen_found {
        walk_fail!(b"chosen_missing");
    }

    out.walk_ok = true;
    out.initrd_present = have_start && have_end && !out.bad_cell_width;
    out
}

#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
#[inline]
fn hh5_overlaps(a0: u64, a1: u64, b0: u64, b1: u64) -> bool {
    a0 < b1 && b0 < a1
}

/// High-half boot handoff descriptor. Lives at a high virtual address (the
/// TTBR1 heap alias) and carries only values that are safe to consume after the
/// HH-4 low-VA retirement: high virtual aliases plus the original physical
/// ranges for bookkeeping. No field is ever dereferenced through a low VA.
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
#[derive(Clone, Copy)]
#[repr(C)]
struct Rpi5HhBootHandoff {
    magic: u64,
    version: u64,
    dtb_phys: u64,
    dtb_virt: u64,
    dtb_size: u64,
    kernel_phys_start: u64,
    kernel_phys_end: u64,
    kernel_virt_start: u64,
    kernel_virt_end: u64,
    heap_phys_start: u64,
    heap_phys_end: u64,
    heap_virt_start: u64,
    heap_virt_end: u64,
    uart_phys: u64,
    uart_virt: u64,
    initrd_phys_start: u64,
    initrd_phys_end: u64,
    initrd_virt_start: u64,
    initrd_virt_end: u64,
    initrd_present: u64,
    empty_ttbr0_root: u64,
    ttbr1_root: u64,
    alloc_base_virt: u64,
    alloc_next_virt: u64,
    alloc_end_virt: u64,
    cmdline_virt: u64,
    cmdline_len: u64,
    max_cpus: u64,
}

#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
const RPI5_HH5_HANDOFF_MAGIC: u64 = 0x5250_4935_4848_3542; // "RPI5HH5B"

/// Compact high-half boot-info record produced by the HH5 kernel-entry shim
/// once the high-half physical-frame allocator is initialized. Lives at a high
/// virtual address in the HH heap; no field is ever dereferenced through a low
/// VA. This is the high-half equivalent of the normal kernel boot-info, built
/// without the normal kernel's global heap or full VM layout.
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
#[derive(Clone, Copy)]
#[repr(C)]
struct Rpi5HhKernelBootInfo {
    magic: u64,
    total_frames: u64,
    free_frames: u64,
    usable_start: u64,
    usable_end: u64,
    handoff_virt: u64,
    max_cpus: u64,
}

#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
const RPI5_HH_BOOTINFO_MAGIC: u64 = 0x5250_4935_4B42_4900; // "RPI5KBI\0"

/*
 * HH-5 high-half initrd / allocator bridge.
 *
 * Part A audit — the previous deferral reason
 * (`high_half_initrd_allocator_bridge_not_ready`) was accurate: the existing
 * normal-kernel bootstrap (the Stage2C builder and PhysicalFrameAllocator) owns
 * a LOW-physical frame allocator and dereferences the firmware DTB/initrd
 * through low identity pointers. After HH-4 retired the low TTBR0 root, low VAs
 * are unmapped, so calling that path would fault. HH-5 therefore builds a
 * high-alias-only bridge (DTB parse, initrd discovery, bump allocator, handoff
 * descriptor) entirely from TTBR1-mapped memory, then defers normal kernel
 * entry with a precise reason instead of violating the no-low-VA contract.
 *
 * Invariants: every memory access uses a high alias; no low VA is dereferenced;
 * no formatting/panic/assert; bounded UART output; no GIC/RP1/PCIe/task
 * scheduling/service-chain start; no user TTBR0 install; no EL0 ERET.
 */
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
fn rpi5_hh5_bridge(hh4: Rpi5Hh4Ready) -> ! {
    unsafe extern "C" {
        static __boot_low_start: u8;
        static __kernel_phys_start: u8;
        static __kernel_phys_end: u8;
        static __hh_heap_start: u8;
        static __hh_heap_end: u8;
    }

    if hh4.empty_ttbr0_root == 0 || hh4.dtb_virt < RPI5_HH_VA_OFFSET {
        let _ = rpi5_hh_write_bytes(&RPI5_HH5_FAILED_MARKER);
        let _ = rpi5_hh_write_line(b"hh4_not_ready");
        rpi5_hh_halt();
    }
    if !rpi5_hh_write_line(&RPI5_HH5_BEGIN_MARKER) {
        rpi5_hh_halt();
    }

    // Inline, bounded, raw-pointer high-UART output (proven HH path). Plain
    // line markers continue to use rpi5_hh_write_line.
    const HH5_TX_POLL_LIMIT: usize = 0x1_0000;
    let hh5_uart_data = RPI5_HH_UART_VIRT as *mut u32;
    let hh5_uart_flags = (RPI5_HH_UART_VIRT + 0x18) as *const u32;
    macro_rules! hh5_write_byte {
        ($byte:expr) => {{
            let mut poll = 0usize;
            while poll < HH5_TX_POLL_LIMIT {
                if unsafe { core::ptr::read_volatile(hh5_uart_flags) } & (1 << 5) == 0 {
                    break;
                }
                poll += 1;
            }
            if poll == HH5_TX_POLL_LIMIT {
                rpi5_hh_halt();
            }
            unsafe {
                core::ptr::write_volatile(hh5_uart_data, $byte as u32);
            }
        }};
    }
    macro_rules! hh5_emit_marker {
        ($marker:expr) => {{
            let mut idx = 0usize;
            let ptr = core::ptr::addr_of!($marker).cast::<u8>();
            while idx < $marker.len() {
                let byte = unsafe { core::ptr::read(ptr.add(idx)) };
                hh5_write_byte!(byte);
                idx += 1;
            }
        }};
    }
    macro_rules! hh5_emit_hex {
        ($value:expr) => {{
            let mut nib = 0usize;
            while nib < 16 {
                let shift = 60 - nib * 4;
                let nibble = (($value >> shift) & 0xf) as u8;
                let digit = if nibble < 10 {
                    b'0' + nibble
                } else {
                    b'a' + nibble - 10
                };
                hh5_write_byte!(digit);
                nib += 1;
            }
        }};
    }
    macro_rules! hh5_crlf {
        () => {{
            hh5_write_byte!(b'\r');
            hh5_write_byte!(b'\n');
        }};
    }
    macro_rules! hh5_hex_line {
        ($marker:expr, $value:expr) => {{
            hh5_emit_marker!($marker);
            hh5_emit_hex!($value);
            hh5_crlf!();
        }};
    }
    macro_rules! hh5_fault {
        ($reason:literal) => {{
            let _ = rpi5_hh_write_bytes(&RPI5_HH5_FAULT_BOUNDARY_MARKER);
            let _ = rpi5_hh_write_line($reason);
            rpi5_hh_halt();
        }};
    }
    macro_rules! hh5_defer {
        ($reason:literal) => {{
            let _ = rpi5_hh_write_bytes(&RPI5_HH5_DEFERRED_MARKER);
            let _ = rpi5_hh_write_line($reason);
            let _ = rpi5_hh_write_line(&RPI5_HH5_DONE_DEFERRED_MARKER);
            rpi5_hh_halt();
        }};
    }

    let dtb_phys = hh4.dtb_phys;
    let dtb_virt = hh4.dtb_virt;

    // Re-prove the DTB magic through the high alias before trusting any offset.
    let dtb_magic = unsafe { core::ptr::read_volatile(dtb_virt as *const u32) };
    if u32::from_be(dtb_magic) != 0xd00d_feed {
        hh5_fault!(b"dtb_magic");
    }
    let dtb_size = unsafe { hh5_be32(dtb_virt + 4) } as u64;
    if dtb_size < 0x40 || dtb_phys + dtb_size > 0x8000_0000 {
        hh5_fault!(b"dtb_size");
    }

    // Part B — parse /chosen for the initrd range and bootargs.
    if !rpi5_hh_write_line(&RPI5_HH5_DTB_CHOSEN_BEGIN_MARKER) {
        rpi5_hh_halt();
    }
    let chosen = unsafe { hh5_parse_chosen(dtb_virt) };
    if !chosen.walk_ok {
        hh5_fault!(b"initrd_dtb_walk");
    }
    if !rpi5_hh_write_line(&RPI5_HH5_DTB_CHOSEN_OK_MARKER) {
        rpi5_hh_halt();
    }

    if !rpi5_hh_write_line(&RPI5_HH5_INITRD_BEGIN_MARKER) {
        rpi5_hh_halt();
    }

    let image_start = core::ptr::addr_of!(__boot_low_start) as u64;
    let kernel_phys_start = core::ptr::addr_of!(__kernel_phys_start) as u64;
    let kernel_phys_end = core::ptr::addr_of!(__kernel_phys_end) as u64;
    let heap_phys_start = core::ptr::addr_of!(__hh_heap_start) as u64;
    let heap_phys_end = core::ptr::addr_of!(__hh_heap_end) as u64;

    let mut initrd_present = false;
    let mut initrd_phys_start = 0u64;
    let mut initrd_phys_end = 0u64;
    let mut initrd_virt_start = 0u64;
    let mut initrd_virt_end = 0u64;

    if chosen.bad_cell_width {
        // /chosen had an initrd property with an unsupported cell width.
        let _ = rpi5_hh_write_bytes(&RPI5_HH5_INITRD_FAILED_MARKER);
        let _ = rpi5_hh_write_line(b"bad_cell_width");
    } else if !chosen.initrd_present {
        let _ = rpi5_hh_write_bytes(&RPI5_HH5_INITRD_FAILED_MARKER);
        let _ = rpi5_hh_write_line(b"missing");
    } else {
        let s = chosen.initrd_start;
        let e = chosen.initrd_end;
        // `linux,initrd-end` is exclusive (first byte past the image).
        if s >= e {
            let _ = rpi5_hh_write_bytes(&RPI5_HH5_INITRD_FAILED_MARKER);
            let _ = rpi5_hh_write_line(b"bad_range");
        } else if e > 0x8000_0000 {
            // Outside the TTBR1 high window, so no safe high alias exists.
            let _ = rpi5_hh_write_bytes(&RPI5_HH5_INITRD_FAILED_MARKER);
            let _ = rpi5_hh_write_line(b"not_mapped");
        } else if hh5_overlaps(s, e, image_start, kernel_phys_end) {
            let _ = rpi5_hh_write_bytes(&RPI5_HH5_INITRD_FAILED_MARKER);
            let _ = rpi5_hh_write_line(b"overlap_kernel");
        } else if hh5_overlaps(s, e, dtb_phys, dtb_phys + dtb_size) {
            let _ = rpi5_hh_write_bytes(&RPI5_HH5_INITRD_FAILED_MARKER);
            let _ = rpi5_hh_write_line(b"overlap_dtb");
        } else {
            initrd_present = true;
            initrd_phys_start = s;
            initrd_phys_end = e;
            initrd_virt_start = s + RPI5_HH_VA_OFFSET;
            initrd_virt_end = e + RPI5_HH_VA_OFFSET;
            hh5_emit_marker!(RPI5_HH5_INITRD_RANGE_MARKER);
            hh5_emit_hex!(initrd_phys_start);
            hh5_emit_marker!(RPI5_HH5_INITRD_PHYS_END_SEP_MARKER);
            hh5_emit_hex!(initrd_phys_end);
            hh5_crlf!();
            hh5_emit_marker!(RPI5_HH5_INITRD_VIRT_MARKER);
            hh5_emit_hex!(initrd_virt_start);
            hh5_emit_marker!(RPI5_HH5_INITRD_VIRT_END_SEP_MARKER);
            hh5_emit_hex!(initrd_virt_end);
            hh5_crlf!();
            if !rpi5_hh_write_line(&RPI5_HH5_INITRD_OK_MARKER) {
                rpi5_hh_halt();
            }
        }
    }

    // Part C — high-half boot allocator bridge over the already-mapped heap.
    if !rpi5_hh_write_line(&RPI5_HH5_ALLOC_BRIDGE_BEGIN_MARKER) {
        rpi5_hh_halt();
    }
    if heap_phys_end <= heap_phys_start || heap_phys_end > 0x8000_0000 {
        let _ = rpi5_hh_write_bytes(&RPI5_HH5_ALLOC_BRIDGE_FAILED_MARKER);
        let _ = rpi5_hh_write_line(b"heap_range");
        hh5_fault!(b"alloc_bridge_heap_range");
    }
    let heap_virt_start = heap_phys_start + RPI5_HH_VA_OFFSET;
    let heap_virt_end = heap_phys_end + RPI5_HH_VA_OFFSET;
    let heap_size = heap_phys_end - heap_phys_start;
    hh5_emit_marker!(RPI5_HH5_ALLOC_BRIDGE_RANGE_MARKER);
    hh5_emit_hex!(heap_phys_start);
    hh5_emit_marker!(RPI5_HH5_ALLOC_BRIDGE_VIRT_SEP_MARKER);
    hh5_emit_hex!(heap_virt_start);
    hh5_emit_marker!(RPI5_HH5_ALLOC_BRIDGE_SIZE_SEP_MARKER);
    hh5_emit_hex!(heap_size);
    hh5_crlf!();

    // Part D — build the handoff descriptor at the start of the high heap, then
    // reserve the rest of the heap as a bump region the future kernel bridge can
    // consume without touching any low VA.
    let descriptor_size = core::mem::size_of::<Rpi5HhBootHandoff>() as u64;
    let alloc_base_virt = (heap_virt_start + descriptor_size + 63) & !63;
    if alloc_base_virt >= heap_virt_end {
        let _ = rpi5_hh_write_bytes(&RPI5_HH5_ALLOC_BRIDGE_FAILED_MARKER);
        let _ = rpi5_hh_write_line(b"descriptor_too_large");
        hh5_fault!(b"alloc_bridge_descriptor");
    }
    if !rpi5_hh_write_line(&RPI5_HH5_ALLOC_BRIDGE_OK_MARKER) {
        rpi5_hh_halt();
    }

    if !rpi5_hh_write_line(&RPI5_HH5_HANDOFF_BEGIN_MARKER) {
        rpi5_hh_halt();
    }
    let handoff = Rpi5HhBootHandoff {
        magic: RPI5_HH5_HANDOFF_MAGIC,
        version: 1,
        dtb_phys,
        dtb_virt,
        dtb_size,
        kernel_phys_start,
        kernel_phys_end,
        kernel_virt_start: kernel_phys_start + RPI5_HH_VA_OFFSET,
        kernel_virt_end: kernel_phys_end + RPI5_HH_VA_OFFSET,
        heap_phys_start,
        heap_phys_end,
        heap_virt_start,
        heap_virt_end,
        uart_phys: 0x10_7d00_1000,
        uart_virt: RPI5_HH_UART_VIRT as u64,
        initrd_phys_start,
        initrd_phys_end,
        initrd_virt_start,
        initrd_virt_end,
        initrd_present: initrd_present as u64,
        empty_ttbr0_root: hh4.empty_ttbr0_root,
        ttbr1_root: hh4.ttbr1_root,
        alloc_base_virt,
        alloc_next_virt: alloc_base_virt,
        alloc_end_virt: heap_virt_end,
        cmdline_virt: if chosen.bootargs_va != 0 {
            chosen.bootargs_va
        } else {
            0
        },
        cmdline_len: chosen.bootargs_len,
        max_cpus: 1,
    };
    let handoff_ptr = heap_virt_start as *mut Rpi5HhBootHandoff;
    unsafe {
        core::ptr::write_volatile(handoff_ptr, handoff);
    }
    // Prove the descriptor is readable back through its high alias.
    let magic_rb = unsafe { core::ptr::read_volatile(heap_virt_start as *const u64) };
    if magic_rb != RPI5_HH5_HANDOFF_MAGIC {
        hh5_fault!(b"handoff_readback");
    }
    hh5_hex_line!(RPI5_HH5_HANDOFF_OK_MARKER, heap_virt_start);

    // Without an initrd there is nothing to boot from; defer as before.
    if !initrd_present {
        hh5_defer!(b"initrd_missing");
    }

    /*
     * Task A — normal-boot audit.
     *
     * The previous low-allocator deferral was too pessimistic:
     * `PhysicalFrameAllocator` is fully self-contained (all bookkeeping lives
     * inside the struct and `alloc_frame` returns physical addresses as plain
     * numbers), so it needs no low direct map. It only needs its metadata placed
     * at a mapped address. We can therefore place it in the TTBR1-mapped HH heap
     * (high alias) and bring up a real boot allocator without ever touching a
     * low VA. Re-affirm the high-alias contract first.
     */
    if !rpi5_hh_write_line(&RPI5_HH5_NORMAL_BOOT_AUDIT_BEGIN_MARKER) {
        rpi5_hh_halt();
    }
    if dtb_virt != dtb_phys + RPI5_HH_VA_OFFSET {
        hh5_fault!(b"audit_dtb_alias");
    }
    if initrd_virt_start != initrd_phys_start + RPI5_HH_VA_OFFSET
        || initrd_virt_end != initrd_phys_end + RPI5_HH_VA_OFFSET
    {
        hh5_fault!(b"audit_initrd_alias");
    }
    if heap_virt_start != heap_phys_start + RPI5_HH_VA_OFFSET {
        hh5_fault!(b"audit_heap_alias");
    }
    if kernel_phys_end <= kernel_phys_start {
        hh5_fault!(b"audit_kernel_range");
    }
    if !rpi5_hh_write_line(&RPI5_HH5_NORMAL_BOOT_AUDIT_DONE_MARKER) {
        rpi5_hh_halt();
    }

    // Task B — high-half-safe boot input adapter: re-read the handoff through
    // its high alias and prove it before any allocator work consumes it.
    let bi_magic = unsafe { core::ptr::read_volatile(heap_virt_start as *const u64) };
    if bi_magic != RPI5_HH5_HANDOFF_MAGIC {
        hh5_fault!(b"boot_input_readback");
    }
    hh5_hex_line!(RPI5_HH5_BOOT_INPUT_OK_MARKER, heap_virt_start);

    // Task C — allocator adapter. `PhysicalFrameAllocator` is ~209 KB, so it must
    // NEVER be materialized as a by-value local (that pushed a huge temporary
    // onto the stack and stalled the bring-up). It is initialized DIRECTLY in
    // place at a high virtual address in the HH heap: zero the destination with a
    // bounded volatile loop (yielding a valid all-zero == new_uninit value), then
    // run the lean in-place single-region init through `&mut`. No full allocator
    // value ever crosses the stack. Each substep is bracketed by a marker.
    if !rpi5_hh_write_line(&RPI5_HH5_ALLOC_ADAPTER_BEGIN_MARKER) {
        rpi5_hh_halt();
    }
    macro_rules! alloc_fail {
        ($reason:literal) => {{
            let _ = rpi5_hh_write_bytes(&RPI5_HH5_ALLOC_ADAPTER_FAILED_MARKER);
            let _ = rpi5_hh_write_line($reason);
            let _ = rpi5_hh_write_bytes(&RPI5_HH5_FAULT_BOUNDARY_MARKER);
            let _ = rpi5_hh_write_line(b"alloc_adapter");
            rpi5_hh_halt()
        }};
    }

    use crate::kernel::frame_allocator::PhysicalFrameAllocator;

    // --- Layout: compute the usable window and the in-heap storage address. ---
    if !rpi5_hh_write_line(&RPI5_HH5_ALLOC_ADAPTER_LAYOUT_BEGIN_MARKER) {
        rpi5_hh_halt();
    }
    // Reserve everything the firmware and boot occupy: the whole YARM image
    // (boot + page tables + HH heap + stack + kernel), the DTB, and the initrd.
    let dtb_end = dtb_phys + dtb_size;
    let mut reserved_top = kernel_phys_end;
    if dtb_end > reserved_top {
        reserved_top = dtb_end;
    }
    if initrd_phys_end > reserved_top {
        reserved_top = initrd_phys_end;
    }
    // 2 MiB-align up past all artifacts; bound the bring-up window and the TTBR1
    // high-map limit (2 GiB). This is a TEMPORARY bring-up allocator window, not
    // the final DTB /memory map. It is kept at 16 MiB (4096 pages) so the page
    // count stays well under the allocator's per-frame tracking capacity
    // (MAX_TRACKED_FRAME_REFS, 8192 == 32 MiB); see Task C / the capacity guard.
    let usable_start = (reserved_top + 0x1f_ffff) & !0x1f_ffffu64;
    const HH_PMEM_WINDOW: u64 = 0x0100_0000;
    let mut usable_end = usable_start + HH_PMEM_WINDOW;
    if usable_end > 0x8000_0000 {
        usable_end = 0x8000_0000;
    }
    if usable_start >= 0x8000_0000 || usable_end <= usable_start + 0x1000 {
        alloc_fail!(b"layout");
    }
    let alloc_align = core::mem::align_of::<PhysicalFrameAllocator>() as u64;
    let alloc_size = core::mem::size_of::<PhysicalFrameAllocator>() as u64;
    let alloc_meta_virt = (alloc_base_virt + (alloc_align - 1)) & !(alloc_align - 1);
    let alloc_meta_end = alloc_meta_virt + alloc_size;
    if alloc_meta_end > heap_virt_end || alloc_meta_end < alloc_meta_virt {
        alloc_fail!(b"layout");
    }
    if !rpi5_hh_write_line(&RPI5_HH5_ALLOC_ADAPTER_LAYOUT_OK_MARKER) {
        rpi5_hh_halt();
    }

    // --- Storage: prove the destination before touching it. ---
    if !rpi5_hh_write_line(&RPI5_HH5_ALLOC_ADAPTER_STORAGE_BEGIN_MARKER) {
        rpi5_hh_halt();
    }
    let handoff_end = heap_virt_start + core::mem::size_of::<Rpi5HhBootHandoff>() as u64;
    if alloc_meta_virt < RPI5_HH_VA_OFFSET
        || alloc_meta_virt < heap_virt_start
        || alloc_meta_end > heap_virt_end
        || alloc_meta_virt < handoff_end
        || (alloc_meta_virt & (alloc_align - 1)) != 0
    {
        alloc_fail!(b"storage");
    }
    hh5_hex_line!(RPI5_HH5_ALLOC_ADAPTER_STORAGE_OK_MARKER, alloc_meta_virt);

    // --- Zero the storage in place (bounded volatile loop, no slices/memset). ---
    if !rpi5_hh_write_line(&RPI5_HH5_ALLOC_ADAPTER_ZERO_BEGIN_MARKER) {
        rpi5_hh_halt();
    }
    // alloc_size is a multiple of 8 (struct is u64-aligned); round up defensively.
    let zero_words = (alloc_size + 7) / 8;
    let mut zi = 0u64;
    while zi < zero_words {
        unsafe {
            core::ptr::write_volatile((alloc_meta_virt + zi * 8) as *mut u64, 0);
        }
        zi += 1;
    }
    if !rpi5_hh_write_line(&RPI5_HH5_ALLOC_ADAPTER_ZERO_DONE_MARKER) {
        rpi5_hh_halt();
    }

    // --- Init in place. The zeroed storage is a valid (all-None/Empty) value,
    // so `init_single_region_assume_zeroed` only writes scalar fields + one free
    // extent: it NEVER re-materializes the ~196 KiB `frame_refs` / `extents`
    // arrays (the big memset/memcpy that stalled the previous full memory-map
    // init). It is non-panicking and bounded. ---
    if !rpi5_hh_write_line(&RPI5_HH5_ALLOC_ADAPTER_INIT_BEGIN_MARKER) {
        rpi5_hh_halt();
    }
    macro_rules! alloc_init_fail {
        ($reason:literal) => {{
            let _ = rpi5_hh_write_bytes(&RPI5_HH5_ALLOC_ADAPTER_FAILED_MARKER);
            let _ = rpi5_hh_write_line($reason);
            let _ = rpi5_hh_write_bytes(&RPI5_HH5_FAULT_BOUNDARY_MARKER);
            let _ = rpi5_hh_write_line(b"alloc_adapter_init");
            rpi5_hh_halt()
        }};
    }

    // Range + capacity: the bring-up window must fit the per-frame tracking
    // capacity so no later sweep can exceed MAX_TRACKED_FRAME_REFS.
    if !rpi5_hh_write_line(&RPI5_HH5_ALLOC_ADAPTER_INIT_RANGE_BEGIN_MARKER) {
        rpi5_hh_halt();
    }
    let usable_pages = (usable_end - usable_start) >> 12;
    let frame_capacity = PhysicalFrameAllocator::tracked_frame_capacity() as u64;
    if usable_pages == 0 {
        alloc_init_fail!(b"init_range_capacity");
    }
    hh5_hex_line!(RPI5_HH5_ALLOC_ADAPTER_INIT_RANGE_OK_MARKER, usable_pages);
    if usable_pages > frame_capacity {
        alloc_init_fail!(b"init_range_capacity");
    }
    hh5_hex_line!(
        RPI5_HH5_ALLOC_ADAPTER_INIT_CAPACITY_OK_MARKER,
        frame_capacity
    );

    let alloc_ptr = alloc_meta_virt as *mut PhysicalFrameAllocator;
    let allocator = unsafe { &mut *alloc_ptr };
    if !rpi5_hh_write_line(&RPI5_HH5_ALLOC_ADAPTER_INIT_CALL_BEGIN_MARKER) {
        rpi5_hh_halt();
    }
    if allocator
        .init_single_region_assume_zeroed(usable_start, usable_end - usable_start)
        .is_err()
    {
        alloc_init_fail!(b"init_returned_error");
    }
    if !rpi5_hh_write_line(&RPI5_HH5_ALLOC_ADAPTER_INIT_CALL_DONE_MARKER) {
        rpi5_hh_halt();
    }
    if !rpi5_hh_write_line(&RPI5_HH5_ALLOC_ADAPTER_INIT_DONE_MARKER) {
        rpi5_hh_halt();
    }

    // --- Probe: one alloc/free via the lean BOOT-PROBE path. The generic
    // alloc/free methods are unsafe here: they take global SpinLockIrqs, use
    // normal-kernel logging and abort paths on a reserved-overlap guard, and the
    // free path clones the whole ~209 KiB allocator into a lock-guarded scratch
    // (pfa_clone_to). The boot probe uses bounded loops only: no lock, no
    // logging, no abort, no large copy, and it never dereferences the physical
    // frame memory. Each substep is marked. ---
    if !rpi5_hh_write_line(&RPI5_HH5_ALLOC_ADAPTER_PROBE_ALLOC_BEGIN_MARKER) {
        rpi5_hh_halt();
    }
    macro_rules! probe_fail {
        ($reason:literal) => {{
            let _ = rpi5_hh_write_bytes(&RPI5_HH5_ALLOC_ADAPTER_FAILED_MARKER);
            let _ = rpi5_hh_write_line($reason);
            let _ = rpi5_hh_write_bytes(&RPI5_HH5_FAULT_BOUNDARY_MARKER);
            let _ = rpi5_hh_write_line(b"alloc_adapter_probe");
            rpi5_hh_halt()
        }};
    }

    let free_before = allocator.free_frames();
    if !rpi5_hh_write_line(&RPI5_HH5_ALLOC_ADAPTER_PROBE_ALLOC_CALL_BEGIN_MARKER) {
        rpi5_hh_halt();
    }
    let test_frame = match allocator.alloc_frame_boot_probe() {
        Ok(frame) => frame,
        Err(_) => {
            probe_fail!(b"probe_alloc_returned_error");
            0
        }
    };
    if !rpi5_hh_write_line(&RPI5_HH5_ALLOC_ADAPTER_PROBE_ALLOC_CALL_DONE_MARKER) {
        rpi5_hh_halt();
    }

    // Validate the frame is page-aligned and strictly inside the bring-up window.
    if !rpi5_hh_write_line(&RPI5_HH5_ALLOC_ADAPTER_PROBE_ALLOC_VALIDATE_BEGIN_MARKER) {
        rpi5_hh_halt();
    }
    if (test_frame & 0xfff) != 0 || test_frame < usable_start || test_frame >= usable_end {
        probe_fail!(b"probe_alloc_out_of_range");
    }
    if !rpi5_hh_write_line(&RPI5_HH5_ALLOC_ADAPTER_PROBE_ALLOC_VALIDATE_OK_MARKER) {
        rpi5_hh_halt();
    }
    hh5_hex_line!(RPI5_HH5_ALLOC_ADAPTER_PROBE_ALLOC_OK_MARKER, test_frame);

    if !rpi5_hh_write_line(&RPI5_HH5_ALLOC_ADAPTER_PROBE_FREE_BEGIN_MARKER) {
        rpi5_hh_halt();
    }
    if !rpi5_hh_write_line(&RPI5_HH5_ALLOC_ADAPTER_PROBE_FREE_CALL_BEGIN_MARKER) {
        rpi5_hh_halt();
    }
    if allocator.free_frame_boot_probe(test_frame).is_err() {
        probe_fail!(b"probe_free_returned_error");
    }
    if !rpi5_hh_write_line(&RPI5_HH5_ALLOC_ADAPTER_PROBE_FREE_CALL_DONE_MARKER) {
        rpi5_hh_halt();
    }
    if allocator.free_frames() != free_before {
        probe_fail!(b"probe_free_returned_error");
    }
    if !rpi5_hh_write_line(&RPI5_HH5_ALLOC_ADAPTER_PROBE_FREE_OK_MARKER) {
        rpi5_hh_halt();
    }

    hh5_emit_marker!(RPI5_HH5_ALLOC_ADAPTER_RANGE_MARKER);
    hh5_emit_hex!(usable_start);
    hh5_emit_marker!(RPI5_HH5_ALLOC_ADAPTER_USABLE_END_SEP_MARKER);
    hh5_emit_hex!(usable_end);
    hh5_crlf!();
    if !rpi5_hh_write_line(&RPI5_HH5_ALLOC_ADAPTER_OK_MARKER) {
        rpi5_hh_halt();
    }

    // Task D — high-half kernel-entry shim. Performs the boot steps that are
    // safe without the normal kernel's global heap / full VM layout, each gated
    // by a marker, then defers at the precise remaining blocker.
    if !rpi5_hh_write_line(&RPI5_HH5_ENTER_KERNEL_BEGIN_MARKER)
        || !rpi5_hh_write_line(&RPI5_KERNEL_ENTRY_BEGIN_MARKER)
    {
        rpi5_hh_halt();
    }

    // DTB parse: re-affirm the FDT magic through the high alias (the /chosen walk
    // already succeeded above).
    if !rpi5_hh_write_line(&RPI5_KERNEL_DTB_PARSE_BEGIN_MARKER) {
        rpi5_hh_halt();
    }
    let reparse_magic = unsafe { core::ptr::read_volatile(dtb_virt as *const u32) };
    if u32::from_be(reparse_magic) != 0xd00d_feed || !chosen.walk_ok {
        hh5_fault!(b"dtb_reparse");
    }
    if !rpi5_hh_write_line(&RPI5_KERNEL_DTB_PARSE_OK_MARKER) {
        rpi5_hh_halt();
    }

    if !rpi5_hh_write_line(&RPI5_KERNEL_INITRD_OK_MARKER) {
        rpi5_hh_halt();
    }

    // PMEM: the high-half allocator created above is the boot physical-memory
    // allocator. Report its free-frame count as proof of a working pmem.
    if !rpi5_hh_write_line(&RPI5_KERNEL_PMEM_BEGIN_MARKER) {
        rpi5_hh_halt();
    }
    let free_frames = allocator.free_frames() as u64;
    if free_frames == 0 {
        hh5_fault!(b"pmem_empty");
    }
    hh5_hex_line!(RPI5_KERNEL_PMEM_OK_MARKER, free_frames);

    // Boot info: build a compact high-half boot-info record after the allocator
    // metadata, then read its magic back to prove it.
    let bootinfo_align = core::mem::align_of::<Rpi5HhKernelBootInfo>() as u64;
    let bootinfo_size = core::mem::size_of::<Rpi5HhKernelBootInfo>() as u64;
    let bootinfo_virt =
        (alloc_meta_virt + alloc_size + (bootinfo_align - 1)) & !(bootinfo_align - 1);
    if bootinfo_virt + bootinfo_size > heap_virt_end {
        hh5_fault!(b"bootinfo_fit");
    }
    let bootinfo = Rpi5HhKernelBootInfo {
        magic: RPI5_HH_BOOTINFO_MAGIC,
        total_frames: allocator.total_frames() as u64,
        free_frames,
        usable_start,
        usable_end,
        handoff_virt: heap_virt_start,
        max_cpus: 1,
    };
    let bootinfo_ptr = bootinfo_virt as *mut Rpi5HhKernelBootInfo;
    unsafe {
        core::ptr::write_volatile(bootinfo_ptr, bootinfo);
    }
    let bootinfo_rb = unsafe { core::ptr::read_volatile(bootinfo_virt as *const u64) };
    if bootinfo_rb != RPI5_HH_BOOTINFO_MAGIC {
        hh5_fault!(b"bootinfo_readback");
    }
    if !rpi5_hh_write_line(&RPI5_KERNEL_BOOTINFO_OK_MARKER) {
        rpi5_hh_halt();
    }

    // ===================== BOOT-4 global heap + kernel VM =====================
    macro_rules! boot4_fail {
        ($failed_marker:expr, $reason:literal, $boundary_reason:literal) => {{
            let _ = rpi5_hh_write_bytes(&$failed_marker);
            let _ = rpi5_hh_write_line($reason);
            let _ = rpi5_hh_write_bytes(&RPI5_BOOT4_FAULT_BOUNDARY_MARKER);
            let _ = rpi5_hh_write_line($boundary_reason);
            rpi5_hh_halt()
        }};
    }

    /*
     * Task A — global-heap / KernelState audit.
     *
     * Finding: the AArch64 global allocator (`KernelGlobalAllocator`) reaches
     * every backing frame through the low identity direct map — its
     * `phys_to_ptr` returns the physical address itself as the kernel pointer,
     * and it draws frames from `PT_FRAME_ALLOCATOR` describing low RAM. HH4
     * retired the low TTBR0 identity map, so any allocation through it would
     * touch an unmapped low VA. Constructing the normal `KernelState` allocates
     * through that global allocator, so it cannot run here yet. BOOT-4 therefore
     * builds and proves a high-alias-only kernel heap REGION and re-validates the
     * high-half VM, then defers precisely before any allocation. The future
     * milestone is an arch-owned high-half phys<->virt direct map so the global
     * allocator can be backed by this region.
     */
    if !rpi5_hh_write_line(&RPI5_BOOT4_GLOBAL_HEAP_AUDIT_BEGIN_MARKER)
        || !rpi5_hh_write_line(&RPI5_BOOT4_GLOBAL_HEAP_AUDIT_DONE_MARKER)
    {
        rpi5_hh_halt();
    }

    // Task B — high-half-safe kernel heap region, carved from the top of the
    // already-validated 16 MiB bring-up window (all of which is free RAM above
    // every firmware/boot artifact and inside the TTBR1 high map). Accessed only
    // through its high alias; no low VA, no large local, no normal allocator.
    if !rpi5_hh_write_line(&RPI5_KERNEL_GLOBAL_HEAP_BEGIN_MARKER) {
        rpi5_hh_halt();
    }
    const KERNEL_HEAP_SIZE: u64 = 0x0040_0000; // 4 MiB
    if usable_end - usable_start < KERNEL_HEAP_SIZE {
        boot4_fail!(
            RPI5_KERNEL_GLOBAL_HEAP_FAILED_MARKER,
            b"window_too_small",
            b"global_heap"
        );
    }
    let kheap_phys_start = (usable_end - KERNEL_HEAP_SIZE) & !0xfffu64;
    let kheap_phys_end = kheap_phys_start + KERNEL_HEAP_SIZE;
    // Stay inside the validated window and the TTBR1 high map; do not collide
    // with the boot-probe front of the window.
    if kheap_phys_start < usable_start
        || kheap_phys_end > usable_end
        || kheap_phys_end > 0x8000_0000
    {
        boot4_fail!(
            RPI5_KERNEL_GLOBAL_HEAP_FAILED_MARKER,
            b"out_of_window",
            b"global_heap"
        );
    }
    // The window already sits above the kernel image, DTB, initrd, HH heap, and
    // page tables, but re-prove no overlap with any of them defensively.
    if hh5_overlaps(
        kheap_phys_start,
        kheap_phys_end,
        image_start,
        kernel_phys_end,
    ) || hh5_overlaps(
        kheap_phys_start,
        kheap_phys_end,
        dtb_phys,
        dtb_phys + dtb_size,
    ) || hh5_overlaps(
        kheap_phys_start,
        kheap_phys_end,
        initrd_phys_start,
        initrd_phys_end,
    ) || hh5_overlaps(
        kheap_phys_start,
        kheap_phys_end,
        heap_phys_start,
        heap_phys_end,
    ) {
        boot4_fail!(
            RPI5_KERNEL_GLOBAL_HEAP_FAILED_MARKER,
            b"overlap",
            b"global_heap"
        );
    }
    let kheap_virt_start = kheap_phys_start + RPI5_HH_VA_OFFSET;
    // Prove the region is mapped and writable end to end without a large loop:
    // write+read-back a sentinel at the first and last 8 bytes.
    const KHEAP_SENTINEL_A: u64 = 0x5250_4935_4B48_5031; // "RPI5KHP1"
    const KHEAP_SENTINEL_B: u64 = 0x5250_4935_4B48_5032; // "RPI5KHP2"
    unsafe {
        core::ptr::write_volatile(kheap_virt_start as *mut u64, KHEAP_SENTINEL_A);
        core::ptr::write_volatile(
            (kheap_virt_start + KERNEL_HEAP_SIZE - 8) as *mut u64,
            KHEAP_SENTINEL_B,
        );
    }
    let kheap_rb_a = unsafe { core::ptr::read_volatile(kheap_virt_start as *const u64) };
    let kheap_rb_b = unsafe {
        core::ptr::read_volatile((kheap_virt_start + KERNEL_HEAP_SIZE - 8) as *const u64)
    };
    if kheap_rb_a != KHEAP_SENTINEL_A || kheap_rb_b != KHEAP_SENTINEL_B {
        boot4_fail!(
            RPI5_KERNEL_GLOBAL_HEAP_FAILED_MARKER,
            b"readback",
            b"global_heap"
        );
    }
    hh5_emit_marker!(RPI5_KERNEL_GLOBAL_HEAP_RANGE_MARKER);
    hh5_emit_hex!(kheap_virt_start);
    hh5_emit_marker!(RPI5_KERNEL_GLOBAL_HEAP_SIZE_SEP_MARKER);
    hh5_emit_hex!(KERNEL_HEAP_SIZE);
    hh5_crlf!();
    if !rpi5_hh_write_line(&RPI5_KERNEL_GLOBAL_HEAP_OK_MARKER) {
        rpi5_hh_halt();
    }

    // Task C — validate the high-half VM layout actually present: every region
    // the kernel needs to reach lives inside the TTBR1 0..2 GiB high map and its
    // virtual alias is phys + HH_VA_OFFSET.
    if !rpi5_hh_write_line(&RPI5_KERNEL_VM_BEGIN_MARKER) {
        rpi5_hh_halt();
    }
    if kernel_phys_end > 0x8000_0000 || kernel_phys_end <= kernel_phys_start {
        boot4_fail!(RPI5_KERNEL_VM_FAILED_MARKER, b"kernel_range", b"kernel_vm");
    }
    if dtb_phys + dtb_size > 0x8000_0000 {
        boot4_fail!(RPI5_KERNEL_VM_FAILED_MARKER, b"dtb_range", b"kernel_vm");
    }
    if initrd_phys_end > 0x8000_0000 {
        boot4_fail!(RPI5_KERNEL_VM_FAILED_MARKER, b"initrd_range", b"kernel_vm");
    }
    if heap_phys_end > 0x8000_0000 || kheap_phys_end > 0x8000_0000 {
        boot4_fail!(RPI5_KERNEL_VM_FAILED_MARKER, b"heap_range", b"kernel_vm");
    }
    // The UART high alias is already proven in use by every marker above.
    if RPI5_HH_UART_VIRT as u64 != 0x10_7d00_1000 + RPI5_HH_VA_OFFSET {
        boot4_fail!(RPI5_KERNEL_VM_FAILED_MARKER, b"uart_alias", b"kernel_vm");
    }
    if !rpi5_hh_write_line(&RPI5_KERNEL_VM_LAYOUT_OK_MARKER)
        || !rpi5_hh_write_line(&RPI5_KERNEL_VM_OK_MARKER)
    {
        rpi5_hh_halt();
    }

    // ============ BOOT-4 phys<->virt direct-map bridge for the allocator ======
    macro_rules! galloc_fail {
        ($reason:literal) => {{
            let _ = rpi5_hh_write_bytes(&RPI5_KERNEL_GLOBAL_ALLOCATOR_FAILED_MARKER);
            let _ = rpi5_hh_write_line($reason);
            let _ = rpi5_hh_write_bytes(&RPI5_BOOT4_FAULT_BOUNDARY_MARKER);
            let _ = rpi5_hh_write_line(b"global_allocator");
            rpi5_hh_halt()
        }};
    }

    /*
     * Task A — phys<->virt direct-map audit.
     *
     * The kernel global allocator draws frames from PT_FRAME_ALLOCATOR and maps
     * each to a pointer via `phys_to_ptr`, which on default AArch64/QEMU is the
     * low identity map (`phys`). HH4 retired that map. The gated
     * `rpi5-highhalf` build adds a runtime HIGHMAP_OFFSET to `phys_to_ptr` /
     * `ptr_to_phys` (0 == identity by default, so QEMU/default behavior is
     * unchanged); BOOT-4 sets it to HH_VA_OFFSET only here, after HH4 + the VM
     * validation above. The PT allocator is then seeded leanly over the
     * high-half kernel heap region so the global allocator's frames live inside
     * the TTBR1 high map.
     */
    if !rpi5_hh_write_line(&RPI5_BOOT4_PHYSMAP_AUDIT_BEGIN_MARKER)
        || !rpi5_hh_write_line(&RPI5_BOOT4_PHYSMAP_AUDIT_DONE_MARKER)
    {
        rpi5_hh_halt();
    }

    // Task C — wire the global allocator to the BOOT-4 high-half heap region.
    if !rpi5_hh_write_line(&RPI5_KERNEL_GLOBAL_ALLOCATOR_BEGIN_MARKER) {
        rpi5_hh_halt();
    }
    // Seed PT_FRAME_ALLOCATOR (the global allocator's frame source) leanly over
    // the kernel heap region's PHYSICAL range; this also makes the normal
    // `ensure_pt_allocator_initialized` (which logs and re-materializes the big
    // arrays) a no-op. The HH5 boot-probe allocator that covers the full window
    // is dormant after its probe, so there is no live double-ownership.
    if crate::kernel::frame_allocator::rpi5_hh_init_pt_allocator_single_region(
        kheap_phys_start,
        KERNEL_HEAP_SIZE,
    )
    .is_err()
    {
        galloc_fail!(b"pt_allocator_init");
    }
    hh5_emit_marker!(RPI5_KERNEL_GLOBAL_ALLOCATOR_HEAP_RANGE_MARKER);
    hh5_emit_hex!(kheap_phys_start);
    hh5_emit_marker!(RPI5_HH5_ALLOC_BRIDGE_VIRT_SEP_MARKER);
    hh5_emit_hex!(kheap_virt_start);
    hh5_emit_marker!(RPI5_HH5_ALLOC_BRIDGE_SIZE_SEP_MARKER);
    hh5_emit_hex!(KERNEL_HEAP_SIZE);
    hh5_crlf!();

    // Switch the gated direct-map offset to the high alias.
    if !rpi5_hh_write_line(&RPI5_KERNEL_PHYSMAP_SWITCH_BEGIN_MARKER) {
        rpi5_hh_halt();
    }
    crate::kernel::global_allocator::set_highmap_offset(RPI5_HH_VA_OFFSET);
    let installed_offset = crate::kernel::global_allocator::highmap_offset();
    if installed_offset != RPI5_HH_VA_OFFSET {
        galloc_fail!(b"physmap_switch");
    }
    hh5_hex_line!(RPI5_KERNEL_PHYSMAP_SWITCH_OK_MARKER, installed_offset);
    if !rpi5_hh_write_line(&RPI5_KERNEL_GLOBAL_ALLOCATOR_PHYSMAP_OK_MARKER) {
        rpi5_hh_halt();
    }

    // Probe: allocate one small object through the real kernel global allocator
    // (the path Box/Vec use). Prove the returned pointer is a high-half alias
    // inside the kernel heap region, write/read a sentinel, then deliberately
    // leak it — the allocator free path clones the ~209 KiB allocator under a
    // lock and is intentionally not exercised at this bring-up stage.
    if !rpi5_hh_write_line(&RPI5_KERNEL_GLOBAL_ALLOCATOR_PROBE_BEGIN_MARKER) {
        rpi5_hh_halt();
    }
    let probe_layout = match core::alloc::Layout::from_size_align(64, 16) {
        Ok(layout) => layout,
        Err(_) => {
            galloc_fail!(b"probe_layout");
            #[allow(unreachable_code)]
            core::alloc::Layout::new::<u64>()
        }
    };
    let probe_ptr = unsafe {
        core::alloc::GlobalAlloc::alloc(
            &crate::kernel::global_allocator::KERNEL_GLOBAL_ALLOCATOR,
            probe_layout,
        )
    };
    if probe_ptr.is_null() {
        galloc_fail!(b"probe_null");
    }
    let probe_va = probe_ptr as u64;
    if probe_va < RPI5_HH_VA_OFFSET
        || probe_va < kheap_virt_start
        || probe_va >= kheap_virt_start + KERNEL_HEAP_SIZE
    {
        galloc_fail!(b"probe_low_va");
    }
    const GALLOC_PROBE_SENTINEL: u64 = 0x5250_4935_4741_4c31; // "RPI5GAL1"
    unsafe {
        core::ptr::write_volatile(probe_ptr as *mut u64, GALLOC_PROBE_SENTINEL);
    }
    let probe_rb = unsafe { core::ptr::read_volatile(probe_ptr as *const u64) };
    if probe_rb != GALLOC_PROBE_SENTINEL {
        galloc_fail!(b"probe_readback");
    }
    hh5_hex_line!(RPI5_KERNEL_GLOBAL_ALLOCATOR_PROBE_OK_MARKER, probe_va);
    if !rpi5_hh_write_line(&RPI5_KERNEL_GLOBAL_ALLOCATOR_HIGHMAP_OK_MARKER) {
        rpi5_hh_halt();
    }

    /*
     * Task D — KernelState is intentionally NOT constructed yet.
     *
     * The kernel global allocator now hands out high-half memory (proven above),
     * so heap allocation works. But `KernelState` bootstrap brings up the
     * scheduler, per-CPU state, thread/IPC/capability tables, and the trap/IRQ
     * model — none of which are initialized on this UART-only high-half path, and
     * which the hard constraints forbid starting here. Defer precisely before
     * touching them: no scheduler/SMP/GIC/RP1/PCIe/driver start, no user TTBR0,
     * no EL0 ERET. The next milestone wires the scheduler/IRQ subsystem.
     */
    hh5_defer!(b"kernel_state_requires_scheduler_init");
}

#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-highhalf"
))]
#[unsafe(no_mangle)]
extern "C" fn yarm_rpi5_hh_rust_continue() -> ! {
    unsafe extern "C" {
        static __hh_ttbr0_root: u8;
        static __hh_ttbr1_root: u8;
    }

    /*
     * Part C — DTB handoff preservation.
     *
     * Firmware/BL31 delivers the DTB physical pointer in x0 at `_start`, where
     * it is immediately saved into the callee-saved x20 register. The low->high
     * transition assembly never clobbers x20 (it uses x19 for the UART alias and
     * x21..x27 for the page-table build), and the branch into this continuation
     * does not change x20 either. Capture the firmware DTB pointer here as the
     * very first instruction of the continuation — before any compiler-generated
     * body code can reuse x20 — and thread it explicitly into the HH-4 stage so
     * the physical pointer is never lost across the calling-convention boundary.
     */
    let dtb_phys: u64;
    unsafe {
        core::arch::asm!(
            "mov {dtb}, x20",
            dtb = out(reg) dtb_phys,
            options(nomem, nostack, preserves_flags)
        );
    }

    if !rpi5_hh_write_line(&RPI5_HH_RUST_ENTRY_MARKER) {
        rpi5_hh_halt();
    }
    if !rpi5_hh_write_line(&RPI5_HH_RUST_AFTER_ENTRY_MARKER) {
        rpi5_hh_halt();
    }

    if !rpi5_hh_write_line(&RPI5_HH_READ_PC_BEGIN_MARKER) {
        rpi5_hh_halt();
    }
    let pc: u64;
    unsafe {
        /*
         * ADR resolves this local label relative to the executing instruction.
         * It neither dereferences memory nor depends on a low-half symbol, so
         * the captured address remains valid after the branch into high code.
         */
        core::arch::asm!(
            "adr {pc}, 2f",
            "2:",
            pc = out(reg) pc,
            options(nomem, nostack, preserves_flags)
        );
    }
    if !rpi5_hh_write_line(&RPI5_HH_READ_PC_CAPTURED_MARKER) {
        rpi5_hh_pc_fail(b"captured_marker_uart_timeout");
    }
    if !rpi5_hh_write_line(&RPI5_HH_READ_PC_PRINT_BEGIN_MARKER) {
        rpi5_hh_pc_fail(b"print_begin_uart_timeout");
    }

    /*
     * Keep the first high-half value print completely local. Each byte polls
     * only the high PL011 alias, has a fixed bound, and is written directly to
     * the data register. No slice walk, iterator, formatter, conversion helper,
     * or out-of-line success-path call is involved.
     */
    const HH_PC_HEX_TX_POLL_LIMIT: usize = 0x1_0000;
    let hh_pc_hex_data = RPI5_HH_UART_VIRT as *mut u32;
    let hh_pc_hex_flags = (RPI5_HH_UART_VIRT + 0x18) as *const u32;
    macro_rules! rpi5_hh_pc_hex_write_byte {
        ($byte:expr, $reason:literal) => {{
            let mut poll = 0usize;
            while poll < HH_PC_HEX_TX_POLL_LIMIT {
                if unsafe { core::ptr::read_volatile(hh_pc_hex_flags) } & (1 << 5) == 0 {
                    break;
                }
                poll += 1;
            }
            if poll == HH_PC_HEX_TX_POLL_LIMIT {
                rpi5_hh_hex_fail($reason);
            }
            unsafe {
                core::ptr::write_volatile(hh_pc_hex_data, $byte as u32);
            }
        }};
    }

    let mut marker_index = 0usize;
    let hex_begin = core::ptr::addr_of!(RPI5_HH_HEX_BEGIN_MARKER).cast::<u8>();
    while marker_index < RPI5_HH_HEX_BEGIN_MARKER.len() {
        let byte = unsafe { core::ptr::read(hex_begin.add(marker_index)) };
        rpi5_hh_pc_hex_write_byte!(byte, b"hex_begin_uart_timeout");
        marker_index += 1;
    }
    rpi5_hh_pc_hex_write_byte!(b'\r', b"hex_begin_cr_uart_timeout");
    rpi5_hh_pc_hex_write_byte!(b'\n', b"hex_begin_lf_uart_timeout");

    marker_index = 0;
    let digit_begin = core::ptr::addr_of!(RPI5_HH_HEX_DIGIT_BEGIN_MARKER).cast::<u8>();
    while marker_index < RPI5_HH_HEX_DIGIT_BEGIN_MARKER.len() {
        let byte = unsafe { core::ptr::read(digit_begin.add(marker_index)) };
        rpi5_hh_pc_hex_write_byte!(byte, b"digit_begin_uart_timeout");
        marker_index += 1;
    }
    rpi5_hh_pc_hex_write_byte!(b'\r', b"digit_begin_cr_uart_timeout");
    rpi5_hh_pc_hex_write_byte!(b'\n', b"digit_begin_lf_uart_timeout");

    let mut prefix_index = 0usize;
    let pc_prefix = core::ptr::addr_of!(RPI5_HH_READ_PC_DONE_MARKER).cast::<u8>();
    while prefix_index < RPI5_HH_READ_PC_DONE_MARKER.len() {
        let byte = unsafe { core::ptr::read(pc_prefix.add(prefix_index)) };
        rpi5_hh_pc_hex_write_byte!(byte, b"pc_prefix_uart_timeout");
        prefix_index += 1;
    }

    let mut nibble_index = 0usize;
    while nibble_index < 16 {
        let shift = 60 - nibble_index * 4;
        let nibble = ((pc >> shift) & 0xf) as u8;
        let digit = if nibble < 10 {
            b'0' + nibble
        } else {
            b'a' + nibble - 10
        };
        rpi5_hh_pc_hex_write_byte!(digit, b"hex_digit_uart_timeout");
        nibble_index += 1;
    }
    rpi5_hh_pc_hex_write_byte!(b'\r', b"pc_value_cr_uart_timeout");
    rpi5_hh_pc_hex_write_byte!(b'\n', b"pc_value_lf_uart_timeout");

    marker_index = 0;
    let digit_done = core::ptr::addr_of!(RPI5_HH_HEX_DIGIT_DONE_MARKER).cast::<u8>();
    while marker_index < RPI5_HH_HEX_DIGIT_DONE_MARKER.len() {
        let byte = unsafe { core::ptr::read(digit_done.add(marker_index)) };
        rpi5_hh_pc_hex_write_byte!(byte, b"digit_done_uart_timeout");
        marker_index += 1;
    }
    rpi5_hh_pc_hex_write_byte!(b'\r', b"digit_done_cr_uart_timeout");
    rpi5_hh_pc_hex_write_byte!(b'\n', b"digit_done_lf_uart_timeout");

    marker_index = 0;
    let hex_done = core::ptr::addr_of!(RPI5_HH_HEX_DONE_MARKER).cast::<u8>();
    while marker_index < RPI5_HH_HEX_DONE_MARKER.len() {
        let byte = unsafe { core::ptr::read(hex_done.add(marker_index)) };
        rpi5_hh_pc_hex_write_byte!(byte, b"hex_done_uart_timeout");
        marker_index += 1;
    }
    rpi5_hh_pc_hex_write_byte!(b'\r', b"hex_done_cr_uart_timeout");
    rpi5_hh_pc_hex_write_byte!(b'\n', b"hex_done_lf_uart_timeout");

    if !rpi5_hh_write_line(&RPI5_HH_READ_SP_BEGIN_MARKER) {
        rpi5_hh_halt();
    }
    let sp: u64;
    unsafe {
        core::arch::asm!("mov {sp}, sp", sp = out(reg) sp, options(nomem, nostack, preserves_flags));
    }
    if !rpi5_hh_write_line(&RPI5_HH_READ_SP_CAPTURED_MARKER) {
        rpi5_hh_sp_hex_fail(b"sp_captured_marker_uart_timeout");
    }

    const HH_SP_HEX_TX_POLL_LIMIT: usize = 0x1_0000;
    let hh_sp_hex_data = RPI5_HH_UART_VIRT as *mut u32;
    let hh_sp_hex_flags = (RPI5_HH_UART_VIRT + 0x18) as *const u32;
    macro_rules! rpi5_hh_sp_hex_write_byte {
        ($byte:expr, $reason:literal) => {{
            let mut poll = 0usize;
            while poll < HH_SP_HEX_TX_POLL_LIMIT {
                if unsafe { core::ptr::read_volatile(hh_sp_hex_flags) } & (1 << 5) == 0 {
                    break;
                }
                poll += 1;
            }
            if poll == HH_SP_HEX_TX_POLL_LIMIT {
                rpi5_hh_sp_hex_fail($reason);
            }
            unsafe {
                core::ptr::write_volatile(hh_sp_hex_data, $byte as u32);
            }
        }};
    }

    marker_index = 0;
    let sp_hex_begin = core::ptr::addr_of!(RPI5_HH_SP_HEX_BEGIN_MARKER).cast::<u8>();
    while marker_index < RPI5_HH_SP_HEX_BEGIN_MARKER.len() {
        let byte = unsafe { core::ptr::read(sp_hex_begin.add(marker_index)) };
        rpi5_hh_sp_hex_write_byte!(byte, b"sp_hex_begin_uart_timeout");
        marker_index += 1;
    }
    rpi5_hh_sp_hex_write_byte!(b'\r', b"sp_hex_begin_cr_uart_timeout");
    rpi5_hh_sp_hex_write_byte!(b'\n', b"sp_hex_begin_lf_uart_timeout");

    marker_index = 0;
    let sp_digit_begin = core::ptr::addr_of!(RPI5_HH_SP_HEX_DIGIT_BEGIN_MARKER).cast::<u8>();
    while marker_index < RPI5_HH_SP_HEX_DIGIT_BEGIN_MARKER.len() {
        let byte = unsafe { core::ptr::read(sp_digit_begin.add(marker_index)) };
        rpi5_hh_sp_hex_write_byte!(byte, b"sp_digit_begin_uart_timeout");
        marker_index += 1;
    }
    rpi5_hh_sp_hex_write_byte!(b'\r', b"sp_digit_begin_cr_uart_timeout");
    rpi5_hh_sp_hex_write_byte!(b'\n', b"sp_digit_begin_lf_uart_timeout");

    prefix_index = 0;
    let sp_prefix = core::ptr::addr_of!(RPI5_HH_READ_SP_DONE_MARKER).cast::<u8>();
    while prefix_index < RPI5_HH_READ_SP_DONE_MARKER.len() {
        let byte = unsafe { core::ptr::read(sp_prefix.add(prefix_index)) };
        rpi5_hh_sp_hex_write_byte!(byte, b"sp_prefix_uart_timeout");
        prefix_index += 1;
    }

    nibble_index = 0;
    while nibble_index < 16 {
        let shift = 60 - nibble_index * 4;
        let nibble = ((sp >> shift) & 0xf) as u8;
        let digit = if nibble < 10 {
            b'0' + nibble
        } else {
            b'a' + nibble - 10
        };
        rpi5_hh_sp_hex_write_byte!(digit, b"sp_hex_digit_uart_timeout");
        nibble_index += 1;
    }
    rpi5_hh_sp_hex_write_byte!(b'\r', b"sp_value_cr_uart_timeout");
    rpi5_hh_sp_hex_write_byte!(b'\n', b"sp_value_lf_uart_timeout");

    marker_index = 0;
    let sp_digit_done = core::ptr::addr_of!(RPI5_HH_SP_HEX_DIGIT_DONE_MARKER).cast::<u8>();
    while marker_index < RPI5_HH_SP_HEX_DIGIT_DONE_MARKER.len() {
        let byte = unsafe { core::ptr::read(sp_digit_done.add(marker_index)) };
        rpi5_hh_sp_hex_write_byte!(byte, b"sp_digit_done_uart_timeout");
        marker_index += 1;
    }
    rpi5_hh_sp_hex_write_byte!(b'\r', b"sp_digit_done_cr_uart_timeout");
    rpi5_hh_sp_hex_write_byte!(b'\n', b"sp_digit_done_lf_uart_timeout");

    marker_index = 0;
    let sp_hex_done = core::ptr::addr_of!(RPI5_HH_SP_HEX_DONE_MARKER).cast::<u8>();
    while marker_index < RPI5_HH_SP_HEX_DONE_MARKER.len() {
        let byte = unsafe { core::ptr::read(sp_hex_done.add(marker_index)) };
        rpi5_hh_sp_hex_write_byte!(byte, b"sp_hex_done_uart_timeout");
        marker_index += 1;
    }
    rpi5_hh_sp_hex_write_byte!(b'\r', b"sp_hex_done_cr_uart_timeout");
    rpi5_hh_sp_hex_write_byte!(b'\n', b"sp_hex_done_lf_uart_timeout");

    if !rpi5_hh_write_line(&RPI5_HH_READ_VBAR_BEGIN_MARKER) {
        rpi5_hh_halt();
    }
    let vbar: u64;
    unsafe {
        core::arch::asm!("mrs {vbar}, VBAR_EL1", vbar = out(reg) vbar, options(nomem, nostack, preserves_flags));
    }
    if !rpi5_hh_write_line(&RPI5_HH_READ_VBAR_CAPTURED_MARKER) {
        rpi5_hh_vbar_hex_fail(b"vbar_captured_marker_uart_timeout");
    }

    const HH_VBAR_HEX_TX_POLL_LIMIT: usize = 0x1_0000;
    let hh_vbar_hex_data = RPI5_HH_UART_VIRT as *mut u32;
    let hh_vbar_hex_flags = (RPI5_HH_UART_VIRT + 0x18) as *const u32;
    macro_rules! rpi5_hh_vbar_hex_write_byte {
        ($byte:expr, $reason:literal) => {{
            let mut poll = 0usize;
            while poll < HH_VBAR_HEX_TX_POLL_LIMIT {
                if unsafe { core::ptr::read_volatile(hh_vbar_hex_flags) } & (1 << 5) == 0 {
                    break;
                }
                poll += 1;
            }
            if poll == HH_VBAR_HEX_TX_POLL_LIMIT {
                rpi5_hh_vbar_hex_fail($reason);
            }
            unsafe {
                core::ptr::write_volatile(hh_vbar_hex_data, $byte as u32);
            }
        }};
    }

    marker_index = 0;
    let vbar_hex_begin = core::ptr::addr_of!(RPI5_HH_VBAR_HEX_BEGIN_MARKER).cast::<u8>();
    while marker_index < RPI5_HH_VBAR_HEX_BEGIN_MARKER.len() {
        let byte = unsafe { core::ptr::read(vbar_hex_begin.add(marker_index)) };
        rpi5_hh_vbar_hex_write_byte!(byte, b"vbar_hex_begin_uart_timeout");
        marker_index += 1;
    }
    rpi5_hh_vbar_hex_write_byte!(b'\r', b"vbar_hex_begin_cr_uart_timeout");
    rpi5_hh_vbar_hex_write_byte!(b'\n', b"vbar_hex_begin_lf_uart_timeout");

    marker_index = 0;
    let vbar_digit_begin = core::ptr::addr_of!(RPI5_HH_VBAR_HEX_DIGIT_BEGIN_MARKER).cast::<u8>();
    while marker_index < RPI5_HH_VBAR_HEX_DIGIT_BEGIN_MARKER.len() {
        let byte = unsafe { core::ptr::read(vbar_digit_begin.add(marker_index)) };
        rpi5_hh_vbar_hex_write_byte!(byte, b"vbar_digit_begin_uart_timeout");
        marker_index += 1;
    }
    rpi5_hh_vbar_hex_write_byte!(b'\r', b"vbar_digit_begin_cr_uart_timeout");
    rpi5_hh_vbar_hex_write_byte!(b'\n', b"vbar_digit_begin_lf_uart_timeout");

    prefix_index = 0;
    let vbar_prefix = core::ptr::addr_of!(RPI5_HH_READ_VBAR_DONE_MARKER).cast::<u8>();
    while prefix_index < RPI5_HH_READ_VBAR_DONE_MARKER.len() {
        let byte = unsafe { core::ptr::read(vbar_prefix.add(prefix_index)) };
        rpi5_hh_vbar_hex_write_byte!(byte, b"vbar_prefix_uart_timeout");
        prefix_index += 1;
    }

    nibble_index = 0;
    while nibble_index < 16 {
        let shift = 60 - nibble_index * 4;
        let nibble = ((vbar >> shift) & 0xf) as u8;
        let digit = if nibble < 10 {
            b'0' + nibble
        } else {
            b'a' + nibble - 10
        };
        rpi5_hh_vbar_hex_write_byte!(digit, b"vbar_hex_digit_uart_timeout");
        nibble_index += 1;
    }
    rpi5_hh_vbar_hex_write_byte!(b'\r', b"vbar_value_cr_uart_timeout");
    rpi5_hh_vbar_hex_write_byte!(b'\n', b"vbar_value_lf_uart_timeout");

    marker_index = 0;
    let vbar_digit_done = core::ptr::addr_of!(RPI5_HH_VBAR_HEX_DIGIT_DONE_MARKER).cast::<u8>();
    while marker_index < RPI5_HH_VBAR_HEX_DIGIT_DONE_MARKER.len() {
        let byte = unsafe { core::ptr::read(vbar_digit_done.add(marker_index)) };
        rpi5_hh_vbar_hex_write_byte!(byte, b"vbar_digit_done_uart_timeout");
        marker_index += 1;
    }
    rpi5_hh_vbar_hex_write_byte!(b'\r', b"vbar_digit_done_cr_uart_timeout");
    rpi5_hh_vbar_hex_write_byte!(b'\n', b"vbar_digit_done_lf_uart_timeout");

    marker_index = 0;
    let vbar_hex_done = core::ptr::addr_of!(RPI5_HH_VBAR_HEX_DONE_MARKER).cast::<u8>();
    while marker_index < RPI5_HH_VBAR_HEX_DONE_MARKER.len() {
        let byte = unsafe { core::ptr::read(vbar_hex_done.add(marker_index)) };
        rpi5_hh_vbar_hex_write_byte!(byte, b"vbar_hex_done_uart_timeout");
        marker_index += 1;
    }
    rpi5_hh_vbar_hex_write_byte!(b'\r', b"vbar_hex_done_cr_uart_timeout");
    rpi5_hh_vbar_hex_write_byte!(b'\n', b"vbar_hex_done_lf_uart_timeout");

    if !rpi5_hh_write_line(&RPI5_HH_READ_TTBR_BEGIN_MARKER) {
        rpi5_hh_halt();
    }
    let ttbr0: u64;
    let ttbr1: u64;
    let tcr: u64;
    unsafe {
        core::arch::asm!("mrs {ttbr0}, TTBR0_EL1", ttbr0 = out(reg) ttbr0, options(nomem, nostack, preserves_flags));
        core::arch::asm!("mrs {ttbr1}, TTBR1_EL1", ttbr1 = out(reg) ttbr1, options(nomem, nostack, preserves_flags));
        core::arch::asm!("mrs {tcr}, TCR_EL1", tcr = out(reg) tcr, options(nomem, nostack, preserves_flags));
    }
    if !rpi5_hh_write_line(&RPI5_HH_READ_TTBR_DONE_MARKER) {
        rpi5_hh_fail(b"read_ttbr_done_uart_timeout");
    }

    if !rpi5_hh_write_line(&RPI5_HH_PRINT_REGS_BEGIN_MARKER) {
        rpi5_hh_halt();
    }
    if !rpi5_hh_write_line(&RPI5_HH_PRINT_REGS_FIRST_BEGIN_MARKER) {
        rpi5_hh_print_regs_first_hex_fail(b"first_begin_uart_timeout");
    }

    const HH_PRINT_REGS_FIRST_HEX_TX_POLL_LIMIT: usize = 0x1_0000;
    let hh_print_regs_first_hex_data = RPI5_HH_UART_VIRT as *mut u32;
    let hh_print_regs_first_hex_flags = (RPI5_HH_UART_VIRT + 0x18) as *const u32;
    macro_rules! rpi5_hh_print_regs_first_hex_write_byte {
        ($byte:expr, $reason:literal) => {{
            let mut poll = 0usize;
            while poll < HH_PRINT_REGS_FIRST_HEX_TX_POLL_LIMIT {
                if unsafe { core::ptr::read_volatile(hh_print_regs_first_hex_flags) } & (1 << 5)
                    == 0
                {
                    break;
                }
                poll += 1;
            }
            if poll == HH_PRINT_REGS_FIRST_HEX_TX_POLL_LIMIT {
                rpi5_hh_print_regs_first_hex_fail($reason);
            }
            unsafe {
                core::ptr::write_volatile(hh_print_regs_first_hex_data, $byte as u32);
            }
        }};
    }

    marker_index = 0;
    let print_regs_first_hex_begin =
        core::ptr::addr_of!(RPI5_HH_PRINT_REGS_FIRST_HEX_BEGIN_MARKER).cast::<u8>();
    while marker_index < RPI5_HH_PRINT_REGS_FIRST_HEX_BEGIN_MARKER.len() {
        let byte = unsafe { core::ptr::read(print_regs_first_hex_begin.add(marker_index)) };
        rpi5_hh_print_regs_first_hex_write_byte!(byte, b"first_hex_begin_uart_timeout");
        marker_index += 1;
    }
    rpi5_hh_print_regs_first_hex_write_byte!(b'\r', b"first_hex_begin_cr_uart_timeout");
    rpi5_hh_print_regs_first_hex_write_byte!(b'\n', b"first_hex_begin_lf_uart_timeout");

    marker_index = 0;
    let print_regs_first_digit_begin =
        core::ptr::addr_of!(RPI5_HH_PRINT_REGS_FIRST_HEX_DIGIT_BEGIN_MARKER).cast::<u8>();
    while marker_index < RPI5_HH_PRINT_REGS_FIRST_HEX_DIGIT_BEGIN_MARKER.len() {
        let byte = unsafe { core::ptr::read(print_regs_first_digit_begin.add(marker_index)) };
        rpi5_hh_print_regs_first_hex_write_byte!(byte, b"first_digit_begin_uart_timeout");
        marker_index += 1;
    }
    rpi5_hh_print_regs_first_hex_write_byte!(b'\r', b"first_digit_begin_cr_uart_timeout");
    rpi5_hh_print_regs_first_hex_write_byte!(b'\n', b"first_digit_begin_lf_uart_timeout");

    prefix_index = 0;
    let print_regs_first_prefix =
        core::ptr::addr_of!(RPI5_HH_PRINT_REGS_FIRST_DONE_MARKER).cast::<u8>();
    while prefix_index < RPI5_HH_PRINT_REGS_FIRST_DONE_MARKER.len() {
        let byte = unsafe { core::ptr::read(print_regs_first_prefix.add(prefix_index)) };
        rpi5_hh_print_regs_first_hex_write_byte!(byte, b"first_prefix_uart_timeout");
        prefix_index += 1;
    }

    nibble_index = 0;
    while nibble_index < 16 {
        let shift = 60 - nibble_index * 4;
        let nibble = ((pc >> shift) & 0xf) as u8;
        let digit = if nibble < 10 {
            b'0' + nibble
        } else {
            b'a' + nibble - 10
        };
        rpi5_hh_print_regs_first_hex_write_byte!(digit, b"first_hex_digit_uart_timeout");
        nibble_index += 1;
    }
    rpi5_hh_print_regs_first_hex_write_byte!(b'\r', b"first_value_cr_uart_timeout");
    rpi5_hh_print_regs_first_hex_write_byte!(b'\n', b"first_value_lf_uart_timeout");

    marker_index = 0;
    let print_regs_first_digit_done =
        core::ptr::addr_of!(RPI5_HH_PRINT_REGS_FIRST_HEX_DIGIT_DONE_MARKER).cast::<u8>();
    while marker_index < RPI5_HH_PRINT_REGS_FIRST_HEX_DIGIT_DONE_MARKER.len() {
        let byte = unsafe { core::ptr::read(print_regs_first_digit_done.add(marker_index)) };
        rpi5_hh_print_regs_first_hex_write_byte!(byte, b"first_digit_done_uart_timeout");
        marker_index += 1;
    }
    rpi5_hh_print_regs_first_hex_write_byte!(b'\r', b"first_digit_done_cr_uart_timeout");
    rpi5_hh_print_regs_first_hex_write_byte!(b'\n', b"first_digit_done_lf_uart_timeout");

    marker_index = 0;
    let print_regs_first_hex_done =
        core::ptr::addr_of!(RPI5_HH_PRINT_REGS_FIRST_HEX_DONE_MARKER).cast::<u8>();
    while marker_index < RPI5_HH_PRINT_REGS_FIRST_HEX_DONE_MARKER.len() {
        let byte = unsafe { core::ptr::read(print_regs_first_hex_done.add(marker_index)) };
        rpi5_hh_print_regs_first_hex_write_byte!(byte, b"first_hex_done_uart_timeout");
        marker_index += 1;
    }
    rpi5_hh_print_regs_first_hex_write_byte!(b'\r', b"first_hex_done_cr_uart_timeout");
    rpi5_hh_print_regs_first_hex_write_byte!(b'\n', b"first_hex_done_lf_uart_timeout");

    if !rpi5_hh_write_line(&RPI5_HH_PRINT_REGS_SP_BEGIN_MARKER) {
        rpi5_hh_print_regs_sp_hex_fail(b"sp_begin_uart_timeout");
    }

    const HH_PRINT_REGS_SP_HEX_TX_POLL_LIMIT: usize = 0x1_0000;
    let hh_print_regs_sp_hex_data = RPI5_HH_UART_VIRT as *mut u32;
    let hh_print_regs_sp_hex_flags = (RPI5_HH_UART_VIRT + 0x18) as *const u32;
    macro_rules! rpi5_hh_print_regs_sp_hex_write_byte {
        ($byte:expr, $reason:literal) => {{
            let mut poll = 0usize;
            while poll < HH_PRINT_REGS_SP_HEX_TX_POLL_LIMIT {
                if unsafe { core::ptr::read_volatile(hh_print_regs_sp_hex_flags) } & (1 << 5) == 0 {
                    break;
                }
                poll += 1;
            }
            if poll == HH_PRINT_REGS_SP_HEX_TX_POLL_LIMIT {
                rpi5_hh_print_regs_sp_hex_fail($reason);
            }
            unsafe {
                core::ptr::write_volatile(hh_print_regs_sp_hex_data, $byte as u32);
            }
        }};
    }

    marker_index = 0;
    let print_regs_sp_hex_begin =
        core::ptr::addr_of!(RPI5_HH_PRINT_REGS_SP_HEX_BEGIN_MARKER).cast::<u8>();
    while marker_index < RPI5_HH_PRINT_REGS_SP_HEX_BEGIN_MARKER.len() {
        let byte = unsafe { core::ptr::read(print_regs_sp_hex_begin.add(marker_index)) };
        rpi5_hh_print_regs_sp_hex_write_byte!(byte, b"sp_hex_begin_uart_timeout");
        marker_index += 1;
    }
    rpi5_hh_print_regs_sp_hex_write_byte!(b'\r', b"sp_hex_begin_cr_uart_timeout");
    rpi5_hh_print_regs_sp_hex_write_byte!(b'\n', b"sp_hex_begin_lf_uart_timeout");

    marker_index = 0;
    let print_regs_sp_digit_begin =
        core::ptr::addr_of!(RPI5_HH_PRINT_REGS_SP_HEX_DIGIT_BEGIN_MARKER).cast::<u8>();
    while marker_index < RPI5_HH_PRINT_REGS_SP_HEX_DIGIT_BEGIN_MARKER.len() {
        let byte = unsafe { core::ptr::read(print_regs_sp_digit_begin.add(marker_index)) };
        rpi5_hh_print_regs_sp_hex_write_byte!(byte, b"sp_digit_begin_uart_timeout");
        marker_index += 1;
    }
    rpi5_hh_print_regs_sp_hex_write_byte!(b'\r', b"sp_digit_begin_cr_uart_timeout");
    rpi5_hh_print_regs_sp_hex_write_byte!(b'\n', b"sp_digit_begin_lf_uart_timeout");

    prefix_index = 0;
    let print_regs_sp_prefix = core::ptr::addr_of!(RPI5_HH_PRINT_REGS_SP_DONE_MARKER).cast::<u8>();
    while prefix_index < RPI5_HH_PRINT_REGS_SP_DONE_MARKER.len() {
        let byte = unsafe { core::ptr::read(print_regs_sp_prefix.add(prefix_index)) };
        rpi5_hh_print_regs_sp_hex_write_byte!(byte, b"sp_prefix_uart_timeout");
        prefix_index += 1;
    }

    nibble_index = 0;
    while nibble_index < 16 {
        let shift = 60 - nibble_index * 4;
        let nibble = ((sp >> shift) & 0xf) as u8;
        let digit = if nibble < 10 {
            b'0' + nibble
        } else {
            b'a' + nibble - 10
        };
        rpi5_hh_print_regs_sp_hex_write_byte!(digit, b"sp_hex_digit_uart_timeout");
        nibble_index += 1;
    }
    rpi5_hh_print_regs_sp_hex_write_byte!(b'\r', b"sp_value_cr_uart_timeout");
    rpi5_hh_print_regs_sp_hex_write_byte!(b'\n', b"sp_value_lf_uart_timeout");

    marker_index = 0;
    let print_regs_sp_digit_done =
        core::ptr::addr_of!(RPI5_HH_PRINT_REGS_SP_HEX_DIGIT_DONE_MARKER).cast::<u8>();
    while marker_index < RPI5_HH_PRINT_REGS_SP_HEX_DIGIT_DONE_MARKER.len() {
        let byte = unsafe { core::ptr::read(print_regs_sp_digit_done.add(marker_index)) };
        rpi5_hh_print_regs_sp_hex_write_byte!(byte, b"sp_digit_done_uart_timeout");
        marker_index += 1;
    }
    rpi5_hh_print_regs_sp_hex_write_byte!(b'\r', b"sp_digit_done_cr_uart_timeout");
    rpi5_hh_print_regs_sp_hex_write_byte!(b'\n', b"sp_digit_done_lf_uart_timeout");

    marker_index = 0;
    let print_regs_sp_hex_done =
        core::ptr::addr_of!(RPI5_HH_PRINT_REGS_SP_HEX_DONE_MARKER).cast::<u8>();
    while marker_index < RPI5_HH_PRINT_REGS_SP_HEX_DONE_MARKER.len() {
        let byte = unsafe { core::ptr::read(print_regs_sp_hex_done.add(marker_index)) };
        rpi5_hh_print_regs_sp_hex_write_byte!(byte, b"sp_hex_done_uart_timeout");
        marker_index += 1;
    }
    rpi5_hh_print_regs_sp_hex_write_byte!(b'\r', b"sp_hex_done_cr_uart_timeout");
    rpi5_hh_print_regs_sp_hex_write_byte!(b'\n', b"sp_hex_done_lf_uart_timeout");

    /*
     * Part A — temporary HH-3 hardware-progress scaffolding.
     *
     * The standalone PC/SP/VBAR hex output and the print-regs PC/SP hex values
     * above have ALREADY been proven on real Raspberry Pi 5 hardware using the
     * inline, bounded, raw-pointer high-UART path. The remaining old helper-based
     * register dump (VBAR/TTBR0/TTBR1/TCR via `rpi5_hh_write_hex_line`) is purely
     * diagnostic: it relies on slice iterators / anonymous string literals that
     * have never produced output on hardware and stall the bring-up exactly at
     * this point. Bypass that helper dump here so HH-3 can advance to
     * RPI5_HH3_DONE. The register *values* are still fully validated below; only
     * their helper-based *printing* is skipped. This is temporary scaffolding —
     * once the helper hex path is reworked into the proven inline form (or
     * removed), this bypass should be retired.
     */
    if !rpi5_hh_write_line(&RPI5_HH_PRINT_REGS_BYPASS_FOR_HH3_PROOF_MARKER) {
        rpi5_hh_fail(b"print_regs_bypass_uart_timeout");
    }
    if !rpi5_hh_write_line(&RPI5_HH_PRINT_REGS_DONE_MARKER) {
        rpi5_hh_fail(b"print_regs_done_uart_timeout");
    }

    let expected_ttbr0 = core::ptr::addr_of!(__hh_ttbr0_root) as u64;
    let expected_ttbr1 = core::ptr::addr_of!(__hh_ttbr1_root) as u64;
    let root_mask = !0xfffu64;
    if pc < RPI5_HH_VA_OFFSET {
        rpi5_hh_fail(b"pc_not_high");
    }
    if sp < RPI5_HH_VA_OFFSET {
        rpi5_hh_fail(b"sp_not_high");
    }
    if vbar < RPI5_HH_VA_OFFSET || vbar & 0x7ff != 0 {
        rpi5_hh_fail(b"vbar_not_high_aligned");
    }
    if ttbr0 & root_mask != expected_ttbr0 || ttbr0 == 0 {
        rpi5_hh_fail(b"ttbr0_root_mismatch");
    }
    if ttbr1 & root_mask != expected_ttbr1 || ttbr1 == 0 {
        rpi5_hh_fail(b"ttbr1_root_mismatch");
    }
    if ttbr0 & root_mask == ttbr1 & root_mask {
        rpi5_hh_fail(b"ttbr_roots_not_distinct");
    }
    if tcr & (1 << 23) != 0 || ((tcr >> 16) & 0x3f) != 25 {
        rpi5_hh_fail(b"ttbr1_walk_configuration");
    }
    if tcr != RPI5_HH_TCR_EL1 {
        rpi5_hh_fail(b"tcr_value_mismatch");
    }

    if !rpi5_hh_write_line(&RPI5_HH3_PRECHECK_DONE_MARKER)
        || !rpi5_hh_write_line(&RPI5_HH_REGISTERS_OK_MARKER)
        || !rpi5_hh_write_line(&RPI5_HH_RUST_UART_OK_MARKER)
        || !rpi5_hh_write_line(&RPI5_HH3_DONE_MARKER)
    {
        rpi5_hh_fail(b"uart_timeout");
    }
    let hh4 = rpi5_hh4_retire_low_ttbr0(dtb_phys);
    rpi5_hh5_bridge(hh4)
}

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
struct Rpi5Stage2CInitTask {
    tid: u64,
    root_table: u64,
    entry: u64,
    stack_pointer: u64,
    mapped_pages: usize,
}

#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-stage1"
))]
unsafe fn rpi5_stage2c_build_init_task(
    allocator: &mut crate::kernel::frame_allocator::PhysicalFrameAllocator,
    elf: &[u8],
    elf_plan: &crate::arch::aarch64_boot_policy::Stage2BElfLoadPlan,
    task_plan: &crate::arch::aarch64_boot_policy::Stage2CTaskPlan,
) -> Result<Rpi5Stage2CInitTask, &'static str> {
    use crate::arch::aarch64_boot_policy::Stage2CUserPermission;

    let root = rpi5_stage2c_alloc_zeroed_frame(allocator)?;
    let mut mapped_pages = 0usize;
    for (segment, planned) in elf_plan.segments[..elf_plan.segment_count]
        .iter()
        .zip(&task_plan.segments[..task_plan.segment_count])
    {
        let flags = match planned.permission {
            Stage2CUserPermission::ReadExecute => RPI5_STAGE2C_USER_RX_FLAGS,
            Stage2CUserPermission::ReadWrite => RPI5_STAGE2C_USER_RW_FLAGS,
        };
        let file_end = segment
            .vaddr
            .checked_add(segment.file_size)
            .ok_or("segment_file_overflow")?;
        let mut page_va = planned.page_range.start;
        while page_va < planned.page_range.end {
            let phys = rpi5_stage2c_alloc_zeroed_frame(allocator)?;
            let page_end = page_va
                .checked_add(RPI5_STAGE1_PAGE_SIZE)
                .ok_or("segment_page_overflow")?;
            let copy_start = page_va.max(segment.vaddr);
            let copy_end = page_end.min(file_end);
            if copy_start < copy_end {
                let source_start = segment
                    .file_offset
                    .checked_add(copy_start - segment.vaddr)
                    .ok_or("segment_source_overflow")?;
                let source_end = source_start
                    .checked_add(copy_end - copy_start)
                    .ok_or("segment_source_overflow")?;
                let source_start =
                    usize::try_from(source_start).map_err(|_| "segment_source_address")?;
                let source_end =
                    usize::try_from(source_end).map_err(|_| "segment_source_address")?;
                let source = elf
                    .get(source_start..source_end)
                    .ok_or("segment_source_bounds")?;
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        source.as_ptr(),
                        (phys + copy_start - page_va) as *mut u8,
                        source.len(),
                    );
                }
            }
            unsafe {
                rpi5_stage2c_map_user_page(root, allocator, page_va, phys, flags)?;
            }
            mapped_pages += 1;
            page_va = page_end;
        }
    }
    let mut stack_va = task_plan.stack_range.start;
    while stack_va < task_plan.stack_range.end {
        let phys = rpi5_stage2c_alloc_zeroed_frame(allocator)?;
        unsafe {
            rpi5_stage2c_map_user_page(
                root,
                allocator,
                stack_va,
                phys,
                RPI5_STAGE2C_USER_RW_FLAGS,
            )?;
        }
        mapped_pages += 1;
        stack_va += RPI5_STAGE1_PAGE_SIZE;
    }
    Ok(Rpi5Stage2CInitTask {
        tid: task_plan.tid,
        root_table: root,
        entry: task_plan.entry,
        stack_pointer: task_plan.stack_pointer,
        mapped_pages,
    })
}

#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-stage1"
))]
fn rpi5_stage2c_alloc_zeroed_frame(
    allocator: &mut crate::kernel::frame_allocator::PhysicalFrameAllocator,
) -> Result<u64, &'static str> {
    let frame = allocator.alloc_frame().map_err(|_| "frame_allocator")?;
    unsafe {
        core::ptr::write_bytes(frame as *mut u8, 0, RPI5_STAGE1_PAGE_SIZE as usize);
    }
    Ok(frame)
}

#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-stage1"
))]
unsafe fn rpi5_stage2c_map_user_page(
    root: u64,
    allocator: &mut crate::kernel::frame_allocator::PhysicalFrameAllocator,
    virtual_address: u64,
    physical_address: u64,
    flags: u64,
) -> Result<(), &'static str> {
    if virtual_address % RPI5_STAGE1_PAGE_SIZE != 0
        || physical_address % RPI5_STAGE1_PAGE_SIZE != 0
        || virtual_address >= (1 << 39)
    {
        return Err("invalid_user_mapping");
    }
    let l1 = ((virtual_address >> 30) & 0x1ff) as usize;
    let l2_table = unsafe { rpi5_stage2c_ensure_table(root, l1, allocator)? };
    let l2 = ((virtual_address >> 21) & 0x1ff) as usize;
    let l3_table = unsafe { rpi5_stage2c_ensure_table(l2_table, l2, allocator)? };
    let l3 = ((virtual_address >> 12) & 0x1ff) as usize;
    let entry = (l3_table as *mut u64).wrapping_add(l3);
    if unsafe { core::ptr::read_volatile(entry) } != 0 {
        return Err("user_mapping_overlap");
    }
    unsafe {
        core::ptr::write_volatile(
            entry,
            (physical_address & RPI5_STAGE1_PTE_ADDR_MASK) | flags | RPI5_STAGE1_PTE_TABLE_OR_PAGE,
        );
    }
    Ok(())
}

#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-stage1"
))]
unsafe fn rpi5_stage2c_ensure_table(
    table: u64,
    index: usize,
    allocator: &mut crate::kernel::frame_allocator::PhysicalFrameAllocator,
) -> Result<u64, &'static str> {
    let entry = (table as *mut u64).wrapping_add(index);
    let current = unsafe { core::ptr::read_volatile(entry) };
    if current != 0 {
        if current & (RPI5_STAGE1_PTE_VALID | RPI5_STAGE1_PTE_TABLE_OR_PAGE)
            != (RPI5_STAGE1_PTE_VALID | RPI5_STAGE1_PTE_TABLE_OR_PAGE)
        {
            return Err("user_table_conflict");
        }
        return Ok(current & RPI5_STAGE1_PTE_ADDR_MASK);
    }
    let child = rpi5_stage2c_alloc_zeroed_frame(allocator)?;
    unsafe {
        core::ptr::write_volatile(
            entry,
            child | RPI5_STAGE1_PTE_VALID | RPI5_STAGE1_PTE_TABLE_OR_PAGE,
        );
    }
    Ok(child)
}

#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-stage1"
))]
fn rpi5_stage1_kernel_core_diagnostics(dtb: &[u8]) -> ! {
    use crate::arch::aarch64_boot_policy::{
        RPI5_STAGE1_GICR_FRAME_STRIDE, RPI5_STAGE1_GICR_SCAN_FRAMES, Stage1KernelRange,
        Stage1MmuMemoryType, Stage2DEnterBridgeState, build_rpi5_stage1_kernel_bootstrap_record,
        parse_platform_dtb_diagnostics, plan_rpi5_stage1_allocator_handoff,
        plan_rpi5_stage1_identity_map, plan_rpi5_stage1_kernel_memory, plan_rpi5_stage2a_initrd,
        plan_rpi5_stage2b_init_elf, plan_rpi5_stage2c_init_task, rpi5_stage1_gicr_typer_plausible,
        rpi5_stage1_timer_delta, rpi5_stage2a_cpio_first_name, rpi5_stage2b_find_init,
        validate_rpi5_stage2d_enter_bridge,
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

    rpi5_emergency_marker(b"RPI5_STAGE2B_BEGIN\r\n\0");
    rpi5_emergency_marker(b"RPI5_INIT_LOOKUP_BEGIN path=/init\r\n\0");
    let init_file = match rpi5_stage2b_find_init(initrd) {
        Ok(file) => file,
        Err(reason) => {
            kernel_diag!("RPI5_INIT_LOOKUP_FAILED reason={}", reason.label());
            kernel_diag!("RPI5_STAGE2B_DEFERRED reason=init_lookup_failed");
            halt_stage1();
        }
    };
    kernel_diag!(
        "RPI5_INIT_LOOKUP_OK offset=0x{:016x} size=0x{:016x}",
        init_file.data_offset,
        init_file.size
    );
    let init_elf = &initrd[init_file.data_offset..init_file.data_offset + init_file.size];
    rpi5_emergency_marker(b"RPI5_INIT_ELF_CHECK_BEGIN\r\n\0");
    let load_plan = match plan_rpi5_stage2b_init_elf(init_elf) {
        Ok(plan) => plan,
        Err(reason) => {
            kernel_diag!("RPI5_INIT_ELF_INVALID reason={}", reason.label());
            kernel_diag!("RPI5_STAGE2B_DEFERRED reason=invalid_init_elf");
            halt_stage1();
        }
    };
    kernel_diag!("RPI5_INIT_ELF_HEADER_OK entry=0x{:016x}", load_plan.entry);
    rpi5_emergency_marker(b"RPI5_INIT_ELF_LOAD_PLAN_BEGIN\r\n\0");
    for (index, segment) in load_plan.segments[..load_plan.segment_count]
        .iter()
        .enumerate()
    {
        kernel_diag!(
            "RPI5_INIT_ELF_SEGMENT index={} vaddr=0x{:016x} memsz=0x{:016x} filesz=0x{:016x} flags=0x{:08x}",
            index,
            segment.vaddr,
            segment.mem_size,
            segment.file_size,
            segment.flags
        );
    }
    rpi5_emergency_marker(b"RPI5_INIT_ELF_LOAD_PLAN_DONE\r\n\0");
    kernel_diag!("RPI5_STAGE2B_DONE status=load_plan_ready");

    rpi5_emergency_marker(b"RPI5_STAGE2C_BEGIN\r\n\0");
    rpi5_emergency_marker(b"RPI5_INIT_TASK_BUILD_BEGIN\r\n\0");
    let task_plan = match plan_rpi5_stage2c_init_task(&load_plan) {
        Ok(plan) => plan,
        Err(reason) => {
            kernel_diag!("RPI5_INIT_TASK_BUILD_FAILED reason={}", reason.label());
            halt_stage1();
        }
    };
    rpi5_emergency_marker(b"RPI5_INIT_ADDRESS_SPACE_BEGIN\r\n\0");
    for index in 0..load_plan.segment_count {
        kernel_diag!("RPI5_INIT_SEGMENT_MAP_BEGIN index={}", index);
    }
    let task = match unsafe {
        rpi5_stage2c_build_init_task(allocator, init_elf, &load_plan, &task_plan)
    } {
        Ok(task) => task,
        Err(reason) => {
            kernel_diag!("RPI5_INIT_SEGMENT_MAP_FAILED reason={}", reason);
            kernel_diag!("RPI5_INIT_ADDRESS_SPACE_FAILED reason={}", reason);
            halt_stage1();
        }
    };
    for (index, segment) in load_plan.segments[..load_plan.segment_count]
        .iter()
        .enumerate()
    {
        kernel_diag!(
            "RPI5_INIT_SEGMENT_MAPPED index={} vaddr=0x{:016x} memsz=0x{:016x} filesz=0x{:016x} flags=0x{:08x}",
            index,
            segment.vaddr,
            segment.mem_size,
            segment.file_size,
            segment.flags
        );
        if let Some(bss) = task_plan.segments[index].bss_range {
            kernel_diag!(
                "RPI5_INIT_BSS_ZEROED index={} start=0x{:016x} end=0x{:016x}",
                index,
                bss.start,
                bss.end
            );
        }
    }
    kernel_diag!(
        "RPI5_INIT_ADDRESS_SPACE_READY root=0x{:016x} pages={}",
        task.root_table,
        task.mapped_pages
    );
    kernel_diag!("RPI5_INIT_STACK_READY sp=0x{:016x}", task.stack_pointer);
    let mut trap_frame = crate::kernel::trapframe::TrapFrame::zeroed();
    trap_frame.set_saved_pc(task.entry as usize);
    trap_frame.set_saved_sp(task.stack_pointer as usize);
    kernel_diag!(
        "RPI5_INIT_TRAP_FRAME_READY entry=0x{:016x} sp=0x{:016x}",
        trap_frame.saved_pc(),
        trap_frame.saved_sp()
    );
    rpi5_emergency_marker(b"RPI5_INIT_TASK_BUILD_DONE\r\n\0");
    kernel_diag!("RPI5_INIT_SPAWN_READY tid={}", task.tid);
    rpi5_emergency_marker(b"RPI5_STAGE2C_DONE\r\n\0");

    rpi5_emergency_marker(b"RPI5_STAGE2D_REAL_BEGIN\r\n\0");
    let (current_ttbr0, current_tcr, kernel_pc): (u64, u64, u64);
    unsafe {
        core::arch::asm!(
            "mrs {ttbr0}, TTBR0_EL1",
            "mrs {tcr}, TCR_EL1",
            "adr {pc}, .",
            ttbr0 = out(reg) current_ttbr0,
            tcr = out(reg) current_tcr,
            pc = out(reg) kernel_pc,
            options(nomem, nostack, preserves_flags)
        );
    }
    let bridge = Stage2DEnterBridgeState {
        expected_tid: task.tid,
        current_tid: None,
        stage2c_root: task.root_table,
        current_ttbr0,
        trap_entry: trap_frame.saved_pc() as u64,
        task_entry: task.entry,
        trap_stack_pointer: trap_frame.saved_sp() as u64,
        task_stack_pointer: task.stack_pointer,
        tcr_el1: current_tcr,
        kernel_pc,
    };
    if let Err(reason) = validate_rpi5_stage2d_enter_bridge(bridge) {
        kernel_diag!("RPI5_ENTER_USER_FAILED reason={}", reason.label());
        kernel_diag!("RPI5_STAGE2D_REAL_DEFERRED reason={}", reason.label());
        kernel_diag!("RPI5_STAGE2E_DEFERRED reason=el0_not_entered");
        halt_stage1();
    }
    kernel_diag!("RPI5_STAGE2D_REAL_DEFERRED reason=eret_sequence_not_reviewed");
    kernel_diag!("RPI5_STAGE2E_DEFERRED reason=el0_not_entered");
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
const RPI5_STAGE2C_USER_RX_FLAGS: u64 = RPI5_STAGE1_PTE_VALID
    | RPI5_STAGE1_PTE_AF
    | (0b11 << 8)
    | (1 << 6)
    | (1 << 7)
    | RPI5_STAGE1_PTE_PXN;
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "aarch64",
    feature = "rpi5-stage1"
))]
const RPI5_STAGE2C_USER_RW_FLAGS: u64 = RPI5_STAGE1_PTE_VALID
    | RPI5_STAGE1_PTE_AF
    | (0b11 << 8)
    | (1 << 6)
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
