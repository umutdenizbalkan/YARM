// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! x86_64 SMP / AP startup.
//!
//! Public surface preserved for the rest of the kernel:
//!
//! - `pub fn start_secondary_cpus(kernel: &mut KernelState) -> Result<usize, KernelError>`
//!   (called from `arch::x86_64::boot::run_with_prepared_kernel`)
//! - `#[cfg(test)] pub fn set_lapic_mmio_base_for_test(base: usize)`
//!   (used by the in-module unit tests)
//!
//! The implementation copies a small mode-transition trampoline into a
//! fixed page in the bootstrap low-identity map (PA 0x7000), drops a
//! handoff struct into that page, then sends the canonical
//! INIT / SIPI / SIPI sequence via the LAPIC.  The AP brings itself
//! through real mode → 32-bit protected mode → long mode and tail-jumps
//! to the high-VA Rust entry, which sets a per-CPU ready flag.

use crate::kernel::boot::{KernelError, KernelState};
use crate::kernel::scheduler::CpuId;
#[cfg(all(not(test), not(feature = "hosted-dev")))]
use core::arch::global_asm;
use core::ptr::{copy_nonoverlapping, read_volatile, write_volatile};
use core::sync::atomic::{AtomicBool, Ordering};

// --- LAPIC ICR ---------------------------------------------------------------

const LAPIC_ICR_LOW_OFFSET: usize = 0x300;
const LAPIC_ICR_HIGH_OFFSET: usize = 0x310;

const ICR_DELIVERY_MODE_INIT: u32 = 0b101 << 8;
const ICR_DELIVERY_MODE_STARTUP: u32 = 0b110 << 8;
const ICR_DELIVERY_STATUS_PENDING: u32 = 1 << 12;
const ICR_LEVEL_DEASSERT: u32 = 0;
const ICR_LEVEL_ASSERT: u32 = 1 << 14;
const ICR_TRIGGER_MODE_LEVEL: u32 = 1 << 15;

// --- Trampoline placement ---------------------------------------------------

// 4 KiB-aligned, below 1 MiB, inside the 0..64 MiB bootstrap low-identity
// window covered by `boot_pml4` PML4[0] / PDPT[0] / boot_pd.
const AP_TRAMPOLINE_PHYS: usize = 0x7000;
const AP_TRAMPOLINE_VECTOR: u8 = (AP_TRAMPOLINE_PHYS >> 12) as u8;
const AP_TRAMPOLINE_PAGE_SIZE: usize = 4096;

const _: () = assert!(AP_TRAMPOLINE_PHYS & 0xfff == 0);
const _: () = assert!(AP_TRAMPOLINE_PHYS < 0x10_0000);

// --- AP stacks --------------------------------------------------------------

const AP_STACK_BYTES: usize = 16 * 1024;
// 32 MiB: above the kernel image / BSS, inside the 64 MiB bootstrap
// low-identity window.  Must stay in sync with `boot.rs::AP_STACK_PHYS_BASE`.
const AP_STACK_PHYS_BASE: u64 = 0x0200_0000;
const BOOTSTRAP_LOW_IDENTITY_BYTES: u64 = 64 * 1024 * 1024;

// AP stacks are addressed through the higher-half direct map
// (PML4[511] -> 1 GiB pages covering PA 0..510 GiB at
// KERNEL_BOOTSTRAP_VIRT_BASE+).  The PA backing remains in the
// low-identity 64 MiB window so any transitional code that touches
// the stack via a low-PA alias still hits valid memory.
const AP_STACK_TOP_BASE: u64 =
    crate::arch::platform_layout::KERNEL_BOOTSTRAP_VIRT_BASE + AP_STACK_PHYS_BASE;

// --- Polling budgets --------------------------------------------------------

const ICR_IDLE_POLL_ITERS: usize = 100_000;
const AP_READY_POLL_ITERS: usize = 5_000_000;
// Conservative ~10 ms equivalent on a slow QEMU virtual core; the
// canonical Intel sequence is INIT, 10 ms wait, SIPI, ~200 us wait, SIPI.
const AP_INIT_TO_SIPI_DELAY: usize = 50_000;
const AP_SIPI_TO_SIPI_DELAY: usize = 1_000;

