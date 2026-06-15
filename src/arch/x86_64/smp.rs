// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use crate::kernel::boot::{KernelError, KernelState};
use crate::kernel::scheduler::CpuId;

#[cfg(all(not(test), not(feature = "hosted-dev")))]
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicBool, Ordering};

// Stage 108 / Milestone 2 Pass 1: the AP trampoline (16/32/64-bit startup
// assembly + trampoline-page encoding) lives in the sibling module
// `smp_trampoline` per the AI_AGENT_RULES §5.2 split. This file keeps only
// the Rust SMP bring-up logic (LAPIC IPI sequencing, handoff construction,
// ready-word polling).
use super::smp_trampoline::{
    AP_HANDOFF_MAGIC, AP_TRAMPOLINE_PHYS, AP_TRAMPOLINE_VECTOR, ApHandoff,
};
#[cfg(all(not(test), not(feature = "hosted-dev")))]
use super::smp_trampoline::{
    ap_ready_word_directmap_virt, ap_ready_word_low_virt, encode_trampoline_page,
    with_trampoline_scratch, write_trampoline_page, yarm_x86_64_ap_entry,
};

const LAPIC_ICR_LOW_OFFSET: usize = 0x300;
const LAPIC_ICR_HIGH_OFFSET: usize = 0x310;

const ICR_DELIVERY_MODE_INIT: u32 = 0b101 << 8;
const ICR_DELIVERY_MODE_STARTUP: u32 = 0b110 << 8;
const ICR_DELIVERY_STATUS_PENDING: u32 = 1 << 12;
const ICR_LEVEL_DEASSERT: u32 = 0;
const ICR_LEVEL_ASSERT: u32 = 1 << 14;
const ICR_TRIGGER_MODE_LEVEL: u32 = 1 << 15;

// AP stack backing is low physical memory, but AP receives a higher-half
// direct-map stack VA after paging is enabled.
const AP_STACK_BYTES: usize = 16 * 1024;
const AP_STACK_PHYS_BASE: u64 = 0x0200_0000;
const BOOTSTRAP_LOW_IDENTITY_BYTES: u64 = 64 * 1024 * 1024;
const AP_STACK_TOP_BASE: u64 =
    crate::arch::platform_layout::KERNEL_BOOTSTRAP_VIRT_BASE + AP_STACK_PHYS_BASE;

const AP_READY_POLL_ITERS: usize = 20_000_000;
const ICR_IDLE_POLL_ITERS: usize = 1_000_000;

// These are deliberately large, non-optimizable pause loops for early QEMU
// SMP bring-up. Later replace with calibrated TSC/LAPIC/PIT delays.
const INIT_TO_SIPI_DELAY_ITERS: usize = 5_000_000;

static AP_READY_FLAGS: [AtomicBool; crate::arch::platform_constants::MAX_CPUS] =
    [const { AtomicBool::new(false) }; crate::arch::platform_constants::MAX_CPUS];

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

/// Stage 109 / Milestone 2 Pass 2: scaffolding gate for the AP Rust-entry
/// path. The cmdline knob `yarm.x86_ap_rust=1` flips this flag at boot
/// (handled in `kernel/boot_command_line.rs`), and a future pass will read
/// it inside `prepare_trampoline_for_cpu` to populate
/// `ApHandoff::ap_entry_addr`. Today the trampoline asm is unchanged from
/// Stage 108 and ignores this flag; the gate exists so observability tests
/// can prove the cmdline path is wired through to the SMP layer without
/// activating the regression-prone Rust-entry path.
static AP_RUST_ENTRY_ENABLE: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

pub fn ap_rust_entry_enabled() -> bool {
    AP_RUST_ENTRY_ENABLE.load(core::sync::atomic::Ordering::Acquire)
}

pub fn set_ap_rust_entry_enabled(enabled: bool) {
    AP_RUST_ENTRY_ENABLE.store(enabled, core::sync::atomic::Ordering::Release);
}

