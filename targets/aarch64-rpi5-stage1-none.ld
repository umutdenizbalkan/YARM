/* SPDX-License-Identifier: Apache-2.0 */
/* Copyright 2026 Umut Deniz Balkan */

ENTRY(_start)

KERNEL_LOAD_BASE = 0x80000;

PHDRS {
    text PT_LOAD FLAGS(5);
    data PT_LOAD FLAGS(6);
}

SECTIONS {
    . = KERNEL_LOAD_BASE;
    __kernel_start = .;
    __kernel_rx_start = .;

    .text.boot : ALIGN(2048) {
        KEEP(*(.text.boot))
    } :text

    .text : ALIGN(16) {
        *(.text*)
    } :text

    .rodata : ALIGN(16) {
        KEEP(*(.rodata.rpi5_raw_entry))
        *(.rodata*)
    } :text

    __kernel_rx_end = .;
    __kernel_rw_start = .;

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
    __kernel_rw_end = .;
    . = ALIGN(4096);
    __kernel_end = .;

    /DISCARD/ : {
        *(.eh_frame*)
        *(.comment*)
        *(.note.GNU-stack)
        *(.ARM.exidx*)
        *(.ARM.extab*)
    }
}
