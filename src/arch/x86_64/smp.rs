// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use crate::kernel::boot::{KernelError, KernelState};
use crate::kernel::scheduler::CpuId;
#[cfg(all(not(test), not(feature = "hosted-dev")))]
use core::arch::global_asm;
use core::ptr::copy_nonoverlapping;
use core::ptr::read_volatile;
use core::ptr::write_volatile;
use core::sync::atomic::{AtomicBool, Ordering};

const LAPIC_ICR_LOW_OFFSET: usize = 0x300;
const LAPIC_ICR_HIGH_OFFSET: usize = 0x310;

const ICR_DELIVERY_MODE_INIT: u32 = 0b101 << 8;
const ICR_DELIVERY_MODE_STARTUP: u32 = 0b110 << 8;
const ICR_DELIVERY_STATUS_PENDING: u32 = 1 << 12;
const ICR_LEVEL_DEASSERT: u32 = 0;
const ICR_LEVEL_ASSERT: u32 = 1 << 14;
const ICR_TRIGGER_MODE_LEVEL: u32 = 1 << 15;

const AP_TRAMPOLINE_PHYS: usize = 0x7000;
const AP_TRAMPOLINE_VECTOR: u8 = (AP_TRAMPOLINE_PHYS >> 12) as u8;
const AP_TRAMPOLINE_SIZE: usize = crate::kernel::vm::PAGE_SIZE;
#[cfg(any(test, feature = "hosted-dev"))]
const AP_HANDOFF_OFFSET: usize = 0x100;
const AP_HANDOFF_MAGIC: u32 = 0x5952_4D41; // "YRMA"
const AP_STACK_BYTES: usize = 16 * 1024;
const AP_STACK_PHYS_BASE: u64 = 0x0200_0000; // 32 MiB; kept in sync with boot.rs.
const BOOTSTRAP_LOW_IDENTITY_BYTES: u64 = 64 * 1024 * 1024;
// APs switch to paging before loading RSP from handoff. Use the higher-half
// direct-map VA for AP stacks instead of a low identity VA (e.g. 0x2000_0000),
// because early identity mapping is intentionally limited during bootstrap.
// Keep physical backing in the low identity window as a safety net for any
// transitional path that still touches these stacks via low physical aliases.
const AP_STACK_TOP_BASE: u64 =
    crate::arch::platform_layout::KERNEL_BOOTSTRAP_VIRT_BASE + AP_STACK_PHYS_BASE;
const AP_READY_POLL_ITERS: usize = 2_000_000;
const ICR_IDLE_POLL_ITERS: usize = 100_000;
// Keep in sync with AP_OFF_TRACE in the trampoline assembly below.
const AP_TRACE_OFFSET: usize = 0x200;
#[allow(dead_code)]
const AP_BREADCRUMB_MAP: &str =
    "s=stack-init,a=entry,u=pre-lgdt,b/L=post-lgdt,f=pre-PE,g=post-PE,r=pre-ljmp,h=post-pmode-jmp,c=pmode,i/j=pre/post-cr3,k/l=pre/post-PAE,m=pre-LME,n=post-LME,o=pre-PG,p=post-PG,x=pre-lmode-jmp,q=post-lmode-jmp,v=pre-handoff-read,w=post-rsp-load,e=pre-ap-entry-call,z=ap-entry-first-instr";

#[repr(C)]
#[derive(Clone, Copy)]
struct ApHandoff {
    magic: u32,
    cpu_id: u32,
    stack_top: u64,
    kernel_state_ptr: u64,
    ready_flag_ptr: u64,
}

