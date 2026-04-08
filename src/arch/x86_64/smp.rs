// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use crate::kernel::boot::{KernelError, KernelState};
use crate::kernel::scheduler::CpuId;
#[cfg(all(not(test), not(feature = "hosted-dev")))]
use core::arch::global_asm;
use core::ptr::copy_nonoverlapping;
use core::ptr::write_volatile;
use core::sync::atomic::{AtomicBool, Ordering};

const LAPIC_ICR_LOW_OFFSET: usize = 0x300;
const LAPIC_ICR_HIGH_OFFSET: usize = 0x310;

const ICR_DELIVERY_MODE_INIT: u32 = 0b101 << 8;
const ICR_DELIVERY_MODE_STARTUP: u32 = 0b110 << 8;
const ICR_LEVEL_ASSERT: u32 = 1 << 14;

const AP_TRAMPOLINE_PHYS: usize = 0x7000;
const AP_TRAMPOLINE_VECTOR: u8 = (AP_TRAMPOLINE_PHYS >> 12) as u8;
const AP_TRAMPOLINE_SIZE: usize = crate::kernel::vm::PAGE_SIZE;
#[cfg(any(test, feature = "hosted-dev"))]
const AP_HANDOFF_OFFSET: usize = 0x100;
const AP_HANDOFF_MAGIC: u32 = 0x5952_4D41; // "YRMA"
const AP_STACK_BYTES: usize = 16 * 1024;
const AP_STACK_TOP_BASE: u64 = 0x0000_0000_2000_0000;
const AP_READY_POLL_ITERS: usize = 2_000_000;

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
    .section .text.ap_trampoline,"ax",@progbits
    .global yarm_ap_trampoline_start
    .global yarm_ap_trampoline_end
    .global yarm_ap_trampoline_handoff
    .code16
    .set AP_OFF_REAL_L1, 1f - yarm_ap_trampoline_start
    .set AP_OFF_GDTR, 2f - yarm_ap_trampoline_start
    .set AP_OFF_PM_L5, 5f - yarm_ap_trampoline_start
    .set AP_OFF_HANDOFF, yarm_ap_trampoline_handoff - yarm_ap_trampoline_start

yarm_ap_trampoline_start:
    cli
    xor ax, ax
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov sp, 0x6ff0

    call 1f
1:
    pop si
    sub si, AP_OFF_REAL_L1
    lgdt [si + AP_OFF_GDTR]

    mov eax, cr0
    or eax, 1
    mov cr0, eax
    .byte 0xEA
    .word 3f
    .word 0x08

2:
    .word 4f - 1
    .long 0

    .code32
3:
    mov ax, 0x18
    mov ds, ax
    mov es, ax
    mov ss, ax

    call 5f
5:
    pop ebx
    sub ebx, AP_OFF_PM_L5

    mov eax, [ebx + AP_OFF_HANDOFF + 24]
    mov cr3, eax

    mov eax, cr4
    or eax, (1 << 5)
    mov cr4, eax

    mov ecx, 0xC0000080
    rdmsr
    or eax, (1 << 8)
    wrmsr

    mov eax, cr0
    or eax, 0x80000000
    mov cr0, eax
    .byte 0xEA
    .long 6f
    .word 0x10

    .code64
6:
    mov ax, 0x18
    mov ds, ax
    mov es, ax
    mov ss, ax

    lea rbx, [rip + yarm_ap_trampoline_start]
    mov rsp, [rbx + AP_OFF_HANDOFF + 8]
    lea rdi, [rbx + AP_OFF_HANDOFF]
    movabs rax, yarm_x86_64_ap_entry
    call rax

7:
    hlt
    jmp 7b

    .align 8
