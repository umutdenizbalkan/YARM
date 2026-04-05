// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use crate::kernel::boot::KernelState;
use crate::kernel::scheduler::CpuId;
use core::cell::UnsafeCell;

const AP_TRAMPOLINE_PHYS_BASE: usize = 0x7000;
const AP_TRAMPOLINE_PAGE_SIZE: usize = 4096;
const AP_STACK_SIZE: usize = 16 * 1024;

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const LAPIC_ICR_LOW_OFFSET: usize = 0x300;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const LAPIC_ICR_HIGH_OFFSET: usize = 0x310;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const LAPIC_ICR_DELIVERY_PENDING: u32 = 1 << 12;

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const IPI_DELIVERY_MODE_INIT: u32 = 0b101 << 8;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const IPI_DELIVERY_MODE_STARTUP: u32 = 0b110 << 8;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const IPI_LEVEL_ASSERT: u32 = 1 << 14;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const IPI_TRIGGER_LEVEL: u32 = 1 << 15;

struct StaticBytes<const N: usize>(UnsafeCell<[u8; N]>);
// SAFETY: early boot startup is serialized on BSP.
unsafe impl<const N: usize> Sync for StaticBytes<N> {}

struct StaticStacks<const CPUS: usize, const N: usize>(UnsafeCell<[[u8; N]; CPUS]>);
// SAFETY: early boot startup is serialized on BSP.
unsafe impl<const CPUS: usize, const N: usize> Sync for StaticStacks<CPUS, N> {}

static AP_TRAMPOLINE_PAGE: StaticBytes<AP_TRAMPOLINE_PAGE_SIZE> =
    StaticBytes(UnsafeCell::new([0; AP_TRAMPOLINE_PAGE_SIZE]));
static AP_STACKS: StaticStacks<{ crate::kernel::scheduler::MAX_CPUS }, AP_STACK_SIZE> =
    StaticStacks(UnsafeCell::new(
        [[0; AP_STACK_SIZE]; crate::kernel::scheduler::MAX_CPUS],
    ));

/// Best-effort x86_64 AP startup sequence.
///
/// The current kernel still executes all scheduler and trap handling state in a
/// BSP-owned `KernelState`, so after sending INIT-SIPI-SIPI we complete the
/// topology handshake through `KernelState::bring_up_cpu`.
pub fn start_secondary_cpus(kernel: &mut KernelState) -> usize {
    let present_bitmap = kernel.present_cpu_bitmap();
    let mut started = 0usize;

    for cpu in 0..crate::kernel::scheduler::MAX_CPUS {
        let cpu_id = cpu as u8;
        if cpu_id == crate::arch::platform_constants::BOOTSTRAP_CPU_ID {
            continue;
        }
        if (present_bitmap & (1u64 << cpu_id)) == 0 {
            continue;
        }

        let _stack_top = prepare_ap_stack(cpu_id);
        let _ = prepare_ap_trampoline(cpu_id, kernel as *mut KernelState as usize);

        #[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
        {
            send_init_sipi_sipi(cpu_id);
        }

        if kernel.bring_up_cpu(CpuId(cpu_id)).is_ok() {
            started = started.saturating_add(1);
        }
    }

    started
}

fn prepare_ap_stack(cpu: u8) -> usize {
    let idx = cpu as usize;
    if idx >= crate::kernel::scheduler::MAX_CPUS {
        return 0;
    }
    // SAFETY: single-threaded during early SMP bring-up.
    let stacks = unsafe { &*AP_STACKS.0.get() };
    stacks[idx].as_ptr() as usize + AP_STACK_SIZE
}

fn prepare_ap_trampoline(cpu: u8, kernel_state_ptr: usize) -> usize {
    // SAFETY: single-threaded during early SMP bring-up.
    let page = unsafe { &mut *AP_TRAMPOLINE_PAGE.0.get() };
    page.fill(0x90);

    // Encode a compact handoff block for future AP trampoline assembly use.
    page[0..8].copy_from_slice(&(cpu as u64).to_le_bytes());
    page[8..16].copy_from_slice(&(prepare_ap_stack(cpu) as u64).to_le_bytes());
    page[16..24].copy_from_slice(&(kernel_state_ptr as u64).to_le_bytes());

    AP_TRAMPOLINE_PHYS_BASE
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
fn lapic_write_u32(base: usize, offset: usize, value: u32) {
    let reg = (base + offset) as *mut u32;
    // SAFETY: caller passes MMIO LAPIC base from platform layout, identity mapped during early boot.
    unsafe { core::ptr::write_volatile(reg, value) };
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
fn lapic_read_u32(base: usize, offset: usize) -> u32 {
    let reg = (base + offset) as *const u32;
    // SAFETY: caller passes MMIO LAPIC base from platform layout, identity mapped during early boot.
    unsafe { core::ptr::read_volatile(reg) }
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
fn wait_icr_idle(base: usize) {
    for _ in 0..100_000 {
        let low = lapic_read_u32(base, LAPIC_ICR_LOW_OFFSET);
        if (low & LAPIC_ICR_DELIVERY_PENDING) == 0 {
            break;
        }
        core::hint::spin_loop();
    }
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
fn send_ipi(base: usize, apic_id: u8, icr_low: u32) {
    wait_icr_idle(base);
    lapic_write_u32(base, LAPIC_ICR_HIGH_OFFSET, (apic_id as u32) << 24);
    lapic_write_u32(base, LAPIC_ICR_LOW_OFFSET, icr_low);
    wait_icr_idle(base);
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
fn io_wait() {
    // SAFETY: architectural delay port used only during early boot sequencing.
    unsafe {
        core::arch::asm!(
            "out dx, al",
            in("dx") 0x80u16,
            in("al") 0u8,
            options(nomem, nostack, preserves_flags)
        )
    }
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
fn send_init_sipi_sipi(apic_id: u8) {
    let lapic = crate::arch::x86_64::platform_layout::LAPIC_MMIO_BASE;

    send_ipi(
        lapic,
        apic_id,
        IPI_DELIVERY_MODE_INIT | IPI_LEVEL_ASSERT | IPI_TRIGGER_LEVEL,
    );
    io_wait();

    let vector = ((AP_TRAMPOLINE_PHYS_BASE >> 12) & 0xFF) as u32;
    send_ipi(lapic, apic_id, IPI_DELIVERY_MODE_STARTUP | vector);
    io_wait();
    send_ipi(lapic, apic_id, IPI_DELIVERY_MODE_STARTUP | vector);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trampoline_handoff_block_is_populated() {
        let ptr = prepare_ap_trampoline(2, 0x1234_5678);
        assert_eq!(ptr, AP_TRAMPOLINE_PHYS_BASE);

        // SAFETY: test-only read of static trampoline page.
        let page = unsafe { &*AP_TRAMPOLINE_PAGE.0.get() };
        assert_eq!(u64::from_le_bytes(page[0..8].try_into().unwrap()), 2);
        assert_eq!(
            u64::from_le_bytes(page[16..24].try_into().unwrap()),
            0x1234_5678
        );
    }

    #[test]
    fn startup_marks_present_secondaries_online() {
        let mut kernel = crate::kernel::boot::Bootstrap::init().expect("init");
        let started = start_secondary_cpus(&mut kernel);
        assert!(started >= 1);
        assert!(kernel.online_cpu_count() >= 2);
    }
}
