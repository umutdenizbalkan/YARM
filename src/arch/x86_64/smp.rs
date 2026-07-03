// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use crate::kernel::boot::{KernelError, KernelState};
use crate::kernel::scheduler::CpuId;

#[cfg(all(not(test), not(feature = "hosted-dev")))]
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

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

/// Stage 183 increment 2: count of APs that reached the GS-initialized, interrupt-masked
/// Rust IDLE-ADMISSION loop (GS-base written + verified). This is DISTINCT from the
/// scheduler `online_cpu_count()`: these APs idle with interrupts masked and are NOT
/// admitted to the production scheduler, so `single_cpu` stays true and no task is ever
/// enqueued onto them. The Stage 183 SMP-liveness audit reports this as `ap_idle_live`.
static AP_IDLE_LIVE_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Number of APs currently in the GS-initialized idle-admission loop (Stage 183 inc.2).
pub fn ap_idle_live_count() -> usize {
    AP_IDLE_LIVE_COUNT.load(Ordering::Acquire)
}

/// Stage 183 increment 3: count of APs whose scheduler-admission PREREQUISITES are all
/// proven (kernel CR3 live + per-AP GDT/TSS loaded + LAPIC access verified + idle
/// task metadata/context validated). STRICTLY intermediate: distinct from both
/// `ap_idle_live` (which only requires the GS-verified idle loop) and the scheduler's
/// `online_cpu_count()` (which stays 1 — env-ready APs are still NOT
/// scheduler-runnable; no task is ever enqueued onto them).
static AP_ENV_READY_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Number of APs with all scheduler-admission prerequisites proven (Stage 183 inc.3).
pub fn ap_env_ready_count() -> usize {
    AP_ENV_READY_COUNT.load(Ordering::Acquire)
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

pub(crate) fn ap_stack_top(cpu: CpuId) -> u64 {
    AP_STACK_TOP_BASE + ((cpu.0 as u64 + 1) * AP_STACK_BYTES as u64)
}

/// Emits the AP per-CPU record + GS-base scaffold markers for `cpu`.
///
/// READ-ONLY since Stage 183 inc.3: the record itself is initialized in
/// `prepare_trampoline_for_cpu` BEFORE SIPI (the AP concurrently writes
/// `env_canary`/`saved_rsp` into the same record via gs:, so any BSP write
/// after SIPI would race it). This emitter only reads and reports.
///
/// **GS-base**: written BY THE AP (`wrmsr IA32_GS_BASE` in the naked entry,
/// verified by rdmsr readback); the admit poll grades it as
/// `X86_AP_GS_OK`/`X86_AP_GS_BAD`, so no deferral marker is emitted here.
fn emit_ap_percpu_scaffold(cpu: CpuId) {
    let record_base = super::percpu::record_base(cpu);

    crate::yarm_log!("X86_AP_PERCPU_BEGIN cpu={}", cpu.0);
    crate::yarm_log!(
        "X86_AP_PERCPU_SLOT_READY cpu={} base=0x{:x} size=0x{:x}",
        cpu.0,
        record_base,
        super::percpu::PerCpuRecord::SIZE
    );

    let record = super::percpu::read_record(cpu);
    crate::yarm_log!(
        "X86_AP_PERCPU_RECORD_READY cpu={} apic_id={} stack=0x{:x}",
        record.cpu_id,
        record.apic_id,
        record.stack_top
    );

    crate::yarm_log!(
        "X86_AP_GS_WRITE_BEGIN cpu={} base=0x{:x}",
        cpu.0,
        record_base
    );
    // The AP performs the IA32_GS_BASE wrmsr itself inside yarm_x86_64_ap_entry and
    // verifies it via rdmsr readback; the admit poll below grades the result.
    crate::yarm_log!(
        "X86_AP_GS_INIT_BY_AP cpu={} reason=wrmsr_in_ap_entry_graded_by_admit_poll",
        cpu.0
    );

    crate::yarm_log!("X86_AP_PERCPU_READY cpu={}", cpu.0);
}

/// Emits the AP per-CPU environment scaffold marker sequence for `cpu`.
///
/// The scaffold contract (this pass):
/// - **STACK**: real, AP-owned (`ap_stack_top` derives it from the per-CPU
///   slot allocated by the trampoline). Marker: `X86_AP_STACK_READY`.
/// - **GDT**: BSP GDT inherited via the trampoline. Safe for the parked
///   AP because the AP runs no user code and takes no interrupts. Marker:
///   `X86_AP_GDT_READY reason=bsp_gdt_shared_safe_while_ap_masked`.
/// - **TSS / IDT / GS / FPU**: explicitly **DEFERRED** with real reasons.
///   The AP parks with interrupts masked and runs no FP code, so none of
///   these are required for safe parking. Future AP scheduler participation
///   will need to flip these to READY with real per-CPU allocations.
///
/// All markers are emitted from the BSP — the AP itself cannot safely use
/// `yarm_log!` (no AP-safe printk lock yet). The values are deterministic
/// per `cpu`, so the BSP-side emission accurately reflects what the AP
/// observes.
fn emit_ap_env_scaffold(cpu: CpuId) {
    let stack_top = ap_stack_top(cpu);
    crate::yarm_log!("X86_AP_ENV_BEGIN cpu={} apic_id={}", cpu.0, cpu.0);
    crate::yarm_log!("X86_AP_STACK_READY cpu={} stack=0x{:x}", cpu.0, stack_top);
    // Stage 183 inc.3: GDT and TSS are now PER-AP (loaded by the AP itself — lgdt +
    // CS/SS reload + ltr); the admit poll below grades them into
    // X86_AP_GDT_LOCAL_OK / X86_AP_TSS_OK|X86_AP_TSS_BAD, so this scaffold no longer
    // claims a shared-GDT or a TSS deferral.
    crate::yarm_log!(
        "X86_AP_GDT_READY cpu={} reason=ap_local_gdt_graded_by_admit_poll",
        cpu.0
    );
    crate::yarm_log!(
        "X86_AP_IDT_DEFERRED cpu={} reason=interrupts_masked_no_handlers",
        cpu.0
    );
    crate::yarm_log!(
        "X86_AP_FPU_DEFERRED cpu={} reason=ap_runs_no_fp_code",
        cpu.0
    );
    crate::yarm_log!("X86_AP_ENV_READY cpu={}", cpu.0);
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
        // Stage 177 (SMP-READY): the per-CPU AP stack range overflows the identity
        // window — a real per-CPU stack aliasing / boot hazard.
        if crate::kernel::boot::smp_ready_enabled() {
            crate::yarm_log!(
                "SMP_READY_AP_STACK_ALIAS end=0x{:x} identity_limit=0x{:x}",
                ap_stack_phys_end,
                BOOTSTRAP_LOW_IDENTITY_BYTES
            );
            crate::yarm_log!(
                "SMP_READY_AP_BOOT_FAIL cpu={} reason=stack_range_invalid",
                cpu.0
            );
        }
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

    // Stage 183 inc.3: BSP-side per-CPU environment prep, all BEFORE SIPI so nothing
    // races the AP's own writes into the record:
    //  - per-AP GDT (BOOT_GDT selector layout) + per-AP TSS (rsp0 = the AP's stack top;
    //    ISTs stay 0 — only consumed via IDT gates, and the AP has no IDT yet);
    //  - the per-CPU record (id/apic/stack), the TSS pointer, and the idle-task
    //    METADATA (entry / stack / CR3 — a validated description, NOT an enqueued task).
    #[cfg(all(not(test), not(feature = "hosted-dev")))]
    let (ap_gdtr_limit, ap_gdt_base, ap_tss_base) =
        super::descriptor_tables::prepare_ap_descriptor_tables(cpu.0 as usize, ap_stack_top(cpu));
    #[cfg(any(test, feature = "hosted-dev"))]
    let (ap_gdtr_limit, ap_gdt_base, ap_tss_base) = (0u16, 0u64, 0u64);

    super::percpu::init_record_for_ap(cpu, cpu.0, ap_stack_top(cpu));
    super::percpu::set_tss_ptr(cpu, ap_tss_base);
    #[cfg(all(not(test), not(feature = "hosted-dev")))]
    super::percpu::set_idle_task_meta(
        cpu,
        yarm_x86_64_ap_entry as *const () as usize as u64,
        ap_stack_top(cpu),
        cr3_or_kernel_ptr,
    );

    let mut gdtr_image = [0u8; 10];
    gdtr_image[..2].copy_from_slice(&ap_gdtr_limit.to_le_bytes());
    gdtr_image[2..].copy_from_slice(&ap_gdt_base.to_le_bytes());

    #[cfg_attr(any(test, feature = "hosted-dev"), allow(unused_mut))]
    let mut handoff = ApHandoff {
        magic: AP_HANDOFF_MAGIC,
        cpu_id: cpu.0 as u32,
        stack_top: ap_stack_top(cpu),
        kernel_state_ptr: cr3_or_kernel_ptr,
        ready_flag_ptr: 0,
        ready_word: 0,
        reserved: 0,
        // Stage 183 inc.2: per-CPU record base for this AP, so its Rust entry can
        // wrmsr IA32_GS_BASE without a higher-half .bss access.
        percpu_record_ptr: super::percpu::record_base(cpu) as u64,
        // Stage 183 inc.2: fine-grained AP stage trace word starts at "none" (0).
        ap_stage: 0,
        _pad_stage: 0,
        // Stage 183 inc.3: the FULL kernel CR3 the AP reloads for its env steps. It is
        // the same root the BSP runs on while grading the markers — the kernel address
        // space (maps kernel text, low identity, .bss per-CPU/GDT/TSS, LAPIC MMIO).
        kernel_cr3: cr3_or_kernel_ptr,
        gdtr_image,
        _pad_gdtr: [0; 6],
        // Filled below after the map check; 0 tells the AP to skip the MMIO read.
        lapic_id_reg_va: 0,
        env_flags: 0,
        lapic_id_out: 0xFFFF_FFFF,
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

        // Stage 183 inc.3: walk-check the env-step VAs under the AP's kernel CR3 —
        // per-CPU record (gs: canary), per-AP GDT/TSS (.bss), and the LAPIC ID
        // register MMIO. The .bss VAs must be mapped (the BSP itself uses this root);
        // the LAPIC VA gates the AP's MMIO read (unmapped ⇒ pass 0 ⇒ AP skips and the
        // admit grading reports X86_AP_LAPIC_BAD honestly instead of faulting).
        let lapic_id_reg = (crate::arch::x86_64::platform_layout::LAPIC_MMIO_BASE + 0x20) as u64;
        let percpu_ok = crate::arch::x86_64::page_table::debug_root_maps_virt(
            root,
            crate::kernel::vm::VirtAddr(handoff.percpu_record_ptr),
        );
        let gdt_ok = crate::arch::x86_64::page_table::debug_root_maps_virt(
            root,
            crate::kernel::vm::VirtAddr(ap_gdt_base),
        );
        let tss_ok = crate::arch::x86_64::page_table::debug_root_maps_virt(
            root,
            crate::kernel::vm::VirtAddr(ap_tss_base),
        );
        let lapic_ok = crate::arch::x86_64::page_table::debug_root_maps_virt(
            root,
            crate::kernel::vm::VirtAddr(lapic_id_reg),
        );
        crate::yarm_log!(
            "YARM_SMP_AP_ENV_MAP_CHECK cpu={} percpu={} gdt={} tss={} lapic={}",
            cpu.0,
            percpu_ok as u8,
            gdt_ok as u8,
            tss_ok as u8,
            lapic_ok as u8
        );
        if !percpu_ok || !gdt_ok || !tss_ok {
            // The AP would triple-fault on lgdt/ltr/gs-store; refuse the SIPI instead.
            return None;
        }
        if lapic_ok {
            handoff.lapic_id_reg_va = lapic_id_reg;
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

    // Stage 177 (SMP-READY): default-off AP bring-up mirror markers. These honestly
    // MIRROR the existing X86_AP_* observability (they add no control flow); the AP
    // stays PARKED and out of the production scheduler (BSP-only is unchanged).
    let smp_ready = crate::kernel::boot::smp_ready_enabled();
    if smp_ready {
        crate::yarm_log!(
            "SMP_READY_BOOT_CPU_OK cpu={} present=0x{:x}",
            crate::arch::platform_constants::BOOTSTRAP_CPU_ID,
            present
        );
    }

    for raw_cpu in 0..crate::arch::platform_constants::MAX_CPUS {
        let cpu = CpuId(raw_cpu as u8);

        if cpu.0 == crate::arch::platform_constants::BOOTSTRAP_CPU_ID {
            continue;
        }

        if (present & (1u64 << cpu.0)) == 0 {
            continue;
        }

        if smp_ready {
            crate::yarm_log!("SMP_READY_AP_TRAMPOLINE_BEGIN cpu={}", cpu.0);
        }

        let Some(handoff_off) = prepare_trampoline_for_cpu(kernel, cpu) else {
            crate::yarm_log!("YARM_SMP_AP_PREPARE_FAILED cpu={} apic_id={}", cpu.0, cpu.0);
            if smp_ready {
                crate::yarm_log!("SMP_READY_AP_FALLBACK cpu={} reason=prepare_failed", cpu.0);
            }
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

            if smp_ready {
                crate::yarm_log!(
                    "SMP_READY_AP_FALLBACK cpu={} reason=trampoline_timeout",
                    cpu.0
                );
            }
            continue;
        }

        AP_READY_FLAGS[cpu.0 as usize].store(true, Ordering::Release);
        crate::yarm_log!("X86_AP_TRAMPOLINE_REACHED cpu={}", cpu.0);
        if smp_ready {
            // The AP reached the (already-split) trampoline and its unique per-CPU
            // stack slot is valid.
            crate::yarm_log!("SMP_READY_AP_ENTRY_OK cpu={}", cpu.0);
            crate::yarm_log!(
                "SMP_READY_AP_STACK_OK cpu={} stack_top=0x{:x}",
                cpu.0,
                ap_stack_top(cpu)
            );
        }

        // Pass 2: poll the trampoline `ready_word` for value `2`. The Rust
        // AP entry (`yarm_x86_64_ap_entry`) writes `2` into the same
        // identity-mapped ready_word slot before the cli/hlt park loop;
        // this avoids any AP-side higher-half memory access for the online
        // signal (the bootstrap PML4 maps text + ap_entry virt but not
        // necessarily kernel .bss). The Rust scaffold static AP_RUST_ONLINE
        // is mirrored for hosted-dev tests.
        let mut rust_online = false;
        for _ in 0..AP_READY_POLL_ITERS {
            // Stage 183 inc.2: the AP fast-forwards ready_word past the trampoline's `2`
            // (online) to its idle-admit stage, so accept `>= 2` for the online signal.
            #[cfg(all(not(test), not(feature = "hosted-dev")))]
            let online = unsafe { read_volatile(ap_ready_word_low_virt(handoff_off)) } >= 2;
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
            if smp_ready {
                crate::yarm_log!(
                    "SMP_READY_AP_FALLBACK cpu={} reason=rust_entry_timeout",
                    cpu.0
                );
            }
            continue;
        }

        crate::yarm_log!("X86_AP_ENTER_RUST cpu={}", cpu.0);

        // AP per-CPU record + GS-base scaffold. Each AP gets a fixed
        // record at a stable address in the global PER_CPU_SLOTS
        // table. GS-base write remains deferred until the AP entry
        // point grows a real Rust body that can WRMSR IA32_GS_BASE
        // from the handoff; the smoke gate accepts the explicit
        // deferral marker.
        emit_ap_percpu_scaffold(cpu);

        // AP per-CPU environment scaffold. Each component is either
        // wired (READY) or explicitly DEFERRED with a real reason — no
        // fake readiness. The AP itself is parked with interrupts
        // masked, so deferring TSS/IDT/GS/FPU is safe and explicitly
        // recorded for the smoke gate.
        emit_ap_env_scaffold(cpu);

        // Legacy markers preserved for the existing smoke-grep contract
        // (see doc/ARCH_X86_64.md §AP markers). Since Stage 183 inc.3 the
        // GDT/TSS are AP-local (loaded by the AP, graded by the admit poll
        // below); the reasons reflect that.
        crate::yarm_log!(
            "X86_AP_GDT_TSS_READY cpu={} reason=ap_local_graded_by_admit_poll",
            cpu.0
        );
        crate::yarm_log!(
            "X86_AP_IDT_READY cpu={} reason=interrupts_masked_no_handlers",
            cpu.0
        );
        crate::yarm_log!(
            "X86_AP_CPU_LOCAL_READY cpu={} reason=gs_base_percpu_record",
            cpu.0
        );
        crate::yarm_log!("X86_AP_ONLINE cpu={}", cpu.0);

        // Stage 183 increment 2+3: poll the AP's idle-admission stage (published into
        // the low identity-mapped ready_word by yarm_x86_64_ap_entry) and emit the
        // admission markers BSP-side (single serial writer — no AP-side log garbling).
        // The AP performed, in order: GS wrmsr+rdmsr verify (inc.2), then the inc.3 env
        // steps — kernel-CR3 reload + .bss canary, per-AP lgdt + CS/SS reload, ltr
        // (per-AP TSS), LAPIC ID MMIO read, idle-context publish — before writing
        // ready_word=3 and idling. Each step's result is graded here from env_flags +
        // the per-CPU record + the TSS busy-bit readback.
        #[cfg(all(not(test), not(feature = "hosted-dev")))]
        {
            use super::smp_trampoline::{
                AP_IDLE_ADMIT_PROOF, AP_STAGE_IDLE_ADMIT_GS_BAD, AP_STAGE_IDLE_ADMIT_OK, ap_env,
                ap_env_flags_low_virt, ap_lapic_id_out_low_virt, ap_stage_name,
                ap_stage_word_low_virt,
            };
            if AP_IDLE_ADMIT_PROOF {
                crate::yarm_log!("X86_AP_SCHED_ADMIT_BEGIN cpu={}", cpu.0);
                // The AP executes the kernel-CR3 transition inside the poll window.
                crate::yarm_log!("X86_AP_KERNEL_CR3_BEGIN cpu={}", cpu.0);
                let mut stage = 0u32;
                for _ in 0..AP_READY_POLL_ITERS {
                    let v = unsafe { read_volatile(ap_ready_word_low_virt(handoff_off)) };
                    if v == AP_STAGE_IDLE_ADMIT_OK || v == AP_STAGE_IDLE_ADMIT_GS_BAD {
                        stage = v;
                        break;
                    }
                    cpu_relax();
                }
                if stage == AP_STAGE_IDLE_ADMIT_OK {
                    crate::yarm_log!("X86_AP_GS_OK cpu={}", cpu.0);
                    super::percpu::mark_gs_base_written(cpu);
                } else if stage == AP_STAGE_IDLE_ADMIT_GS_BAD {
                    crate::yarm_log!("X86_AP_GS_BAD cpu={}", cpu.0);
                } else {
                    // The AP never published a terminal admit stage → it did not reach idle.
                    // Read the fine-grained stage word so the failure names the exact block
                    // the AP died in instead of a bare "timeout" (Stage 183 inc.2 diagnostic).
                    let last_raw = unsafe { read_volatile(ap_stage_word_low_virt(handoff_off)) };
                    crate::yarm_log!(
                        "X86_AP_SCHED_ADMIT_FAIL cpu={} reason=timeout last_stage={} last_stage_raw=0x{:x}",
                        cpu.0,
                        ap_stage_name(last_raw),
                        last_raw
                    );
                    crate::yarm_log!("X86_AP_IDLE_FAIL cpu={}", cpu.0);
                }
                if stage == AP_STAGE_IDLE_ADMIT_OK {
                    // Stage 183 inc.3: grade the env steps the AP ran between the GS
                    // verify and idle. env_flags/lapic_id_out are AP-written into the
                    // low handoff; the canary/saved_rsp are AP-written via gs: into
                    // the per-CPU record; the TSS busy bit is AP-written by ltr into
                    // the per-AP GDT. All are read back here on the BSP.
                    let env = unsafe { read_volatile(ap_env_flags_low_virt(handoff_off)) };
                    let lapic_id = unsafe { read_volatile(ap_lapic_id_out_low_virt(handoff_off)) };
                    let record = super::percpu::read_record(cpu);

                    // 1. Kernel CR3: reload flag + the higher-half .bss canary the AP
                    //    stored through gs: prove the kernel address space is live.
                    let cr3_ok = (env & ap_env::KERNEL_CR3_RELOADED) != 0
                        && record.env_canary == super::percpu::AP_ENV_CANARY;
                    if cr3_ok {
                        crate::yarm_log!(
                            "X86_AP_KERNEL_CR3_OK cpu={} cr3=0x{:x} bss_canary=0x{:x}",
                            cpu.0,
                            record.idle_cr3,
                            record.env_canary
                        );
                    } else if (env & ap_env::KERNEL_CR3_RELOADED) == 0 {
                        crate::yarm_log!(
                            "X86_AP_KERNEL_CR3_FAIL cpu={} reason=reload_flag_missing",
                            cpu.0
                        );
                    } else {
                        crate::yarm_log!(
                            "X86_AP_KERNEL_CR3_FAIL cpu={} reason=bss_canary_missing got=0x{:x}",
                            cpu.0,
                            record.env_canary
                        );
                    }

                    // 2. Per-AP GDT (lgdt + kernel CS/SS reload done by the AP).
                    let gdt_ok = (env & ap_env::GDT_LOADED) != 0;
                    if gdt_ok {
                        crate::yarm_log!(
                            "X86_AP_GDT_LOCAL_OK cpu={} reason=lgdt_plus_kernel_cs_ss_reload",
                            cpu.0
                        );
                    }

                    // 3. Per-AP TSS: ltr flag + the BUSY type ltr wrote into this
                    //    AP's GDT descriptor (read back from .bss).
                    let tss_busy = super::descriptor_tables::ap_tss_descriptor_busy(cpu.0 as usize);
                    let tss_ok = gdt_ok && (env & ap_env::TSS_LOADED) != 0 && tss_busy;
                    if !gdt_ok {
                        crate::yarm_log!("X86_AP_TSS_BAD cpu={} reason=gdt_not_loaded", cpu.0);
                    } else if tss_ok {
                        crate::yarm_log!(
                            "X86_AP_TSS_OK cpu={} rsp0=0x{:x} busy=1 ist=zero_until_ap_idt",
                            cpu.0,
                            super::descriptor_tables::ap_tss_rsp0(cpu.0 as usize)
                        );
                    } else if (env & ap_env::TSS_LOADED) == 0 {
                        crate::yarm_log!("X86_AP_TSS_BAD cpu={} reason=ltr_flag_missing", cpu.0);
                    } else {
                        crate::yarm_log!("X86_AP_TSS_BAD cpu={} reason=busy_bit_not_set", cpu.0);
                    }

                    // 4. LAPIC access: the AP read ITS OWN LAPIC ID register over MMIO
                    //    under the kernel CR3; the id must match this cpu's APIC id.
                    let lapic_ok = (env & ap_env::LAPIC_READ) != 0 && lapic_id == cpu.0 as u32;
                    if lapic_ok {
                        crate::yarm_log!("X86_AP_LAPIC_OK cpu={} apic_id={}", cpu.0, lapic_id);
                    } else if (env & ap_env::LAPIC_READ) == 0 {
                        crate::yarm_log!(
                            "X86_AP_LAPIC_BAD cpu={} reason=read_skipped_or_unmapped",
                            cpu.0
                        );
                    } else {
                        crate::yarm_log!(
                            "X86_AP_LAPIC_BAD cpu={} reason=id_mismatch got={} want={}",
                            cpu.0,
                            lapic_id,
                            cpu.0
                        );
                    }

                    // 5. AP timer policy: DEFERRED. Wiring the LAPIC timer before the
                    //    AP has an IDT would triple-fault on the first tick; the AP
                    //    IDT (+ per-AP IST stacks) is the next increment's gate.
                    crate::yarm_log!(
                        "X86_AP_LAPIC_TIMER_DEFERRED cpu={} reason=no_ap_idt_interrupts_masked",
                        cpu.0
                    );

                    // 6. Idle task/context: BSP-recorded idle METADATA (entry / stack /
                    //    CR3 — reserved, validated, NOT enqueued; the scheduler never
                    //    selects an AP) + the AP-published live rsp inside its stack.
                    let meta_ok =
                        (record.idle_flags & super::percpu::idle_flag::IDLE_TASK_META_SET) != 0
                            && record.idle_entry != 0
                            && record.idle_stack_top == ap_stack_top(cpu);
                    let stack_lo = record.idle_stack_top.saturating_sub(AP_STACK_BYTES as u64);
                    let ctx_ok = (env & ap_env::IDLE_CTX_PUBLISHED) != 0
                        && record.saved_rsp > stack_lo
                        && record.saved_rsp <= record.idle_stack_top;
                    if meta_ok {
                        crate::yarm_log!(
                            "X86_AP_IDLE_TASK_READY cpu={} entry=0x{:x} stack=0x{:x} enqueued=0",
                            cpu.0,
                            record.idle_entry,
                            record.idle_stack_top
                        );
                    }
                    if meta_ok && ctx_ok {
                        crate::yarm_log!(
                            "X86_AP_IDLE_CONTEXT_OK cpu={} rsp=0x{:x}",
                            cpu.0,
                            record.saved_rsp
                        );
                    } else {
                        crate::yarm_log!(
                            "X86_AP_IDLE_CONTEXT_BAD cpu={} reason=meta_or_live_rsp_invalid rsp=0x{:x}",
                            cpu.0,
                            record.saved_rsp
                        );
                    }

                    // All prerequisites proven → ap_env_ready (still NOT scheduler-
                    // online: bring_up_cpu is never called for APs, online stays 1).
                    if cr3_ok && gdt_ok && tss_ok && lapic_ok && meta_ok && ctx_ok {
                        AP_ENV_READY_COUNT.fetch_add(1, Ordering::AcqRel);
                        crate::yarm_log!("X86_AP_SCHED_PREREQ_OK cpu={}", cpu.0);
                    } else {
                        crate::yarm_log!("X86_AP_SCHED_PREREQ_INCOMPLETE cpu={}", cpu.0);
                    }
                }
                if stage == AP_STAGE_IDLE_ADMIT_OK || stage == AP_STAGE_IDLE_ADMIT_GS_BAD {
                    crate::yarm_log!("X86_AP_IDLE_ENTER cpu={} reason=cli_hlt_idle", cpu.0);
                    crate::yarm_log!("X86_AP_SCHED_ADMIT_DONE cpu={}", cpu.0);
                    // Only a GS-verified admit counts as clean idle-live.
                    if stage == AP_STAGE_IDLE_ADMIT_OK {
                        AP_IDLE_LIVE_COUNT.fetch_add(1, Ordering::AcqRel);
                    }
                }
            } else {
                crate::yarm_log!("X86_AP_RUST_PARK cpu={} reason=no_ap_scheduler_yet", cpu.0);
            }
        }
        #[cfg(any(test, feature = "hosted-dev"))]
        crate::yarm_log!("X86_AP_RUST_PARK cpu={} reason=no_ap_scheduler_yet", cpu.0);

        if smp_ready {
            // Honest mirrors of the existing X86_AP_* state: the AP's GDT/IDT/TSS are
            // the trampoline-inherited-while-masked set (safe for a parked, IRQ-masked
            // AP — NOT a production per-CPU env), the AP is Rust-online, and it idles
            // in a cli/hlt park loop. It is NOT admitted to the production scheduler
            // (BSP-only) — recorded as an explicit AP scheduler fallback.
            crate::yarm_log!(
                "SMP_READY_AP_GDT_IDT_OK cpu={} reason=trampoline_inherited_while_masked",
                cpu.0
            );
            crate::yarm_log!(
                "SMP_READY_AP_TSS_OK cpu={} reason=trampoline_inherited_while_masked",
                cpu.0
            );
            crate::yarm_log!("SMP_READY_AP_ONLINE cpu={}", cpu.0);
            crate::yarm_log!("SMP_READY_AP_IDLE_OK cpu={} reason=cli_hlt_park", cpu.0);
            crate::yarm_log!(
                "SMP_READY_AP_FALLBACK cpu={} reason=scheduler_bsp_only",
                cpu.0
            );
        }
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
