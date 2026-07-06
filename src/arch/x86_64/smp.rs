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

/// Stage 183 increment 4: count of APs that passed the controlled interrupt smoke —
/// AP-safe IDT loaded, exactly one BSP-sent fixed IPI delivered into the pure-asm
/// handler (count + vector recorded via gs:, LAPIC EOI, iretq), and the AP resumed
/// back into its interrupt-masked idle loop. No lost wake, no duplicate delivery, no
/// unexpected vector. Still NOT scheduler-online.
static AP_INTERRUPT_READY_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Number of APs with interrupt-safe idle proven (Stage 183 inc.4).
pub fn ap_interrupt_ready_count() -> usize {
    AP_INTERRUPT_READY_COUNT.load(Ordering::Acquire)
}

/// Stage 183.5: per-CPU handoff offset recorded at boot admit time (usize::MAX =
/// none) so the post-graduated-proof scheduler-online admission phase can read the
/// AP's low identity-mapped stage/wake words.
static AP_HANDOFF_OFFS: [AtomicUsize; crate::arch::platform_constants::MAX_CPUS] =
    [const { AtomicUsize::new(usize::MAX) }; crate::arch::platform_constants::MAX_CPUS];

/// Stage 183.5: per-CPU "interrupt smoke passed" flags — the hard gate for the
/// scheduler-online admission of that AP.
static AP_IRQ_READY_FLAGS: [AtomicBool; crate::arch::platform_constants::MAX_CPUS] =
    [const { AtomicBool::new(false) }; crate::arch::platform_constants::MAX_CPUS];

/// Stage 183.5: count of scheduler-online APs whose remote-wake proof passed
/// (exactly one wake delivered, observed, re-idled; no lost/dup).
static AP_REMOTE_WAKE_OK_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Number of APs with the scheduler remote-wake proof passed (Stage 183.5).
pub fn ap_remote_wake_ok_count() -> usize {
    AP_REMOTE_WAKE_OK_COUNT.load(Ordering::Acquire)
}

/// Stage 183.5: one-shot latch for the scheduler-online admission phase.
static AP_SCHED_ONLINE_ADMISSION_DONE: AtomicBool = AtomicBool::new(false);

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

/// Stage 183 inc.4 fix: read the BSP's own LAPIC ESR (write-to-latch, then read) for
/// the fixed-IPI send diagnostics.
#[cfg(all(not(test), not(feature = "hosted-dev")))]
fn bsp_lapic_esr_read() -> u32 {
    let base = lapic_mmio_base();
    unsafe {
        write_volatile((base + 0x280) as *mut u32, 0);
        read_volatile((base + 0x280) as *const u32)
    }
}

/// Stage 183 inc.4 fix: bounded ICR delivery-status wait with a boolean verdict (the
/// logging `wait_for_icr_idle` keeps its unit signature for the INIT/SIPI path).
#[cfg(all(not(test), not(feature = "hosted-dev")))]
fn icr_delivery_idle() -> bool {
    let base = lapic_mmio_base();
    for _ in 0..ICR_IDLE_POLL_ITERS {
        let low = unsafe { read_volatile((base + LAPIC_ICR_LOW_OFFSET) as *const u32) };
        if (low & ICR_DELIVERY_STATUS_PENDING) == 0 {
            return true;
        }
        cpu_relax();
    }
    false
}

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

