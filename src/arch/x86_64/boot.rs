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

    // ===========================================================
    // Low-VA boot data: page tables, GDT64 + descriptor, and a
    // tiny dedicated 4 KiB stack used while running in 32-bit
    // protected mode.  All these symbols are referenced by 32-bit
    // immediate operands in pvh_start32 below, so they MUST stay
    // in the low-VA prefix declared by the linker script.
    // ===========================================================

    .section .data.boot,"aw",@progbits
    .align 4096
boot_pml4:
    // PML4[0]   -> boot_pdpt_low   : low identity for first 4 GiB.
    .quad boot_pdpt_low + 0x3
    // PML4[1..510] -> null.
    .zero 4080
    // PML4[511] -> boot_pdpt_direct : higher-half kernel window.
    .quad boot_pdpt_direct + 0x3

    .align 4096
boot_pdpt_low:
    .quad boot_pd + 0x3
    .zero 16
    .quad boot_pd_hi + 0x3
    .zero 4064

    .align 4096
boot_pdpt_direct:
    // Higher-half mapping rooted at PML4[511] = 0xFFFF_FF80_0000_0000:
    //   PDPT[0..509] -> 1 GiB direct identity huge pages
    //                   (PA 0..510 GiB at VA 0xFFFF_FF80_0000_0000+).
    //                   Used as the bootstrap PA->VA direct map.
    //   PDPT[510] -> boot_pd      : low identity 0..64 MiB at
    //                                VA 0xFFFF_FFFF_8000_0000+.
    //                                THIS WINDOW HOSTS THE KERNEL IMAGE.
    //   PDPT[511] -> boot_pd_hi   : 3..4 GiB at
    //                                VA 0xFFFF_FFFF_C000_0000+ (PCD set,
    //                                covers LAPIC/IOAPIC MMIO).
    .set direct_map_page_flags, 0x83
    .set direct_map_index, 0
    .rept 510
    .quad (direct_map_index * 0x40000000) | direct_map_page_flags
    .set direct_map_index, direct_map_index + 1
    .endr
    .quad boot_pd + 0x3
    .quad boot_pd_hi + 0x3

    .align 4096
boot_pd:
    // Bootstrap identity map for first 64 MiB.
    // - first 2 MiB via 4 KiB PTEs (boot_pt0)
    // - 2..64 MiB via 2 MiB executable pages
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
    .set pte_index, 0
    .rept 512
    .quad (pte_index * 0x1000) | pte_flags_exec
    .set pte_index, pte_index + 1
    .endr

    .align 4096
boot_pd_hi:
    // Bootstrap identity map for the 3..4 GiB window with PCD (uncached)
    // so LAPIC/IOAPIC MMIO at 0xFE??_???? is reachable as
    // 0xFFFF_FFFF_FE??_???? without needing a separate fixmap.
    .set hi_page_flags_exec, 0x93
    .set hi_page_index, 0
    .rept 512
    .quad (0xC0000000 + (hi_page_index * 0x200000)) | hi_page_flags_exec
    .set hi_page_index, hi_page_index + 1
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

    // 4 KiB stack used only while running in 32-bit protected mode
    // (until the long-mode handoff sets RSP to the high alias of
    // boot_stack).  Keeping this in .data.boot means its symbol value
    // fits in a 32-bit immediate, which is required for `mov esp, ...`.
    .align 16
pmode_boot_stack:
    .skip 0x1000
pmode_boot_stack_end:

    // ===========================================================
    // Low-VA boot stack.  Lives in .bss.bootstack which the linker
    // script keeps in the low-VA prefix.  Referenced from the
    // long-mode entry by `movabs rsp, offset boot_stack_end` and
    // OR'd with KERNEL_VIRT_BASE to obtain the high alias.
    // ===========================================================

    .section .bss.bootstack,"aw",@nobits
    .align 16
boot_stack:
    .skip 0x01000000
