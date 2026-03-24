#[cfg(not(feature = "hosted-dev"))]
use core::arch::global_asm;

#[cfg(not(feature = "hosted-dev"))]
global_asm!(
    r#"
    .section .note.Xen,"a",@note
    .align 4
    .long 4
    .long 4
    .long 18
    .asciz "Xen"
    .align 4
    .long pvh_start32

    .section .bss.bootstack,"aw",@nobits
    .align 16
boot_stack:
    .skip 16384
boot_stack_end:

    .section .data.boot,"aw",@progbits
    .align 4096
boot_pml4:
    .quad boot_pdpt + 0x3
    .zero 4088

    .align 4096
boot_pdpt:
    .quad boot_pd + 0x3
    .zero 4088

    .align 4096
boot_pd:
    .set page_flags, 0x83
    .rept 256
    .quad (. - boot_pd) + page_flags
    .endr
    .zero 2048

    .align 8
gdt64:
    .quad 0x0000000000000000
    .quad 0x00af9a000000ffff
    .quad 0x00af92000000ffff
gdt64_end:

gdt64_ptr:
    .word gdt64_end - gdt64 - 1
    .long gdt64
    .long 0

    .section .text.boot32,"ax",@progbits
    .code32
    .global pvh_start32
    .type pvh_start32,@function
pvh_start32:
    cli
    mov esi, ebx
    mov esp, offset boot_stack_end
    mov eax, offset boot_pml4
    mov cr3, eax
    mov eax, cr4
    or eax, 0x20
    mov cr4, eax
    mov ecx, 0xC0000080
    rdmsr
    or eax, 0x100
    wrmsr
    lgdt [gdt64_ptr]
    mov eax, cr0
    or eax, 0x80000001
    mov cr0, eax
    push 0x08
    mov eax, offset long_mode_entry
    push eax
    retf

    .section .text.boot,"ax",@progbits
    .code64

    .weak _start
    .type _start,@function
_start:
long_mode_entry:
    cli
    lea rsp, [rip + boot_stack_end]
    xor rbp, rbp
    mov edi, esi
    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov ss, ax
    .weak yarm_kernel_main
    call yarm_kernel_main
1:
    hlt
    jmp 1b
    "#
);