#[cfg(all(not(test), not(feature = "hosted-dev")))]
global_asm!(
    r#"
    .section .text.ap_trampoline_src,"ax",@progbits
    .global yarm_ap_trampoline_start
    .global yarm_ap_trampoline_end
    .global yarm_ap_trampoline_handoff
    .code16
    .set AP_TRAMPOLINE_BASE, 0x7000
    .set AP_OFF_GDTR, ap_gdtr - yarm_ap_trampoline_start
    .set AP_OFF_PM_L5, 5f - yarm_ap_trampoline_start
    .set AP_OFF_HANDOFF, yarm_ap_trampoline_handoff - yarm_ap_trampoline_start
    .set AP_OFF_TRACE, 0x200

yarm_ap_trampoline_start:
    cli
    mov dword ptr cs:[AP_OFF_TRACE], 0x31504159 // "YAP1"
    xor ax, ax
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov sp, 0x7c00
    mov dx, 0x3F8
    mov al, 's' // stack initialized
    out dx, al
    mov word ptr cs:[AP_OFF_TRACE + 4], sp

    // Diagnostic: AP reached real mode, write 'a' to UART (uses no
    // segments other than implicit string default; out instruction
    // doesn't depend on memory layout).
    mov dx, 0x3F8
    mov al, 'a'
    out dx, al
    mov dword ptr cs:[AP_OFF_TRACE], 0x32504159 // "YAP2"

    mov al, 'u' // before lgdt
    out dx, al
    .set AP_GDTR_OFF, ap_gdtr - yarm_ap_trampoline_start
    lgdt cs:[AP_GDTR_OFF]
    mov dword ptr cs:[AP_OFF_TRACE], 0x33504159 // "YAP3"

    // Diagnostic: AP loaded GDTR.
    mov dx, 0x3F8
    mov al, 'b'
    out dx, al
    mov al, 'L'
    out dx, al
    mov al, 'f' // before CR0.PE write
    out dx, al

    mov eax, cr0
    or eax, 1
    mov cr0, eax
    mov al, 'g' // after CR0.PE write
    out dx, al
    mov al, 'r' // immediately before far jump to pmode
    out dx, al
    .set AP_PM_ENTRY, AP_TRAMPOLINE_BASE + (3f - yarm_ap_trampoline_start)
    .byte 0x66
    .byte 0xEA
        .long AP_PM_ENTRY
    .word 0x0008

    .set AP_GDT_BASE, AP_TRAMPOLINE_BASE + (ap_gdt - yarm_ap_trampoline_start)
    .set AP_GDT_LIMIT, (ap_gdt_end - ap_gdt) - 1
ap_gdtr:
    .word AP_GDT_LIMIT
    .long AP_GDT_BASE

    .code32
3:
    mov dx, 0x3F8
    mov al, 'h' // immediately after pmode far jump
    out dx, al
    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov ss, ax

    // Diagnostic: AP entered 32-bit pmode.
    mov dx, 0x3F8
    mov al, 'c'
    out dx, al

    call 5f
5:
    pop ebx
    sub ebx, AP_OFF_PM_L5

    mov dx, 0x3F8
    mov al, 'i' // before CR3 load
    out dx, al
    mov eax, [ebx + AP_OFF_HANDOFF + 16]
    mov cr3, eax
    mov dword ptr [ebx + AP_OFF_TRACE + 8], eax
    mov al, 'j' // after CR3 load
    out dx, al

    mov al, 'k' // before CR4.PAE
    out dx, al
    mov eax, cr4
    mov dword ptr [ebx + AP_OFF_TRACE + 12], eax
    or eax, (1 << 5)
    mov cr4, eax
    mov eax, cr4
    mov dword ptr [ebx + AP_OFF_TRACE + 16], eax
    mov al, 'l' // after CR4.PAE
    out dx, al

    mov al, 'm' // before EFER.LME block
    out dx, al
    mov al, 'M' // before mov ecx, IA32_EFER
    out dx, al
    mov ecx, 0xC0000080
    mov al, 'R' // immediately before rdmsr
    out dx, al
    rdmsr
    mov al, 'S' // immediately after rdmsr
    out dx, al
    mov dword ptr [ebx + AP_OFF_TRACE + 20], eax
    mov dword ptr [ebx + AP_OFF_TRACE + 24], edx
    or eax, (1 << 8)
    mov al, 'W' // immediately before wrmsr
    out dx, al
    wrmsr
    mov al, 'N' // immediately after wrmsr
    out dx, al
    rdmsr
    mov dword ptr [ebx + AP_OFF_TRACE + 28], eax
    mov dword ptr [ebx + AP_OFF_TRACE + 32], edx
    mov al, 'n' // after EFER.LME
    out dx, al

    mov al, 'o' // before CR0.PG
    out dx, al
    mov eax, cr0
    or eax, 0x80000000
    mov cr0, eax
    mov al, 'p' // after CR0.PG
    out dx, al

    // Diagnostic: AP enabled paging+long mode (still 32-bit instruction
    // semantics until the next far jump).
    mov dx, 0x3F8
    mov al, 'd'
    out dx, al

    .set AP_LM_ENTRY, AP_TRAMPOLINE_BASE + (6f - yarm_ap_trampoline_start)
    mov al, 'x' // immediately before long-mode far jump
    out dx, al
    .byte 0xEA
        .long AP_LM_ENTRY
    .word 0x18

    .code64
6:
    mov dx, 0x3F8
    mov al, 'q' // immediately after long-mode far jump
    out dx, al
    mov ax, 0x20
    mov ds, ax
    mov es, ax
    mov ss, ax

    // Diagnostic: AP in 64-bit mode at low VA (RIP still =0x70xx since
    // the far jump used a low offset).  Out is fine here since CR3 maps
    // PML4[0] for low identity.
    mov dx, 0x3F8
    mov al, 'e'
    out dx, al

    mov al, 'v' // before reading handoff struct in low runtime page
    out dx, al
    mov rbx, AP_TRAMPOLINE_BASE
    mov rsp, [rbx + AP_OFF_HANDOFF + 8]
    mov al, 'w' // after loading rsp from handoff
    out dx, al
    lea rdi, [rbx + AP_OFF_HANDOFF]
    mov al, 'e'
    out dx, al
    movabs rax, yarm_x86_64_ap_entry
    call rax

7:
    hlt
    jmp 7b

    .align 8
ap_gdt:
    .quad 0x0000000000000000
    .quad 0x00cf9a000000ffff
    .quad 0x00cf92000000ffff
    .quad 0x00af9a000000ffff
    .quad 0x00af92000000ffff
ap_gdt_end:

    .align 8
yarm_ap_trampoline_handoff:
    .zero 40

yarm_ap_trampoline_end:
    .code64
"#
);