boot_stack_end:

    // ===========================================================
    // pvh_start32 — runs in 32-bit protected mode at LOW VA.
    // ===========================================================

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
    // Preserve PVH start_info ptr.
    mov esi, ebx

    // BSS zeroing is deferred to long_mode_entry where 64-bit
    // immediates are available; __bss_start / __bss_end live in
    // the high-VA suffix and won't fit in a 32-bit immediate here.

    mov esp, offset pmode_boot_stack_end

    // FIXUP: rebuild boot_pml4 / boot_pdpt_* pointer entries from
    // their actual runtime addresses (the assembler-time .quad
    // forms above use symbol values that are correct for the LMA
    // we are loaded at, but writing them again with `or 0x3` makes
    // the present + RW + user flags explicit and keeps the entries
    // robust against minor relocations of .data.boot).
    mov eax, offset boot_pdpt_low
    or eax, 0x3
    mov dword ptr [boot_pml4 + 0], eax
    mov dword ptr [boot_pml4 + 4], 0

    mov eax, offset boot_pdpt_direct
    or eax, 0x3
    mov dword ptr [boot_pml4 + 4088], eax
    mov dword ptr [boot_pml4 + 4092], 0

    mov eax, offset boot_pd
    or eax, 0x3
    mov dword ptr [boot_pdpt_low + 0], eax
    mov dword ptr [boot_pdpt_low + 4], 0
    mov dword ptr [boot_pdpt_direct + 4080], eax
    mov dword ptr [boot_pdpt_direct + 4084], 0

    mov eax, offset boot_pd_hi
    or eax, 0x3
    mov dword ptr [boot_pdpt_low + 24], eax
    mov dword ptr [boot_pdpt_low + 28], 0
    mov dword ptr [boot_pdpt_direct + 4088], eax
    mov dword ptr [boot_pdpt_direct + 4092], 0

    mov eax, offset boot_pt0
    or eax, 0x3
    mov dword ptr [boot_pd + 0], eax
    mov dword ptr [boot_pd + 4], 0

    mov bl, 'A'
    call uart_putc32
    mov eax, offset boot_pml4
    mov cr3, eax
    mov bl, 'B'
    call uart_putc32

    mov eax, cr4
    // CR4.PAE (bit 5), CR4.OSFXSR (bit 9), CR4.OSXMMEXCPT (bit 10)
    or eax, 0x620
    // Conditionally enable CR4.SMEP and CR4.SMAP when CPUID advertises them.
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
    // EFER.SCE (bit 8) — enable SYSCALL/SYSRET.
    or eax, 0x100
    // Conditionally enable EFER.NXE (bit 11).
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
    // Clear CR0.EM, set CR0.PE | CR0.MP | CR0.PG.
    and eax, 0xFFFFFFFB
    or eax, 0x80000003
    mov cr0, eax

    // Far-return into the LOW-VA 64-bit thunk.  long_mode_low_thunk's
    // symbol value fits in a 32-bit immediate because it lives in
    // the .text.boot64_thunk low-VA section.
    push 0x08
    mov eax, offset long_mode_low_thunk
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

    // ===========================================================
    // .text.boot64_thunk — 64-bit code at LOW VA.
    //
    // pvh_start32 far-returns here in long mode.  The thunk uses
    // a 64-bit movabs to load the high-VA absolute address of
    // long_mode_entry and jumps to it.  After this jump every
    // RIP-relative call resolves through PML4[511]/PDPT[510] (the
    // high alias of the kernel image), so subsequent CR3 switches
    // that drop PML4[0] do not strand the kernel.
    // ===========================================================

    .section .text.boot64_thunk,"ax",@progbits
    .code64
    .global long_mode_low_thunk
    .type long_mode_low_thunk,@function
long_mode_low_thunk:
    cli
    // Diagnostic 'D': blast a single byte to the UART without waiting
    // for the line-status register.  At QEMU speeds the previous 'C'
    // byte has already drained, so this almost always lands.
    mov dx, 0x3F8
    mov al, 'D'
    out dx, al

    movabs rax, offset long_mode_entry
    jmp rax

    // ===========================================================
    // .text.boot — 64-bit code at HIGH VA.  Reached only via the
    // thunk above.  All RIP-relative references from here resolve
    // to high-VA symbols.
    // ===========================================================

    .section .text.boot,"ax",@progbits
    .code64

    .weak _start
    .type _start,@function
_start:
    .global long_mode_entry
    .type long_mode_entry,@function
long_mode_entry:
    cli

    // Establish a high-VA stack.  boot_stack_end lives at LOW VA in
    // .bss.bootstack; OR with KERNEL_VIRT_BASE = 0xFFFF_FFFF_8000_0000
    // to obtain the high alias mapped via PML4[511]/PDPT[510]/boot_pd.
    movabs rsp, offset boot_stack_end
    movabs rax, 0xFFFFFFFF80000000
    or rsp, rax

    // Zero kernel BSS now that we have a stack and 64-bit immediates
    // are available.  __bss_start / __bss_end are linker symbols in
    // the high-VA suffix.
    movabs rdi, offset __bss_start
    movabs rcx, offset __bss_end
    sub rcx, rdi
    xor eax, eax
    rep stosb

    // Re-establish a stack pointer because the rep stosb above does
    // not touch RSP, but be conservative and ensure stack is still
    // aligned for the upcoming call.
    and rsp, -16

    // Build a minimal catch-all IDT in memory: all 256 gates point at
    // emergency_idt_stub.  boot_idt and boot_idt_ptr live in the
    // high-VA .data section, so RIP-relative LEA from high RIP works.
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

    xor rbp, rbp
    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov ss, ax

    mov dil, 'E'
    call uart_putc64
    mov dil, 'F'
    call uart_putc64

    // Move PVH start_info pointer into rdi (first argument).  esi
    // holds the low-PA pointer (always < 4 GiB), so a 32-bit move
    // is sufficient and zero-extends the high half.
    mov edi, esi
    .weak yarm_kernel_main
    call yarm_kernel_main
    mov dil, 'G'
    call uart_putc64
1:
    hlt
    jmp 1b

    // ===========================================================
    // Default high-linked .text and .data: utility helpers and
    // statics referenced from long_mode_entry.
    // ===========================================================

    .text
    .code64
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

    .data
    .align 16
boot_idt:
    .zero 4096
boot_idt_end:

boot_idt_ptr:
    .word boot_idt_end - boot_idt - 1
    .quad boot_idt
    "#
);


