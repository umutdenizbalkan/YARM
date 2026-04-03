// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#[cfg(not(feature = "hosted-dev"))]
use core::arch::global_asm;

#[cfg(not(feature = "hosted-dev"))]
global_asm!(
    r#"
    .intel_syntax noprefix

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
    .zero 16
    .quad boot_pd_hi + 0x3
    .zero 4064

    .align 4096
boot_pd:
    // Bootstrap identity map:
    // - first 2MiB via 4KiB PTEs for early transition flexibility
    // - 2MiB..64MiB via 2MiB executable pages to tolerate firmware/kernel placement
    .set page_flags_pt, 0x03
    .set page_flags_exec, 0x83
    .quad boot_pt0 + page_flags_pt
    .set page_index, 1
    .rept 31
    .quad (page_index * 0x200000) | page_flags_exec
    .set page_index, page_index + 1
    .endr
    .zero 3840

    .align 4096
boot_pt0:
    .set pte_flags_exec, 0x003
    // Keep the first 2MiB executable until long-mode handoff is complete.
    .set pte_index, 0
    .rept 512
    .quad (pte_index * 0x1000) | pte_flags_exec
    .set pte_index, pte_index + 1
    .endr

    .align 4096
boot_pd_hi:
    // Bootstrap identity map for high MMIO space used during early x86_64 init.
    // Map the default xAPIC + IOAPIC windows:
    //   0xFEC0_0000..0xFEDF_FFFF  (PD idx 502, 2MiB page)
    //   0xFEE0_0000..0xFEFF_FFFF  (PD idx 503, 2MiB page)
    // Mark APIC MMIO mappings uncacheable (PCD=1) for architectural correctness.
    .set hi_page_flags_exec, 0x93
    .zero 4016
    .quad 0xFEC00000 | hi_page_flags_exec
    .quad 0xFEE00000 | hi_page_flags_exec
    .zero 64

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

    .align 16
boot_idt:
    .rept 256
    .word emergency_idt_stub
    .word 0x08
    .byte 0
    .byte 0x8E
    .word emergency_idt_stub >> 16
    .long emergency_idt_stub >> 32
    .long 0
    .endr
boot_idt_end:

boot_idt_ptr:
    .word boot_idt_end - boot_idt - 1
    .quad boot_idt

    .section .text.boot32,"ax",@progbits
    .code32
    .global pvh_start32
    .type pvh_start32,@function
pvh_start32:
    cli
    mov al, 0xFF
    out 0x21, al
    out 0xA1, al
    mov esi, ebx

    // Zero .bss/.common for static mut / atomics expected to start at 0.
    mov edi, offset __bss_start
    mov ecx, offset __bss_end
    sub ecx, edi
    xor eax, eax
    mov edx, ecx
    shr ecx, 2
    rep stosl
    mov ecx, edx
    and ecx, 3
    rep stosb

    mov esp, offset boot_stack_end

    // FIX: The assembler/linker fills the sub-table pointer quads with full
    // 64-bit *virtual* addresses.  The CPU uses those bits as *physical*
    // addresses during a page-table walk, so a high-half VMA in the upper 32
    // bits causes an immediate page fault the first time paging is enabled.
    // Under our link convention (LMA == VMA & 0xFFFFFFFF) the lower 32 bits
    // already hold the correct physical address; we only need to zero the
    // upper 32 bits of each sub-table pointer entry before loading CR3.
    mov dword ptr [boot_pml4 + 4], 0   // PML4[0]  upper dword → physical
    mov dword ptr [boot_pdpt + 4], 0   // PDPT[0]  upper dword → physical
    mov dword ptr [boot_pdpt + 28], 0  // PDPT[3]  upper dword → physical
    mov dword ptr [boot_pd   + 4], 0   // PD[0]    upper dword → physical
    mov dword ptr [boot_pd_hi + 4], 0  // PD_HI[0] upper dword → physical
    // The 2 MiB entries in boot_pd and all boot_pt0 entries are assembled
    // from small immediates whose upper 32 bits are already zero.

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
    lidt [rip + boot_idt_ptr]
    lea rsp, [rip + boot_stack_end]
    xor rbp, rbp
    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov dil, 'E'
    call uart_putc64
    mov dil, 'F'
    call uart_putc64
    // FIX: move start-info pointer into rdi (first argument) *after* the
    // last uart_putc64 call that uses dil, so we don't clobber its low byte.
    mov edi, esi
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

emergency_idt_stub:
    cli
1:
    hlt
    jmp 1b
    "#
);