#[cfg(all(not(test), not(feature = "hosted-dev")))]
unsafe extern "C" {
    static yarm_ap_trampoline_start: u8;
    static yarm_ap_trampoline_end: u8;
    static yarm_ap_trampoline_handoff: u8;
}

#[cfg(all(not(test), not(feature = "hosted-dev")))]
#[unsafe(no_mangle)]
extern "C" fn yarm_x86_64_ap_entry(handoff_ptr: *const ApHandoff) -> ! {
    unsafe {
        core::arch::asm!(
            "mov dx, 0x3F8",
            "mov al, 'z'",
            "out dx, al",
            options(nostack, nomem, preserves_flags)
        );
    }
    crate::yarm_log!("YARM_SMP_AP_TRACE_MAP {}", AP_BREADCRUMB_MAP);
    let handoff = unsafe { &*handoff_ptr };
    crate::yarm_log!(
        "YARM_SMP_AP_ENTRY cpu={} magic=0x{:08x} stack_top=0x{:x} ready_ptr=0x{:x}",
        handoff.cpu_id,
        handoff.magic,
        handoff.stack_top,
        handoff.ready_flag_ptr
    );
    if handoff.magic == AP_HANDOFF_MAGIC {
        let ready_addr = handoff.ready_flag_ptr;
        let ready_ptr = ready_addr as usize as *const AtomicBool;
        unsafe { (*ready_ptr).store(true, Ordering::Release) };
        crate::yarm_log!(
            "YARM_SMP_AP_READY_SET cpu={} ready_addr=0x{:x}",
            handoff.cpu_id,
            ready_addr
        );
    } else {
        crate::yarm_log!(
            "YARM_SMP_AP_BAD_HANDOFF cpu={} expected_magic=0x{:08x}",
            handoff.cpu_id,
            AP_HANDOFF_MAGIC
        );
    }
    loop {
        core::hint::spin_loop();
    }
}

#[cfg(any(test, feature = "hosted-dev"))]
const AP_STUB: [u8; 16] = [
    0xFA, // cli
    0xF4, // hlt
    0xEB, 0xFC, // jmp .-2
    0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90,
];

static AP_READY_FLAGS: [AtomicBool; crate::arch::platform_constants::MAX_CPUS] =
    [const { AtomicBool::new(false) }; crate::arch::platform_constants::MAX_CPUS];

struct TrampolineScratch(core::cell::UnsafeCell<[u8; AP_TRAMPOLINE_SIZE]>);

unsafe impl Sync for TrampolineScratch {}

static TRAMPOLINE_SCRATCH: TrampolineScratch =
    TrampolineScratch(core::cell::UnsafeCell::new([0; AP_TRAMPOLINE_SIZE]));

fn with_trampoline_scratch<R>(f: impl FnOnce(&mut [u8; AP_TRAMPOLINE_SIZE]) -> R) -> R {
    unsafe { f(&mut *TRAMPOLINE_SCRATCH.0.get()) }
}

