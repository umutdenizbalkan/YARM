/* SPDX-License-Identifier: Apache-2.0 */
/* Copyright 2026 Umut Deniz Balkan */

ENTRY(_start)

SECTIONS
{
    . = 0x40000000;
    __kernel_start = .;

    .text.boot : {
        KEEP(*(.text.boot))
    }

    .text : {
        *(.text*)
    }

    .rodata : {
        *(.rodata*)
    }

    .data : {
        *(.data*)
    }

    .bss : {
        __bss_start = .;
        *(.bss*)
        *(COMMON)
        . = ALIGN(16);
        __bss_end = .;
    }

    .bootstack (NOLOAD) : {
        . = ALIGN(16);
        __boot_stack_bottom = .;
        . += 16384;
        __boot_stack_top = .;
    }

    . = ALIGN(4096);
    __kernel_end = .;
}