// --- Handoff struct ---------------------------------------------------------

const AP_HANDOFF_MAGIC: u32 = 0x5952_4D41; // "YRMA"

#[repr(C)]
#[derive(Clone, Copy)]
struct ApHandoff {
    magic: u32,           // +0
    cpu_id: u32,          // +4
    stack_top: u64,       // +8
    page_table_root: u64, // +16  (PA of BSP PML4 = current CR3 & ~0xfff)
    ready_flag_ptr: u64,  // +24
}
const _: () = assert!(core::mem::size_of::<ApHandoff>() == 32);

// In hosted/test builds we don't have the real-mode trampoline; stub the
// page with a known offset so `trampoline_handoff_snapshot_for_test` can
// read the encoded handoff back out.
#[cfg(any(test, feature = "hosted-dev"))]
const AP_HANDOFF_OFFSET: usize = 0x100;

// --- AP trampoline blob (real machine only) ---------------------------------

// The blob lives in `.rodata.ap_trampoline_src` (a kernel-image section
// linked at high VA).  Before sending INIT/SIPI we copy the blob into the
// low-identity page at `AP_TRAMPOLINE_PHYS` and patch in the handoff.
//
// All absolute addresses encoded inside the blob are computed at
// assembly time as `0x7000 + (label - blob_start)`, so the bytes are
// position-independent with respect to the blob's link-time address and
// position-correct with respect to its run-time copy at PA 0x7000.
//
// UART markers emitted by the AP itself (port 0x3F8):
//   'I' real-mode entry
//   'P' 32-bit protected mode
//   'L' 64-bit long mode
//   'R' about to call into Rust
//   'Y' Rust AP entry reached
#[cfg(all(not(test), not(feature = "hosted-dev")))]
global_asm!(
    r#"
.section .rodata.ap_trampoline_src,"a",@progbits
.global yarm_ap_trampoline_blob_start
.global yarm_ap_trampoline_blob_end
.global yarm_ap_trampoline_handoff
.set TRAMP_PHYS_BASE, 0x7000

.align 16
.code16
yarm_ap_trampoline_blob_start:
    cli
    cld

    // DS = ES = SS = CS so [disp16] resolves to (0x7000 + disp).
    mov ax, cs
    mov ds, ax
    mov es, ax
    mov ss, ax
    xor sp, sp

    // 'I' — AP reached real-mode trampoline.
    mov dx, 0x3F8
    mov al, 0x49
    out dx, al

    // Load 32-bit GDTR (data32 prefix to force m16:32 form).
    // Encoded directly because the LLVM Intel parser does not accept
    // multi-symbol arithmetic inside a memory operand.
    //   0x66          data32
    //   0x0F 0x01 /2  lgdt m16:32  (ModRM 0x16: disp16-only)
    //   word          disp = ap_gdtr_pseudo - blob_start  (DS:disp = 0x7000+disp)
    .byte 0x66, 0x0F, 0x01, 0x16
    .word ap_gdtr_pseudo - yarm_ap_trampoline_blob_start

    // Enable CR0.PE.
    mov eax, cr0
    or eax, 1
    mov cr0, eax

    // Far jump to 32-bit code segment (0x08:pm32_entry).
    .byte 0x66, 0xEA
    .long TRAMP_PHYS_BASE + (pm32_entry - yarm_ap_trampoline_blob_start)
    .word 0x08

.code32
pm32_entry:
    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov fs, ax
    mov gs, ax
    mov ss, ax

    // 'P'.
    mov dx, 0x3F8
    mov al, 0x50
    out dx, al

    // CR3 = handoff.page_table_root.  Encoded as `mov eax, [moffs32]`
    // (opcode 0xA1) so the operand carries a single 32-bit absolute
    // address — the LLVM Intel parser otherwise rejects multi-symbol
    // memory operands.
    .byte 0xA1
    .long TRAMP_PHYS_BASE + (yarm_ap_trampoline_handoff - yarm_ap_trampoline_blob_start) + 16
    mov cr3, eax

    // CR4.PAE.
    mov eax, cr4
    or eax, (1 << 5)
    mov cr4, eax

    // EFER.LME.
    mov ecx, 0xC0000080
    rdmsr
    or eax, (1 << 8)
    wrmsr

    // CR0.PG (long mode active after the next far jump).
    mov eax, cr0
    or eax, (1 << 31)
    mov cr0, eax

    // Far jump to 64-bit code segment (0x18:lm64_entry).
    .byte 0xEA
    .long TRAMP_PHYS_BASE + (lm64_entry - yarm_ap_trampoline_blob_start)
    .word 0x18

.code64
lm64_entry:
    mov ax, 0x20
    mov ds, ax
    mov es, ax
    mov fs, ax
    mov gs, ax
    mov ss, ax

    // 'L'.
    mov dx, 0x3F8
    mov al, 0x4C
    out dx, al

    // ESI = absolute PA of the handoff (zero-extends to RSI in long
    // mode).  Encoded as `mov esi, imm32` (0xBE) directly — the LLVM
    // Intel parser does not accept multi-symbol immediate expressions.
    .byte 0xBE
    .long TRAMP_PHYS_BASE + (yarm_ap_trampoline_handoff - yarm_ap_trampoline_blob_start)
    // RSP = handoff.stack_top (high-VA direct-map address).
    mov rsp, [rsi + 8]
    // First arg = &handoff.
    mov rdi, rsi

    // 'R'.
    mov al, 0x52
    out dx, al

    // Tail-jump into the high-VA Rust entry.  movabs encodes the
    // 64-bit absolute kernel-image address.
    movabs rax, offset yarm_x86_64_ap_entry
    jmp rax

.code16
.align 8
ap_gdt:
    .quad 0x0000000000000000  // 0x00 null
    .quad 0x00CF9A000000FFFF  // 0x08 32-bit code
    .quad 0x00CF92000000FFFF  // 0x10 32-bit data
    .quad 0x00AF9A000000FFFF  // 0x18 64-bit code (L=1)
    .quad 0x00AF92000000FFFF  // 0x20 64-bit data
ap_gdt_end:

.align 4
ap_gdtr_pseudo:
    .word ap_gdt_end - ap_gdt - 1
    .long TRAMP_PHYS_BASE + (ap_gdt - yarm_ap_trampoline_blob_start)

.align 8
yarm_ap_trampoline_handoff:
    .zero 32

yarm_ap_trampoline_blob_end:
    "#
);