fn encode_handoff(page: &mut [u8; AP_TRAMPOLINE_SIZE], handoff: ApHandoff) {
    page.fill(0);
    #[cfg(all(not(test), not(feature = "hosted-dev")))]
    unsafe {
        let mut start = &yarm_ap_trampoline_start as *const u8 as usize;
        let mut end = &yarm_ap_trampoline_end as *const u8 as usize;
        let mut handoff_ptr = &yarm_ap_trampoline_handoff as *const u8 as usize;
        if start < crate::arch::platform_layout::KERNEL_BOOTSTRAP_VIRT_BASE as usize {
            let base = crate::arch::platform_layout::KERNEL_BOOTSTRAP_VIRT_BASE as usize;
            start = start.saturating_add(base);
            end = end.saturating_add(base);
            handoff_ptr = handoff_ptr.saturating_add(base);
        }
        let start = start as *const u8;
        let end = end as *const u8;
        let handoff_ptr = handoff_ptr as *const u8;
        let len = end.offset_from(start) as usize;
        #[cfg(not(test))]
        crate::yarm_log!(
            "YARM_SMP_TRAMPOLINE_SOURCE_RANGE start=0x{:x} end=0x{:x} len={}",
            start as usize,
            end as usize,
            len
        );
        let dest_alias = (crate::arch::platform_layout::KERNEL_BOOTSTRAP_VIRT_BASE
            + AP_TRAMPOLINE_PHYS as u64) as usize;
        if start as usize == dest_alias {
            crate::yarm_log!(
                "YARM_SMP_TRAMPOLINE_SOURCE_INVALID reason=self_referential start=0x{:x}",
                start as usize
            );
        }
        debug_assert!(len > 0, "AP trampoline source length must be > 0");
        if len <= AP_TRAMPOLINE_SIZE {
            copy_nonoverlapping(start, page.as_mut_ptr(), len);
            let handoff_off = handoff_ptr.offset_from(start) as usize;
            let handoff_bytes = core::slice::from_raw_parts(
                (&handoff as *const ApHandoff).cast::<u8>(),
                core::mem::size_of::<ApHandoff>(),
            );
            page[handoff_off..handoff_off + handoff_bytes.len()].copy_from_slice(handoff_bytes);
        }
        #[cfg(not(test))]
        if len > AP_TRAMPOLINE_SIZE {
            crate::yarm_log!(
                "YARM_SMP_TRAMPOLINE_SOURCE_OVERSIZE len={} max={}",
                len,
                AP_TRAMPOLINE_SIZE
            );
        }
        return;
    }

    #[cfg(any(test, feature = "hosted-dev"))]
    {
        page[..AP_STUB.len()].copy_from_slice(&AP_STUB);

        let handoff_bytes = unsafe {
            core::slice::from_raw_parts(
                (&handoff as *const ApHandoff).cast::<u8>(),
                core::mem::size_of::<ApHandoff>(),
            )
        };
        page[AP_HANDOFF_OFFSET..AP_HANDOFF_OFFSET + handoff_bytes.len()]
            .copy_from_slice(handoff_bytes);
    }
}

#[cfg(not(test))]
fn write_trampoline_page(page: &[u8; AP_TRAMPOLINE_SIZE]) {
    let trampoline_virt = (crate::arch::platform_layout::KERNEL_BOOTSTRAP_VIRT_BASE
        + AP_TRAMPOLINE_PHYS as u64) as usize;
    unsafe {
        copy_nonoverlapping(
            page.as_ptr(),
            trampoline_virt as *mut u8,
            AP_TRAMPOLINE_SIZE,
        );
    }
}

#[cfg(not(test))]
fn trampoline_trace_word() -> u32 {
    let trampoline_virt =
        (crate::arch::platform_layout::KERNEL_BOOTSTRAP_VIRT_BASE + AP_TRAMPOLINE_PHYS as u64)
            as usize;
    unsafe { read_volatile((trampoline_virt + AP_TRACE_OFFSET) as *const u32) }
}

#[cfg(not(test))]
fn trampoline_trace_dword(offset: usize) -> u32 {
    let trampoline_virt =
        (crate::arch::platform_layout::KERNEL_BOOTSTRAP_VIRT_BASE + AP_TRAMPOLINE_PHYS as u64)
            as usize;
    unsafe { read_volatile((trampoline_virt + AP_TRACE_OFFSET + offset) as *const u32) }
}

#[cfg(not(test))]
fn log_trampoline_head_bytes() {
    let mut b = [0u8; 16];
    for (i, slot) in b.iter_mut().enumerate() {
        let trampoline_virt =
            (crate::arch::platform_layout::KERNEL_BOOTSTRAP_VIRT_BASE + AP_TRAMPOLINE_PHYS as u64)
                as usize;
        *slot = unsafe { read_volatile((trampoline_virt + i) as *const u8) };
    }
    crate::yarm_log!(
        "YARM_SMP_TRAMPOLINE_HEAD phys=0x{:x} bytes={:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x}",
        AP_TRAMPOLINE_PHYS,
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    );
}

