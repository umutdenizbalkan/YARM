/* SPDX-License-Identifier: Apache-2.0 */
/* Copyright 2026 Umut Deniz Balkan */

ENTRY(_start)

/*
 * QEMU virt aarch64 memory map (relevant regions):
 *   0x09000000 - 0x09001000  PL011 UART0
 *   0x40000000+              RAM (default 1 GiB, configurable via -m)
 *
 * We link the kernel image starting at 0x40080000:
 *   - Skips the first 512 KiB of RAM, giving room for QEMU ATF/firmware stubs
 *     placed by some board configs at 0x40000000.
 *   - Keeps everything in the first 1 GiB of RAM (L1[1] in bootstrap page tables).
 */
KERNEL_LOAD_BASE = 0x40080000;

PHDRS {
    text PT_LOAD FLAGS(5);   /* R-X */
    data PT_LOAD FLAGS(6);   /* RW- */
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