#[cfg(all(not(test), not(feature = "hosted-dev")))]
unsafe extern "C" {
    static yarm_ap_trampoline_blob_start: u8;
    static yarm_ap_trampoline_blob_end: u8;
    static yarm_ap_trampoline_handoff: u8;
}

#[cfg(all(not(test), not(feature = "hosted-dev")))]
#[unsafe(no_mangle)]
extern "C" fn yarm_x86_64_ap_entry(handoff_ptr: *const ApHandoff) -> ! {
    // Emit a single UART byte before any Rust formatting — proves the AP
    // reached high-VA Rust code with a working stack and CR3.
    unsafe {
        core::arch::asm!(
            "out dx, al",
            in("dx") 0x3F8u16,
            in("al") 0x59u8, // 'Y'
            options(nostack, nomem, preserves_flags),
        );
    }

    let handoff = unsafe { &*handoff_ptr };
    if handoff.magic == AP_HANDOFF_MAGIC {
        let ready = handoff.ready_flag_ptr as usize as *const AtomicBool;
        unsafe { (*ready).store(true, Ordering::Release) };
        crate::yarm_log!(
            "YARM_SMP_AP_ONLINE cpu={} stack_top=0x{:x} cr3=0x{:x}",
            handoff.cpu_id,
            handoff.stack_top,
            handoff.page_table_root
        );
    } else {
        crate::yarm_log!(
            "YARM_SMP_AP_BAD_HANDOFF magic=0x{:08x} expected=0x{:08x}",
            handoff.magic,
            AP_HANDOFF_MAGIC
        );
    }
    loop {
        unsafe {
            core::arch::asm!("hlt", options(nomem, nostack, preserves_flags));
        }
    }
}

// --- AP ready flags ---------------------------------------------------------

