// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use crate::kernel::boot::{KernelError, KernelState};
use crate::kernel::scheduler::CpuId;

#[cfg(all(not(test), not(feature = "hosted-dev")))]
use core::arch::global_asm;
#[cfg(all(not(test), not(feature = "hosted-dev")))]
use core::ptr::{copy_nonoverlapping, read_volatile, write_volatile};
use core::sync::atomic::{AtomicBool, Ordering};

const LAPIC_ICR_LOW_OFFSET: usize = 0x300;
const LAPIC_ICR_HIGH_OFFSET: usize = 0x310;

const ICR_DELIVERY_MODE_INIT: u32 = 0b101 << 8;
const ICR_DELIVERY_MODE_STARTUP: u32 = 0b110 << 8;
const ICR_DELIVERY_STATUS_PENDING: u32 = 1 << 12;
const ICR_LEVEL_DEASSERT: u32 = 0;
const ICR_LEVEL_ASSERT: u32 = 1 << 14;
const ICR_TRIGGER_MODE_LEVEL: u32 = 1 << 15;

// SIPI vector 0x07 starts execution at physical 0x7000.
// This page must be reserved, identity-mapped, writable, and executable.
const AP_TRAMPOLINE_PHYS: usize = 0x7000;
const AP_TRAMPOLINE_VECTOR: u8 = (AP_TRAMPOLINE_PHYS >> 12) as u8;
const AP_TRAMPOLINE_SIZE: usize = crate::kernel::vm::PAGE_SIZE;

// AP stack backing is low physical memory, but AP receives a higher-half
// direct-map stack VA after paging is enabled.
const AP_STACK_BYTES: usize = 16 * 1024;
const AP_STACK_PHYS_BASE: u64 = 0x0200_0000;
const BOOTSTRAP_LOW_IDENTITY_BYTES: u64 = 64 * 1024 * 1024;
const AP_STACK_TOP_BASE: u64 =
    crate::arch::platform_layout::KERNEL_BOOTSTRAP_VIRT_BASE + AP_STACK_PHYS_BASE;

const AP_HANDOFF_MAGIC: u32 = 0x5952_4D41; // "YRMA"

// ApHandoff layout:
//   0  magic:            u32
//   4  cpu_id:           u32
//   8  stack_top:        u64
//   16 kernel_state_ptr: u64   // production: CR3/PML4 physical address
//   24 ready_flag_ptr:   u64   // diagnostic/layout only for now
//   32 ready_word:       u32   // AP assembly writes 1 here directly
//   36 reserved:         u32
const AP_HANDOFF_READY_WORD_OFFSET: usize = 32;

const AP_READY_POLL_ITERS: usize = 20_000_000;
const ICR_IDLE_POLL_ITERS: usize = 1_000_000;

// These are deliberately large, non-optimizable pause loops for early QEMU
// SMP bring-up. Later replace with calibrated TSC/LAPIC/PIT delays.
const INIT_TO_SIPI_DELAY_ITERS: usize = 5_000_000;

#[repr(C)]
#[derive(Clone, Copy)]
struct ApHandoff {
    magic: u32,
    cpu_id: u32,
    stack_top: u64,

    // Production meaning: CR3/PML4 physical address used by the AP before
    // entering long mode.
    kernel_state_ptr: u64,

    // Kept for layout/debug. The assembly currently writes ready_word
    // directly at AP_TRAMPOLINE_BASE + AP_OFF_HANDOFF + 32 instead of
    // dereferencing this pointer.
    ready_flag_ptr: u64,

    ready_word: u32,
    reserved: u32,
}

static AP_READY_FLAGS: [AtomicBool; crate::arch::platform_constants::MAX_CPUS] =
    [const { AtomicBool::new(false) }; crate::arch::platform_constants::MAX_CPUS];

#[cfg(all(not(test), not(feature = "hosted-dev")))]
struct TrampolineScratch(core::cell::UnsafeCell<[u8; AP_TRAMPOLINE_SIZE]>);

#[cfg(all(not(test), not(feature = "hosted-dev")))]
unsafe impl Sync for TrampolineScratch {}

#[cfg(all(not(test), not(feature = "hosted-dev")))]
static TRAMPOLINE_SCRATCH: TrampolineScratch =
    TrampolineScratch(core::cell::UnsafeCell::new([0; AP_TRAMPOLINE_SIZE]));

