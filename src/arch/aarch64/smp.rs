// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! QEMU-SMP2 / AP-SCHED3 — AArch64 secondary CPUs on the existing GICv2.
//!
//! AP-SCHED3 §B: PRODUCTION, with no knob. Every secondary the admission policy planned
//! (`arch::aarch64_cpu_admission`) and PSCI started brings up its translation regime, its own GIC
//! CPU interface and reschedule SGI, its own timer, proves that readiness from its own registers,
//! and only then takes the scheduler's one production admission (`admit_dispatch`: out of
//! wake-only, eligible for balanced placement). It then idles in the SAME authenticated park the
//! boot CPU uses (`trap::enter_ap_idle`). Any failed prerequisite leaves it explicitly
//! non-dispatching: wake-only (nothing is placed on it), withdrawn as an SGI target, its interrupts
//! disabled, and named. `yarm.ap_user_dispatch` no longer has any authority over admission; it only
//! still arms the SMP2 / overtaken witnesses' provisioning.
//!
//! (QEMU-SMP2 had admitted CPU 1 only under that knob, to PINNED dispatch, with no timer.)
//!
//! # Owners
//!
//! * **Send** — [`send_reschedule_sgi`], called only by the owners that commit a REMOTE enqueue
//!   (the NR6/NR7 direct drains) after their commit, with no domain lock held.
//! * **Claim / complete** — unchanged: the vector entry's `irq::claim_interrupt` takes the whole
//!   `GICC_IAR` token and the vector tail's `irq::complete_interrupt` writes it back, once.
//!   [`note_sgi_arrival`] only counts what the claim returned.
//! * **Dispatch** — unchanged: the shared bridge's idle queue advance, the one the timer drives at
//!   the authenticated idle boundary, is driven by a reschedule SGI taken there too.
//!
//! The pure decisions — identity lookup, `GICD_SGIR` encoding, the token's source field — are in
//! `arch::gicv2_sgi`, where the hosted suite runs them.

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use crate::arch::gicv2_sgi::{
    RESCHEDULE_SGI_INTID, RESCHEDULE_SGI_PRIORITY, SgiRefusal, TargetTable, sgi_source_interface,
};
use crate::arch::platform_constants::MAX_CPUS;
use crate::kernel::scheduler::CpuId;

// GICv2 distributor / CPU-interface registers used here (IHI 0048B). Every register below that
// covers INTIDs 0..=31 is BANKED per CPU interface: each CPU programs and reads its own copy.
const GICD_CTLR: usize = 0x000;
const GICD_ISENABLER0: usize = 0x100;
const GICD_IPRIORITYR0: usize = 0x400;
const GICD_ITARGETSR0: usize = 0x800;
const GICD_SGIR: usize = 0xF00;
const GICC_CTLR: usize = 0x000;
const GICC_PMR: usize = 0x004;

/// Each CPU's own GIC CPU-interface bit, published by that CPU.
static TARGETS: TargetTable<MAX_CPUS> = TargetTable::new();
/// Set on the BSP immediately before its first return to EL0, right after it publishes the boot
/// state: the end of the boot window in which it holds `&mut KernelState` through the
/// `BootExclusive` token (BLR1; formerly `borrow_kernel_for_boot`). No AP touches shared kernel
/// state before it.
static BSP_BOOT_BORROW_ENDED: AtomicBool = AtomicBool::new(false);
/// CPUs admitted to production dispatch.
static ADMITTED: AtomicU64 = AtomicU64::new(0);
/// AP-SCHED3 §B — the secondaries the boot CPU started through PSCI (the set every one of which
/// must complete its admission).
static STARTED: AtomicU64 = AtomicU64::new(0);

/// Record that the boot CPU started logical CPU `cpu`.
pub fn note_started(cpu: CpuId) {
    STARTED.fetch_or(1u64 << (cpu.0 as u64 & 63), Ordering::AcqRel);
}

/// The secondaries the boot CPU started.
pub fn started_bitmap() -> u64 {
    STARTED.load(Ordering::Acquire)
}

/// AP-SCHED3 §C — per CPU: its own timer is parked behind the idle backstop because it is idle.
static AP_TIMER_PARKED: [AtomicBool; MAX_CPUS] = [const { AtomicBool::new(false) }; MAX_CPUS];