#[cfg(not(test))]
fn log_trampoline_layout(page: &[u8; AP_TRAMPOLINE_SIZE]) {
    let first_80 = &page[..0x80];
    crate::yarm_log!(
        "YARM_SMP_TRAMPOLINE_7000_7080 bytes={:02x?}",
        first_80
    );
    let far16 = page
        .windows(5)
        .position(|w| w[0] == 0xEA && w[3] == 0x08 && w[4] == 0x00);
    let far32 = page
        .windows(8)
        .position(|w| w[0] == 0x66 && w[1] == 0xEA && w[6] == 0x08 && w[7] == 0x00);
    if let Some(far_off) = far16.or(far32) {
        let is_32 = far32 == Some(far_off);
        let gdtr_off = if is_32 { far_off + 8 } else { far_off + 5 };
        if gdtr_off + 6 <= page.len() {
            let limit = u16::from_le_bytes([page[gdtr_off], page[gdtr_off + 1]]);
            let base = u32::from_le_bytes([
                page[gdtr_off + 2],
                page[gdtr_off + 3],
                page[gdtr_off + 4],
                page[gdtr_off + 5],
            ]);
            crate::yarm_log!(
                "YARM_SMP_TRAMPOLINE_GDTR off=0x{:x} limit=0x{:x} base=0x{:x} bytes={:02x?}",
                gdtr_off,
                limit,
                base,
                &page[gdtr_off..gdtr_off + 6]
            );
            let gdt_off = (base as usize).saturating_sub(AP_TRAMPOLINE_PHYS);
            if gdt_off + 32 <= page.len() {
                crate::yarm_log!(
                    "YARM_SMP_TRAMPOLINE_GDT base=0x{:x} off=0x{:x} bytes={:02x?}",
                    base,
                    gdt_off,
                    &page[gdt_off..gdt_off + 32]
                );
                let g = &page[gdt_off..gdt_off + 32];
                crate::yarm_log!(
                    "YARM_SMP_TRAMPOLINE_GDT32 bytes={:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x}",
                    g[0], g[1], g[2], g[3], g[4], g[5], g[6], g[7],
                    g[8], g[9], g[10], g[11], g[12], g[13], g[14], g[15],
                    g[16], g[17], g[18], g[19], g[20], g[21], g[22], g[23],
                    g[24], g[25], g[26], g[27], g[28], g[29], g[30], g[31]
                );
            }
        }
        let end = core::cmp::min(far_off + 10, page.len());
        crate::yarm_log!(
            "YARM_SMP_TRAMPOLINE_FARJMP off=0x{:x} bytes={:02x?}",
            far_off,
            &page[far_off..end]
        );
        let pm_entry_linear = if is_32 {
            u32::from_le_bytes([
                page[far_off + 2],
                page[far_off + 3],
                page[far_off + 4],
                page[far_off + 5],
            ]) as usize
        } else {
            u16::from_le_bytes([page[far_off + 1], page[far_off + 2]]) as usize
        };
        let pm_entry_off = pm_entry_linear.saturating_sub(AP_TRAMPOLINE_PHYS);
        if pm_entry_off + 16 <= page.len() {
                crate::yarm_log!(
                    "YARM_SMP_TRAMPOLINE_PM_ENTRY off=0x{:x} bytes={:02x?}",
                    pm_entry_off,
                    &page[pm_entry_off..pm_entry_off + 16]
                );
        }
    } else {
        crate::yarm_log!("YARM_SMP_TRAMPOLINE_FARJMP off=<none> bytes=<none>");
    }
    if let Some(lm_off) = page
        .windows(7)
        .position(|w| w[0] == 0xEA && w[5] == 0x18 && w[6] == 0x00)
    {
        crate::yarm_log!(
            "YARM_SMP_TRAMPOLINE_LM_FARJMP off=0x{:x} sel=0x0018 bytes={:02x?}",
            lm_off,
            &page[lm_off..core::cmp::min(lm_off + 8, page.len())]
        );
    }
    if let Some(msr_off) = page
        .windows(5)
        .position(|w| w == [0xB9, 0x80, 0x00, 0x00, 0xC0])
    {
        let end = core::cmp::min(msr_off + 24, page.len());
        crate::yarm_log!(
            "YARM_SMP_TRAMPOLINE_EFER_BLOCK off=0x{:x} bytes={:02x?}",
            msr_off,
            &page[msr_off..end]
        );
    }
}

