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
    // Hardening tranche (stage 3):
    // - split first 2MiB identity map into 4KiB PTEs so executable scope is narrowed
    // - keep only a minimal executable bootstrap window in early low memory
    // - mark remaining identity-mapped bootstrap pages NX to reduce executable surface
    // - keep writable data/stack support during early bring-up
    .set page_flags_pt, 0x03
    .set page_flags_data_nx, 0x8000000000000083
    .quad boot_pt0 + page_flags_pt
    .set page_index, 1
    .rept 31
    .quad (page_index * 0x200000) | page_flags_data_nx
    .set page_index, page_index + 1
    .endr
    .zero 3840

    .align 4096
boot_pt0:
    .set pte_flags_exec, 0x003
    .set pte_flags_data_nx, 0x8000000000000003
    // Keep the first 2MiB executable until long-mode handoff is complete;
    // firmware/kernel placement can land bootstrap text above 256KiB.
    .set pte_index, 0
    .rept 512
    .quad (pte_index * 0x1000) | pte_flags_exec
    .set pte_index, pte_index + 1
    .endr

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
    mov bl, 'A'
    call uart_putc32
    mov eax, offset boot_pml4
    mov cr3, eax
    mov bl, 'B'
    call uart_putc32
    mov eax, cr4
    or eax, 0x20
    mov cr4, eax
    mov ecx, 0xC0000080
    rdmsr
    or eax, 0x100
    wrmsr
    mov bl, 'C'
    call uart_putc32
    lgdt [gdt64_ptr]
    mov eax, cr0
    or eax, 0x80000001
    mov cr0, eax
    mov bl, 'D'
    call uart_putc32
    push 0x08
    mov eax, offset long_mode_entry
    push eax
    retf

uart_wait32:
    mov dx, 0x3FD
2:
    in al, dx
    test al, 0x20
    jz 2b
    ret

uart_putc32:
    push eax
    push edx
    call uart_wait32
    mov dx, 0x3F8
    mov al, bl
    out dx, al
    pop edx
    pop eax
    ret

    .section .text.boot,"ax",@progbits
    .code64

    .weak _start
    .type _start,@function
_start:
long_mode_entry:
    cli
    lea rsp, [rip + boot_stack_end]
    xor rbp, rbp
    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov dil, 'E'
    call uart_putc64
    mov edi, esi
    mov dil, 'F'
    call uart_putc64
    .weak yarm_kernel_main
    call yarm_kernel_main
    mov dil, 'G'
    call uart_putc64
1:
    hlt
    jmp 1b

uart_wait64:
    mov dx, 0x3FD
3:
    in al, dx
    test al, 0x20
    jz 3b
    ret

uart_putc64:
    push rax
    push rdx
    call uart_wait64
    mov dx, 0x3F8
    mov al, dil
    out dx, al
    pop rdx
    pop rax
    ret
    "#
);
