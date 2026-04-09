// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

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
    // Early bootstrap stack used until Rust runtime/state setup completes.
    // KernelState is placed in dedicated .bss storage, so this stack no longer
    // needs to reserve tens of MiB for by-value KernelState construction.
    .skip 0x00200000
boot_stack_end:

    .section .data.boot,"aw",@progbits
    .align 4096
boot_pml4:
    .quad boot_pdpt_low + 0x3
    .zero 2040
    // Mirror physical memory into the higher-half direct-map window.
    .quad boot_pdpt_direct + 0x3
    .zero 2032

    .align 4096
boot_pdpt_low:
    .quad boot_pd + 0x3
    .zero 16
    .quad boot_pd_hi + 0x3
    .zero 4064

    .align 4096
boot_pdpt_direct:
    // 512 * 1GiB = 512GiB of higher-half direct physical mapping.
    // This gives page-table code a stable virtual alias for PT pages allocated
    // anywhere in early-boot RAM (up to the direct-map span).
    .set direct_map_page_flags, 0x83
    .set direct_map_index, 0
    .rept 512
    .quad (direct_map_index * 0x40000000) | direct_map_page_flags
    .set direct_map_index, direct_map_index + 1
    .endr

    .align 4096
boot_pd:
    // Bootstrap identity map:
    // - first 2MiB via 4KiB PTEs for early transition flexibility
    // - 2MiB..64MiB via 2MiB executable pages to tolerate firmware/kernel placement
    // NOTE: W^X hardening is completed later once the final kernel page tables
    // are installed and CR3 is switched away from this bootstrap map.
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
    .zero 4096
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
    // Remap legacy 8259 PIC vectors away from CPU exception slots.
    // Master: 0x20..0x27, Slave: 0x28..0x2F.
    mov al, 0x11
    out 0x20, al
    out 0xA0, al
    mov al, 0x20
    out 0x21, al
    mov al, 0x28
    out 0xA1, al
    mov al, 0x04
    out 0x21, al
    mov al, 0x02
    out 0xA1, al
    mov al, 0x01
    out 0x21, al
    out 0xA1, al
    // Mask all PIC IRQ inputs; LAPIC/IOAPIC takeover happens later.
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
    rep stosd
    mov ecx, edx
    and ecx, 3
    rep stosb

    mov esp, offset boot_stack_end

    // FIXUP: The assembler/linker may emit full 64-bit *virtual* addresses
    // for page-table *pointer* entries. During the hardware walk these bits
    // are interpreted as physical addresses, so we must clear the upper dword
    // on pointer entries before loading CR3.
    //
    // We intentionally do NOT patch bootstrap 2MiB leaf mappings (boot_pd and
    // boot_pd_hi data leaves). Their values are emitted from 32-bit immediates
    // here, so upper dwords are already correct under the low-physical boot map.
    mov dword ptr [boot_pml4 + 4], 0   // PML4[0]  upper dword → physical
    mov dword ptr [boot_pml4 + 2052], 0 // PML4[256] upper dword → physical
    mov dword ptr [boot_pdpt_low + 4], 0   // PDPT[0]  upper dword → physical
    mov dword ptr [boot_pdpt_low + 28], 0  // PDPT[3]  upper dword → physical
    mov dword ptr [boot_pd   + 4], 0   // PD[0]    upper dword → physical
    // The 2 MiB entries in boot_pd_hi at indices 502 and 503 are assembled
    // from low 32-bit immediates; their upper 32 bits are already zero.

    mov bl, 'A'
    call uart_putc32
    mov eax, offset boot_pml4
    mov cr3, eax
    mov bl, 'B'
    call uart_putc32
    mov eax, cr4
    // Enable baseline long-mode prerequisites and SSE support:
    // - CR4.PAE (bit 5)
    // - CR4.OSFXSR (bit 9)
    // - CR4.OSXMMEXCPT (bit 10)
    or eax, 0x620
    // Conditionally enable supervisor protections when CPUID advertises them:
    // - CR4.SMEP (bit 20) => CPUID.(EAX=7,ECX=0):EBX[7]
    // - CR4.SMAP (bit 21) => CPUID.(EAX=7,ECX=0):EBX[20]
    push eax
    mov eax, 7
    xor ecx, ecx
    cpuid
    pop eax
    test ebx, 0x80
    jz 3f
    or eax, 0x100000
3:
    test ebx, 0x100000
    jz 4f
    or eax, 0x200000
4:
    mov cr4, eax
    mov ecx, 0xC0000080
    rdmsr
    // Enable SYSCALL:
    // - EFER.SCE (bit 8)
    or eax, 0x100
    // Conditionally enable EFER.NXE (bit 11) when supported:
    // CPUID.(EAX=0x80000001):EDX[20]
    mov edi, eax
    mov eax, 0x80000000
    cpuid
    cmp eax, 0x80000001
    jb 5f
    mov eax, 0x80000001
    cpuid
    test edx, 0x100000
    jz 5f
    or edi, 0x800