/// AP-SCHED3 §C — an idle secondary's timer fires at this many quantum periods (one second at the
/// 10 ms quantum): a backstop that bounds how long an idle CPU can miss work whose notification
/// failed, not a tick. An idle CPU has no quantum to consume and no clock to advance (the global
/// tick is the clock owner's), and every enqueue committed onto it is followed by an SGI.
pub const AP_IDLE_BACKSTOP_PERIODS: u64 = 100;

/// Per-CPU hardware arrivals of the reschedule SGI, as the claim returned them:
/// `[total, from_el0, at_idle_boundary, in_other_kernel_code]`.
static ARRIVALS: [[AtomicU32; 4]; MAX_CPUS] =
    [const { [const { AtomicU32::new(0) }; 4] }; MAX_CPUS];
/// Per-CPU reschedule SGIs this CPU wrote to `GICD_SGIR`.
static SENT: [AtomicU32; MAX_CPUS] = [const { AtomicU32::new(0) }; MAX_CPUS];

fn read32(base: usize, offset: usize) -> u32 {
    // SAFETY: `base` is the DTB-derived GIC page every root maps as privileged Device-nGnRE.
    unsafe { core::ptr::read_volatile((base + offset) as *const u32) }
}

fn write32(base: usize, offset: usize, value: u32) {
    // SAFETY: as `read32`.
    unsafe { core::ptr::write_volatile((base + offset) as *mut u32, value) }
}

fn gic_bases() -> Option<(usize, usize)> {
    let (dist, cpu_if) = crate::arch::aarch64::page_table::gic_mmio_bases();
    (dist != 0 && cpu_if != 0).then_some((dist as usize, cpu_if as usize))
}

/// `true` when the default-off `yarm.ap_user_dispatch` knob is set. AP-SCHED3 §B: it arms the
/// SMP2 / overtaken witnesses' provisioning and nothing else; admission never asks it.
pub fn requested() -> bool {
    crate::kernel::boot::ap_user_dispatch_enabled()
}

/// Bring up THIS CPU's banked GIC state for the reschedule SGI and publish its interface bit.
///
/// Every write is read back; the bit is published LAST, and only if every readback agrees, so no
/// CPU is ever named as a target before it can take the interrupt. Programs nothing that is not
/// banked to this CPU except the distributor enable, which it only reads.
pub fn bring_up_sgi_on_this_cpu(cpu: CpuId) -> Result<u8, &'static str> {
    let (dist, cpu_if) = gic_bases().ok_or("no_gic_bases")?;
    // CPU interface: the same unmasked priority and group-0 enable the BSP's PPI bring-up writes.
    write32(cpu_if, GICC_PMR, 0xff);
    write32(cpu_if, GICC_CTLR, read32(cpu_if, GICC_CTLR) | 0x1);
    // The SGI's priority byte, in the banked word covering INTIDs 0..=3.
    let lane = u32::from(RESCHEDULE_SGI_INTID & 0x3) * 8;
    let word = read32(dist, GICD_IPRIORITYR0);
    write32(
        dist,
        GICD_IPRIORITYR0,
        (word & !(0xff << lane)) | (u32::from(RESCHEDULE_SGI_PRIORITY) << lane),
    );
    // Enable it (write-1-to-set; the zero bits leave every other INTID alone).
    write32(dist, GICD_ISENABLER0, 1 << RESCHEDULE_SGI_INTID);
    let pmr = read32(cpu_if, GICC_PMR);
    let gicc = read32(cpu_if, GICC_CTLR);
    let gicd = read32(dist, GICD_CTLR);
    let enabled = read32(dist, GICD_ISENABLER0);
    let prio = (read32(dist, GICD_IPRIORITYR0) >> lane) & 0xff;
    if pmr == 0 || gicc & 1 == 0 || gicd & 1 == 0 {
        return Err("interface_or_distributor_disabled");
    }
    if enabled & (1 << RESCHEDULE_SGI_INTID) == 0 {
        return Err("sgi_not_enabled");
    }
    // A GICv2 need not implement every priority bit; the implemented high bits must match.
    if prio & 0xf0 != u32::from(RESCHEDULE_SGI_PRIORITY) & 0xf0 {
        return Err("sgi_priority_readback");
    }
    let itargetsr0 = read32(dist, GICD_ITARGETSR0);
    let mask = TARGETS
        .publish(cpu.0 as usize, itargetsr0)
        .ok_or("itargetsr0_names_no_single_interface")?;
    crate::kernel::printk::printk_emit_sync(format_args!(
        "AARCH64_SMP2_SGI_READY cpu={} interface_mask=0x{:x} itargetsr0=0x{:08x} intid={} priority=0x{:x} pmr=0x{:x} result=ok",
        cpu.0, mask, itargetsr0, RESCHEDULE_SGI_INTID, prio, pmr
    ));
    Ok(mask)
}