#[cfg(all(not(test), not(feature = "hosted-dev")))]
fn with_trampoline_scratch<R>(f: impl FnOnce(&mut [u8; AP_TRAMPOLINE_SIZE]) -> R) -> R {
    unsafe { f(&mut *TRAMPOLINE_SCRATCH.0.get()) }
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
    .set AP_OFF_HANDOFF, yarm_ap_trampoline_handoff - yarm_ap_trampoline_start
    .set AP_OFF_GDTR, ap_gdtr - yarm_ap_trampoline_start
    .set AP_GDT_BASE, AP_TRAMPOLINE_BASE + (ap_gdt - yarm_ap_trampoline_start)
    .set AP_GDT_LIMIT, (ap_gdt_end - ap_gdt) - 1
    .set AP_PM_ENTRY, AP_TRAMPOLINE_BASE + (ap_protected_entry - yarm_ap_trampoline_start)
    .set AP_LM_ENTRY, AP_TRAMPOLINE_BASE + (ap_long_entry - yarm_ap_trampoline_start)

yarm_ap_trampoline_start:
    cli
    cld

    // Breadcrumb: AP started executing real-mode trampoline.
    mov dx, 0x3F8
    mov al, 'g'
    out dx, al

    xor ax, ax
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov sp, 0x7c00

    mov al, 'G'
    out dx, al

    // CS after SIPI has base 0x7000, so cs:[AP_OFF_GDTR] resolves to the
    // physical descriptor inside the copied low trampoline page.
    lgdt cs:[AP_OFF_GDTR]

    mov al, 'p'
    out dx, al

    mov eax, cr0
    or eax, 1
    mov cr0, eax

    mov al, 'P'
    out dx, al

    // 16-bit far jump with 32-bit offset into protected mode.
    .byte 0x66
    .byte 0xEA
    .long AP_PM_ENTRY
    .word 0x0008

ap_gdtr:
    .word AP_GDT_LIMIT
    .long AP_GDT_BASE

    .align 8
ap_gdt:
    .quad 0x0000000000000000
    .quad 0x00cf9a000000ffff    // 0x08: 32-bit code
    .quad 0x00cf92000000ffff    // 0x10: 32-bit data
    .quad 0x00af9a000000ffff    // 0x18: 64-bit code
    .quad 0x00af92000000ffff    // 0x20: 64-bit data
ap_gdt_end:

    .code32
ap_protected_entry:
    mov dx, 0x3F8
    mov al, 'h'        // reached 32-bit protected mode
    out dx, al

    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov ss, ax

    // EBX = physical base of the copied trampoline page.
    mov ebx, AP_TRAMPOLINE_BASE

    mov al, '4'        // before CR4.PAE
    out dx, al

    // Architecturally clean IA-32e entry order:
    //   1. CR4.PAE = 1
    //   2. CR3 = PML4 physical address
    //   3. EFER.LME = 1
    //   4. CR0.PG = 1
    //   5. far jump to 64-bit code selector
    mov eax, cr4
    or eax, (1 << 5)
    mov cr4, eax

    mov al, 'A'        // after CR4.PAE
    out dx, al

    mov al, '3'        // before CR3 load
    out dx, al

    // Load CR3 from handoff.kernel_state_ptr.
    // YARM currently keeps the bootstrap PML4 below 4 GiB. Rust rejects
    // larger CR3 values because this 32-bit stage loads EAX into CR3.
    mov eax, [ebx + AP_OFF_HANDOFF + 16]
    mov cr3, eax

    mov al, 'C'        // after CR3 load
    out dx, al

    mov al, 'e'        // before EFER.LME
    out dx, al

    mov ecx, 0xC0000080
    rdmsr
    or eax, (1 << 8)
    wrmsr

    mov al, 'E'        // after EFER.LME
    out dx, al

    mov al, 'x'        // before CR0.PG
    out dx, al

    mov eax, cr0
    or eax, 0x80000000
    mov cr0, eax

    mov al, 'X'        // after CR0.PG
    out dx, al

    mov al, 'j'        // before long-mode far jump
    out dx, al

    .byte 0xEA
    .long AP_LM_ENTRY
    .word 0x0018

    .code64
ap_long_entry:
    mov dx, 0x3F8
    mov al, 'L'        // reached long mode
    out dx, al

    mov ax, 0x20
    mov ds, ax
    mov es, ax
    mov ss, ax

    mov rbx, AP_TRAMPOLINE_BASE

    mov al, 's'        // before stack load
    out dx, al

    mov rsp, [rbx + AP_OFF_HANDOFF + 8]
    and rsp, -16

    mov al, 'S'        // after stack load
    out dx, al

    // Stage SMP-1 proof stops here:
    // AP reached long mode, loaded stack, and writes ready_word.
    // Do not enter Rust yet. The AP has no per-CPU IDT/TSS/GS/scheduler/log
    // environment installed.
    mov dword ptr [AP_TRAMPOLINE_BASE + AP_OFF_HANDOFF + 32], 1

    mov al, 'R'        // ready word written
    out dx, al

    // Park AP fully offline in assembly.
    cli

1:
    hlt
    jmp 1b

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

// Kept as a future AP Rust entry point, but SMP-1 does not call it yet.
// The AP is parked in assembly after writing ready_word.
#[cfg(all(not(test), not(feature = "hosted-dev")))]
#[unsafe(no_mangle)]
extern "C" fn yarm_x86_64_ap_entry(_handoff_ptr: *const ApHandoff) -> ! {
    unsafe {
        core::arch::asm!("cli", options(nostack, nomem, preserves_flags));
    }

    loop {
        unsafe {
            core::arch::asm!("hlt", options(nostack, nomem, preserves_flags));
        }
    }
}

#[cfg(all(not(test), not(feature = "hosted-dev")))]
fn trampoline_symbol_addr(sym: *const u8) -> usize {
    let raw = sym as usize;
    let base = crate::arch::platform_layout::KERNEL_BOOTSTRAP_VIRT_BASE as usize;

    if raw < base {
        raw.saturating_add(base)
    } else {
        raw
    }
}

#[cfg(all(not(test), not(feature = "hosted-dev")))]
fn encode_trampoline_page(
    page: &mut [u8; AP_TRAMPOLINE_SIZE],
    handoff: ApHandoff,
) -> Option<usize> {
    page.fill(0);

    let (start, end, handoff_addr) = unsafe {
        (
            trampoline_symbol_addr(core::ptr::addr_of!(yarm_ap_trampoline_start).cast()),
            trampoline_symbol_addr(core::ptr::addr_of!(yarm_ap_trampoline_end).cast()),
            trampoline_symbol_addr(core::ptr::addr_of!(yarm_ap_trampoline_handoff).cast()),
        )
    };

    if end <= start {
        crate::yarm_log!(
            "YARM_SMP_TRAMPOLINE_SOURCE_INVALID reason=bad_range start=0x{:x} end=0x{:x}",
            start,
            end
        );
        return None;
    }

    let len = end - start;
    if len > AP_TRAMPOLINE_SIZE {
        crate::yarm_log!(
            "YARM_SMP_TRAMPOLINE_SOURCE_OVERSIZE len={} max={}",
            len,
            AP_TRAMPOLINE_SIZE
        );
        return None;
    }

    if handoff_addr < start || handoff_addr >= end {
        crate::yarm_log!(
            "YARM_SMP_TRAMPOLINE_HANDOFF_INVALID start=0x{:x} handoff=0x{:x} end=0x{:x}",
            start,
            handoff_addr,
            end
        );
        return None;
    }

    let handoff_off = handoff_addr - start;
    let handoff_len = core::mem::size_of::<ApHandoff>();

    if handoff_off + handoff_len > AP_TRAMPOLINE_SIZE {
        crate::yarm_log!(
            "YARM_SMP_TRAMPOLINE_HANDOFF_OVERSIZE off=0x{:x} len={}",
            handoff_off,
            handoff_len
        );
        return None;
    }

    unsafe {
        copy_nonoverlapping(start as *const u8, page.as_mut_ptr(), len);
    }

    let mut patched = handoff;
    patched.ready_flag_ptr =
        (AP_TRAMPOLINE_PHYS + handoff_off + AP_HANDOFF_READY_WORD_OFFSET) as u64;
    patched.ready_word = 0;
    patched.reserved = 0;

    let handoff_bytes = unsafe {
        core::slice::from_raw_parts(
            (&patched as *const ApHandoff).cast::<u8>(),
            core::mem::size_of::<ApHandoff>(),
        )
    };

    page[handoff_off..handoff_off + handoff_bytes.len()].copy_from_slice(handoff_bytes);

    crate::yarm_log!(
        "YARM_SMP_TRAMPOLINE_PREPARED src=0x{:x} len={} handoff_off=0x{:x} ready_phys=0x{:x} dst_phys=0x{:x}",
        start,
        len,
        handoff_off,
        patched.ready_flag_ptr,
        AP_TRAMPOLINE_PHYS
    );

    Some(handoff_off)
}

#[cfg(all(not(test), not(feature = "hosted-dev")))]
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

#[cfg(all(not(test), not(feature = "hosted-dev")))]
fn ap_ready_word_low_virt(handoff_off: usize) -> *const u32 {
    (AP_TRAMPOLINE_PHYS + handoff_off + AP_HANDOFF_READY_WORD_OFFSET) as *const u32
}

#[cfg(all(not(test), not(feature = "hosted-dev")))]
fn ap_ready_word_directmap_virt(handoff_off: usize) -> *const u32 {
    let trampoline_virt = (crate::arch::platform_layout::KERNEL_BOOTSTRAP_VIRT_BASE
        + AP_TRAMPOLINE_PHYS as u64) as usize;

    (trampoline_virt + handoff_off + AP_HANDOFF_READY_WORD_OFFSET) as *const u32
}

fn lapic_mmio_base() -> usize {
    super::platform_layout::LAPIC_MMIO_BASE
}

#[cfg(all(not(test), not(feature = "hosted-dev")))]
fn write_icr(apic_id: u8, value: u32) {
    let base = lapic_mmio_base();

    unsafe {
        write_volatile(
            (base + LAPIC_ICR_HIGH_OFFSET) as *mut u32,
            (apic_id as u32) << 24,
        );
        write_volatile((base + LAPIC_ICR_LOW_OFFSET) as *mut u32, value);
    }
}

#[cfg(any(test, feature = "hosted-dev"))]
fn write_icr(_apic_id: u8, _value: u32) {}

#[cfg(all(not(test), not(feature = "hosted-dev")))]
fn wait_for_icr_idle(apic_id: u8, phase: &str) {
    let base = lapic_mmio_base();

    for _ in 0..ICR_IDLE_POLL_ITERS {
        let low = unsafe { read_volatile((base + LAPIC_ICR_LOW_OFFSET) as *const u32) };
        if (low & ICR_DELIVERY_STATUS_PENDING) == 0 {
            return;
        }
        cpu_relax();
    }

    let low = unsafe { read_volatile((base + LAPIC_ICR_LOW_OFFSET) as *const u32) };
    crate::yarm_log!(
        "YARM_SMP_ICR_STUCK apic_id={} phase={} low=0x{:08x} base=0x{:x}",
        apic_id,
        phase,
        low,
        base
    );
}

#[cfg(any(test, feature = "hosted-dev"))]
fn wait_for_icr_idle(_apic_id: u8, _phase: &str) {}

fn send_init_sipi_sipi(apic_id: u8) {
    crate::yarm_log!(
        "YARM_SMP_IPI_SEQUENCE_BEGIN apic_id={} trampoline_phys=0x{:x} vector=0x{:02x}",
        apic_id,
        AP_TRAMPOLINE_PHYS,
        AP_TRAMPOLINE_VECTOR
    );

    wait_for_icr_idle(apic_id, "before_init_assert");

    crate::yarm_log!("YARM_SMP_IPI_INIT_ASSERT_BEGIN apic_id={}", apic_id);
    write_icr(
        apic_id,
        ICR_DELIVERY_MODE_INIT | ICR_TRIGGER_MODE_LEVEL | ICR_LEVEL_ASSERT,
    );
    crate::yarm_log!("YARM_SMP_IPI_INIT_ASSERT_WRITTEN apic_id={}", apic_id);
    wait_for_icr_idle(apic_id, "init_assert");

    spin_delay(INIT_TO_SIPI_DELAY_ITERS);

    crate::yarm_log!("YARM_SMP_IPI_INIT_DEASSERT_BEGIN apic_id={}", apic_id);
    write_icr(
        apic_id,
        ICR_DELIVERY_MODE_INIT | ICR_TRIGGER_MODE_LEVEL | ICR_LEVEL_DEASSERT,
    );
    crate::yarm_log!("YARM_SMP_IPI_INIT_DEASSERT_WRITTEN apic_id={}", apic_id);
    wait_for_icr_idle(apic_id, "init_deassert");

    spin_delay(INIT_TO_SIPI_DELAY_ITERS);

    crate::yarm_log!(
        "YARM_SMP_IPI_SIPI1_BEGIN apic_id={} vector=0x{:02x}",
        apic_id,
        AP_TRAMPOLINE_VECTOR
    );
    write_icr(
        apic_id,
        ICR_DELIVERY_MODE_STARTUP | AP_TRAMPOLINE_VECTOR as u32,
    );
    crate::yarm_log!(
        "YARM_SMP_IPI_SIPI1_WRITTEN apic_id={} vector=0x{:02x}",
        apic_id,
        AP_TRAMPOLINE_VECTOR
    );

    // SIPI1 is proven to start the AP in current QEMU runs.
    // Do not wait for ICR idle and do not send SIPI2 yet; return to poll.
    crate::yarm_log!(
        "YARM_SMP_IPI_SEQUENCE_DONE apic_id={} vector=0x{:02x} sipi2=skipped",
        apic_id,
        AP_TRAMPOLINE_VECTOR
    );
}

#[inline(always)]
fn cpu_relax() {
    #[cfg(all(target_arch = "x86_64", not(test), not(feature = "hosted-dev")))]
    unsafe {
        core::arch::asm!("pause", options(nostack, nomem, preserves_flags));
    }

    #[cfg(any(test, feature = "hosted-dev", not(target_arch = "x86_64")))]
    core::hint::spin_loop();
}

fn spin_delay(iterations: usize) {
    for _ in 0..iterations {
        cpu_relax();
    }
}

fn ap_stack_top(cpu: CpuId) -> u64 {
    AP_STACK_TOP_BASE + ((cpu.0 as u64 + 1) * AP_STACK_BYTES as u64)
}

#[cfg(all(not(test), not(feature = "hosted-dev")))]
fn current_cr3() -> u64 {
    let cr3: u64;
    unsafe {
        core::arch::asm!(
            "mov {}, cr3",
            out(reg) cr3,
            options(nostack, preserves_flags)
        );
    }
    cr3
}

fn prepare_trampoline_for_cpu(kernel: &KernelState, cpu: CpuId) -> Option<usize> {
    #[cfg(all(not(test), not(feature = "hosted-dev")))]
    let _ = kernel;

    AP_READY_FLAGS[cpu.0 as usize].store(false, Ordering::Release);

    let ap_stack_phys_end = AP_STACK_PHYS_BASE.saturating_add(
        (crate::arch::platform_constants::MAX_CPUS as u64).saturating_mul(AP_STACK_BYTES as u64),
    );

    if ap_stack_phys_end > BOOTSTRAP_LOW_IDENTITY_BYTES {
        crate::yarm_log!(
            "YARM_SMP_AP_STACK_RANGE_INVALID end=0x{:x} identity_limit=0x{:x}",
            ap_stack_phys_end,
            BOOTSTRAP_LOW_IDENTITY_BYTES
        );
        return None;
    }

    #[cfg(all(not(test), not(feature = "hosted-dev")))]
    let cr3_or_kernel_ptr = current_cr3();

    #[cfg(any(test, feature = "hosted-dev"))]
    let cr3_or_kernel_ptr = kernel as *const KernelState as usize as u64;

    #[cfg(all(not(test), not(feature = "hosted-dev")))]
    if cr3_or_kernel_ptr > u32::MAX as u64 {
        crate::yarm_log!(
            "YARM_SMP_AP_CR3_UNSUPPORTED cpu={} cr3=0x{:x} reason=trampoline_uses_32bit_cr3_load",
            cpu.0,
            cr3_or_kernel_ptr
        );
        return None;
    }

    let handoff = ApHandoff {
        magic: AP_HANDOFF_MAGIC,
        cpu_id: cpu.0 as u32,
        stack_top: ap_stack_top(cpu),
        kernel_state_ptr: cr3_or_kernel_ptr,
        ready_flag_ptr: 0,
        ready_word: 0,
        reserved: 0,
    };

    crate::yarm_log!(
        "YARM_SMP_AP_PREPARE cpu={} stack_top=0x{:x} cr3_or_kernel=0x{:x} trampoline=0x{:x}",
        cpu.0,
        handoff.stack_top,
        handoff.kernel_state_ptr,
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
            crate::kernel::vm::VirtAddr(yarm_x86_64_ap_entry as *const () as usize as u64),
        );

        crate::yarm_log!(
            "YARM_SMP_AP_CR3_MAP_CHECK cpu={} cr3=0x{:x} low7000={} ap_stack={} ap_entry={}",
            cpu.0,
            handoff.kernel_state_ptr,
            low_ok as u8,
            stack_ok as u8,
            entry_ok as u8
        );

        if !low_ok || !stack_ok || !entry_ok {
            return None;
        }

        with_trampoline_scratch(|page| {
            let Some(handoff_off) = encode_trampoline_page(page, handoff) else {
                return None;
            };
            write_trampoline_page(page);
            Some(handoff_off)
        })
    }

    #[cfg(any(test, feature = "hosted-dev"))]
    {
        Some(0)
    }
}