static AP_READY_FLAGS: [AtomicBool; crate::arch::platform_constants::MAX_CPUS] =
    [const { AtomicBool::new(false) }; crate::arch::platform_constants::MAX_CPUS];

// --- Test-only LAPIC redirect ------------------------------------------------

#[cfg(test)]
static TEST_LAPIC_BASE: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

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

// --- LAPIC ICR helpers ------------------------------------------------------

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

fn wait_for_icr_idle(apic_id: u8, phase: &str) -> bool {
    let base = lapic_mmio_base();
    for _ in 0..ICR_IDLE_POLL_ITERS {
        let low = unsafe { read_volatile((base + LAPIC_ICR_LOW_OFFSET) as *const u32) };
        if (low & ICR_DELIVERY_STATUS_PENDING) == 0 {
            return true;
        }
        core::hint::spin_loop();
    }
    crate::yarm_log!(
        "YARM_SMP_ICR_STUCK apic_id={} phase={}",
        apic_id,
        phase
    );
    false
}

fn spin_delay(iterations: usize) {
    for _ in 0..iterations {
        core::hint::spin_loop();
    }
}

// --- Per-CPU prep -----------------------------------------------------------

fn ap_stack_top(cpu: CpuId) -> u64 {
    AP_STACK_TOP_BASE + ((cpu.0 as u64 + 1) * AP_STACK_BYTES as u64)
}

#[cfg(all(not(test), not(feature = "hosted-dev")))]
fn current_cr3() -> u64 {
    let cr3: u64;
    unsafe {
        core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nostack, preserves_flags));
    }
    cr3
}

#[cfg(all(not(test), not(feature = "hosted-dev")))]
fn copy_trampoline_to_low_identity(handoff: ApHandoff) {
    let dest_va = (crate::arch::platform_layout::KERNEL_BOOTSTRAP_VIRT_BASE
        + AP_TRAMPOLINE_PHYS as u64) as usize;
    unsafe {
        let blob_start = &yarm_ap_trampoline_blob_start as *const u8;
        let blob_end = &yarm_ap_trampoline_blob_end as *const u8;
        let handoff_sym = &yarm_ap_trampoline_handoff as *const u8;
        let len = blob_end.offset_from(blob_start) as usize;
        debug_assert!(
            len > 0 && len <= AP_TRAMPOLINE_PAGE_SIZE,
            "AP trampoline blob must fit in the trampoline page"
        );
        let handoff_off = handoff_sym.offset_from(blob_start) as usize;
        debug_assert!(handoff_off + core::mem::size_of::<ApHandoff>() <= len);

        // Zero the destination page first, then copy the blob, then
        // patch the handoff in place.
        core::ptr::write_bytes(dest_va as *mut u8, 0, AP_TRAMPOLINE_PAGE_SIZE);
        copy_nonoverlapping(blob_start, dest_va as *mut u8, len);
        let handoff_dest = (dest_va + handoff_off) as *mut ApHandoff;
        core::ptr::write_unaligned(handoff_dest, handoff);

        crate::yarm_log!(
            "YARM_SMP_TRAMPOLINE_READY phys=0x{:x} blob_len={} handoff_off=0x{:x} cr3=0x{:x} stack_top=0x{:x} ready_ptr=0x{:x}",
            AP_TRAMPOLINE_PHYS,
            len,
            handoff_off,
            handoff.page_table_root,
            handoff.stack_top,
            handoff.ready_flag_ptr
        );
    }
}

// Hosted/test path: fake the trampoline page in a static scratch buffer
// so `trampoline_handoff_snapshot_for_test` can verify the encoding.
#[cfg(any(test, feature = "hosted-dev"))]
const AP_STUB: [u8; 4] = [0xFA, 0xF4, 0xEB, 0xFC]; // cli; hlt; jmp .-2

#[cfg(any(test, feature = "hosted-dev"))]
struct TrampolineScratch(core::cell::UnsafeCell<[u8; AP_TRAMPOLINE_PAGE_SIZE]>);
#[cfg(any(test, feature = "hosted-dev"))]
unsafe impl Sync for TrampolineScratch {}
#[cfg(any(test, feature = "hosted-dev"))]
static TRAMPOLINE_SCRATCH: TrampolineScratch =
    TrampolineScratch(core::cell::UnsafeCell::new([0u8; AP_TRAMPOLINE_PAGE_SIZE]));