/// Send the reschedule SGI from `sender` to `target`: the remote half of a committed enqueue.
///
/// Refused — writing nothing — when the target never published its interface bit (it cannot
/// take the interrupt, and a guessed target list could reach another CPU), when it is the
/// sender, or when the controller is unknown.
pub fn send_reschedule_sgi(sender: CpuId, target: CpuId) -> Result<u32, SgiRefusal> {
    let sgir = TARGETS.sgir_for(sender.0 as usize, target.0 as usize, RESCHEDULE_SGI_INTID)?;
    let (dist, _) = gic_bases().ok_or(SgiRefusal::ControllerUnconfigured)?;
    // The wake announces an enqueue committed in Normal memory; `GICD_SGIR` is Device-nGnRE. The
    // barrier completes that publication, for every observer in the inner-shareable domain,
    // before the controller write can raise the interrupt the target will dispatch on.
    // SAFETY: a data synchronization barrier.
    unsafe { core::arch::asm!("dsb ishst", options(nostack, preserves_flags)) };
    // QEMU-SMP2 §5: recorded BEFORE the controller write, so the target's arrival record can
    // never precede it — the one causal edge the grader holds this send to.
    #[cfg(feature = "aarch64-smp2-witness")]
    crate::kernel::boot::smp2_record::push(
        crate::kernel::boot::smp2_record::Kind::SgiSent,
        sender.0,
        [u64::from(target.0), u64::from(sgir), 0, 0, 0],
    );
    write32(dist, GICD_SGIR, sgir);
    if let Some(n) = SENT.get(sender.0 as usize) {
        n.fetch_add(1, Ordering::AcqRel);
    }
    Ok(sgir)
}

/// Where a claimed reschedule SGI was taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArrivalOrigin {
    /// From EL0 (`irq_lower_a64`).
    User,
    /// At this CPU's authenticated idle boundary (parked when the claim was taken).
    IdleBoundary,
    /// Any other EL1 point.
    Kernel,
}

/// Count one claimed reschedule SGI. `raw_token` is the whole `GICC_IAR` value.
pub fn note_sgi_arrival(cpu: CpuId, raw_token: u32, origin: ArrivalOrigin, elr: u64) {
    let Some(row) = ARRIVALS.get(cpu.0 as usize) else {
        return;
    };
    row[0].fetch_add(1, Ordering::AcqRel);
    let lane = match origin {
        ArrivalOrigin::User => 1,
        ArrivalOrigin::IdleBoundary => 2,
        ArrivalOrigin::Kernel => 3,
    };
    row[lane].fetch_add(1, Ordering::AcqRel);
    // AP-SCHED3 witness: a reschedule SGI taken at this CPU's authenticated idle boundary.
    #[cfg(feature = "aarch64-apsched-witness")]
    if origin == ArrivalOrigin::IdleBoundary {
        crate::kernel::apsched_witness::note_idle_ipi(cpu, 1);
    }
    let source_cpu = TARGETS
        .cpu_of_interface(sgi_source_interface(raw_token))
        .map_or(u64::MAX, |c| c as u64);
    #[cfg(feature = "aarch64-smp2-witness")]
    crate::arch::aarch64::smp2_witness::note_arrival(
        cpu,
        raw_token,
        source_cpu,
        match origin {
            ArrivalOrigin::User => crate::kernel::boot::smp2_record::ORIGIN_USER,
            ArrivalOrigin::IdleBoundary => crate::kernel::boot::smp2_record::ORIGIN_IDLE,
            ArrivalOrigin::Kernel => crate::kernel::boot::smp2_record::ORIGIN_KERNEL,
        },
        elr,
    );
    #[cfg(not(feature = "aarch64-smp2-witness"))]
    let _ = (source_cpu, elr);
}