#[cfg(test)]
struct TestTrampolinePage(core::cell::UnsafeCell<[u8; AP_TRAMPOLINE_SIZE]>);

#[cfg(test)]
unsafe impl Sync for TestTrampolinePage {}

#[cfg(test)]
static TEST_TRAMPOLINE_PAGE: TestTrampolinePage =
    TestTrampolinePage(core::cell::UnsafeCell::new([0; AP_TRAMPOLINE_SIZE]));

#[cfg(test)]
fn write_trampoline_page(page: &[u8; AP_TRAMPOLINE_SIZE]) {
    unsafe {
        let ptr = TEST_TRAMPOLINE_PAGE.0.get() as *mut u8;
        copy_nonoverlapping(page.as_ptr(), ptr, AP_TRAMPOLINE_SIZE);
    }
}

#[cfg(test)]
fn trampoline_handoff_snapshot_for_test() -> ApHandoff {
    unsafe {
        let base = TEST_TRAMPOLINE_PAGE.0.get() as *const u8;
        let ptr = base.add(AP_HANDOFF_OFFSET).cast::<ApHandoff>();
        core::ptr::read_unaligned(ptr)
    }
}

#[cfg(test)]
static TEST_LAPIC_BASE: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

#[cfg(test)]
pub fn set_lapic_mmio_base_for_test(base: usize) {
    TEST_LAPIC_BASE.store(base, Ordering::Relaxed);
}

fn lapic_mmio_base() -> usize {
    #[cfg(test)]
    {
        let test_base = TEST_LAPIC_BASE.load(Ordering::Relaxed);
        if test_base != 0 {
            return test_base;
        }
    }
    super::platform_layout::LAPIC_MMIO_BASE
}

fn write_icr(apic_id: u8, value: u32) {
    let base = lapic_mmio_base();
    #[cfg(not(test))]
    crate::yarm_log!(
        "YARM_SMP_ICR_WRITE apic_id={} high=0x{:08x} low=0x{:08x} base=0x{:x}",
        apic_id,
        (apic_id as u32) << 24,
        value,
        base
    );
    unsafe {
        write_volatile(
            (base + LAPIC_ICR_HIGH_OFFSET) as *mut u32,
            (apic_id as u32) << 24,
        );
        write_volatile((base + LAPIC_ICR_LOW_OFFSET) as *mut u32, value);
    }
}

fn wait_for_icr_idle(apic_id: u8, phase: &str) {
    let base = lapic_mmio_base();
    let mut idle = false;
    for _ in 0..ICR_IDLE_POLL_ITERS {
        let low = unsafe { read_volatile((base + LAPIC_ICR_LOW_OFFSET) as *const u32) };
        if (low & ICR_DELIVERY_STATUS_PENDING) == 0 {
            idle = true;
            break;
        }
        core::hint::spin_loop();
    }
    if !idle {
        crate::yarm_log!(
            "YARM_SMP_ICR_STUCK apic_id={} phase={} low=0x{:08x} base=0x{:x}",
            apic_id,
            phase,
            unsafe { read_volatile((base + LAPIC_ICR_LOW_OFFSET) as *const u32) },
            base
        );
    }
}

fn send_init_sipi_sipi(apic_id: u8) {
    #[cfg(not(test))]
    crate::yarm_log!(
        "YARM_SMP_IPI_SEQUENCE_BEGIN apic_id={} trampoline_phys=0x{:x} vector=0x{:02x}",
        apic_id,
        AP_TRAMPOLINE_PHYS,
        AP_TRAMPOLINE_VECTOR
    );
    write_icr(
        apic_id,
        ICR_DELIVERY_MODE_INIT | ICR_TRIGGER_MODE_LEVEL | ICR_LEVEL_ASSERT,
    );
    wait_for_icr_idle(apic_id, "init_assert");
    write_icr(
        apic_id,
        ICR_DELIVERY_MODE_INIT | ICR_TRIGGER_MODE_LEVEL | ICR_LEVEL_DEASSERT,
    );
    wait_for_icr_idle(apic_id, "init_deassert");
    spin_delay(20_000);
    write_icr(
        apic_id,
        ICR_DELIVERY_MODE_STARTUP | AP_TRAMPOLINE_VECTOR as u32,
    );
    wait_for_icr_idle(apic_id, "sipi1");
    spin_delay(200);
    #[cfg(not(test))]
    crate::yarm_log!(
        "YARM_SMP_IPI_SEQUENCE_DONE apic_id={} vector=0x{:02x}",
        apic_id,
        AP_TRAMPOLINE_VECTOR
    );
    write_icr(
        apic_id,
        ICR_DELIVERY_MODE_STARTUP | AP_TRAMPOLINE_VECTOR as u32,
    );
    wait_for_icr_idle(apic_id, "sipi2");
    spin_delay(200);

    #[cfg(test)]
    AP_READY_FLAGS[apic_id as usize].store(true, Ordering::Release);
}

