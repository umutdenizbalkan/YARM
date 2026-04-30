/* SPDX-License-Identifier: Apache-2.0 */
/* Copyright 2026 Umut Deniz Balkan */

ENTRY(_start)

USER_LOAD_BASE = 0x00400000;

PHDRS {
  text PT_LOAD FLAGS(5); /* R-X */
  data PT_LOAD FLAGS(6); /* RW- */
}

SECTIONS {
  . = USER_LOAD_BASE;

  .text : ALIGN(16) { *(.text*) } :text
  .rodata : ALIGN(16) { *(.rodata*) } :text
  .data : ALIGN(4096) { *(.data*) } :data
  .bss (NOLOAD) : ALIGN(16) { *(.bss*) *(COMMON) } :data

  /DISCARD/ : {
    *(.eh_frame*)
    *(.comment*)
    *(.note.GNU-stack)
  }
}