/// Raise the reschedule SGI on THIS CPU only (`GICD_SGIR` TargetListFilter `0b10`).
///
/// For the one case no remote CPU can cover: tasks placed on an AP from the AP itself, outside a
/// trap, while it has no timer. The claim, completion and dispatch that follow are the same
/// production owners a remote wake reaches. Refused (writing nothing) before this CPU published
/// its own interface.
#[cfg_attr(not(feature = "aarch64-smp2-witness"), allow(dead_code))]
pub fn kick_self(cpu: CpuId) -> bool {
    if TARGETS.mask_of(cpu.0 as usize) == 0 {
        return false;
    }
    let Some((dist, _)) = gic_bases() else {
        return false;
    };
    let sgir = (0b10u32 << 24) | u32::from(RESCHEDULE_SGI_INTID);
    // SAFETY: a data synchronization barrier.
    unsafe { core::arch::asm!("dsb ishst", options(nostack, preserves_flags)) };
    #[cfg(feature = "aarch64-smp2-witness")]
    crate::kernel::boot::smp2_record::push(
        crate::kernel::boot::smp2_record::Kind::SgiSent,
        cpu.0,
        [u64::from(cpu.0), u64::from(sgir), 1, 0, 0],
    );
    write32(dist, GICD_SGIR, sgir);
    if let Some(n) = SENT.get(cpu.0 as usize) {
        n.fetch_add(1, Ordering::AcqRel);
    }
    true
}

/// QEMU-LOCK2 witness only — this CPU's view of the reschedule SGI at the distributor, read without
/// side effects: `(pending_sources, active, pending)`. `pending_sources` is the banked
/// `GICD_SPENDSGIR` byte for the SGI — one bit per SOURCE CPU interface whose request is pending for
/// THIS interface (GICv2 keeps one pending state per source and coalesces repeats from the same
/// source into it); `active` / `pending` are this CPU's banked `GICD_ISACTIVER0` / `GICD_ISPENDR0`
/// bits for the INTID. Reads only — the claim register `GICC_IAR` is never touched here, so the
/// observation can neither acknowledge nor reorder the interrupt. `None` before the controller is
/// known.
#[cfg(feature = "aarch64-lock2-witness")]
pub fn sgi_pending_view() -> Option<(u8, bool, bool)> {
    const GICD_ISPENDR0: usize = 0x200;
    const GICD_ISACTIVER0: usize = 0x300;
    const GICD_SPENDSGIR0: usize = 0xF20;
    let (dist, _) = gic_bases()?;
    let intid = usize::from(RESCHEDULE_SGI_INTID);
    let word = read32(dist, GICD_SPENDSGIR0 + (intid & !3));
    let sources = ((word >> ((intid & 3) * 8)) & 0xff) as u8;
    let active = read32(dist, GICD_ISACTIVER0) & (1 << intid) != 0;
    let pending = read32(dist, GICD_ISPENDR0) & (1 << intid) != 0;
    Some((sources, active, pending))
}

/// QEMU-LOCK2 witness only — the GIC CPU-interface bit `cpu` published (0 = never published).
#[cfg(feature = "aarch64-lock2-witness")]
pub fn interface_mask(cpu: CpuId) -> u8 {
    TARGETS.mask_of(cpu.0 as usize)
}

/// `(total, from_el0, at_idle_boundary, in_other_kernel_code, sent)` for `cpu`.
pub fn sgi_counters(cpu: CpuId) -> (u32, u32, u32, u32, u32) {
    let idx = cpu.0 as usize;
    let Some(row) = ARRIVALS.get(idx) else {
        return (0, 0, 0, 0, 0);
    };
    (
        row[0].load(Ordering::Acquire),
        row[1].load(Ordering::Acquire),
        row[2].load(Ordering::Acquire),
        row[3].load(Ordering::Acquire),
        SENT[idx].load(Ordering::Acquire),
    )
}

/// The BSP's boot `&mut KernelState` window is over (called immediately before its first EL0
/// entry). An AP waits for this before it acquires anything.
pub fn note_bsp_boot_borrow_ended() {
    // AP-SCHED3 witness: the set P's run must cover — every secondary the boot CPU started — named
    // once, at the moment those CPUs may begin their admissions (observation only).
    #[cfg(feature = "aarch64-apsched-witness")]
    crate::kernel::apsched_witness::note_release_plan(started_bitmap(), started_bitmap());
    BSP_BOOT_BORROW_ENDED.store(true, Ordering::Release);
    // SAFETY: wake any AP waiting in `wfe`.
    unsafe { core::arch::asm!("sev", options(nomem, nostack, preserves_flags)) };
}