pub fn start_secondary_cpus(kernel: &mut KernelState) -> Result<usize, KernelError> {
    let present = kernel.present_cpu_bitmap();

    for raw_cpu in 0..crate::arch::platform_constants::MAX_CPUS {
        let cpu = CpuId(raw_cpu as u8);

        if cpu.0 == crate::arch::platform_constants::BOOTSTRAP_CPU_ID {
            continue;
        }

        if (present & (1u64 << cpu.0)) == 0 {
            continue;
        }

        let Some(handoff_off) = prepare_trampoline_for_cpu(kernel, cpu) else {
            crate::yarm_log!(
                "YARM_SMP_AP_PREPARE_FAILED cpu={} apic_id={}",
                cpu.0,
                cpu.0
            );
            continue;
        };

        crate::yarm_log!(
            "YARM_SMP_AP_WAIT_BEGIN cpu={} apic_id={} handoff_off=0x{:x} poll_iters={}",
            cpu.0,
            cpu.0,
            handoff_off,
            AP_READY_POLL_ITERS
        );

        send_init_sipi_sipi(cpu.0);

        #[cfg(any(test, feature = "hosted-dev"))]
        AP_READY_FLAGS[cpu.0 as usize].store(true, Ordering::Release);

        let mut ready = false;

        for _ in 0..AP_READY_POLL_ITERS {
            #[cfg(all(not(test), not(feature = "hosted-dev")))]
            let ap_ready = unsafe { read_volatile(ap_ready_word_low_virt(handoff_off)) } == 1;

            #[cfg(any(test, feature = "hosted-dev"))]
            let ap_ready = AP_READY_FLAGS[cpu.0 as usize].load(Ordering::Acquire);

            if ap_ready {
                ready = true;
                break;
            }

            cpu_relax();
        }

        if !ready {
            #[cfg(all(not(test), not(feature = "hosted-dev")))]
            {
                let low_val = unsafe { read_volatile(ap_ready_word_low_virt(handoff_off)) };
                let high_val = unsafe { read_volatile(ap_ready_word_directmap_virt(handoff_off)) };

                crate::yarm_log!(
                    "YARM_SMP_AP_TIMEOUT cpu={} apic_id={} trampoline=0x{:x} handoff_off=0x{:x} ready_low={} ready_high={}",
                    cpu.0,
                    cpu.0,
                    AP_TRAMPOLINE_PHYS,
                    handoff_off,
                    low_val,
                    high_val
                );
            }

            #[cfg(any(test, feature = "hosted-dev"))]
            crate::yarm_log!(
                "YARM_SMP_AP_TIMEOUT cpu={} apic_id={} trampoline=0x{:x} handoff_off=0x{:x}",
                cpu.0,
                cpu.0,
                AP_TRAMPOLINE_PHYS,
                handoff_off
            );

            continue;
        }

        AP_READY_FLAGS[cpu.0 as usize].store(true, Ordering::Release);

        // AP bootstrap proof succeeded:
        // - AP reached long mode
        // - AP loaded its stack
        // - AP wrote ready_word from assembly
        // - AP is parked in an assembly cli/hlt loop
        //
        // Do NOT call kernel.bring_up_cpu(cpu) yet.
        // Do NOT increment/return started CPU count yet.
        // Return Ok(0): no real scheduler CPU was brought online.
        return Ok(0);
    }

    Ok(0)
}