5:
    mov ecx, 0xC0000080
    mov eax, edi
    wrmsr
    mov bl, 'C'
    call uart_putc32
    lgdt [gdt64_ptr]
    mov eax, cr0
    // Enable paging/protected mode, set CR0.MP, and clear CR0.EM so
    // x87/SSE instructions (e.g. xorps/movups emitted by Rust) are valid.
    and eax, 0xFFFFFFFB
    or eax, 0x80000003
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
    // Populate a minimal catch-all IDT in-memory: all 256 gates -> emergency_idt_stub.
    lea r8, [rip + emergency_idt_stub]
    lea rdi, [rip + boot_idt]
    mov ecx, 256
0:
    mov word ptr [rdi + 0], r8w
    mov word ptr [rdi + 2], 0x08
    mov byte ptr [rdi + 4], 0
    mov byte ptr [rdi + 5], 0x8E
    mov rax, r8
    shr rax, 16
    mov word ptr [rdi + 6], ax
    mov rax, r8
    shr rax, 32
    mov dword ptr [rdi + 8], eax
    mov dword ptr [rdi + 12], 0
    add rdi, 16
    dec ecx
    jnz 0b
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

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const RING3_INIT_SERVER_TID: u64 = 1;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const RING3_INIT_SERVER_ENTRY: u64 = 0x0040_1000;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const RING3_INIT_SERVER_CODE_PAGE: u64 = 0x0040_0000;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const RING3_INIT_SERVER_ASID: u16 = 1;

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
pub fn bootstrap_first_user_task(
    kernel: &mut crate::kernel::boot::KernelState,
) -> Result<(), crate::kernel::boot::KernelError> {
    use crate::kernel::boot::UserImageSpec;
    use crate::kernel::task::TaskClass;
    use crate::kernel::vm::{PageFlags, VirtAddr};

    if kernel.task_asid(RING3_INIT_SERVER_TID).is_some() {
        return Ok(());
    }

    let (asid, aspace_cap) = kernel.create_user_address_space()?;
    if asid.0 != RING3_INIT_SERVER_ASID {
        return Err(crate::kernel::boot::KernelError::WrongObject);
    }
    kernel.spawn_user_task_from_image(UserImageSpec {
        tid: RING3_INIT_SERVER_TID,
        entry: RING3_INIT_SERVER_ENTRY as usize,
        asid: Some(asid),
        class: TaskClass::SystemServer,
    })?;

    let (_mem_id, mem_cap) = kernel.alloc_anonymous_memory_object()?;
    kernel.map_user_page_with_caps(
        aspace_cap,
        mem_cap,
        VirtAddr(RING3_INIT_SERVER_CODE_PAGE),
        PageFlags::USER_RW,
    )?;

    // mov eax, SYSCALL_YIELD_NR ; int 0x80 ; jmp $
    let code: [u8; 9] = [0xB8, 0x00, 0x00, 0x00, 0x00, 0xCD, 0x80, 0xEB, 0xFE];
    kernel.write_user_memory(
        RING3_INIT_SERVER_TID,
        RING3_INIT_SERVER_ENTRY as usize,
        &code,
    )?;
    let _ = kernel.protect_user_page(
        aspace_cap,
        VirtAddr(RING3_INIT_SERVER_CODE_PAGE),
        PageFlags::USER_RX,
    )?;
    Ok(())
}

#[cfg(any(feature = "hosted-dev", not(target_arch = "x86_64")))]
pub fn bootstrap_first_user_task(
    _kernel: &mut crate::kernel::boot::KernelState,
) -> Result<(), crate::kernel::boot::KernelError> {
    Ok(())
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
pub fn enter_dispatched_user_task_if_available(
    kernel: &crate::kernel::boot::KernelState,
    dispatched_tid: Option<u64>,
) {
    if let Some(tid) = dispatched_tid
        && let Some(context) = kernel.thread_user_context(tid)
        && context.instruction_ptr.0 != 0
        && context.stack_ptr.0 != 0
    {
        crate::yarm_log!(
            "YARM_RING3_INIT_TASK tid={} entry=0x{:x} stack_top=0x{:x}",
            tid,
            context.instruction_ptr.0,
            context.stack_ptr.0
        );
        super::descriptor_tables::enter_user_mode_iret(
            context.instruction_ptr.0,
            context.stack_ptr.0,
            context.arg0 as u64,
            context.arg1 as u64,
        );
    }
}

#[cfg(any(feature = "hosted-dev", not(target_arch = "x86_64")))]
pub fn enter_dispatched_user_task_if_available(
    _kernel: &crate::kernel::boot::KernelState,
    _dispatched_tid: Option<u64>,
) {
}