pub fn bsp_boot_borrow_ended() -> bool {
    BSP_BOOT_BORROW_ENDED.load(Ordering::Acquire)
}

/// `true` once `cpu` has been admitted to production dispatch.
pub fn admitted(cpu: CpuId) -> bool {
    ADMITTED.load(Ordering::Acquire) & (1u64 << (cpu.0 as u64 & 63)) != 0
}

/// The CPUs admitted to production dispatch (bit 0, the boot CPU, is not recorded here).
pub fn admitted_bitmap() -> u64 {
    ADMITTED.load(Ordering::Acquire)
}

/// AP-SCHED3 §C — park THIS secondary's own timer for its idle wait (called at every idle
/// arrival): the next deadline moves out to the backstop. The boot CPU never parks — it is the
/// clock owner — and a CPU that never armed its timer has nothing to park. Returns whether this
/// call parked it.
pub fn park_ap_timer_for_idle(cpu: usize) -> bool {
    if cpu == crate::arch::platform_constants::BOOTSTRAP_CPU_ID as usize
        || cpu >= MAX_CPUS
        || !admitted(CpuId(cpu as u8))
    {
        return false;
    }
    AP_TIMER_PARKED[cpu].store(true, Ordering::Release);
    crate::arch::aarch64::irq::program_timer_deadline(
        CpuId(cpu as u8),
        crate::arch::platform_constants::BOOTSTRAP_TIMER_DEADLINE_TICKS,
    );
    true
}

/// AP-SCHED3 §C — leave the idle park: THIS secondary is about to run a task, so its own quantum
/// period is restored from now. Called on the one path an idle CPU leaves idle by (the idle queue
/// advance that resumes a task). Returns whether the timer was parked.
pub fn unpark_ap_timer(cpu: usize) -> bool {
    if cpu == crate::arch::platform_constants::BOOTSTRAP_CPU_ID as usize || cpu >= MAX_CPUS {
        return false;
    }
    if !AP_TIMER_PARKED[cpu].swap(false, Ordering::AcqRel) {
        return false;
    }
    crate::arch::aarch64::irq::program_timer_deadline(
        CpuId(cpu as u8),
        crate::arch::platform_constants::BOOTSTRAP_TIMER_DEADLINE_TICKS,
    );
    true
}

/// `cpu`'s timer is parked behind the idle backstop.
pub fn ap_timer_parked(cpu: usize) -> bool {
    AP_TIMER_PARKED
        .get(cpu)
        .is_some_and(|f| f.load(Ordering::Acquire))
}

/// The distance THIS CPU's next timer deadline is armed at: the requested quantum period, or the
/// idle backstop while its timer is parked.
pub fn timer_distance_for_this_cpu(period: u64) -> u64 {
    if ap_timer_parked(crate::arch::aarch64::this_cpu_index()) {
        period.saturating_mul(AP_IDLE_BACKSTOP_PERIODS)
    } else {
        period
    }
}

/// Turn on this AP's stage-1 translation with the BSP's bootstrap registers and root: the
/// identity map of the kernel image, RAM and the GIC/UART device blocks, the same tables the BSP
/// ran on before its first user root. Local invalidation only — nothing has run on this CPU with
/// translation enabled, so there is nothing another CPU could have cached for it.
fn enable_ap_translation() {
    let (mair, tcr, root) = crate::arch::aarch64::boot::bootstrap_mmu_registers();
    // SAFETY: MMU and cache enable on a CPU that has not yet touched Normal memory through any
    // translation; the root is a static, reserved identity table.
    unsafe {
        core::arch::asm!(
            "msr MAIR_EL1, {mair}",
            "msr TCR_EL1, {tcr}",
            "msr TTBR0_EL1, {root}",
            "msr TTBR1_EL1, xzr",
            "isb",
            "tlbi vmalle1",
            "dsb nsh",
            "isb",
            "ic iallu",
            "dsb nsh",
            "isb",
            mair = in(reg) mair,
            tcr = in(reg) tcr,
            root = in(reg) root,
            options(nostack, preserves_flags)
        );
        let mut sctlr: u64;
        core::arch::asm!("mrs {0}, SCTLR_EL1", out(reg) sctlr, options(nostack, preserves_flags));
        sctlr |= (1 << 0) | (1 << 2) | (1 << 12);
        core::arch::asm!(
            "msr SCTLR_EL1, {0}",
            "isb",
            in(reg) sctlr,
            options(nostack, preserves_flags)
        );
    }
}