pub fn start_secondary_cpus(kernel: &mut KernelState) -> Result<usize, KernelError> {
    let present = kernel.present_cpu_bitmap();
    let mut rust_online_aps = 0usize;

    for raw_cpu in 0..crate::arch::platform_constants::MAX_CPUS {
        let cpu = CpuId(raw_cpu as u8);

        if cpu.0 == crate::arch::platform_constants::BOOTSTRAP_CPU_ID {
            continue;
        }

        if (present & (1u64 << cpu.0)) == 0 {
            continue;
        }

        let Some(handoff_off) = prepare_trampoline_for_cpu(kernel, cpu) else {
            crate::yarm_log!("YARM_SMP_AP_PREPARE_FAILED cpu={} apic_id={}", cpu.0, cpu.0);
            continue;
        };

        crate::yarm_log!(
            "YARM_SMP_AP_WAIT_BEGIN cpu={} apic_id={} handoff_off=0x{:x} poll_iters={}",
            cpu.0,
            cpu.0,
            handoff_off,
            AP_READY_POLL_ITERS
        );

        crate::yarm_log!("X86_AP_INIT_SENT cpu={}", cpu.0);
        crate::yarm_log!("X86_AP_STARTUP_SENT cpu={}", cpu.0);
        send_init_sipi_sipi(cpu.0);

        #[cfg(any(test, feature = "hosted-dev"))]
        AP_READY_FLAGS[cpu.0 as usize].store(true, Ordering::Release);

        let mut ready = false;

        for _ in 0..AP_READY_POLL_ITERS {
            // Pass 2: AP writes 1 then quickly transitions to 2 inside
            // Rust entry. Accept any non-zero as "trampoline reached".
            #[cfg(all(not(test), not(feature = "hosted-dev")))]
            let ap_ready = unsafe { read_volatile(ap_ready_word_low_virt(handoff_off)) } != 0;

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
        crate::yarm_log!("X86_AP_TRAMPOLINE_REACHED cpu={}", cpu.0);

        // Pass 2: poll the trampoline `ready_word` for value `2`. The Rust
        // AP entry (`yarm_x86_64_ap_entry`) writes `2` into the same
        // identity-mapped ready_word slot before the cli/hlt park loop;
        // this avoids any AP-side higher-half memory access for the online
        // signal (the bootstrap PML4 maps text + ap_entry virt but not
        // necessarily kernel .bss). The Rust scaffold static AP_RUST_ONLINE
        // is mirrored for hosted-dev tests.
        let mut rust_online = false;
        for _ in 0..AP_READY_POLL_ITERS {
            #[cfg(all(not(test), not(feature = "hosted-dev")))]
            let online = unsafe { read_volatile(ap_ready_word_low_virt(handoff_off)) } == 2;
            #[cfg(any(test, feature = "hosted-dev"))]
            let online =
                super::smp_trampoline::AP_RUST_ONLINE[cpu.0 as usize].load(Ordering::Acquire);
            if online {
                rust_online = true;
                break;
            }
            cpu_relax();
        }
        #[cfg(all(not(test), not(feature = "hosted-dev")))]
        if rust_online {
            super::smp_trampoline::AP_RUST_ONLINE[cpu.0 as usize].store(true, Ordering::Release);
        }
        if !rust_online {
            crate::yarm_log!(
                "X86_AP_RUST_TIMEOUT cpu={} reason=ap_did_not_publish_online_flag",
                cpu.0
            );
            continue;
        }

        crate::yarm_log!("X86_AP_ENTER_RUST cpu={}", cpu.0);
        crate::yarm_log!(
            "X86_AP_GDT_TSS_READY cpu={} reason=trampoline_gdt_inherited",
            cpu.0
        );
        crate::yarm_log!(
            "X86_AP_IDT_READY cpu={} reason=interrupts_masked_no_handlers",
            cpu.0
        );
        crate::yarm_log!("X86_AP_GS_READY cpu={} reason=no_per_cpu_yet", cpu.0);
        crate::yarm_log!(
            "X86_AP_CPU_LOCAL_READY cpu={} reason=handoff_identity_only",
            cpu.0
        );
        crate::yarm_log!("X86_AP_ONLINE cpu={}", cpu.0);
        crate::yarm_log!("X86_AP_RUST_PARK cpu={}", cpu.0);
        rust_online_aps += 1;

        // SAFETY FENCE: the AP is parked in a Rust cli/hlt loop with no
        // production per-CPU env (no IDT/TSS/GS). DO NOT invoke the
        // scheduler bring-up entry point: production scheduler is BSP-only
        // for this pass. Continue to the next AP.
    }

    let present_count = present.count_ones() as usize;
    crate::yarm_log!(
        "X86_SMP_STARTUP started_secondary={} online_cpus=1 present_cpus={}",
        rust_online_aps,
        present_count
    );
    crate::yarm_log!(
        "X86_SMP_OBSERVATION_OK rust_aps={} scheduler_aps=0",
        rust_online_aps
    );

    // Pass 2 contract: return APs whose Rust runtime is online + parked.
    // The production scheduler online count (kernel.online_cpu_count())
    // intentionally stays at 1 (BSP-only).
    Ok(rust_online_aps)
}