#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const RING3_INIT_SERVER_TID: u64 = 1;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const INITRAMFS_HELLO_WORLD_IMAGE_ID: u64 = 0x494E_4954_5848_454C; // "INITXHEL"
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const AP_STACK_PHYS_BASE: u64 = 0x0200_0000; // 32 MiB, above kernel image+BSS, below 64 MiB identity limit.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const AP_STACK_BYTES: u64 = 16 * 1024;

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
fn initramfs_static_hello_world_elf() -> [u8; 256] {
    let mut image = [0u8; 256];
    // ELF header.
    image[..4].copy_from_slice(b"\x7FELF");
    image[4] = 2; // ELFCLASS64
    image[5] = 1; // little-endian
    image[6] = 1; // EV_CURRENT
    image[7] = 0; // SYSV ABI
    image[16..18].copy_from_slice(&2u16.to_le_bytes()); // ET_EXEC
    image[18..20].copy_from_slice(&0x3Eu16.to_le_bytes()); // EM_X86_64
    image[20..24].copy_from_slice(&1u32.to_le_bytes()); // EV_CURRENT
    let entry = 0x0040_1000u64;
    image[24..32].copy_from_slice(&entry.to_le_bytes());
    image[32..40].copy_from_slice(&64u64.to_le_bytes()); // e_phoff
    image[52..54].copy_from_slice(&(64u16).to_le_bytes()); // e_ehsize
    image[54..56].copy_from_slice(&(56u16).to_le_bytes()); // e_phentsize
    image[56..58].copy_from_slice(&(1u16).to_le_bytes()); // e_phnum

    // Single PT_LOAD segment.
    let ph = 64usize;
    image[ph..ph + 4].copy_from_slice(&1u32.to_le_bytes()); // PT_LOAD
    image[ph + 4..ph + 8].copy_from_slice(&5u32.to_le_bytes()); // RX
    image[ph + 8..ph + 16].copy_from_slice(&128u64.to_le_bytes()); // p_offset
    image[ph + 16..ph + 24].copy_from_slice(&(entry & !0xFFF).to_le_bytes()); // p_vaddr
    image[ph + 24..ph + 32].copy_from_slice(&0u64.to_le_bytes()); // p_paddr
    image[ph + 32..ph + 40].copy_from_slice(&9u64.to_le_bytes()); // p_filesz
    image[ph + 40..ph + 48].copy_from_slice(&16u64.to_le_bytes()); // p_memsz
    image[ph + 48..ph + 56].copy_from_slice(&0x1000u64.to_le_bytes()); // p_align

    // mov eax, SYSCALL_YIELD_NR; syscall; jmp syscall.
    // Use the production LSTAR syscall fast path for bring-up.
    image[128..137].copy_from_slice(&[0xB8, 0x00, 0x00, 0x00, 0x00, 0x0F, 0x05, 0xEB, 0xFC]);
    image
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const INITRD_INIT_ELF_MAX_SIZE: usize = 16 * 1024 * 1024;

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
fn load_init_elf_from_initramfs_vfs() -> Option<alloc::vec::Vec<u8>> {
    let bytes = crate::kernel::boot::Bootstrap::boot_initrd_bytes()?;
    let entry = yarm_srv_common::cpio::CpioArchive::new(bytes)
        .find("/init")
        .ok()
        .flatten()
        .or_else(|| yarm_srv_common::cpio::CpioArchive::new(bytes).find("init").ok().flatten())?;
    let file_data = entry.file_data();
    crate::yarm_log!("YARM_INITRD_INIT_FOUND len={}", file_data.len());
    if file_data.len() > INITRD_INIT_ELF_MAX_SIZE {
        crate::yarm_log!(
            "YARM_INITRD_INIT_TOO_LARGE len={} cap={}",
            file_data.len(),
            INITRD_INIT_ELF_MAX_SIZE
        );
        return None;
    }
    Some(alloc::vec::Vec::from(file_data))
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
pub fn bootstrap_first_user_task(
    kernel: &mut crate::kernel::boot::KernelState,
) -> Result<(), crate::kernel::boot::KernelError> {
    use crate::kernel::boot::UserImageSpec;
    use crate::kernel::task::TaskClass;
    crate::yarm_log!("BOOTSTRAP_STAGE: begin");

    if kernel.task_asid(RING3_INIT_SERVER_TID).is_some() {
        return Ok(());
    }

    let (asid, _aspace_cap) = match kernel.create_user_address_space() {
        Ok(value) => value,
        Err(err) => {
            crate::yarm_log!("BOOTSTRAP_ERROR: {:?}", err);
            return Err(err);
        }
    };
    let image = load_init_elf_from_initramfs_vfs();
    let fallback = initramfs_static_hello_world_elf();
    let (image_bytes, source, fallback_reason): (&[u8], &str, Option<&str>) =
        match image.as_deref() {
            Some(initrd_image) => (initrd_image, "initrd", None),
            None => (&fallback, "synthetic", Some("missing_or_invalid_initrd_init")),
        };
    let (entry, heap_base) = match kernel.load_elf_pt_load_segments(asid, image_bytes) {
        Ok(result) => {
            crate::yarm_log!("BOOTSTRAP_STAGE: after ELF load");
            crate::yarm_log!("BOOTSTRAP_STAGE: after copy_to_user");
            result
        }
        Err(err) => {
            crate::yarm_log!("BOOTSTRAP_ERROR: {:?}", err);
            return Err(err);
        }
    };
    match source {
        "initrd" => crate::yarm_log!("YARM_INITRD_INIT_ELF_SELECTED entry=0x{:x}", entry),
        _ => crate::yarm_log!("YARM_SYNTHETIC_INIT_ELF_SELECTED entry=0x{:x}", entry),
    }
    if let Some(reason) = fallback_reason {
        crate::yarm_log!(
            "YARM_FIRST_USER_IMAGE_SOURCE source={} len={} reason={}",
            source,
            image_bytes.len(),
            reason
        );
    } else {
        crate::yarm_log!(
            "YARM_FIRST_USER_IMAGE_SOURCE source={} len={}",
            source,
            image_bytes.len()
        );
    }
    crate::yarm_log!("BOOTSTRAP_STAGE: before stack allocation");
    kernel.register_task_with_class(RING3_INIT_SERVER_TID, TaskClass::SystemServer)?;
    let (_pm_eid, pm_send_cap_root, pm_recv_cap_root) = kernel.create_endpoint(8)?;
    let pm_request_send_init = kernel.grant_capability_task_to_task_with_rights(
        0,
        pm_send_cap_root,
        RING3_INIT_SERVER_TID,
        crate::kernel::capabilities::CapRights::SEND,
    )?;
    let pm_reply_recv_init = kernel.grant_capability_task_to_task_with_rights(
        0,
        pm_recv_cap_root,
        RING3_INIT_SERVER_TID,
        crate::kernel::capabilities::CapRights::RECEIVE,
    )?;
    let (_sup_eid, _sup_send_root, sup_fault_recv_root) = kernel.create_endpoint(8)?;
    let supervisor_fault_recv_init = kernel.grant_capability_task_to_task_with_rights(
        0,
        sup_fault_recv_root,
        RING3_INIT_SERVER_TID,
        crate::kernel::capabilities::CapRights::RECEIVE,
    )?;
    kernel.set_supervisor_endpoint_for_task(RING3_INIT_SERVER_TID, supervisor_fault_recv_init)?;
    let mut startup_args = UserImageSpec::DEFAULT_STARTUP_ARGS;
    startup_args[0] = RING3_INIT_SERVER_TID;
    startup_args[1] = pm_request_send_init.0;
    startup_args[2] = pm_reply_recv_init.0;
    if startup_args.len() > 3 {
        startup_args[3] = supervisor_fault_recv_init.0;
    }
    crate::yarm_log!(
        "YARM_FIRST_USER_STARTUP_ARGS tid={} arg0={} arg1={} arg2={} arg3={}",
        RING3_INIT_SERVER_TID,
        startup_args[0],
        startup_args[1],
        startup_args[2],
        startup_args[3]
    );
    match kernel.spawn_user_task_from_image(UserImageSpec {
        tid: RING3_INIT_SERVER_TID,
        entry,
        asid: Some(asid),
        class: TaskClass::SystemServer,
        startup_args,
    }) {
        Ok(_) => crate::yarm_log!("BOOTSTRAP_STAGE: after stack allocation"),
        Err(err) => {
            crate::yarm_log!("BOOTSTRAP_ERROR: {:?}", err);
            return Err(err);
        }
    }
    kernel.set_task_brk_bounds(RING3_INIT_SERVER_TID, heap_base, heap_base)?;
    let (phase, image_id) = if source == "initrd" {
        ("initrd_init_elf", 0x494e495448454c4fu64)
    } else {
        ("kernel_static_init_elf", INITRAMFS_HELLO_WORLD_IMAGE_ID)
    };
    crate::yarm_log!(
        "YARM_INIT_DONE arch=x86_64 phase={} image_id=0x{:x} seeded=0 initramfs_handled=1 devfs_handled=0",
        phase,
        image_id
    );
    Ok(())
}

#[cfg(any(feature = "hosted-dev", not(target_arch = "x86_64")))]
pub fn bootstrap_first_user_task(
    _kernel: &mut crate::kernel::boot::KernelState,
) -> Result<(), crate::kernel::boot::KernelError> {
    Ok(())
}

pub fn release_secondary_cpus_after_bootstrap() {}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
pub fn enter_dispatched_user_task_if_available(
    kernel: &crate::kernel::boot::KernelState,
    dispatched_tid: Option<u64>,
) {
    const DEBUG_DISPATCH_CONTEXT_LOG: bool = false;
    let Some(tid) = dispatched_tid else {
        if DEBUG_DISPATCH_CONTEXT_LOG {
            crate::yarm_log!("ENTER_USER_LOOKUP_MISS tid=<none>");
        }
        return;
    };
    if DEBUG_DISPATCH_CONTEXT_LOG {
        crate::yarm_log!("ENTER_USER_LOOKUP tid={}", tid);
    }
    if tid == 0 {
        if DEBUG_DISPATCH_CONTEXT_LOG {
            crate::yarm_log!("ENTER_USER_REJECT_IDLE tid={}", tid);
        }
        return;
    }
    let Some(asid) = kernel.task_asid(tid) else {
        if DEBUG_DISPATCH_CONTEXT_LOG {
            crate::yarm_log!("ENTER_USER_LOOKUP_MISS tid={} reason=missing_asid", tid);
        }
        return;
    };
    let Some(context) = kernel.thread_user_context(tid) else {
        if DEBUG_DISPATCH_CONTEXT_LOG {
            crate::yarm_log!("ENTER_USER_LOOKUP_MISS tid={} reason=missing_context", tid);
        }
        return;
    };
    if context.instruction_ptr.0 != 0 && context.stack_ptr.0 != 0 {
        let entry_resolve =
            super::page_table::resolve_page(asid, context.instruction_ptr).is_some();
        let stack_probe = crate::kernel::vm::VirtAddr(context.stack_ptr.0.saturating_sub(8));
        let stack_resolve = super::page_table::resolve_page(asid, stack_probe).is_some();
        crate::yarm_log!(
            "YARM_RING3_PRECHECK tid={} asid={} entry=0x{:x} entry_ok={} stack_top=0x{:x} stack_probe=0x{:x} stack_ok={}",
            tid,
            asid.0,
            context.instruction_ptr.0,
            entry_resolve,
            context.stack_ptr.0,
            stack_probe.0,
            stack_resolve
        );
        if !entry_resolve || !stack_resolve {
            return;
        }
        if (context.stack_ptr.0 & 0xF) != 0 {
            crate::yarm_log!(
                "ENTER_USER_ABORT reason=stack_unaligned rsp=0x{:x}",
                context.stack_ptr.0
            );
            return;
        }
        crate::yarm_log!("BOOTSTRAP_STAGE: before enter_user_mode");
        let Ok(intended_cr3) = super::page_table::activate_asid(asid) else {
            return;
        };
        let mut active_cr3: u64 = 0;
        unsafe {
            core::arch::asm!(
                "mov {}, cr3",
                out(reg) active_cr3,
                options(nostack, preserves_flags)
            );
        }
        let cs: u16 = 0x23;
        let ss: u16 = 0x1b;
        let rflags: u64 = 0x202;
        crate::yarm_log!(
            "ENTER_USER asid={} rip=0x{:x} rsp=0x{:x}",
            asid.0,
            context.instruction_ptr.0,
            context.stack_ptr.0
        );
        crate::yarm_log!(
            "ENTER_USER_CTX intended_cr3=0x{:x} active_cr3=0x{:x} cs=0x{:x} ss=0x{:x} rflags=0x{:x}",
            intended_cr3,
            active_cr3,
            cs,
            ss,
            rflags
        );
        crate::yarm_log!(
            "YARM_RING3_INIT_TASK tid={} asid={} intended_cr3=0x{:x} entry=0x{:x} stack_top=0x{:x}",
            tid,
            asid.0,
            intended_cr3,
            context.instruction_ptr.0,
            context.stack_ptr.0
        );
        if DEBUG_DISPATCH_CONTEXT_LOG {
            crate::yarm_log!("USER_ENTRY rip=0x{:x}", context.instruction_ptr.0);
        }
        #[allow(unreachable_code)]
        {
            super::descriptor_tables::enter_user_mode_iret(
                context.instruction_ptr.0,
                context.stack_ptr.0,
                context.arg0 as u64,
                context.arg1 as u64,
                context.arg2 as u64,
                context.arg3 as u64,
                context.arg4 as u64,
                context.arg5 as u64,
            );
            crate::yarm_log!("RETURNED_FROM_USER");
        }
    }
}

#[cfg(any(feature = "hosted-dev", not(target_arch = "x86_64")))]
pub fn enter_dispatched_user_task_if_available(
    _kernel: &crate::kernel::boot::KernelState,
    _dispatched_tid: Option<u64>,
) {
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const MAX_PVH_MEMMAP_ENTRIES: usize = 128;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const MAX_PVH_MODULES: usize = 32;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const PVH_MAGIC: u32 = 0x336e_c578;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const MAX_PVH_PHYS_EXCLUSIVE: u64 = 1u64 << 52;

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
#[repr(C)]
struct PvhStartInfo {
    _magic: u32,
    _version: u32,
    _flags: u32,
    nr_modules: u32,
    modlist_paddr: u64,
    cmdline_paddr: u64,
    _rsdp_paddr: u64,
    memmap_paddr: u64,
    memmap_entries: u32,
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
#[repr(C)]
#[derive(Clone, Copy)]
struct PvhModule {
    paddr_start: u64,
    size: u64,
    cmdline_paddr: u64,
    _reserved: u64,
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
#[repr(C)]
#[derive(Clone, Copy)]
struct PvhMemMapEntry {
    addr: u64,
    size: u64,
    kind: u32,
    _reserved: u32,
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
#[derive(Clone, Copy)]
struct PvhModuleWindow {
    start: u64,
    end: u64,
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
#[derive(Clone, Copy)]
struct PvhModuleSummary {
    module_count: usize,
    initramfs: Option<PvhModuleWindow>,
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
fn read_pvh_module_summary(start_info_ptr: usize) -> Option<PvhModuleSummary> {
    if start_info_ptr == 0 {
        return None;
    }
    let start_info = unsafe { &*(start_info_ptr as *const PvhStartInfo) };
    crate::yarm_log!(
        "YARM_BOOT_PVH_START_INFO ptr=0x{:x} magic=0x{:x} version={} flags=0x{:x} nr_modules={} modlist_paddr=0x{:x} cmdline_paddr=0x{:x}",
        start_info_ptr,
        start_info._magic,
        start_info._version,
        start_info._flags,
        start_info.nr_modules,
        start_info.modlist_paddr,
        start_info.cmdline_paddr
    );
    if start_info._magic != PVH_MAGIC {
        return None;
    }
    if start_info.nr_modules == 0 || start_info.modlist_paddr == 0 {
        return Some(PvhModuleSummary {
            module_count: 0,
            initramfs: None,
        });
    }

    let module_count = core::cmp::min(start_info.nr_modules as usize, MAX_PVH_MODULES);
    let mut initramfs = None;
    let modlist_ptr = (crate::arch::platform_layout::KERNEL_BOOTSTRAP_VIRT_BASE
        .saturating_add(start_info.modlist_paddr)) as *const PvhModule;
    for idx in 0..module_count {
        let module_ptr = modlist_ptr.wrapping_add(idx);
        let module = unsafe { core::ptr::read_unaligned(module_ptr) };
        crate::yarm_log!(
            "YARM_BOOT_PVH_MODULE idx={} start=0x{:x} size=0x{:x} end=0x{:x} cmdline_paddr=0x{:x}",
            idx,
            module.paddr_start,
            module.size,
            module.paddr_start.saturating_add(module.size),
            module.cmdline_paddr
        );
        if module.cmdline_paddr != 0 {
            let cmdline_ptr = (crate::arch::platform_layout::KERNEL_BOOTSTRAP_VIRT_BASE
                .saturating_add(module.cmdline_paddr)) as *const u8;
            let mut len = 0usize;
            while len < 96 {
                let b = unsafe { core::ptr::read_volatile(cmdline_ptr.add(len)) };
                if b == 0 {
                    break;
                }
                len += 1;
            }
            if len > 0 {
                let raw = unsafe { core::slice::from_raw_parts(cmdline_ptr, len) };
                if let Ok(text) = core::str::from_utf8(raw) {
                    crate::yarm_log!("YARM_BOOT_PVH_MODULE_CMDLINE idx={} text={}", idx, text);
                }
            }
        }
        if module.paddr_start == 0 || module.size == 0 {
            continue;
        }
        let module_end = module.paddr_start.saturating_add(module.size);
        if initramfs.is_none() {
            initramfs = Some(PvhModuleWindow {
                start: module.paddr_start,
                end: module_end,
            });
        }
    }

    Some(PvhModuleSummary {
        module_count,
        initramfs,
    })
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
fn log_pvh_boot_metadata(start_info_ptr: usize) {
    use crate::arch::x86_64::console::write_line;

    if start_info_ptr == 0 {
        write_line("PVH: null ptr");
        return;
    }
    let start_info = unsafe { &*(start_info_ptr as *const PvhStartInfo) };
    if start_info._magic != PVH_MAGIC {
        write_line("PVH: bad magic");
        return;
    }
    write_line("PVH: magic OK");
    if let Some(summary) = read_pvh_module_summary(start_info_ptr) {
        if let Some(window) = summary.initramfs {
            let len = window.end.saturating_sub(window.start) as usize;
            if len > 0 {
                let page = crate::kernel::vm::PAGE_SIZE as u64;
                let reserved_start = window.start & !(page - 1);
                let reserved_end = (window.end + (page - 1)) & !(page - 1);
                crate::kernel::boot::Bootstrap::install_boot_reserved_range(
                    reserved_start,
                    reserved_end,
                );
                // SAFETY: PVH module window is immutable boot-provided memory.
                let initrd_ptr = (crate::arch::platform_layout::KERNEL_BOOTSTRAP_VIRT_BASE
                    .saturating_add(window.start)) as *const u8;
                let bytes = unsafe { core::slice::from_raw_parts(initrd_ptr, len) };
                crate::kernel::boot::Bootstrap::install_boot_initrd_bytes(bytes);
            }
        }
        crate::yarm_log!(
            "YARM_BOOT_PVH_MODULES total={} initramfs_start=0x{:x} initramfs_end=0x{:x}",
            summary.module_count,
            summary.initramfs.map(|window| window.start).unwrap_or(0),
            summary.initramfs.map(|window| window.end).unwrap_or(0)
        );
    }
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
fn init_pt_allocator_from_pvh_memmap(start_info_ptr: usize) {
    let (regions, used) = collect_pvh_usable_regions(start_info_ptr);
    if used == 0 {
        return;
    }

    let default_reserved = crate::kernel::boot::Bootstrap::default_reserved_ranges_for_arch_boot();
    // Reserve contiguous AP stack backing memory so frame allocator cannot
    // reuse it after SMP bring-up.
    let ap_stack_total =
        AP_STACK_BYTES.saturating_mul(crate::arch::platform_constants::MAX_CPUS as u64);
    let ap_stack_end = AP_STACK_PHYS_BASE.saturating_add(ap_stack_total);
    let reserved = [default_reserved[0], (AP_STACK_PHYS_BASE, ap_stack_end)];
    let (sanitized, sanitized_len) =
        crate::kernel::boot::Bootstrap::apply_reserved_ranges(&regions[..used], &reserved);
    if sanitized_len > 0 {
        let _ =
            crate::kernel::frame_allocator::init_pt_frame_allocator(&sanitized[..sanitized_len]);
    }
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
fn collect_pvh_usable_regions(
    start_info_ptr: usize,
) -> (
    [crate::kernel::frame_allocator::MemoryRegion; MAX_PVH_MEMMAP_ENTRIES],
    usize,
) {
    const PAGE_SIZE_U64: u64 = crate::kernel::vm::PAGE_SIZE as u64;
    const RESERVED_LOW_EXCLUSIVE: u64 = crate::arch::platform_layout::NEXT_ANON_PHYS_BASE;
    const DIRECT_MAP_LIMIT: u64 = crate::arch::platform_layout::KERNEL_PHYS_DIRECT_MAP_BYTES;
    const MEMMAP_ENTRY_SIZE: u64 = core::mem::size_of::<PvhMemMapEntry>() as u64;
    const MEMMAP_ENTRY_ALIGN: u64 = core::mem::align_of::<PvhMemMapEntry>() as u64;

    let mut regions = [crate::kernel::frame_allocator::MemoryRegion {
        start: 0,
        len: 0,
        usable: false,
    }; MAX_PVH_MEMMAP_ENTRIES];

    if start_info_ptr == 0 {
        return (regions, 0);
    }
    let start_info = unsafe { &*(start_info_ptr as *const PvhStartInfo) };
    if start_info._magic != PVH_MAGIC
        || start_info.memmap_paddr == 0
        || start_info.memmap_entries == 0
        || !start_info.memmap_paddr.is_multiple_of(MEMMAP_ENTRY_ALIGN)
    {
        return (regions, 0);
    }
    let count = core::cmp::min(start_info.memmap_entries as usize, MAX_PVH_MEMMAP_ENTRIES);
    let Some(memmap_bytes) = (count as u64).checked_mul(MEMMAP_ENTRY_SIZE) else {
        return (regions, 0);
    };
    let Some(memmap_end) = start_info.memmap_paddr.checked_add(memmap_bytes) else {
        return (regions, 0);
    };
    if memmap_end > MAX_PVH_PHYS_EXCLUSIVE {
        return (regions, 0);
    }

    let mut used = 0usize;
    for idx in 0..count {
        let Some(entry_paddr) = start_info
            .memmap_paddr
            .checked_add((idx as u64).saturating_mul(MEMMAP_ENTRY_SIZE))
        else {
            break;
        };
        let entry = unsafe { core::ptr::read_unaligned(entry_paddr as *const PvhMemMapEntry) };
        if entry.kind != 1 || entry.size == 0 {
            continue;
        }
        let Some(raw_end) = entry.addr.checked_add(entry.size) else {
            continue;
        };
        let mut start = entry.addr;
        let end = raw_end.min(MAX_PVH_PHYS_EXCLUSIVE).min(DIRECT_MAP_LIMIT);
        if end <= RESERVED_LOW_EXCLUSIVE || start >= end {
            continue;
        }
        if start < RESERVED_LOW_EXCLUSIVE {
            start = RESERVED_LOW_EXCLUSIVE;
        }
        let aligned_start = start.div_ceil(PAGE_SIZE_U64) * PAGE_SIZE_U64;
        let aligned_end = (end / PAGE_SIZE_U64) * PAGE_SIZE_U64;
        if aligned_end <= aligned_start || used >= regions.len() {
            continue;
        }
        if regions[..used].iter().any(|existing| {
            let existing_end = existing.start.saturating_add(existing.len);
            aligned_start < existing_end && aligned_end > existing.start
        }) {
            continue;
        }
        regions[used] = crate::kernel::frame_allocator::MemoryRegion {
            start: aligned_start,
            len: aligned_end - aligned_start,
            usable: true,
        };
        used += 1;
    }
    (regions, used)
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
static PREPARED_PVH_BOOT_MEMMAP_LEN: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
static mut PREPARED_PVH_BOOT_MEMMAP: [crate::kernel::frame_allocator::MemoryRegion;
    MAX_PVH_MEMMAP_ENTRIES] = [crate::kernel::frame_allocator::MemoryRegion {
    start: 0,
    len: 0,
    usable: false,
}; MAX_PVH_MEMMAP_ENTRIES];

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
fn store_prepared_boot_memory_map(regions: &[crate::kernel::frame_allocator::MemoryRegion]) {
    let count = core::cmp::min(regions.len(), MAX_PVH_MEMMAP_ENTRIES);
    unsafe {
        PREPARED_PVH_BOOT_MEMMAP[..count].copy_from_slice(&regions[..count]);
    }
    PREPARED_PVH_BOOT_MEMMAP_LEN.store(count, core::sync::atomic::Ordering::Release);
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
fn debug_uart_marker(byte: u8) {
    unsafe {
        core::arch::asm!(
            "2:",
            "in al, dx",
            "test al, 0x20",
            "jz 2b",
            in("dx") 0x3FDu16,
            lateout("al") _,
            options(nomem, nostack)
        );
        core::arch::asm!(
            "out dx, al",
            in("dx") 0x3F8u16,
            in("al") byte,
            options(nomem, nostack)
        );
    }
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
pub fn run_with_prepared_kernel(run: fn(&mut crate::kernel::boot::KernelState)) {
    use crate::kernel::boot::Bootstrap;

    // Descriptor tables are scaffolded during prepare_arch_boot() before run_kernel_boot().
    // Keep the run path free from additional serial-marker calls so boot remains deterministic.
    let prepared_len = PREPARED_PVH_BOOT_MEMMAP_LEN.load(core::sync::atomic::Ordering::Acquire);
    crate::arch::x86_64::console::write_line("KI0");
    let kernel_state = if prepared_len > 0 {
        let boot_regions = unsafe { &PREPARED_PVH_BOOT_MEMMAP[..prepared_len] };
        let reserved_ranges = Bootstrap::default_reserved_ranges_for_arch_boot();
        Bootstrap::init_static_with_boot_memory_map(
            Bootstrap::default_capacity_profile(),
            boot_regions,
            &reserved_ranges,
        )
        .expect("kernel init with pvh memmap")
    } else {
        Bootstrap::init_static().expect("kernel init")
    };
    let kernel_state =
        unsafe { core::ptr::read(kernel_state as *mut crate::kernel::boot::KernelState) };
    crate::arch::x86_64::console::write_line("KI1");
    crate::yarm_log!("YARM_BOOT_INIT_READY prepared_pvh_regions={}", prepared_len);
    let kernel = crate::arch::x86_64::descriptor_tables::install_trap_kernel_state(kernel_state);
    crate::arch::irq_guard::configure_external_irq_controller_from_platform_layout();
    crate::yarm_log!("YARM_SMP_LAPIC_READY source=platform_layout");
    let started_secondary = crate::arch::x86_64::smp::start_secondary_cpus(kernel).unwrap_or(0);
    crate::yarm_log!(
        "YARM_SMP_STARTUP started_secondary={} online_cpus={} present_cpus={}",
        started_secondary,
        kernel.online_cpu_count(),
        kernel.present_cpu_count()
    );
    crate::yarm_log!("YARM_BOOT_STAGE post_smp_startup");
    kernel.program_timer_deadline_current_cpu(
        crate::arch::platform_layout::BOOTSTRAP_TIMER_DEADLINE_TICKS,
    );
    crate::arch::x86_64::irq::enable_interrupts_for_boot();
    debug_uart_marker(b'J');
    crate::yarm_log!("YARM_BOOT_STAGE pre_boot_ok");
    crate::yarm_log!(
        "YARM_BOOT_OK present_cpus={} present_bitmap=0x{:x} online_cpus={}",
        kernel.present_cpu_count(),
        kernel.present_cpu_bitmap(),
        kernel.online_cpu_count()
    );
    crate::yarm_log!("YARM_BOOT_STAGE pre_scheduler_bootstrap");
    run(kernel);
}

#[cfg(any(feature = "hosted-dev", not(target_arch = "x86_64")))]
pub fn run_with_prepared_kernel(run: fn(&mut crate::kernel::boot::KernelState)) {
    use crate::kernel::boot::Bootstrap;
    let mut kernel = Bootstrap::init().expect("kernel init");
    crate::yarm_log!(
        "YARM_BOOT_OK present_cpus={} present_bitmap=0x{:x} online_cpus={}",
        kernel.present_cpu_count(),
        kernel.present_cpu_bitmap(),
        kernel.online_cpu_count()
    );
    run(&mut kernel);
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
pub fn prepare_arch_boot(start_info_ptr: usize) {
    crate::arch::x86_64::descriptor_tables::ensure_boot_descriptor_tables_scaffolded();
    crate::arch::x86_64::console::write_line("KM0");
    crate::arch::x86_64::console::write_line("KM1");
    log_pvh_boot_metadata(start_info_ptr);
    crate::arch::x86_64::console::write_line("KM2");
    let (regions, used) = collect_pvh_usable_regions(start_info_ptr);
    store_prepared_boot_memory_map(&regions[..used]);
    init_pt_allocator_from_pvh_memmap(start_info_ptr);
    crate::arch::x86_64::console::write_line("KM3");
}

#[cfg(any(feature = "hosted-dev", not(target_arch = "x86_64")))]
pub fn prepare_arch_boot(_start_info_ptr: usize) {}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
pub fn emit_panic(info: &core::panic::PanicInfo<'_>) {
    use core::fmt::Write;

    struct PanicUartWriter;
    impl Write for PanicUartWriter {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            for &byte in s.as_bytes() {
                debug_uart_marker(byte);
            }
            Ok(())
        }
    }
    let mut writer = PanicUartWriter;
    let _ = writer.write_str("PANIC ");
    if let Some(location) = info.location() {
        let _ = write!(
            writer,
            "{}:{}:{}",
            location.file(),
            location.line(),
            location.column()
        );
    } else {
        let _ = writer.write_str("<unknown>");
    }
    let _ = writer.write_str(": ");
    let _ = write!(writer, "{}", info.message());
    let _ = writer.write_str("\n");
}

#[cfg(any(feature = "hosted-dev", not(target_arch = "x86_64")))]
pub fn emit_panic(_info: &core::panic::PanicInfo<'_>) {}
