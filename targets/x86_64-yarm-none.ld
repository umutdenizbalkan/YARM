/* SPDX-License-Identifier: Apache-2.0 */
/* Copyright 2026 Umut Deniz Balkan */

ENTRY(_start)

KERNEL_VIRT_BASE = 0xFFFFFFFF80000000;

PHDRS {
  ap       PT_LOAD FLAGS(5);
  note     PT_NOTE FLAGS(4);
  text_lo  PT_LOAD FLAGS(5);
  data_lo  PT_LOAD FLAGS(6);
  text_hi  PT_LOAD FLAGS(5);
  data_hi  PT_LOAD FLAGS(6);
}

SECTIONS {
  .ap_trampoline 0x7000 : AT(0x7000) {
    KEEP(*(.text.ap_trampoline))
  } :ap

  . = 0x100000;
  __kernel_phys_start = .;
  __kernel_low_start = .;

  .note.Xen : ALIGN(4) {
    KEEP(*(.note.Xen))
  } :note

  .text.boot32 : ALIGN(16) {
    *(.text.boot32)
  } :text_lo

  .text.boot64_thunk : ALIGN(16) {
    *(.text.boot64_thunk)
  } :text_lo

  .data.boot : ALIGN(4096) {
    *(.data.boot)
  } :data_lo

  .bss.bootstack (NOLOAD) : ALIGN(16) {
    *(.bss.bootstack)
  } :data_lo

  __kernel_low_end = .;

  . = . + KERNEL_VIRT_BASE;
  __kernel_start = .;
  __kernel_rx_start = .;

  .text.boot : AT(__kernel_low_end) ALIGN(16) {
    *(.text.boot)
  } :text_hi

  .text : ALIGN(16) {
    *(.text*)
  } :text_hi

  .rodata : ALIGN(16) {
    *(.rodata*)
  } :text_hi
  __kernel_rx_end = .;

  __kernel_rw_start = .;

  .data : ALIGN(16) {
    *(.data*)
  } :data_hi

  __bss_start = .;

  .bss.kernel_state (NOLOAD) : ALIGN(64) {
    *(.bss.kernel_state)
  } :data_hi

  .bss (NOLOAD) : ALIGN(16) {
    *(.bss*)
    *(COMMON)
  } :data_hi

  __bss_end = .;
  __kernel_rw_end = .;
  __kernel_end = .;

  __kernel_phys_end = __kernel_end - KERNEL_VIRT_BASE;

  /DISCARD/ : {
    *(.eh_frame*)
    *(.comment*)
    *(.note.GNU-stack)
  }
}
