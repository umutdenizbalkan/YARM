ENTRY(_start)

PHDRS {
  note PT_NOTE FLAGS(4);
  text PT_LOAD FLAGS(5);
  data PT_LOAD FLAGS(6);
}

SECTIONS {
  . = 0x100000;

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

  .data.boot : ALIGN(4096) {
    *(.data.boot)
  } :data

  .data : ALIGN(16) {
    *(.data*)
  } :data

  .bss.bootstack (NOLOAD) : ALIGN(16) {
    *(.bss.bootstack)
  } :data

  .bss (NOLOAD) : ALIGN(16) {
    *(.bss*)
    *(COMMON)
  } :data

  /DISCARD/ : {
    *(.eh_frame*)
    *(.comment*)
    *(.note.GNU-stack)
  }
}