#[cfg(any(test, feature = "hosted-dev"))]
fn copy_trampoline_to_scratch(handoff: ApHandoff) {
    unsafe {
        let dest = (*TRAMPOLINE_SCRATCH.0.get()).as_mut_ptr();
        core::ptr::write_bytes(dest, 0, AP_TRAMPOLINE_PAGE_SIZE);
        copy_nonoverlapping(AP_STUB.as_ptr(), dest, AP_STUB.len());
        let handoff_dest = dest.add(AP_HANDOFF_OFFSET) as *mut ApHandoff;
        core::ptr::write_unaligned(handoff_dest, handoff);
    }
}

#[cfg(test)]
fn trampoline_handoff_snapshot_for_test() -> ApHandoff {
    unsafe {
        let base = (*TRAMPOLINE_SCRATCH.0.get()).as_ptr();
        let p = base.add(AP_HANDOFF_OFFSET) as *const ApHandoff;
        core::ptr::read_unaligned(p)
    }
}

fn prepare_trampoline_for_cpu(kernel: &KernelState, cpu: CpuId) {
    AP_READY_FLAGS[cpu.0 as usize].store(false, Ordering::Release);

    // Sanity: the AP stack PA backing must stay inside the bootstrap
    // low-identity window.  This is also asserted from the unit test.
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
        page_table_root: current_cr3() & !0xfffu64,
        #[cfg(any(test, feature = "hosted-dev"))]
        page_table_root: kernel as *const _ as usize as u64,
        ready_flag_ptr: (&AP_READY_FLAGS[cpu.0 as usize] as *const AtomicBool as usize) as u64,
    };

    #[cfg(all(not(test), not(feature = "hosted-dev")))]
    {
        let _ = kernel;
        copy_trampoline_to_low_identity(handoff);
    }

    #[cfg(any(test, feature = "hosted-dev"))]
    {
        let _ = kernel;
        copy_trampoline_to_scratch(handoff);
    }
}

// --- INIT/SIPI/SIPI ---------------------------------------------------------

fn send_init_sipi_sipi(apic_id: u8) {
    crate::yarm_log!("YARM_SMP_SEND_INIT apic_id={}", apic_id);
    write_icr(
        apic_id,
        ICR_DELIVERY_MODE_INIT | ICR_TRIGGER_MODE_LEVEL | ICR_LEVEL_ASSERT,
    );
    let _ = wait_for_icr_idle(apic_id, "init_assert");
    write_icr(
        apic_id,
        ICR_DELIVERY_MODE_INIT | ICR_TRIGGER_MODE_LEVEL | ICR_LEVEL_DEASSERT,
    );
    let _ = wait_for_icr_idle(apic_id, "init_deassert");
    spin_delay(AP_INIT_TO_SIPI_DELAY);

    crate::yarm_log!(
        "YARM_SMP_SEND_SIPI apic_id={} vector=0x{:02x} attempt=1",
        apic_id,
        AP_TRAMPOLINE_VECTOR
    );
    write_icr(
        apic_id,
        ICR_DELIVERY_MODE_STARTUP | AP_TRAMPOLINE_VECTOR as u32,
    );
    let _ = wait_for_icr_idle(apic_id, "sipi1");
    spin_delay(AP_SIPI_TO_SIPI_DELAY);

    crate::yarm_log!(
        "YARM_SMP_SEND_SIPI apic_id={} vector=0x{:02x} attempt=2",
        apic_id,
        AP_TRAMPOLINE_VECTOR
    );
    write_icr(
        apic_id,
        ICR_DELIVERY_MODE_STARTUP | AP_TRAMPOLINE_VECTOR as u32,
    );
    let _ = wait_for_icr_idle(apic_id, "sipi2");

    // Hosted/test: synthesize an "AP came online" signal because there
    // is no real LAPIC delivering INIT/SIPI.  This keeps the unit test
    // for kernel CPU accounting deterministic.
    #[cfg(test)]
    AP_READY_FLAGS[apic_id as usize].store(true, Ordering::Release);
}

