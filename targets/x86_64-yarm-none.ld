/* SPDX-License-Identifier: Apache-2.0 */

ENTRY(_start)

PHDRS {
  note PT_NOTE FLAGS(4);
  text PT_LOAD FLAGS(5);
  data PT_LOAD FLAGS(6);
}

SECTIONS {
  . = 0x100000;
  __kernel_start = .;
  __kernel_rx_start = .;

  .note.Xen : ALIGN(4) {
    KEEP(*(.note.Xen))
  } :note

  .text.boot32 : ALIGN(16) {
    *(.text.boot32)
  } :text

  .text.boot : ALIGN(16) {
    *(.text.boot)
  } :text

  .text : ALIGN(16) {
    *(.text*)
  } :text

  .rodata : ALIGN(16) {
    *(.rodata*)
  } :text
  __kernel_rx_end = .;

  __kernel_rw_start = .;

  .data.boot : ALIGN(4096) {
    *(.data.boot)
  } :data

  .data : ALIGN(16) {
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
  __kernel_end = .;

  /DISCARD/ : {
    *(.eh_frame*)
    *(.comment*)
    *(.note.GNU-stack)
  }
}