/// AP-SCHED3 §B — how far a secondary got, for its one state record.
#[derive(Clone, Copy)]
struct ApState {
    initialized: bool,
    irq_ready: bool,
}

/// A secondary that could not be admitted: it stays wake-only (the scheduler refuses every enqueue
/// to it and balanced placement skips it), it is withdrawn as an SGI target and its own timer and
/// interrupts are disabled, and it waits masked. Named with the states it did reach.
fn park_unadmitted(cpu: CpuId, state: ApState, reason: &str) -> ! {
    TARGETS.withdraw(cpu.0 as usize);
    crate::arch::aarch64::irq::disable_this_cpu_interrupts();
    crate::kernel::printk::printk_emit_sync(format_args!(
        "AARCH64_AP_UNADMITTED cpu={} reason={} result=fail",
        cpu.0, reason
    ));
    crate::kernel::printk::printk_emit_sync(format_args!(
        "AARCH64_AP_STATE cpu={} discovered=1 initialized={} irq_ready={} admitted=0 dispatching=0 reason={}",
        cpu.0,
        u8::from(state.initialized),
        u8::from(state.irq_ready),
        reason
    ));
    loop {
        // SAFETY: a wait instruction.
        unsafe { core::arch::asm!("wfe", options(nomem, nostack, preserves_flags)) };
    }
}

/// The dispatch-stack size this image gave each secondary (`yarm_aarch64_ap_stack_stride`).
/// The secondary stack pool: (its end, the per-slot stride, the slot count). Slot `cpu - 1`'s top
/// is `end - (cpu - 1) * stride`, as `yarm_aarch64_secondary_entry` computes it.
#[cfg(not(feature = "rpi5-stage1"))]
pub fn ap_stack_pool() -> (usize, usize, usize) {
    unsafe extern "C" {
        static secondary_boot_stacks_end: u8;
        static yarm_aarch64_ap_stack_slots: u64;
    }
    // SAFETY: the pool's end symbol and two read-only constants the image defines.
    unsafe {
        (
            &raw const secondary_boot_stacks_end as usize,
            ap_stack_bytes(),
            core::ptr::read_volatile(&raw const yarm_aarch64_ap_stack_slots) as usize,
        )
    }
}

fn ap_stack_bytes() -> usize {
    unsafe extern "C" {
        static yarm_aarch64_ap_stack_stride: u64;
    }
    // SAFETY: a read-only constant the image defines.
    unsafe { core::ptr::read_volatile(&raw const yarm_aarch64_ap_stack_stride) as usize }
}

/// AP-SCHED3 §B — THIS secondary's interrupt readiness, checked from its own state rather than
/// assumed from the calls that set it: its banked timer PPI and reschedule SGI enabled, its CPU
/// interface enabled with an unmasked priority, its own timer armed, and its SGI interface bit
/// published.
fn this_ap_irq_ready(cpu: CpuId) -> bool {
    let (ppi, sgi, cpu_if) = crate::arch::aarch64::irq::this_cpu_irq_readback();
    ppi && sgi
        && cpu_if
        && crate::arch::aarch64::irq::arch_timer_is_armed()
        && TARGETS.mask_of(cpu.0 as usize) != 0
}