4:
    .quad 0x0000000000000000
    .quad 0x00cf9a000000ffff
    .quad 0x00af9a000000ffff
    .quad 0x00cf92000000ffff

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
    let handoff = unsafe { &*handoff_ptr };
    if handoff.magic == AP_HANDOFF_MAGIC {
        let ready_ptr = handoff.ready_flag_ptr as usize as *const AtomicBool;
        unsafe { (*ready_ptr).store(true, Ordering::Release) };
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

fn encode_handoff(page: &mut [u8; AP_TRAMPOLINE_SIZE], handoff: ApHandoff) {
    page.fill(0);
    #[cfg(all(not(test), not(feature = "hosted-dev")))]
    unsafe {
        let start = &yarm_ap_trampoline_start as *const u8;
        let end = &yarm_ap_trampoline_end as *const u8;
        let handoff_ptr = &yarm_ap_trampoline_handoff as *const u8;
        let len = end.offset_from(start) as usize;
        if len <= AP_TRAMPOLINE_SIZE {
            copy_nonoverlapping(start, page.as_mut_ptr(), len);
            let handoff_off = handoff_ptr.offset_from(start) as usize;
            let handoff_bytes = core::slice::from_raw_parts(
                (&handoff as *const ApHandoff).cast::<u8>(),
                core::mem::size_of::<ApHandoff>(),
            );
            page[handoff_off..handoff_off + handoff_bytes.len()].copy_from_slice(handoff_bytes);
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
    page[AP_HANDOFF_OFFSET..AP_HANDOFF_OFFSET + handoff_bytes.len()].copy_from_slice(handoff_bytes);
    }
}

#[cfg(not(test))]
fn write_trampoline_page(page: &[u8; AP_TRAMPOLINE_SIZE]) {
    unsafe {
        copy_nonoverlapping(
            page.as_ptr(),
            AP_TRAMPOLINE_PHYS as *mut u8,
            AP_TRAMPOLINE_SIZE,
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
fn trampoline_page_snapshot_for_test() -> [u8; AP_TRAMPOLINE_SIZE] {
    let mut out = [0u8; AP_TRAMPOLINE_SIZE];
    unsafe {
        let ptr = TEST_TRAMPOLINE_PAGE.0.get() as *const u8;
        copy_nonoverlapping(ptr, out.as_mut_ptr(), AP_TRAMPOLINE_SIZE);
    }
    out
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
    unsafe {
        write_volatile(
            (base + LAPIC_ICR_HIGH_OFFSET) as *mut u32,
            (apic_id as u32) << 24,
        );
        write_volatile((base + LAPIC_ICR_LOW_OFFSET) as *mut u32, value);
    }
}

fn send_init_sipi_sipi(apic_id: u8) {
    write_icr(apic_id, ICR_DELIVERY_MODE_INIT | ICR_LEVEL_ASSERT);
    spin_delay(20_000);
    write_icr(
        apic_id,
        ICR_DELIVERY_MODE_STARTUP | AP_TRAMPOLINE_VECTOR as u32,
    );
    spin_delay(200);
    write_icr(
        apic_id,
        ICR_DELIVERY_MODE_STARTUP | AP_TRAMPOLINE_VECTOR as u32,
    );
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
    let mut page = [0u8; AP_TRAMPOLINE_SIZE];
    AP_READY_FLAGS[cpu.0 as usize].store(false, Ordering::Release);
    let handoff = ApHandoff {
        magic: AP_HANDOFF_MAGIC,
        cpu_id: cpu.0 as u32,
        stack_top: ap_stack_top(cpu),
        #[cfg(all(not(test), not(feature = "hosted-dev")))]
        kernel_state_ptr: {
            let mut cr3: u64 = 0;
            unsafe { core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nostack, preserves_flags)); }
            cr3
        },
        #[cfg(any(test, feature = "hosted-dev"))]
        kernel_state_ptr: kernel as *const _ as usize as u64,
        ready_flag_ptr: (&AP_READY_FLAGS[cpu.0 as usize] as *const AtomicBool as usize) as u64,
    };
    encode_handoff(&mut page, handoff);
    write_trampoline_page(&page);
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
        send_init_sipi_sipi(cpu.0);

        let mut ready = false;
        for _ in 0..AP_READY_POLL_ITERS {
            if AP_READY_FLAGS[cpu.0 as usize].load(Ordering::Acquire) {
                ready = true;
                break;
            }
            core::hint::spin_loop();
        }

        if ready {
            match kernel.bring_up_cpu(cpu) {
                Ok(()) => started += 1,
                Err(KernelError::WrongObject) => {}
                Err(err) => return Err(err),
            }
        } else {
            crate::yarm_log!("YARM_SMP_AP_TIMEOUT cpu={} trampoline=0x{:x}", cpu.0, AP_TRAMPOLINE_PHYS);
        }
    }

    Ok(started)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_handoff(page: &[u8; AP_TRAMPOLINE_SIZE]) -> ApHandoff {
        unsafe {
            let ptr = page[AP_HANDOFF_OFFSET..].as_ptr().cast::<ApHandoff>();
            core::ptr::read_unaligned(ptr)
        }
    }

    #[test]
    fn trampoline_handoff_encoding_contains_cpu_stack_and_kernel_state() {
        let mut kernel = crate::kernel::boot::Bootstrap::init().expect("init");
        prepare_trampoline_for_cpu(&kernel, CpuId(2));

        let page = trampoline_page_snapshot_for_test();
        let handoff = read_handoff(&page);
        assert_eq!(handoff.magic, AP_HANDOFF_MAGIC);
        assert_eq!(handoff.cpu_id, 2);
        assert_eq!(handoff.stack_top, ap_stack_top(CpuId(2)));
        assert_eq!(
            handoff.kernel_state_ptr,
            (&mut kernel as *mut KernelState as usize) as u64
        );
        assert_ne!(handoff.ready_flag_ptr, 0);
    }

    #[test]
    fn secondary_cpu_startup_updates_online_cpu_accounting() {
        let mut kernel = crate::kernel::boot::Bootstrap::init().expect("init");
        let mut lapic_regs = [0u32; 256];
        set_lapic_mmio_base_for_test(lapic_regs.as_mut_ptr() as usize);

        let started = start_secondary_cpus(&mut kernel).expect("smp startup");
        assert_eq!(started, kernel.present_cpu_count().saturating_sub(1));
        assert_eq!(kernel.online_cpu_count(), kernel.present_cpu_count());
    }
}
