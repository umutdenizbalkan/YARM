/* SPDX-License-Identifier: Apache-2.0 */
/* Copyright 2026 Umut Deniz Balkan */

/*
 * Non-default RPi5 Stage2 high-half linker scaffold.
 *
 * This script establishes load-address and symbol contracts only. The current
 * hardware-proven Stage1 image continues to use aarch64-rpi5-stage1-none.ld.
 * A future low trampoline will establish TTBR1 before branching to these
 * high-half sections.
 */

ENTRY(_start)

KERNEL_PHYS_LOAD_BASE = 0x0000000000080000;
KERNEL_VIRT_BASE = 0xffffff8000000000;
KERNEL_VIRT_LOAD_BASE = 0xffffff8000080000;
__kernel_va_offset = KERNEL_VIRT_BASE;

PHDRS {
    boot PT_LOAD FLAGS(5);
    text PT_LOAD FLAGS(5);
    data PT_LOAD FLAGS(6);
}

SECTIONS {
    . = KERNEL_PHYS_LOAD_BASE;
    __boot_low_start = .;

    .text.boot : ALIGN(4096) {
        KEEP(*(.text.boot))
    } :boot

    .rodata.boot : ALIGN(16) {
        KEEP(*(.rodata.rpi5_raw_entry))
        *(.rodata.boot*)
    } :boot

    . = ALIGN(4096);
    __boot_low_end = .;

    __kernel_phys_start = __boot_low_end;
    . = KERNEL_VIRT_BASE + __kernel_phys_start;
    __kernel_virt_start = .;

    .text : AT(__kernel_phys_start) ALIGN(4096) {
        *(.text*)
    } :text

    .rodata : ALIGN(4096) {
        *(.rodata*)
    } :text

    .data : ALIGN(4096) {
        *(.data*)
    } :data

    __bss_start = .;

    .bss.bootstack (NOLOAD) : ALIGN(16) {
        *(.bss.bootstack)
    } :data

    .bss (NOLOAD) : ALIGN(16) {
        *(.bss*)
        *(COMMON)
    } :data

    __bss_end = .;
    . = ALIGN(4096);
    __kernel_virt_end = .;
    __kernel_phys_end = __kernel_virt_end - __kernel_va_offset;

    ASSERT(KERNEL_VIRT_LOAD_BASE == KERNEL_VIRT_BASE + KERNEL_PHYS_LOAD_BASE,
           "RPi5 high-half load bases violate the VA/PA offset contract")
    ASSERT(__kernel_virt_start == __kernel_va_offset + __kernel_phys_start,
           "RPi5 high-half section placement violates the VA/PA offset contract")

    /DISCARD/ : {
        *(.eh_frame*)
        *(.comment*)
        *(.note.GNU-stack)
        *(.ARM.exidx*)
        *(.ARM.extab*)
    }
}
