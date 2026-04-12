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
    // Keep this inside the bootstrap identity-map footprint.
    .skip 0x00200000
boot_stack_end:

    .section .data.boot,"aw",@progbits
    .align 4096
boot_pml4:
    // PML4[0]   → boot_pdpt_low   : identity map for first 4GiB.
    .quad boot_pdpt_low + 0x3
    // PML4[1..510] → null.
    .zero 4080
    // PML4[511] → canonical upper-half direct physical map.
    .quad boot_pdpt_direct + 0x3

    .align 4096
boot_pdpt_low:
    .quad boot_pd + 0x3
    .zero 16
    .quad boot_pd_hi + 0x3
    .zero 4064

    .align 4096
boot_pdpt_direct:
    // Higher-half direct physical mapping for KERNEL_BOOTSTRAP_VIRT_BASE.
    // Keep the top canonical 2GiB window wired to bootstrap PDs:
    // - PDPT[510] -> boot_pd    (low identity/kernel image window)
    // - PDPT[511] -> boot_pd_hi (3GiB..4GiB high aliases)
    // so linked high-half symbols remain valid during bootstrap.
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
    // Bootstrap identity map for the 3GiB..4GiB window (PDPT[3]).
    // This keeps early stack/data accesses valid even when linker placement
    // pushes boot sections near the top of low 32-bit virtual space.
    // Includes PCD (bit 4) so LAPIC/IOAPIC MMIO in this range is uncacheable.
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

    // FIXUP: Materialize bootstrap page-table *pointer* entries from low
    // runtime offsets so CR3 walks never see relocated virtual pointers.
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
    // Force a canonical low bootstrap stack pointer (zero-extended from 32-bit
    // physical offset) before entering Rust code.
    mov esp, offset boot_stack_end
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
    paddr_end: u64,
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
    for idx in 0..module_count {
        let module_ptr = (start_info.modlist_paddr as *const PvhModule).wrapping_add(idx);
        let module = unsafe { core::ptr::read_unaligned(module_ptr) };
        if module.paddr_start == 0 || module.paddr_end <= module.paddr_start {
            continue;
        }
        if initramfs.is_none() {
            initramfs = Some(PvhModuleWindow {
                start: module.paddr_start,
                end: module.paddr_end,
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
    if used > 0 {
        let _ = crate::kernel::frame_allocator::init_pt_frame_allocator(&regions[..used]);
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
    let kernel_state = if prepared_len > 0 {
        let boot_regions = unsafe { &PREPARED_PVH_BOOT_MEMMAP[..prepared_len] };
        let reserved_ranges = Bootstrap::default_reserved_ranges_for_arch_boot();
        Bootstrap::init_with_boot_memory_map(
            Bootstrap::default_capacity_profile(),
            boot_regions,
            &reserved_ranges,
        )
        .expect("kernel init with pvh memmap")
    } else {
        Bootstrap::init().expect("kernel init")
    };
    crate::yarm_log!("YARM_BOOT_INIT_READY prepared_pvh_regions={}", prepared_len);
    let kernel = crate::arch::x86_64::descriptor_tables::install_trap_kernel_state(kernel_state);
    let started_secondary = crate::arch::x86_64::smp::start_secondary_cpus(kernel).unwrap_or(0);
    crate::yarm_log!(
        "YARM_SMP_STARTUP started_secondary={} online_cpus={} present_cpus={}",
        started_secondary,
        kernel.online_cpu_count(),
        kernel.present_cpu_count()
    );
    kernel.program_timer_deadline_current_cpu(
        crate::arch::platform_layout::BOOTSTRAP_TIMER_DEADLINE_TICKS,
    );
    crate::arch::x86_64::irq::enable_interrupts_for_boot();
    debug_uart_marker(b'J');
    crate::yarm_log!(
        "YARM_BOOT_OK present_cpus={} present_bitmap=0x{:x} online_cpus={}",
        kernel.present_cpu_count(),
        kernel.present_cpu_bitmap(),
        kernel.online_cpu_count()
    );
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