fn spin_delay(iterations: usize) {
    for _ in 0..iterations {
        core::hint::spin_loop();
    }
}

fn ap_stack_top(cpu: CpuId) -> u64 {
    AP_STACK_TOP_BASE + ((cpu.0 as u64 + 1) * AP_STACK_BYTES as u64)
}

fn prepare_trampoline_for_cpu(kernel: &KernelState, cpu: CpuId) {
    #[cfg(all(not(test), not(feature = "hosted-dev")))]
    let _ = kernel;
    AP_READY_FLAGS[cpu.0 as usize].store(false, Ordering::Release);
    let ap_stack_phys_end = AP_STACK_PHYS_BASE.saturating_add(
        (crate::arch::platform_constants::MAX_CPUS as u64).saturating_mul(AP_STACK_BYTES as u64),
    );
    debug_assert!(
        ap_stack_phys_end <= BOOTSTRAP_LOW_IDENTITY_BYTES,
        "AP stack backing must stay in bootstrap low identity map"
    );
    let handoff = ApHandoff {
        magic: AP_HANDOFF_MAGIC,
        cpu_id: cpu.0 as u32,
        stack_top: ap_stack_top(cpu),
        #[cfg(all(not(test), not(feature = "hosted-dev")))]
        kernel_state_ptr: {
            let mut cr3: u64 = 0;
            unsafe {
                core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nostack, preserves_flags));
            }
            cr3
        },
        #[cfg(any(test, feature = "hosted-dev"))]
        kernel_state_ptr: kernel as *const _ as usize as u64,
        ready_flag_ptr: (&AP_READY_FLAGS[cpu.0 as usize] as *const AtomicBool as usize) as u64,
    };
    #[cfg(not(test))]
    crate::yarm_log!(
        "YARM_SMP_AP_PREPARE cpu={} stack_top=0x{:x} kernel_state=0x{:x} ready_ptr=0x{:x} trampoline=0x{:x}",
        cpu.0,
        handoff.stack_top,
        handoff.kernel_state_ptr,
        handoff.ready_flag_ptr,
        AP_TRAMPOLINE_PHYS
    );
    #[cfg(all(not(test), not(feature = "hosted-dev")))]
    {
        let root = handoff.kernel_state_ptr & !0xfffu64;
        let low_ok = crate::arch::x86_64::page_table::debug_root_maps_virt(
            root,
            crate::kernel::vm::VirtAddr(AP_TRAMPOLINE_PHYS as u64),
        );
        let stack_ok = crate::arch::x86_64::page_table::debug_root_maps_virt(
            root,
            crate::kernel::vm::VirtAddr(handoff.stack_top.saturating_sub(8)),
        );
        let entry_ok = crate::arch::x86_64::page_table::debug_root_maps_virt(
            root,
            crate::kernel::vm::VirtAddr(yarm_x86_64_ap_entry as usize as u64),
        );
        crate::yarm_log!(
            "YARM_SMP_AP_CR3_MAP_CHECK cpu={} cr3=0x{:x} low7000={} ap_stack={} ap_entry={}",
            cpu.0,
            handoff.kernel_state_ptr,
            low_ok as u8,
            stack_ok as u8,
            entry_ok as u8
        );
    }
    #[cfg(not(test))]
    crate::yarm_log!("YARM_SMP_AP_TRACE_MAP {}", AP_BREADCRUMB_MAP);
    with_trampoline_scratch(|page| {
        encode_handoff(page, handoff);
        #[cfg(not(test))]
        log_trampoline_layout(page);
        #[cfg(not(test))]
        crate::yarm_log!(
            "YARM_SMP_TRAMPOLINE_SRC len={} first8={:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x}",
            AP_TRAMPOLINE_SIZE,
            page[0], page[1], page[2], page[3], page[4], page[5], page[6], page[7]
        );
        write_trampoline_page(page);
    });
    #[cfg(not(test))]
    log_trampoline_head_bytes();
}