/// AP-SCHED3 §B — a secondary's life, from its vector base to its first park. Production: no knob.
///
/// Order is the contract, and dispatch eligibility is published LAST:
/// 1. translation on, before any shared Normal-memory synchronization;
/// 2. wait for the boot CPU's boot `&mut KernelState` window to END — the release flag alone is set
///    inside that window;
/// 3. a dispatch-sized stack, or no admission (`no_dispatch_stack`);
/// 4. its own GIC CPU interface and reschedule SGI, its interface bit published LAST within that,
///    so it is never named a target before it can take the interrupt;
/// 5. its own timer: the banked PPI, then `CNTP` armed and read back;
/// 6. readiness checked from its own registers;
/// 7. its kernel root, recorded (it holds no user root);
/// 8. the scheduler's ONE production transition (`admit_dispatch`): out of wake-only, idle
///    placeholder cleared, eligible for balanced placement — only now can a task land here;
/// 9. the same authenticated idle park the boot CPU uses (IRQs open only there).
pub fn ap_main(cpu: CpuId) -> ! {
    enable_ap_translation();
    while !bsp_boot_borrow_ended() {
        // SAFETY: a wait instruction; `note_bsp_boot_borrow_ended` issues the matching `sev`.
        unsafe { core::arch::asm!("wfe", options(nomem, nostack, preserves_flags)) };
    }
    let mut state = ApState {
        initialized: true,
        irq_ready: false,
    };
    let Some(shared) = crate::arch::aarch64::boot::trap_shared_kernel() else {
        park_unadmitted(cpu, state, "no_shared_kernel");
    };
    if !crate::arch::aarch64_cpu_admission::dispatch_capable(cpu.0 as usize, ap_stack_bytes()) {
        park_unadmitted(
            cpu,
            state,
            crate::arch::aarch64_cpu_admission::Exclusion::NoDispatchStack.marker(),
        );
    }
    if let Err(reason) = bring_up_sgi_on_this_cpu(cpu) {
        park_unadmitted(cpu, state, reason);
    }
    if !crate::arch::aarch64::irq::enable_ap_arch_timer_ppi() {
        park_unadmitted(cpu, state, "timer_ppi_readback");
    }
    crate::arch::aarch64::irq::program_timer_deadline(
        cpu,
        crate::arch::platform_constants::BOOTSTRAP_TIMER_DEADLINE_TICKS,
    );
    if !this_ap_irq_ready(cpu) {
        park_unadmitted(cpu, state, "no_irq_ready");
    }
    state.irq_ready = true;
    let root_released = crate::arch::hal::leave_user_root(cpu);
    if let Err(e) = shared.admit_ap_dispatch_split(cpu) {
        crate::kernel::printk::printk_emit_sync(format_args!(
            "AARCH64_AP_SCHED_ADMISSION_REFUSED cpu={} reason={:?} result=fail",
            cpu.0, e
        ));
        park_unadmitted(cpu, state, "scheduler_refused");
    }
    ADMITTED.fetch_or(1u64 << (cpu.0 as u64 & 63), Ordering::AcqRel);
    crate::kernel::printk::printk_emit_sync(format_args!(
        "AARCH64_AP_SCHED_ADMITTED cpu={} mpidr=0x{:x} mode=production wake_only=0 balance_excluded=0 timer=own sgi_ready=1 interface_mask=0x{:x} root=kernel root_released={} stack_bytes=0x{:x} result=ok",
        cpu.0,
        crate::arch::aarch64::read_mpidr_el1(),
        TARGETS.mask_of(cpu.0 as usize),
        u8::from(root_released),
        ap_stack_bytes()
    ));
    crate::kernel::printk::printk_emit_sync(format_args!(
        "AARCH64_AP_STATE cpu={} discovered=1 initialized=1 irq_ready=1 admitted=1 dispatching=1 reason=none",
        cpu.0
    ));
    #[cfg(feature = "aarch64-smp2-witness")]
    crate::arch::aarch64::smp2_witness::ap_admitted(shared, cpu);
    #[cfg(feature = "aarch64-overtaken-witness")]
    crate::arch::aarch64::overtaken_witness::ap_admitted(shared, cpu);
    #[cfg(feature = "aarch64-apsched-witness")]
    crate::kernel::apsched_witness::ap_admitted(cpu);
    crate::arch::aarch64::trap::enter_ap_idle(cpu)
}

/// AP-SCHED3 §B — a secondary whose identity did not validate (its MPIDR_EL1 affinity is not the
/// one the boot CPU planned for the logical id it was started with). It has published nothing and
/// touches no shared state; it is named and waits, masked, for ever.
pub fn park_identity_mismatch(cpu: CpuId, mpidr: u64, planned: u64) -> ! {
    crate::kernel::printk::printk_emit_sync(format_args!(
        "AARCH64_AP_IDENTITY_MISMATCH cpu={} mpidr=0x{:x} planned_affinity=0x{:x} result=fail",
        cpu.0, mpidr, planned
    ));
    crate::kernel::printk::printk_emit_sync(format_args!(
        "AARCH64_AP_STATE cpu={} discovered=1 initialized=0 irq_ready=0 admitted=0 dispatching=0 reason=identity_mismatch",
        cpu.0
    ));
    loop {
        // SAFETY: a wait instruction.
        unsafe { core::arch::asm!("wfe", options(nomem, nostack, preserves_flags)) };
    }
}