// --- Public entry -----------------------------------------------------------

pub fn start_secondary_cpus(kernel: &mut KernelState) -> Result<usize, KernelError> {
    crate::yarm_log!(
        "YARM_SMP_INIT_BEGIN trampoline_phys=0x{:x} vector=0x{:02x} stack_top_base=0x{:x} stack_bytes={}",
        AP_TRAMPOLINE_PHYS,
        AP_TRAMPOLINE_VECTOR,
        AP_STACK_TOP_BASE,
        AP_STACK_BYTES
    );

    let mut started = 0usize;
    let present = kernel.present_cpu_bitmap();

    for cpu_idx in 0..crate::arch::platform_constants::MAX_CPUS {
        let cpu = CpuId(cpu_idx as u8);
        if cpu.0 == crate::arch::platform_constants::BOOTSTRAP_CPU_ID {
            continue;
        }
        if (present & (1u64 << cpu.0)) == 0 {
            continue;
        }

        prepare_trampoline_for_cpu(kernel, cpu);
        send_init_sipi_sipi(cpu.0);

        let mut ready = false;
        let mut polled = 0usize;
        for _ in 0..AP_READY_POLL_ITERS {
            if AP_READY_FLAGS[cpu.0 as usize].load(Ordering::Acquire) {
                ready = true;
                break;
            }
            polled += 1;
            core::hint::spin_loop();
        }

        if ready {
            crate::yarm_log!(
                "YARM_SMP_AP_ONLINE_OBSERVED cpu={} polled_iters={}",
                cpu.0,
                polled
            );
            match kernel.bring_up_cpu(cpu) {
                Ok(()) => started += 1,
                Err(KernelError::WrongObject) => {}
                Err(err) => return Err(err),
            }
        } else {
            crate::yarm_log!(
                "YARM_SMP_AP_TIMEOUT cpu={} trampoline_phys=0x{:x} polled_iters={} ready_flag={}",
                cpu.0,
                AP_TRAMPOLINE_PHYS,
                polled,
                AP_READY_FLAGS[cpu.0 as usize].load(Ordering::Acquire) as u8
            );
        }
    }

    Ok(started)
}

// --- Tests ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trampoline_handoff_encoding_contains_cpu_stack_and_kernel_state() {
        std::thread::Builder::new()
            .name("trampoline_handoff_encoding".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                let kernel = crate::kernel::boot::Bootstrap::init_static().expect("init");
                prepare_trampoline_for_cpu(kernel, CpuId(2));
                let h = trampoline_handoff_snapshot_for_test();
                assert_eq!(h.magic, AP_HANDOFF_MAGIC);
                assert_eq!(h.cpu_id, 2);
                assert_eq!(h.stack_top, ap_stack_top(CpuId(2)));
                assert_eq!(
                    h.page_table_root,
                    (kernel as *mut KernelState as usize) as u64
                );
                assert_ne!(h.ready_flag_ptr, 0);
            })
            .expect("spawn test thread")
            .join()
            .expect("join test thread");
    }

    #[test]
    fn secondary_cpu_startup_updates_online_cpu_accounting() {
        std::thread::Builder::new()
            .name("smp_startup_accounting".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                let kernel = crate::kernel::boot::Bootstrap::init_static().expect("init");
                let mut lapic_regs = [0u32; 256];
                set_lapic_mmio_base_for_test(lapic_regs.as_mut_ptr() as usize);
                let started = start_secondary_cpus(kernel).expect("smp startup");
                assert_eq!(started, kernel.present_cpu_count().saturating_sub(1));
                assert_eq!(kernel.online_cpu_count(), kernel.present_cpu_count());
            })
            .expect("spawn test thread")
            .join()
            .expect("join test thread");
    }

    #[test]
    fn ap_stack_backing_fits_bootstrap_low_identity_window() {
        let total_ap_stack_bytes = (crate::arch::platform_constants::MAX_CPUS as u64)
            .saturating_mul(AP_STACK_BYTES as u64);
        assert!(AP_STACK_PHYS_BASE + total_ap_stack_bytes <= BOOTSTRAP_LOW_IDENTITY_BYTES);
    }
}