pub fn start_secondary_cpus(kernel: &mut KernelState) -> Result<usize, KernelError> {
    let mut started = 0usize;
    let present = kernel.present_cpu_bitmap();

    for cpu in 0..crate::arch::platform_constants::MAX_CPUS {
        let cpu = CpuId(cpu as u8);
        if cpu.0 == crate::arch::platform_constants::BOOTSTRAP_CPU_ID {
            continue;
        }
        if (present & (1u64 << cpu.0)) == 0 {
            continue;
        }

        prepare_trampoline_for_cpu(kernel, cpu);
        #[cfg(not(test))]
        crate::yarm_log!(
            "YARM_SMP_AP_WAIT_BEGIN cpu={} poll_iters={}",
            cpu.0,
            AP_READY_POLL_ITERS
        );
        send_init_sipi_sipi(cpu.0);

        let mut ready = false;
        #[cfg(not(test))]
        let mut ready_iter = 0usize;
        for _ in 0..AP_READY_POLL_ITERS {
            if AP_READY_FLAGS[cpu.0 as usize].load(Ordering::Acquire) {
                ready = true;
                break;
            }
            #[cfg(not(test))]
            {
                ready_iter += 1;
            }
            core::hint::spin_loop();
        }

        if ready {
            #[cfg(not(test))]
            crate::yarm_log!(
                "YARM_SMP_AP_READY_OBSERVED cpu={} poll_iter={}",
                cpu.0,
                ready_iter
            );
            match kernel.bring_up_cpu(cpu) {
                Ok(()) => started += 1,
                Err(KernelError::WrongObject) => {}
                Err(err) => return Err(err),
            }
        } else {
            #[cfg(not(test))]
            crate::yarm_log!(
                "YARM_SMP_AP_TIMEOUT cpu={} trampoline=0x{:x} trace=0x{:08x} cr3=0x{:08x} cr4_pre=0x{:08x} cr4_post=0x{:08x} efer_lo_pre=0x{:08x} efer_hi_pre=0x{:08x} efer_lo_post=0x{:08x} efer_hi_post=0x{:08x}",
                cpu.0,
                AP_TRAMPOLINE_PHYS,
                trampoline_trace_word(),
                trampoline_trace_dword(8),
                trampoline_trace_dword(12),
                trampoline_trace_dword(16),
                trampoline_trace_dword(20),
                trampoline_trace_dword(24),
                trampoline_trace_dword(28),
                trampoline_trace_dword(32)
            );
        }
    }

    Ok(started)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trampoline_handoff_encoding_contains_cpu_stack_and_kernel_state() {
        std::thread::Builder::new()
            .name("trampoline_handoff_encoding_contains_cpu_stack_and_kernel_state".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(run_trampoline_handoff_encoding_contains_cpu_stack_and_kernel_state)
            .expect("spawn test thread")
            .join()
            .expect("join test thread");
    }

    fn run_trampoline_handoff_encoding_contains_cpu_stack_and_kernel_state() {
        let kernel = crate::kernel::boot::Bootstrap::init_static().expect("init");
        prepare_trampoline_for_cpu(kernel, CpuId(2));

        let handoff = trampoline_handoff_snapshot_for_test();
        assert_eq!(handoff.magic, AP_HANDOFF_MAGIC);
        assert_eq!(handoff.cpu_id, 2);
        assert_eq!(handoff.stack_top, ap_stack_top(CpuId(2)));
        assert_eq!(
            handoff.kernel_state_ptr,
            (kernel as *mut KernelState as usize) as u64
        );
        assert_ne!(handoff.ready_flag_ptr, 0);
    }

    #[test]
    fn secondary_cpu_startup_updates_online_cpu_accounting() {
        std::thread::Builder::new()
            .name("secondary_cpu_startup_updates_online_cpu_accounting".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(run_secondary_cpu_startup_updates_online_cpu_accounting)
            .expect("spawn test thread")
            .join()
            .expect("join test thread");
    }

    fn run_secondary_cpu_startup_updates_online_cpu_accounting() {
        let kernel = crate::kernel::boot::Bootstrap::init_static().expect("init");
        let mut lapic_regs = [0u32; 256];
        set_lapic_mmio_base_for_test(lapic_regs.as_mut_ptr() as usize);

        let started = start_secondary_cpus(kernel).expect("smp startup");
        assert_eq!(started, kernel.present_cpu_count().saturating_sub(1));
        assert_eq!(kernel.online_cpu_count(), kernel.present_cpu_count());
    }

    #[test]
    fn ap_stack_backing_fits_bootstrap_low_identity_window() {
        let total_ap_stack_bytes = (crate::arch::platform_constants::MAX_CPUS as u64)
            .saturating_mul(AP_STACK_BYTES as u64);
        assert!(AP_STACK_PHYS_BASE + total_ap_stack_bytes <= BOOTSTRAP_LOW_IDENTITY_BYTES);
    }
}