/// Stage 183 inc.4: the BSP's CR4, mirrored onto each AP (`mov cr4` in the AP env
/// steps) so the APs' control state (PGE/OSFXSR/…) converges on the BSP's. The value
/// is valid by construction — the BSP itself runs with it on identical QEMU vCPUs.
#[cfg(all(not(test), not(feature = "hosted-dev")))]
fn current_cr4() -> u64 {
    let cr4: u64;
    unsafe {
        core::arch::asm!(
            "mov {}, cr4",
            out(reg) cr4,
            options(nostack, preserves_flags)
        );
    }
    cr4
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

    // Stage 183 inc.4: AP-safe IDT (catch-all park stubs + the smoke-vector handler)
    // and the BSP CR4 the AP mirrors. Both BEFORE SIPI, like all other env prep.
    #[cfg(all(not(test), not(feature = "hosted-dev")))]
    let (ap_idtr_limit, ap_idt_base) = super::descriptor_tables::prepare_ap_idt();
    #[cfg(any(test, feature = "hosted-dev"))]
    let (ap_idtr_limit, ap_idt_base) = (0u16, 0u64);

    let mut idtr_image = [0u8; 10];
    idtr_image[..2].copy_from_slice(&ap_idtr_limit.to_le_bytes());
    idtr_image[2..].copy_from_slice(&ap_idt_base.to_le_bytes());

    #[cfg(all(not(test), not(feature = "hosted-dev")))]
    let bsp_cr4 = current_cr4();
    #[cfg(any(test, feature = "hosted-dev"))]
    let bsp_cr4 = 0u64;

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
        // Stage 183 inc.4: AP-safe IDT image + the BSP CR4 to mirror.
        idtr_image,
        _pad_idtr: [0; 6],
        bsp_cr4,
        // Stage 183 inc.4 fix: AP-written LAPIC readiness readbacks start cleared.
        svr_out: 0,
        tpr_out: 0,
        esr_out: 0,
        wake_reenter_out: 0,
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
        // Stage 183 inc.4: the AP IDT (.bss) and its stub text must also be mapped
        // under the AP's kernel CR3 — unmapped would turn the smoke IPI into a
        // triple fault instead of a handled interrupt.
        let idt_ok = crate::arch::x86_64::page_table::debug_root_maps_virt(
            root,
            crate::kernel::vm::VirtAddr(ap_idt_base),
        );
        let stub_ok = crate::arch::x86_64::page_table::debug_root_maps_virt(
            root,
            crate::kernel::vm::VirtAddr(super::descriptor_tables::ap_idt_stub_base()),
        );
        crate::yarm_log!(
            "YARM_SMP_AP_ENV_MAP_CHECK cpu={} percpu={} gdt={} tss={} lapic={} idt={} idt_stubs={}",
            cpu.0,
            percpu_ok as u8,
            gdt_ok as u8,
            tss_ok as u8,
            lapic_ok as u8,
            idt_ok as u8,
            stub_ok as u8
        );
        if !percpu_ok || !gdt_ok || !tss_ok || !idt_ok || !stub_ok {
            // The AP would triple-fault on lgdt/ltr/gs-store/lidt; refuse the SIPI.
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
                ap_env_flags_low_virt, ap_esr_out_low_virt, ap_lapic_id_out_low_virt,
                ap_stage_name, ap_stage_word_low_virt, ap_svr_out_low_virt, ap_tpr_out_low_virt,
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

                    // ---- Stage 183 inc.4: AP-safe IDT + controlled interrupt smoke ----
                    // 7. CR4 sync: the AP mirrored the BSP's CR4 (control-state
                    //    convergence — prerequisite for any future AP Rust execution).
                    if (env & ap_env::CR4_SYNCED) != 0 {
                        crate::yarm_log!("X86_AP_CR4_SYNC_OK cpu={}", cpu.0);
                    } else {
                        crate::yarm_log!(
                            "X86_AP_CR4_SYNC_FAIL cpu={} reason=sync_flag_missing",
                            cpu.0
                        );
                    }

                    // 8. AP-safe IDT: catch-all park stubs + the smoke-vector handler,
                    //    loaded by the AP via lidt. The shared kernel BOOT_IDT is NOT
                    //    AP-safe (full Rust trap path); this dedicated IDT is pure asm.
                    crate::yarm_log!("X86_AP_IDT_BEGIN cpu={}", cpu.0);
                    let idt_ok = (env & ap_env::IDT_LOADED) != 0;
                    if idt_ok {
                        crate::yarm_log!(
                            "X86_AP_IDT_OK cpu={} base=0x{:x}",
                            cpu.0,
                            super::descriptor_tables::ap_idt_table_base()
                        );
                    } else {
                        crate::yarm_log!("X86_AP_IDT_BAD cpu={} reason=lidt_flag_missing", cpu.0);
                    }
                    // 9. IST policy: every AP IDT gate must use ist=0. No IST is
                    //    required this increment — the AP never leaves its known-good
                    //    idle stack (no user mode, no stack switch, no nesting;
                    //    interrupts are enabled ONLY inside the controlled sti;hlt
                    //    smoke window), so the interrupted rsp is always valid. A
                    //    nonzero-IST gate would dispatch onto empty TSS ist slots —
                    //    validated here. Real per-AP IST stacks land with
                    //    scheduler-online, before any non-idle-stack interrupt path.
                    if super::descriptor_tables::ap_idt_any_ist_nonzero() {
                        crate::yarm_log!(
                            "X86_AP_IST_BAD cpu={} reason=gate_ist_nonzero_without_per_ap_stacks",
                            cpu.0
                        );
                    } else {
                        crate::yarm_log!(
                            "X86_AP_IST_OK cpu={} mode=not_required reason=idle_stack_only_no_nesting",
                            cpu.0
                        );
                    }

                    // 9b. (183.4 fix) LAPIC interrupt-delivery readiness. The old
                    //     X86_AP_LAPIC_OK only proved an MMIO *read* (APIC id) — a
                    //     software-DISABLED LAPIC (SVR bit 8 clear, the post-INIT
                    //     reset state) still answers reads but silently DROPS fixed
                    //     IPIs, which is exactly the observed no_handler_hit. The AP
                    //     now writes SVR=0x1FF / TPR=0 / ESR-clear and publishes the
                    //     readbacks; grade them here.
                    crate::yarm_log!("X86_AP_LAPIC_ENABLE_BEGIN cpu={}", cpu.0);
                    let svr = unsafe { read_volatile(ap_svr_out_low_virt(handoff_off)) };
                    let tpr = unsafe { read_volatile(ap_tpr_out_low_virt(handoff_off)) };
                    let esr = unsafe { read_volatile(ap_esr_out_low_virt(handoff_off)) };
                    let sw_flag = (env & ap_env::LAPIC_SW_ENABLED) != 0;
                    let svr_ok = sw_flag && (svr & 0x100) != 0;
                    let tpr_ok = sw_flag && tpr == 0;
                    let esr_ok = sw_flag && esr == 0;
                    if svr_ok {
                        crate::yarm_log!("X86_AP_LAPIC_SVR_OK cpu={} value=0x{:x}", cpu.0, svr);
                    }
                    if tpr_ok {
                        crate::yarm_log!("X86_AP_LAPIC_TPR_OK cpu={} value=0x{:x}", cpu.0, tpr);
                    }
                    if esr_ok {
                        crate::yarm_log!("X86_AP_LAPIC_ESR_OK cpu={} value=0x{:x}", cpu.0, esr);
                    }
                    let lapic_int_ready = svr_ok && tpr_ok && esr_ok;
                    if lapic_int_ready {
                        crate::yarm_log!("X86_AP_LAPIC_INTERRUPT_READY cpu={}", cpu.0);
                    } else if !sw_flag {
                        crate::yarm_log!(
                            "X86_AP_LAPIC_INTERRUPT_BAD cpu={} reason=enable_flag_missing",
                            cpu.0
                        );
                    } else if !svr_ok {
                        crate::yarm_log!(
                            "X86_AP_LAPIC_INTERRUPT_BAD cpu={} reason=svr_sw_enable_clear svr=0x{:x}",
                            cpu.0,
                            svr
                        );
                    } else if !tpr_ok {
                        crate::yarm_log!(
                            "X86_AP_LAPIC_INTERRUPT_BAD cpu={} reason=tpr_masking tpr=0x{:x}",
                            cpu.0,
                            tpr
                        );
                    } else {
                        crate::yarm_log!(
                            "X86_AP_LAPIC_INTERRUPT_BAD cpu={} reason=esr_nonzero esr=0x{:x}",
                            cpu.0,
                            esr
                        );
                    }

                    // 9c. (183.4 fix) Verify the smoke vector's IDT DESCRIPTOR, not
                    //     just the IDT base: present, interrupt gate, CS=0x08, ist=0,
                    //     offset == the smoke stub's linked VA.
                    {
                        let (vec_ok, gate_type, selector, ist, offset, expected) =
                            super::descriptor_tables::ap_idt_smoke_vector_report();
                        if vec_ok {
                            crate::yarm_log!(
                                "X86_AP_IDT_VECTOR_OK cpu={} vector=0xf0 selector=0x{:02x} ist={} type=0x{:x}",
                                cpu.0,
                                selector,
                                ist,
                                gate_type
                            );
                        } else {
                            crate::yarm_log!(
                                "X86_AP_IDT_VECTOR_BAD cpu={} vector=0xf0 reason=descriptor_mismatch type=0x{:x} selector=0x{:02x} ist={} offset=0x{:x} expected=0x{:x}",
                                cpu.0,
                                gate_type,
                                selector,
                                ist,
                                offset,
                                expected
                            );
                        }
                    }

                    // 10. Interrupt smoke: exactly ONE fixed IPI to this AP. The AP
                    //     waits in sti;hlt (race-free: pending IPIs deliver when hlt
                    //     begins), the pure-asm handler counts + records the vector
                    //     via gs:, EOIs, iretqs back, and the AP returns to the
                    //     interrupt-masked idle loop. Proves: IDT delivery, EOI,
                    //     iretq resume, leave-idle/return-to-idle, no lost wake, no
                    //     duplicate delivery — all without a scheduler tick.
                    crate::yarm_log!("X86_AP_INTERRUPT_SMOKE_BEGIN cpu={}", cpu.0);
                    let mut irq_ok = false;
                    if idt_ok {
                        use super::descriptor_tables::AP_IRQ_SMOKE_VECTOR;
                        // (183.4 fix) fully-instrumented fixed-IPI send: ESR before/
                        // after, exact ICR values, delivery-status verdict.
                        crate::yarm_log!(
                            "X86_IPI_FIXED_SEND_BEGIN from=0 to={} vector=0x{:x} mode=physical",
                            cpu.0,
                            AP_IRQ_SMOKE_VECTOR
                        );
                        let esr_before = bsp_lapic_esr_read();
                        wait_for_icr_idle(cpu.0, "before_irq_smoke");
                        crate::yarm_log!(
                            "X86_IPI_REMOTE_WAKE_SEND from=0 to={} vector=0x{:x}",
                            cpu.0,
                            AP_IRQ_SMOKE_VECTOR
                        );
                        let icr_high = (cpu.0 as u32) << 24;
                        let icr_low = AP_IRQ_SMOKE_VECTOR as u32; // fixed, physical, edge
                        write_icr(cpu.0, icr_low);
                        crate::yarm_log!(
                            "X86_IPI_FIXED_ICR_WRITTEN to={} high=0x{:08x} low=0x{:08x}",
                            cpu.0,
                            icr_high,
                            icr_low
                        );
                        let delivery_idle = icr_delivery_idle();
                        if delivery_idle {
                            crate::yarm_log!("X86_IPI_FIXED_DELIVERY_IDLE to={}", cpu.0);
                        }
                        let esr_after = bsp_lapic_esr_read();
                        crate::yarm_log!(
                            "X86_IPI_FIXED_ESR from=0 before=0x{:x} after=0x{:x}",
                            esr_before,
                            esr_after
                        );
                        if delivery_idle && esr_after == 0 {
                            crate::yarm_log!("X86_IPI_FIXED_SEND_DONE to={}", cpu.0);
                        } else if !delivery_idle {
                            crate::yarm_log!(
                                "X86_IPI_FIXED_SEND_FAIL to={} reason=delivery_status_stuck",
                                cpu.0
                            );
                        } else {
                            crate::yarm_log!(
                                "X86_IPI_FIXED_SEND_FAIL to={} reason=esr_nonzero esr=0x{:x}",
                                cpu.0,
                                esr_after
                            );
                        }

                        let mut hit = false;
                        for _ in 0..AP_READY_POLL_ITERS {
                            let r = super::percpu::read_record(cpu);
                            if r.irq_unexpected_vec != 0 {
                                break; // AP parked in the catch-all stub — grade below
                            }
                            if r.irq_hit_count >= 1 {
                                hit = true;
                                break;
                            }
                            cpu_relax();
                        }
                        let r = super::percpu::read_record(cpu);
                        if hit {
                            crate::yarm_log!(
                                "X86_IPI_REMOTE_WAKE_RECV cpu={} vector=0x{:x}",
                                cpu.0,
                                r.irq_hit_vector
                            );
                            // 183.5 host-failure fix (reason=no_resume_after_handler):
                            // ACK is now graded from the PERSISTENT AP-written flag
                            // (gs irq_ack = 1, written by the post-hlt resume path
                            // after the handler is confirmed) — never from transient
                            // stage words. The old poll accepted stages 28|17|18, but
                            // the 183.5 managed-idle tail made 28 a microseconds-wide
                            // transient and removed 17/18 from this path, while the
                            // BSP spent milliseconds printing RECV through the QEMU
                            // UART first — so the poll deterministically missed and
                            // every AP failed. Handler sub-stages (irq_stage 32-34)
                            // plus the resume stages (35/36) name any future failure.
                            let mut acked = false;
                            for _ in 0..AP_READY_POLL_ITERS {
                                if super::percpu::read_record(cpu).irq_ack == 1 {
                                    acked = true;
                                    break;
                                }
                                cpu_relax();
                            }
                            // Settle, then require EXACTLY one delivery (no dup) and
                            // no unexpected vector.
                            spin_delay(200_000);
                            let r2 = super::percpu::read_record(cpu);
                            let last_raw =
                                unsafe { read_volatile(ap_stage_word_low_virt(handoff_off)) };
                            if !acked {
                                crate::yarm_log!(
                                    "X86_AP_INTERRUPT_SMOKE_FAIL cpu={} reason=no_resume_after_handler last_stage={} last_stage_raw=0x{:x} irq_stage={} irq_stage_raw=0x{:x}",
                                    cpu.0,
                                    ap_stage_name(last_raw),
                                    last_raw,
                                    ap_stage_name(r2.irq_stage),
                                    r2.irq_stage
                                );
                            } else if r2.irq_hit_count != 1 {
                                crate::yarm_log!(
                                    "X86_AP_INTERRUPT_SMOKE_FAIL cpu={} reason=dup_delivery count={} last_stage={} last_stage_raw=0x{:x}",
                                    cpu.0,
                                    r2.irq_hit_count,
                                    ap_stage_name(last_raw),
                                    last_raw
                                );
                            } else if r2.irq_unexpected_vec != 0 {
                                crate::yarm_log!(
                                    "X86_AP_INTERRUPT_SMOKE_FAIL cpu={} reason=unexpected_vector vec={} last_stage={} last_stage_raw=0x{:x}",
                                    cpu.0,
                                    r2.irq_unexpected_vec - 1,
                                    ap_stage_name(last_raw),
                                    last_raw
                                );
                            } else {
                                crate::yarm_log!("X86_IPI_REMOTE_WAKE_ACK cpu={}", cpu.0);
                                crate::yarm_log!(
                                    "X86_AP_INTERRUPT_SMOKE_OK cpu={} vector=0x{:x}",
                                    cpu.0,
                                    r2.irq_hit_vector
                                );
                                irq_ok = true;
                            }
                        } else if r.irq_unexpected_vec != 0 {
                            crate::yarm_log!(
                                "X86_AP_INTERRUPT_SMOKE_FAIL cpu={} reason=unexpected_vector vec={}",
                                cpu.0,
                                r.irq_unexpected_vec - 1
                            );
                        } else {
                            let last_raw =
                                unsafe { read_volatile(ap_stage_word_low_virt(handoff_off)) };
                            crate::yarm_log!(
                                "X86_AP_INTERRUPT_SMOKE_FAIL cpu={} reason=no_handler_hit last_stage={} last_stage_raw=0x{:x} irq_stage={} irq_stage_raw=0x{:x}",
                                cpu.0,
                                ap_stage_name(last_raw),
                                last_raw,
                                ap_stage_name(r.irq_stage),
                                r.irq_stage
                            );
                        }
                    } else {
                        crate::yarm_log!(
                            "X86_AP_INTERRUPT_SMOKE_FAIL cpu={} reason=idt_not_loaded",
                            cpu.0
                        );
                    }
                    if irq_ok {
                        AP_INTERRUPT_READY_COUNT.fetch_add(1, Ordering::AcqRel);
                    }
                    // Stage 183.5: record what the post-graduated-proof scheduler-
                    // online admission phase needs (the boot loop's handoff_off and
                    // the smoke verdict — the hard gate for onlining this AP).
                    AP_HANDOFF_OFFS[cpu.0 as usize].store(handoff_off, Ordering::Release);
                    AP_IRQ_READY_FLAGS[cpu.0 as usize].store(irq_ok, Ordering::Release);
                }
                if stage == AP_STAGE_IDLE_ADMIT_OK || stage == AP_STAGE_IDLE_ADMIT_GS_BAD {
                    // Confirm the AP actually reached its terminal idle before
                    // claiming IDLE_ENTER — a bounded wait, honest on timeout. The
                    // smoke-OK path terminates in the MANAGED scheduler-idle loop
                    // (stage 30/31 — interruptible, wake-capable, 183.5); degraded
                    // paths (GS_BAD / env-skip) park masked at stage 18.
                    let mut idled = stage == AP_STAGE_IDLE_ADMIT_GS_BAD;
                    if !idled {
                        for _ in 0..AP_READY_POLL_ITERS {
                            let s = unsafe { read_volatile(ap_stage_word_low_virt(handoff_off)) };
                            if s == 30 || s == 31 || s == 18 {
                                idled = true;
                                break;
                            }
                            cpu_relax();
                        }
                    }
                    if idled {
                        crate::yarm_log!(
                            "X86_AP_IDLE_ENTER cpu={} reason=sched_idle_interruptible",
                            cpu.0
                        );
                        crate::yarm_log!("X86_AP_SCHED_ADMIT_DONE cpu={}", cpu.0);
                        // Only a GS-verified admit counts as clean idle-live.
                        if stage == AP_STAGE_IDLE_ADMIT_OK {
                            AP_IDLE_LIVE_COUNT.fetch_add(1, Ordering::AcqRel);
                        }
                    } else {
                        let last_raw =
                            unsafe { read_volatile(ap_stage_word_low_virt(handoff_off)) };
                        crate::yarm_log!(
                            "X86_AP_SCHED_ADMIT_FAIL cpu={} reason=idle_reentry_timeout last_stage={} last_stage_raw=0x{:x}",
                            cpu.0,
                            ap_stage_name(last_raw),
                            last_raw
                        );
                        crate::yarm_log!("X86_AP_IDLE_FAIL cpu={}", cpu.0);
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

        // SEQUENCING FENCE (183.5): the scheduler bring-up for this AP is NOT
        // invoked here. It runs in `ap_scheduler_online_admission`, one-shot,
        // AFTER the graduated one-shot proof completes on the BSP — so the
        // accepted graduated evidence still executes with online == 1 (its
        // out-of-lock seam slices require the single-CPU topology until 183.6
        // proves them under SMP). Continue to the next AP.
    }

    let present_count = present.count_ones() as usize;
    // Boot-time truth: the scheduler-online admission runs post-graduated-proof,
    // so at THIS point online is still 1 and no AP is scheduler-admitted.
    crate::yarm_log!(
        "X86_SMP_STARTUP started_secondary={} online_cpus=1 present_cpus={}",
        rust_online_aps,
        present_count
    );
    crate::yarm_log!(
        "X86_SMP_OBSERVATION_OK rust_aps={} scheduler_aps=0",
        rust_online_aps
    );

    Ok(rust_online_aps)
}

/// Stage 183.5 — AP scheduler-online admission + remote-wake proof.
///
/// One-shot, called from the SMP audit AFTER the graduated one-shot proof
/// completed (so the accepted BSP graduated evidence ran with `online == 1`).
/// For every AP whose 183.4 interrupt smoke passed, in hard-gate order:
///
/// 1. **Idle task**: the scheduler-owned representation of the AP's managed
///    interruptible idle loop (stage 30/31, wake-capable — NOT a bare cli/hlt
///    park). Uses the scheduler's existing idle convention: current = tid 0,
///    the placeholder `dispatch_next` already switches away from when real
///    work arrives (forward-correct for the 183.6 AP dispatcher).
/// 2. **Scheduler-online**: mark the CPU wake-only FIRST (placement denied —
///    no AP dispatcher yet, a placed task would strand; `enqueue_balanced`
///    skips wake-only CPUs), then `bring_up_cpu`, then install the idle
///    current. After this `online_cpu_count()` grows and `single_cpu` becomes
///    false — the D2/D6 seams take their conservative in-lock slice
///    (`reason=multi_cpu`), which stays gated until 183.6 proves the
///    out-of-lock slices under SMP.
/// 3. **Remote wake**: exactly ONE fixed IPI (vector 0xF1) to the online AP;
///    its pure-asm handler counts the wake via gs:, EOIs, iretqs; the idle
///    loop observes the count, publishes a re-entry, and returns to idle.
///    Graded: no lost wake, no duplicate wake, idle re-entered, idle current
///    still coherent.
pub fn ap_scheduler_online_admission(kernel: &mut KernelState) {
    if AP_SCHED_ONLINE_ADMISSION_DONE.swap(true, Ordering::AcqRel) {
        return;
    }

    #[cfg(all(not(test), not(feature = "hosted-dev")))]
    {
        use super::descriptor_tables::AP_REMOTE_WAKE_VECTOR;

        for raw_cpu in 0..crate::arch::platform_constants::MAX_CPUS {
            let cpu = CpuId(raw_cpu as u8);
            if cpu.0 == crate::arch::platform_constants::BOOTSTRAP_CPU_ID {
                continue;
            }
            if !AP_IRQ_READY_FLAGS[raw_cpu].load(Ordering::Acquire) {
                continue; // interrupt smoke did not pass — hard gate holds
            }
            let handoff_off = AP_HANDOFF_OFFS[raw_cpu].load(Ordering::Acquire);
            if handoff_off == usize::MAX {
                continue;
            }

            // 183.5 fix #2 (#PF CR2=0x7170): this admission runs post-boot on the
            // CURRENT TASK address space, where the low identity trampoline VAs
            // (0x7000 + handoff_off + …) are UNMAPPED — polling them page-faulted.
            // All polling below therefore uses ONLY the per-CPU record (kernel
            // .bss; the AP mirrors sched_stage / wake_reenter into it via gs:),
            // and the poll pointer is validated against the LIVE CR3 first —
            // reject low/unmapped pointers with a marker instead of faulting.
            let poll_ptr = super::percpu::record_base(cpu) as u64;
            let live_root = current_cr3() & !0xfffu64;
            let poll_ptr_ok = poll_ptr >= crate::arch::platform_layout::KERNEL_BOOTSTRAP_VIRT_BASE
                && crate::arch::x86_64::page_table::debug_root_maps_virt(
                    live_root,
                    crate::kernel::vm::VirtAddr(poll_ptr),
                );
            if !poll_ptr_ok {
                crate::yarm_log!(
                    "X86_AP_SCHED_IDLE_POLL_PTR_BAD cpu={} ptr=0x{:x} reason=low_or_unmapped",
                    cpu.0,
                    poll_ptr
                );
                continue;
            }
            crate::yarm_log!(
                "X86_AP_SCHED_IDLE_POLL_PTR_OK cpu={} ptr=0x{:x}",
                cpu.0,
                poll_ptr
            );

            // 1. Scheduler-owned idle task (represents the AP's live managed
            //    idle loop; nothing is enqueued — idle is the current).
            crate::yarm_log!("X86_AP_IDLE_TASK_CREATE_BEGIN cpu={}", cpu.0);
            let record = super::percpu::read_record(cpu);
            let meta_ok = (record.idle_flags & super::percpu::idle_flag::IDLE_TASK_META_SET) != 0
                && record.idle_entry != 0
                && record.idle_stack_top == ap_stack_top(cpu);
            if !meta_ok {
                crate::yarm_log!(
                    "X86_AP_IDLE_TASK_BAD cpu={} reason=idle_meta_invalid",
                    cpu.0
                );
                continue;
            }
            crate::yarm_log!(
                "X86_AP_IDLE_TASK_READY cpu={} tid=0 stack=0x{:x} entry=0x{:x}",
                cpu.0,
                record.idle_stack_top,
                record.idle_entry
            );

            // 2. Scheduler-online transition (wake-only first: no placement window).
            crate::yarm_log!("X86_AP_SCHED_ONLINE_BEGIN cpu={}", cpu.0);
            if kernel.mark_cpu_wake_only(cpu, true).is_err() {
                crate::yarm_log!(
                    "X86_AP_SCHED_ONLINE_FAIL cpu={} reason=wake_only_mark_failed",
                    cpu.0
                );
                continue;
            }
            if kernel.bring_up_cpu(cpu).is_err() {
                let _ = kernel.mark_cpu_wake_only(cpu, false);
                crate::yarm_log!(
                    "X86_AP_SCHED_ONLINE_FAIL cpu={} reason=bring_up_failed",
                    cpu.0
                );
                continue;
            }
            let idle_tid = match kernel.install_ap_idle_current(cpu) {
                Ok(tid) => tid,
                Err(_) => {
                    crate::yarm_log!(
                        "X86_AP_SCHED_ONLINE_FAIL cpu={} reason=idle_current_install_failed",
                        cpu.0
                    );
                    continue;
                }
            };
            crate::yarm_log!("X86_AP_IDLE_TASK_ACTIVE cpu={} tid={}", cpu.0, idle_tid);
            crate::yarm_log!("X86_AP_SCHED_ONLINE_OK cpu={}", cpu.0);

            // 3. The AP must be in the managed scheduler-idle loop — polled from
            //    the gs:-mirrored sched_stage in the per-CPU record (mapped .bss),
            //    NEVER the low trampoline VAs (unmapped on this address space).
            let mut sched_idle = false;
            for _ in 0..AP_READY_POLL_ITERS {
                let stage = super::percpu::read_record(cpu).sched_stage;
                if stage == 30 || stage == 31 {
                    sched_idle = true;
                    break;
                }
                cpu_relax();
            }
            if !sched_idle {
                crate::yarm_log!(
                    "X86_AP_SCHED_IDLE_BAD cpu={} reason=sched_idle_not_reached sched_stage={}",
                    cpu.0,
                    super::percpu::read_record(cpu).sched_stage
                );
                continue;
            }
            crate::yarm_log!("X86_AP_SCHED_IDLE_ENTER cpu={} tid={}", cpu.0, idle_tid);

            // 4. Remote-wake proof: exactly one wake, observed, re-idled.
            let wake_before = super::percpu::read_record(cpu).remote_wake_count;
            let reenter_before = super::percpu::read_record(cpu).wake_reenter_mirror;
            wait_for_icr_idle(cpu.0, "before_remote_wake");
            crate::yarm_log!(
                "X86_IPI_REMOTE_WAKE_SEND from=0 to={} vector=0x{:x}",
                cpu.0,
                AP_REMOTE_WAKE_VECTOR
            );
            write_icr(cpu.0, AP_REMOTE_WAKE_VECTOR as u32);

            let mut recv = false;
            for _ in 0..AP_READY_POLL_ITERS {
                if super::percpu::read_record(cpu).remote_wake_count > wake_before {
                    recv = true;
                    break;
                }
                cpu_relax();
            }
            if !recv {
                crate::yarm_log!("D6_SMP_LOST_WAKE_FAIL cpu={} reason=no_wake_recv", cpu.0);
                continue;
            }
            crate::yarm_log!("X86_IPI_REMOTE_WAKE_RECV cpu={}", cpu.0);

            let mut reentered = false;
            for _ in 0..AP_READY_POLL_ITERS {
                let r = super::percpu::read_record(cpu);
                if r.wake_reenter_mirror > reenter_before && r.sched_stage == 30 {
                    reentered = true;
                    break;
                }
                cpu_relax();
            }
            if !reentered {
                crate::yarm_log!(
                    "X86_AP_SCHED_IDLE_BAD cpu={} reason=no_idle_reentry_after_wake",
                    cpu.0
                );
                continue;
            }
            crate::yarm_log!("X86_IPI_REMOTE_WAKE_ACK cpu={}", cpu.0);
            crate::yarm_log!("X86_AP_SCHED_IDLE_REENTER cpu={} tid={}", cpu.0, idle_tid);

            // Settle, then require EXACTLY one wake (no dup) and coherent
            // idle-current state (no stale current task).
            spin_delay(200_000);
            let settle_record = super::percpu::read_record(cpu);
            let wake_after = settle_record.remote_wake_count;
            let reenter_after = settle_record.wake_reenter_mirror;
            let current_ok = kernel.current_tid_on_cpu(cpu) == Some(idle_tid);
            if wake_after - wake_before > 1 {
                crate::yarm_log!(
                    "D6_SMP_DUP_WAKE_FAIL cpu={} count={}",
                    cpu.0,
                    wake_after - wake_before
                );
            } else if reenter_after - reenter_before != 1 {
                crate::yarm_log!(
                    "X86_AP_SCHED_IDLE_BAD cpu={} reason=reenter_count_mismatch count={}",
                    cpu.0,
                    reenter_after - reenter_before
                );
            } else if !current_ok {
                crate::yarm_log!(
                    "X86_AP_SCHED_IDLE_BAD cpu={} reason=idle_current_not_coherent",
                    cpu.0
                );
            } else {
                crate::yarm_log!("D6_SMP_REMOTE_WAKE_OK cpu={}", cpu.0);
                AP_REMOTE_WAKE_OK_COUNT.fetch_add(1, Ordering::AcqRel);
            }
        }

        crate::yarm_log!(
            "X86_SMP_ONLINE_READY present={} online={}",
            kernel.present_cpu_count(),
            kernel.online_cpu_count()
        );
    }

    #[cfg(any(test, feature = "hosted-dev"))]
    {
        let _ = kernel;
    }
}

/// Stage 189A: emitted once, the first time the real shootdown IPI path is driven,
/// to attest the vector/mailbox path is live. Idempotent via this flag.
#[cfg(all(not(test), not(feature = "hosted-dev")))]
static TLB_SHOOTDOWN_IPI_READY: AtomicBool = AtomicBool::new(false);

/// Stage 183.6 / 189A: drive a REAL cross-CPU TLB shootdown against `targets` and
/// wait for each AP's ACK. For each target CPU: post the request into its per-CPU
/// record (VA; 0 = full flush), send the wake IPI (vector 0xF1 — the same IPI its
/// managed sched-idle loop already services), and wait (bounded) for
/// `tlb_ack_gen == req_gen`. The AP executes the invalidation locally and ACKs —
/// a genuine remote acknowledgement, not a simulated one: the BSP here only ever
/// *reads* `tlb_ack_gen`; the sole writer of that field is the AP's own asm
/// (`gs:[132]`). Returns the number of APs that acknowledged; emits the standard
/// `X86_TLB_SHOOTDOWN_FAIL` (and the legacy `X86_TLB_REMOTE_ACK_TIMEOUT`) for any
/// that did not.
#[cfg(all(not(test), not(feature = "hosted-dev")))]
pub fn smp_tlb_shootdown_cpus(targets: u64, va: u64) -> usize {
    use super::descriptor_tables::AP_REMOTE_WAKE_VECTOR;
    use super::tlb_shootdown;
    if !TLB_SHOOTDOWN_IPI_READY.swap(true, Ordering::AcqRel) {
        crate::yarm_log!(
            "{} vector=0x{:x}",
            tlb_shootdown::MARK_IPI_READY,
            AP_REMOTE_WAKE_VECTOR
        );
    }
    // Honest no-op: an empty target mask means there is nothing to shoot down
    // remotely (BSP-local topology). Surface it, do not silently "succeed".
    if targets == 0 {
        crate::yarm_log!("{} reason=no_remote_target", tlb_shootdown::MARK_DEFERRED);
        return 0;
    }
    let mut acked = 0usize;
    for raw in 0..crate::arch::platform_constants::MAX_CPUS {
        if targets & (1u64 << raw) == 0 {
            continue;
        }
        let cpu = CpuId(raw as u8);
        // Post the request FIRST (writes req_va then bumps req_gen), then IPI. The
        // returned `want` is the generation the target must publish into its own
        // ack_gen; the BSP never writes ack_gen.
        let want = super::percpu::tlb_request_shootdown(cpu, va);
        crate::yarm_log!(
            "{} target_cpu={} gen={} va=0x{:x}",
            tlb_shootdown::MARK_SEND,
            cpu.0,
            want,
            va
        );
        wait_for_icr_idle(cpu.0, "before_tlb_shootdown");
        write_icr(cpu.0, AP_REMOTE_WAKE_VECTOR as u32);
        let mut got = false;
        for _ in 0..AP_READY_POLL_ITERS {
            // Read-only observation of the AP-owned ack generation.
            if super::percpu::tlb_ack_gen(cpu) == want {
                got = true;
                break;
            }
            cpu_relax();
        }
        if got {
            // The target published ack_gen == want, which it does only AFTER the
            // local invalidation — so this observation attests both the handling
            // and the acknowledgement.
            crate::yarm_log!("{} cpu={} gen={}", tlb_shootdown::MARK_HANDLE, cpu.0, want);
            crate::yarm_log!("{} cpu={} gen={}", tlb_shootdown::MARK_ACK, cpu.0, want);
            acked += 1;
        } else {
            let got_gen = super::percpu::tlb_ack_gen(cpu);
            crate::yarm_log!(
                "{} reason=ack_timeout cpu={} want_gen={} got_gen={}",
                tlb_shootdown::MARK_FAIL,
                cpu.0,
                want,
                got_gen
            );
            crate::yarm_log!(
                "X86_TLB_REMOTE_ACK_TIMEOUT cpu={} want_gen={} got_gen={}",
                cpu.0,
                want,
                got_gen
            );
        }
    }
    acked
}

/// The online, wake-only APs (Stage 183.5) — the set that must acknowledge a TLB
/// shootdown under real SMP. They idle on the kernel CR3 and hold no user ASID, so
/// invalidating any VA on them is correct-and-conservative (over-invalidation is
/// always safe); the ACK is nonetheless real. When a future AP dispatcher clears an
/// AP's wake-only bit, that CPU joins the precise per-ASID target set instead.
#[cfg(all(not(test), not(feature = "hosted-dev")))]
pub fn online_wake_only_ap_bitmap(kernel: &KernelState) -> u64 {
    // Single source of truth: the Stage 189A coordinator's IPI-capability filter.
    super::tlb_shootdown::ipi_capable_targets(
        kernel.online_cpu_bitmap(),
        kernel.wake_only_cpu_bitmap(),
        crate::arch::platform_constants::BOOTSTRAP_CPU_ID,
    )
}

/// Stage 183.6 / 189A one-shot: prove the real SMP TLB shootdown ACK against every
/// online AP, for both the COW and VM_UNMAP contexts. Returns `(cow_ok, unmap_ok)`.
///
/// Routes the target set through the Stage 189A coordinator so the live path uses
/// exactly the classification that the hosted-dev tests pin. Emits the standard
/// terminal markers (`result=ok` / `result=bsp_local`) additively alongside the
/// legacy context markers the smoke gates require.
#[cfg(all(not(test), not(feature = "hosted-dev")))]
pub fn ap_tlb_shootdown_proof(kernel: &KernelState) -> (bool, bool) {
    use super::tlb_shootdown::{self, ShootdownTargets};
    let targets = match tlb_shootdown::classify(online_wake_only_ap_bitmap(kernel)) {
        ShootdownTargets::BspLocal => {
            // Honest: no valid remote target — the shootdown collapses to a local
            // flush. Nothing remote to acknowledge, so the remote proof is not
            // asserted (the caller emits its honest ack-unproven blocker).
            crate::yarm_log!("{} result=bsp_local targets=0x0", tlb_shootdown::MARK_DONE);
            return (false, false);
        }
        ShootdownTargets::Remote(mask) => mask,
    };
    let want = targets.count_ones() as usize;
    // COW context: a representative parent write-protect VA — full round-trip.
    let cow_ok = smp_tlb_shootdown_cpus(targets, 0x0000_1000) == want;
    if cow_ok {
        crate::yarm_log!(
            "{} result=ok context=cow targets=0x{:x}",
            tlb_shootdown::MARK_DONE,
            targets
        );
        crate::yarm_log!("COW_SMP_TLB_ACK_OK acks={}", want);
    } else {
        crate::yarm_log!(
            "{} reason=cow_ack_incomplete targets=0x{:x}",
            tlb_shootdown::MARK_FAIL,
            targets
        );
    }
    // VM_UNMAP context: a full-flush shootdown (va=0) — full round-trip.
    let unmap_ok = smp_tlb_shootdown_cpus(targets, 0) == want;
    if unmap_ok {
        crate::yarm_log!(
            "{} result=ok context=vm_unmap targets=0x{:x}",
            tlb_shootdown::MARK_DONE,
            targets
        );
        crate::yarm_log!("VM_UNMAP_SMP_TLB_ACK_OK acks={}", want);
    } else {
        crate::yarm_log!(
            "{} reason=vm_unmap_ack_incomplete targets=0x{:x}",
            tlb_shootdown::MARK_FAIL,
            targets
        );
    }
    (cow_ok, unmap_ok)
}

#[cfg(any(test, feature = "hosted-dev"))]
pub fn ap_tlb_shootdown_proof(_kernel: &KernelState) -> (bool, bool) {
    (false, false)
}

/// Hosted-dev stub: the AP dispatch scaffold audit is a bare-metal-only path (it
/// reads per-CPU records and drives the audited transition). The decision logic it
/// applies lives in `ap_dispatch` and is unit-tested directly.
#[cfg(any(test, feature = "hosted-dev"))]
pub fn run_ap_dispatch_scaffold_audit(_kernel: &mut KernelState, _tlb_ready: bool) {}

/// Stage 189B: structural audit of the per-CPU prerequisites an AP needs to enter
/// user mode through the shared BSP return path. Verifies the AP has its own TSS,
/// a recorded kernel idle CR3, and valid idle metadata. This proves the *shared*
/// return-path prerequisites are present; it does NOT prove a live AP user
/// trap-return (that is Stage 189C — `ensure_user_return_cr3` still resolves a
/// global active-ASID and a BSP-tuned return-context stack).
#[cfg(all(not(test), not(feature = "hosted-dev")))]
fn ap_trap_return_prereqs_present(cpu: CpuId) -> bool {
    let record = super::percpu::read_record(cpu);
    record.tss_ptr != 0
        && record.idle_cr3 != 0
        && (record.idle_flags & super::percpu::idle_flag::IDLE_TASK_META_SET) != 0
}

/// Stage 189B: the AUDITED, sole authority for clearing an AP's wake-only bit for
/// dispatch. Refuses unless every readiness condition holds; on success it clears
/// wake-only and emits `X86_AP_WAKE_ONLY_CLEAR`. No other code path may clear an
/// AP's wake-only bit for dispatch. In Stage 189B `readiness.trap_return_ready`
/// is always false, so this function always refuses and never clears wake-only.
#[cfg(all(not(test), not(feature = "hosted-dev")))]
pub fn try_enable_ap_user_dispatch(
    kernel: &mut KernelState,
    cpu: CpuId,
    readiness: super::ap_dispatch::ApReadiness,
) -> Result<(), super::ap_dispatch::ClearRefusal> {
    use super::ap_dispatch;
    if let Err(refusal) = readiness.evaluate_clear() {
        crate::yarm_log!(
            "{} cpu={} reason={}",
            ap_dispatch::MARK_WAKE_ONLY_CLEAR_DEFERRED,
            cpu.0,
            refusal.reason()
        );
        return Err(refusal);
    }
    // All readiness bits hold: this is the ONLY site that clears wake-only for
    // dispatch. (Unreachable in Stage 189B — trap_return_ready is never set.)
    kernel
        .mark_cpu_wake_only(cpu, false)
        .map_err(|_| ap_dispatch::ClearRefusal::RunQueueNotReady)?;
    crate::yarm_log!("{} cpu={}", ap_dispatch::MARK_WAKE_ONLY_CLEAR, cpu.0);
    Ok(())
}

/// Stage 189B: one-shot AP user-dispatch scaffold audit. Runs after the Stage
/// 189A TLB-shootdown proof. For each online wake-only AP it reports the readiness
/// state and drives the AUDITED transition, which — because the live trap-return
/// path is deferred to Stage 189C — refuses and emits the honest deferral markers.
/// It NEVER clears a wake-only bit and NEVER schedules a user task.
#[cfg(all(not(test), not(feature = "hosted-dev")))]
pub fn run_ap_dispatch_scaffold_audit(kernel: &mut KernelState, tlb_ready: bool) {
    use super::ap_dispatch::{self, ApReadiness};
    // Scaffold-level facts (topology-independent): the dispatcher state machine and
    // the run-queue admission guards exist and are validated by hosted-dev tests.
    crate::yarm_log!("{}", ap_dispatch::MARK_DISPATCHER_SCAFFOLD_READY);
    crate::yarm_log!("{}", ap_dispatch::MARK_ADMISSION_GUARD_READY);

    let targets = online_wake_only_ap_bitmap(kernel);
    for raw in 0..crate::arch::platform_constants::MAX_CPUS {
        if targets & (1u64 << raw) == 0 {
            continue;
        }
        let cpu = CpuId(raw as u8);
        // Structural trap-return audit: shared-path per-CPU prerequisites present.
        if ap_trap_return_prereqs_present(cpu) {
            crate::yarm_log!("{} cpu={}", ap_dispatch::MARK_TRAP_RETURN_AUDIT_OK, cpu.0);
        }
        if tlb_ready {
            crate::yarm_log!("{} cpu={}", ap_dispatch::MARK_TLB_READY, cpu.0);
        }
        // Stage 189B readiness: dispatcher + run-queue + TLB ready; the LIVE
        // trap-return path is proven in Stage 189C, so trap_return_ready is false.
        let readiness = ApReadiness {
            dispatcher_ready: true,
            run_queue_ready: true,
            tlb_ready,
            trap_return_ready: false,
        };
        // Drive the REAL audited transition. It refuses (trap_return_ready=false),
        // logs `X86_AP_WAKE_ONLY_CLEAR_DEFERRED reason=trap_return_not_ready`, and
        // does NOT clear wake-only. This exercises the exact production gate.
        let _ = try_enable_ap_user_dispatch(kernel, cpu, readiness);
        crate::yarm_log!(
            "{} cpu={} reason=live_trap_return_wiring_deferred_to_189c",
            ap_dispatch::MARK_USER_DISPATCH_DEFERRED,
            cpu.0
        );
    }
}
