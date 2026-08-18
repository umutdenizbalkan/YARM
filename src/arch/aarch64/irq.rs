// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#[cfg(any(test, target_arch = "aarch64"))]
use core::ptr::{read_volatile, write_volatile};
#[cfg(any(test, target_arch = "aarch64"))]
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[cfg(any(test, target_arch = "aarch64"))]
#[cfg_attr(feature = "hosted-dev", allow(dead_code))]
const GICC_EOIR_OFFSET: usize = 0x10;
#[cfg(any(test, target_arch = "aarch64"))]
const GICC_CTLR_OFFSET: usize = 0x00;
#[cfg(any(test, target_arch = "aarch64"))]
const GICC_PMR_OFFSET: usize = 0x04;
#[cfg(any(test, target_arch = "aarch64"))]
const GICC_IAR_OFFSET: usize = 0x0c;
#[cfg(any(test, target_arch = "aarch64"))]
const GICC_PMR_UNMASK_ALL: u32 = 0xff;
#[cfg(any(test, target_arch = "aarch64"))]
const GICC_CTLR_ENABLE_GROUP0: u32 = 0x1;

// ── GICv2 distributor ─────────────────────────────────────────────────────────────────────
//
// Canonical 199E ProductionTick. The distributor is what routes the EL1 physical timer's
// private peripheral interrupt (PPI, INTID 30) to this CPU's interface. Its registers live on
// the DTB-derived distributor page, which every AArch64 address space now maps as a privileged
// Device/XN leaf — that mapping is the reason these accesses are reachable at all once TTBR0
// has moved off the bootstrap identity root.
#[cfg(any(test, target_arch = "aarch64"))]
const GICD_CTLR_OFFSET: usize = 0x000;
#[cfg(any(test, target_arch = "aarch64"))]
const GICD_TYPER_OFFSET: usize = 0x004;
/// Set-enable for INTID 0..=31. SGIs and PPIs are *banked per CPU*, so CPU0 writing bit 30 here
/// enables CPU0's own timer PPI and no other CPU's.
#[cfg(any(test, target_arch = "aarch64"))]
const GICD_ISENABLER0_OFFSET: usize = 0x100;
/// One priority byte per INTID; the first 32 entries are banked per CPU like ISENABLER0.
#[cfg(any(test, target_arch = "aarch64"))]
const GICD_IPRIORITYR_OFFSET: usize = 0x400;
#[cfg(any(test, target_arch = "aarch64"))]
const GICD_CTLR_ENABLE_GROUP0: u32 = 0x1;

/// EL1 physical timer PPI. Fixed by the ARM generic timer binding, not by the platform.
pub const ARCH_TIMER_PPI_INTID: u16 = 30;
/// GICv2 reserves 1020..=1023 as special INTIDs; 1023 is "spurious". A claim that returns one of
/// these completes nothing: no EOI, no tick, no re-arm.
#[cfg(any(test, target_arch = "aarch64"))]
const GIC_FIRST_SPECIAL_INTID: u16 = 1020;
#[cfg(any(test, target_arch = "aarch64"))]
const GIC_INTID_MASK: u32 = 0x3ff;
/// Highest priority, so the timer PPI is never held off by the priority mask.
#[cfg(any(test, target_arch = "aarch64"))]
const GIC_TIMER_PPI_PRIORITY: u32 = 0x00;

#[cfg(any(test, target_arch = "aarch64"))]
static GIC_CPU_IF_BASE: AtomicUsize = AtomicUsize::new(0);
#[cfg(any(test, target_arch = "aarch64"))]
static GIC_CONFIGURED: AtomicBool = AtomicBool::new(false);

#[cfg(any(test, target_arch = "aarch64"))]
pub fn init_gic_cpu_if_base(base: usize) {
    if base == 0 {
        return;
    }
    gic_write_u32(base, GICC_PMR_OFFSET, GICC_PMR_UNMASK_ALL);
    gic_write_u32(base, GICC_CTLR_OFFSET, GICC_CTLR_ENABLE_GROUP0);
    GIC_CPU_IF_BASE.store(base, Ordering::Relaxed);
    GIC_CONFIGURED.store(true, Ordering::Relaxed);
}

#[cfg(all(not(test), not(target_arch = "aarch64")))]
pub fn init_gic_cpu_if_base(_base: usize) {}

pub fn configure_gic_from_platform_layout() {
    init_gic_cpu_if_base(super::platform_layout::GIC_CPU_IF_BASE);
}

pub fn try_configure_gic_from_description(description: &[u8]) -> bool {
    let Some(base) =
        crate::arch::irq_description::parse_usize_token(description, "gic_cpu_if_base")
    else {
        return false;
    };
    if base == 0 {
        return false;
    }
    init_gic_cpu_if_base(base);
    true
}

#[cfg(any(test, target_arch = "aarch64"))]
#[cfg_attr(feature = "hosted-dev", allow(dead_code))]
fn gic_write_eoir(base: usize, irq_line: u16) {
    gic_write_u32(base, GICC_EOIR_OFFSET, irq_line as u32);
}

#[cfg(any(test, target_arch = "aarch64"))]
#[cfg_attr(feature = "hosted-dev", allow(dead_code))]
fn gic_write_u32(base: usize, offset: usize, value: u32) {
    unsafe {
        write_volatile((base + offset) as *mut u32, value);
    }
}

#[cfg(any(test, target_arch = "aarch64"))]
#[cfg_attr(feature = "hosted-dev", allow(dead_code))]
fn gic_read_iar(base: usize) -> u32 {
    unsafe { read_volatile((base + GICC_IAR_OFFSET) as *const u32) }
}

#[cfg(any(test, target_arch = "aarch64"))]
#[cfg_attr(feature = "hosted-dev", allow(dead_code))]
fn gic_read_u32(base: usize, offset: usize) -> u32 {
    unsafe { read_volatile((base + offset) as *const u32) }
}

/// `true` when `CNTP_CTL_EL0` says the EL1 physical timer is ENABLED (bit 0) and NOT masked
/// (bit 1). ISTATUS (bit 2) is deliberately excluded: it reflects whether the condition is
/// *currently* met, which is transient, not whether the timer is armed.
///
/// Split out from the system-register read so the contract is provable on any host.
pub fn cntp_ctl_is_armed(cntp_ctl_el0: u64) -> bool {
    (cntp_ctl_el0 & 0x3) == 1
}

/// Reads `CNTP_CTL_EL0` and reports whether the BSP's periodic timer is actually armed.
///
/// This is a *readback*, not a hope: `start_bsp_periodic_timer` refuses to unmask IRQs unless
/// this returns `true`, so a timer that failed to program can never leave the CPU with
/// interrupts enabled and nothing to service them.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
pub fn arch_timer_is_armed() -> bool {
    let ctl: u64;
    unsafe {
        core::arch::asm!("mrs {0}, cntp_ctl_el0", out(reg) ctl, options(nomem, nostack, preserves_flags));
    }
    cntp_ctl_is_armed(ctl)
}

/// Programs the distributor and CPU interface so CPU0's banked arch-timer PPI can be delivered,
/// reading every write back. Returns `false` — changing nothing further — if any readback
/// disagrees, so a partially-configured controller never reaches the unmask step.
///
/// Split from [`enable_bsp_arch_timer_ppi`] so the register contract is exercised against a
/// scratch register file on the host, where no GIC exists.
#[cfg(any(test, target_arch = "aarch64"))]
#[cfg_attr(feature = "hosted-dev", allow(dead_code))]
fn program_arch_timer_ppi(dist_base: usize, cpu_if_base: usize) -> bool {
    // CPU interface first: an unmasked priority and an enabled interface, so an interrupt the
    // distributor forwards is actually signalled to the core.
    gic_write_u32(cpu_if_base, GICC_PMR_OFFSET, GICC_PMR_UNMASK_ALL);
    gic_write_u32(cpu_if_base, GICC_CTLR_OFFSET, GICC_CTLR_ENABLE_GROUP0);
    // Distributor: enable group 0 forwarding, preserving the other control bits.
    let dist_ctlr = gic_read_u32(dist_base, GICD_CTLR_OFFSET);
    gic_write_u32(
        dist_base,
        GICD_CTLR_OFFSET,
        dist_ctlr | GICD_CTLR_ENABLE_GROUP0,
    );
    // Give the timer PPI the highest priority, via a read-modify-write of the word holding its
    // byte, so the neighbouring INTIDs' priorities are preserved.
    let intid = ARCH_TIMER_PPI_INTID as usize;
    let priority_word = GICD_IPRIORITYR_OFFSET + (intid & !0x3);
    let lane_shift = ((intid & 0x3) * 8) as u32;
    let priority = gic_read_u32(dist_base, priority_word);
    let priority = (priority & !(0xffu32 << lane_shift)) | (GIC_TIMER_PPI_PRIORITY << lane_shift);
    gic_write_u32(dist_base, priority_word, priority);
    // Enable CPU0's banked PPI 30. ISENABLER is write-1-to-set, so the zero bits leave every
    // other INTID exactly as it was.
    gic_write_u32(
        dist_base,
        GICD_ISENABLER0_OFFSET,
        1u32 << (ARCH_TIMER_PPI_INTID as u32),
    );
    // Every write is read back, and the whole bring-up is judged by one predicate.
    gic_bring_up_readbacks_agree(
        gic_read_u32(cpu_if_base, GICC_PMR_OFFSET),
        gic_read_u32(cpu_if_base, GICC_CTLR_OFFSET),
        gic_read_u32(dist_base, GICD_CTLR_OFFSET),
        gic_read_u32(dist_base, GICD_ISENABLER0_OFFSET),
    )
}

/// Judges the four bring-up readbacks. Fail-closed: every one of them must agree, or the caller
/// programs nothing further and never unmasks.
///
/// `pmr` is required only to be non-zero rather than an exact `0xff` echo: a conforming GICv2
/// need not implement all eight priority bits, and a mask of zero is the one value that would
/// still hold the timer off.
pub fn gic_bring_up_readbacks_agree(
    pmr: u32,
    gicc_ctlr: u32,
    gicd_ctlr: u32,
    gicd_isenabler0: u32,
) -> bool {
    pmr != 0
        && (gicc_ctlr & 0x1) != 0
        && (gicd_ctlr & 0x1) != 0
        && (gicd_isenabler0 & (1u32 << (ARCH_TIMER_PPI_INTID as u32))) != 0
}

/// Brings the GIC up for CPU0's arch-timer PPI using the DTB-derived bases published to the
/// page-table layer. Never guesses an address: absent or misaligned bases return `false` and
/// program nothing, which is the RPi5/GICv3 outcome.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
pub fn enable_bsp_arch_timer_ppi() -> bool {
    let (dist_pa, cpu_if_pa) = crate::arch::aarch64::page_table::gic_mmio_bases();
    if dist_pa == 0 || cpu_if_pa == 0 {
        return false;
    }
    let (dist_base, cpu_if_base) = (dist_pa as usize, cpu_if_pa as usize);
    if !program_arch_timer_ppi(dist_base, cpu_if_base) {
        return false;
    }
    crate::yarm_log!(
        "AARCH64_GIC_PPI_ENABLED intid={} dist=0x{:x} cpu_if=0x{:x} typer=0x{:x}",
        ARCH_TIMER_PPI_INTID,
        dist_pa,
        cpu_if_pa,
        gic_read_u32(dist_base, GICD_TYPER_OFFSET)
    );
    // The claim/complete seam addresses the CPU interface through the same DTB-derived base.
    GIC_CPU_IF_BASE.store(cpu_if_base, Ordering::Relaxed);
    GIC_CONFIGURED.store(true, Ordering::Relaxed);
    true
}

#[cfg(all(not(feature = "hosted-dev"), not(target_arch = "aarch64")))]
pub fn enable_bsp_arch_timer_ppi() -> bool {
    false
}

#[cfg(feature = "hosted-dev")]
pub fn enable_bsp_arch_timer_ppi() -> bool {
    false
}

/// Claims the pending interrupt from the CPU interface (`GICC_IAR`) WITHOUT completing it.
///
/// Claim and completion are deliberately separate on this port. The arch timer's PPI is
/// level-sensitive: it stays asserted until `CNTP_TVAL_EL0` is reprogrammed. Writing `GICC_EOIR`
/// before that re-arm would complete an interrupt whose source is still asserting, and the
/// distributor would immediately re-present it — an IRQ storm. So the vector entry claims here,
/// the shared timer handler ticks and re-arms (deasserting the level), and only then does
/// [`complete_interrupt`] run.
///
/// `None` means no controller is configured, in which case nothing was claimed and nothing may
/// be completed.
#[cfg(any(test, target_arch = "aarch64"))]
#[cfg_attr(feature = "hosted-dev", allow(dead_code))]
pub fn claim_interrupt() -> Option<u16> {
    if !GIC_CONFIGURED.load(Ordering::Relaxed) {
        return None;
    }
    let iar = gic_read_iar(GIC_CPU_IF_BASE.load(Ordering::Relaxed));
    Some((iar & GIC_INTID_MASK) as u16)
}

#[cfg(all(not(test), not(target_arch = "aarch64")))]
pub fn claim_interrupt() -> Option<u16> {
    None
}

/// `true` for the GICv2 special INTIDs (1020..=1023). A claim that yields one of these must not
/// be completed, must not tick, and must not re-arm.
pub fn intid_is_special(intid: u16) -> bool {
    #[cfg(any(test, target_arch = "aarch64"))]
    {
        intid >= GIC_FIRST_SPECIAL_INTID
    }
    #[cfg(all(not(test), not(target_arch = "aarch64")))]
    {
        let _ = intid;
        true
    }
}

/// Completes a previously claimed INTID by writing `GICC_EOIR`. Exactly one completion per
/// claim, and only after the interrupt's source has been deasserted.
#[cfg(any(test, target_arch = "aarch64"))]
#[cfg_attr(feature = "hosted-dev", allow(dead_code))]
pub fn complete_interrupt(intid: u16) {
    if !GIC_CONFIGURED.load(Ordering::Relaxed) || intid_is_special(intid) {
        return;
    }
    gic_write_eoir(GIC_CPU_IF_BASE.load(Ordering::Relaxed), intid);
}

#[cfg(all(not(test), not(target_arch = "aarch64")))]
pub fn complete_interrupt(intid: u16) {
    let _ = intid;
}

#[derive(Clone, Copy)]
pub struct Aarch64IrqState {
    pub interrupts_were_enabled: bool,
}

#[cfg(feature = "hosted-dev")]
pub fn irq_save() -> Aarch64IrqState {
    Aarch64IrqState {
        interrupts_were_enabled: true,
    }
}

#[cfg(feature = "hosted-dev")]
pub fn irq_restore(_state: Aarch64IrqState) {}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
pub fn irq_save() -> Aarch64IrqState {
    unsafe {
        let daif: usize;
        core::arch::asm!("mrs {0}, daif", out(reg) daif, options(nomem, preserves_flags));
        core::arch::asm!("msr daifset, #2", options(nomem, preserves_flags));
        Aarch64IrqState {
            interrupts_were_enabled: (daif & (1 << 7)) == 0,
        }
    }
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
pub fn irq_restore(state: Aarch64IrqState) {
    if !state.interrupts_were_enabled {
        return;
    }
    unsafe {
        core::arch::asm!("msr daifclr, #2", options(nomem, preserves_flags));
    }
}

#[cfg(all(not(feature = "hosted-dev"), not(target_arch = "aarch64")))]
pub fn irq_save() -> Aarch64IrqState {
    Aarch64IrqState {
        interrupts_were_enabled: true,
    }
}

#[cfg(all(not(feature = "hosted-dev"), not(target_arch = "aarch64")))]
pub fn irq_restore(_state: Aarch64IrqState) {}

#[cfg(feature = "hosted-dev")]
pub fn external_irq_eoi(_irq_line: u16) {}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
pub fn external_irq_eoi(irq_line: u16) {
    if !GIC_CONFIGURED.load(Ordering::Relaxed) {
        return;
    }
    gic_write_eoir(GIC_CPU_IF_BASE.load(Ordering::Relaxed), irq_line);
}

#[cfg(all(not(feature = "hosted-dev"), not(target_arch = "aarch64")))]
pub fn external_irq_eoi(_irq_line: u16) {}

/// No-op on AArch64: acknowledgement is not a single step here.
///
/// The shared timer handler calls this at the top of its arm, which is the wrong place for a
/// GICv2 completion — the arch timer's level source is still asserted at that point. AArch64
/// therefore claims in the vector entry ([`claim_interrupt`]) and completes after the handler
/// has re-armed `CNTP` ([`complete_interrupt`]). Reading `GICC_IAR` here as well would consume a
/// second, unrelated claim. x86_64 and RISC-V keep their own single-step acknowledgement.
pub fn acknowledge_interrupt(irq_line: u16) {
    let _ = irq_line;
}

#[cfg(feature = "hosted-dev")]
pub fn program_timer_deadline(_cpu: crate::kernel::scheduler::CpuId, _ticks_from_now: u64) {}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
pub fn program_timer_deadline(_cpu: crate::kernel::scheduler::CpuId, ticks_from_now: u64) {
    let clamped = ticks_from_now.clamp(1, u32::MAX as u64);
    unsafe {
        core::arch::asm!("msr cntp_tval_el0, {0}", in(reg) clamped, options(nostack, preserves_flags));
        core::arch::asm!("msr cntp_ctl_el0, {0}", in(reg) 1u64, options(nostack, preserves_flags));
        core::arch::asm!("isb", options(nostack, preserves_flags));
    }
}

#[cfg(all(not(feature = "hosted-dev"), not(target_arch = "aarch64")))]
pub fn program_timer_deadline(_cpu: crate::kernel::scheduler::CpuId, _ticks_from_now: u64) {}

#[cfg(feature = "hosted-dev")]
pub fn enable_interrupts_for_boot() {}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
pub fn enable_interrupts_for_boot() {
    unsafe {
        core::arch::asm!("msr daifclr, #2", options(nostack, preserves_flags));
    }
}

#[cfg(all(not(feature = "hosted-dev"), not(target_arch = "aarch64")))]
pub fn enable_interrupts_for_boot() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gic_eoir_write_targets_expected_register() {
        let mut regs = [0u32; 64];
        let base = regs.as_mut_ptr() as usize;
        gic_write_eoir(base, 55);
        assert_eq!(regs[GICC_EOIR_OFFSET / core::mem::size_of::<u32>()], 55);
    }

    #[test]
    fn init_gic_marks_controller_configured() {
        GIC_CONFIGURED.store(false, Ordering::Relaxed);
        let mut regs = [0u32; 64];
        let base = regs.as_mut_ptr() as usize;
        init_gic_cpu_if_base(base);
        assert!(GIC_CONFIGURED.load(Ordering::Relaxed));
        assert_eq!(
            regs[GICC_PMR_OFFSET / core::mem::size_of::<u32>()],
            GICC_PMR_UNMASK_ALL
        );
        assert_eq!(
            regs[GICC_CTLR_OFFSET / core::mem::size_of::<u32>()],
            GICC_CTLR_ENABLE_GROUP0
        );
    }

    #[test]
    fn gic_configuration_parses_description() {
        let mut regs = [0u32; 64];
        let description = crate::std::format!("gic_cpu_if_base=0x{:x}", regs.as_mut_ptr() as usize);
        GIC_CONFIGURED.store(false, Ordering::Relaxed);
        assert!(try_configure_gic_from_description(description.as_bytes()));
        assert!(GIC_CONFIGURED.load(Ordering::Relaxed));
        assert_eq!(
            regs[GICC_PMR_OFFSET / core::mem::size_of::<u32>()],
            GICC_PMR_UNMASK_ALL
        );
        assert_eq!(
            regs[GICC_CTLR_OFFSET / core::mem::size_of::<u32>()],
            GICC_CTLR_ENABLE_GROUP0
        );
    }

    /// Canonical 199E: the shared handler's single-step acknowledgement is inert on AArch64, so
    /// it cannot consume a claim the vector entry already took, nor complete a level source that
    /// is still asserted.
    #[test]
    fn acknowledge_interrupt_is_inert_on_aarch64() {
        let mut regs = [0u32; 64];
        let base = regs.as_mut_ptr() as usize;
        GIC_CPU_IF_BASE.store(base, Ordering::Relaxed);
        GIC_CONFIGURED.store(true, Ordering::Relaxed);
        regs[GICC_IAR_OFFSET / core::mem::size_of::<u32>()] = 31;

        acknowledge_interrupt(0);

        assert_eq!(
            regs[GICC_EOIR_OFFSET / core::mem::size_of::<u32>()],
            0,
            "no completion may be written from the shared handler's acknowledge seam"
        );
    }

    const WORD: usize = core::mem::size_of::<u32>();

    /// A scratch register file standing in for the two GIC pages. Plain stores, so a readback
    /// returns what was written — which is exactly what the fail-closed checks depend on.
    fn scratch_gic() -> ([u32; 512], [u32; 64]) {
        ([0u32; 512], [0u32; 64])
    }

    #[test]
    fn ppi_bring_up_enables_intid_30_and_reads_it_back() {
        let (mut dist, mut cpu_if) = scratch_gic();
        let dist_base = dist.as_mut_ptr() as usize;
        let cpu_if_base = cpu_if.as_mut_ptr() as usize;

        assert!(program_arch_timer_ppi(dist_base, cpu_if_base));

        assert_eq!(
            dist[GICD_ISENABLER0_OFFSET / WORD] & (1 << 30),
            1 << 30,
            "CPU0's banked arch-timer PPI is enabled"
        );
        assert_eq!(
            dist[GICD_CTLR_OFFSET / WORD] & GICD_CTLR_ENABLE_GROUP0,
            GICD_CTLR_ENABLE_GROUP0,
            "the distributor forwards group 0"
        );
        assert_eq!(
            cpu_if[GICC_CTLR_OFFSET / WORD] & GICC_CTLR_ENABLE_GROUP0,
            GICC_CTLR_ENABLE_GROUP0,
            "the CPU interface signals group 0 to the core"
        );
        assert_eq!(
            cpu_if[GICC_PMR_OFFSET / WORD],
            GICC_PMR_UNMASK_ALL,
            "the priority mask does not hold the timer off"
        );
        // Priority byte for INTID 30 is lane 2 of the word at 0x400 + 28.
        let priority_word = dist[(GICD_IPRIORITYR_OFFSET + 28) / WORD];
        assert_eq!(
            (priority_word >> 16) & 0xff,
            GIC_TIMER_PPI_PRIORITY,
            "the timer PPI gets the highest priority"
        );
    }

    /// Only INTID 30's bit is set, so no other CPU's banked PPI and no shared SPI is touched.
    #[test]
    fn ppi_bring_up_enables_nothing_but_the_timer() {
        let (mut dist, mut cpu_if) = scratch_gic();
        let dist_base = dist.as_mut_ptr() as usize;
        let cpu_if_base = cpu_if.as_mut_ptr() as usize;

        assert!(program_arch_timer_ppi(dist_base, cpu_if_base));

        assert_eq!(
            dist[GICD_ISENABLER0_OFFSET / WORD],
            1 << 30,
            "exactly one INTID is enabled"
        );
        for word in 1..8 {
            assert_eq!(
                dist[GICD_ISENABLER0_OFFSET / WORD + word],
                0,
                "no shared peripheral interrupt is enabled"
            );
        }
    }

    /// Every readback must agree. Any one of them disagreeing stops the bring-up dead, so the
    /// caller never reaches the CNTP arm or the DAIF unmask.
    #[test]
    fn ppi_bring_up_fails_closed_when_any_readback_disagrees() {
        let enabled = 1u32 << (ARCH_TIMER_PPI_INTID as u32);
        assert!(gic_bring_up_readbacks_agree(0xff, 0x1, 0x1, enabled));
        // A GICv2 implementing only five priority bits echoes 0xf8 — still a valid unmask.
        assert!(gic_bring_up_readbacks_agree(0xf8, 0x1, 0x1, enabled));

        assert!(
            !gic_bring_up_readbacks_agree(0x00, 0x1, 0x1, enabled),
            "a priority mask that still blocks everything"
        );
        assert!(
            !gic_bring_up_readbacks_agree(0xff, 0x0, 0x1, enabled),
            "a CPU interface that did not enable"
        );
        assert!(
            !gic_bring_up_readbacks_agree(0xff, 0x1, 0x0, enabled),
            "a distributor that did not enable"
        );
        assert!(
            !gic_bring_up_readbacks_agree(0xff, 0x1, 0x1, 0),
            "a PPI that did not enable"
        );
        assert!(
            !gic_bring_up_readbacks_agree(0xff, 0x1, 0x1, enabled >> 1),
            "a neighbouring INTID enabling instead of the timer's is not good enough"
        );
    }

    /// Claim and completion are two steps, and completion writes exactly the claimed INTID.
    #[test]
    fn claim_and_complete_are_separate_steps() {
        let mut regs = [0u32; 64];
        let base = regs.as_mut_ptr() as usize;
        GIC_CPU_IF_BASE.store(base, Ordering::Relaxed);
        GIC_CONFIGURED.store(true, Ordering::Relaxed);
        regs[GICC_IAR_OFFSET / WORD] = ARCH_TIMER_PPI_INTID as u32;

        let claimed = claim_interrupt().expect("a configured controller claims");
        assert_eq!(claimed, ARCH_TIMER_PPI_INTID);
        assert_eq!(
            regs[GICC_EOIR_OFFSET / WORD],
            0,
            "claiming must not complete — the level source is still asserted"
        );

        complete_interrupt(claimed);
        assert_eq!(
            regs[GICC_EOIR_OFFSET / WORD],
            ARCH_TIMER_PPI_INTID as u32,
            "completion writes exactly the INTID that was claimed"
        );
    }

    /// A spurious claim completes nothing.
    #[test]
    fn spurious_intids_are_never_completed() {
        let mut regs = [0u32; 64];
        let base = regs.as_mut_ptr() as usize;
        GIC_CPU_IF_BASE.store(base, Ordering::Relaxed);
        GIC_CONFIGURED.store(true, Ordering::Relaxed);
        regs[GICC_IAR_OFFSET / WORD] = 1023;

        let claimed = claim_interrupt().expect("a configured controller claims");
        assert_eq!(claimed, 1023);
        assert!(intid_is_special(claimed));
        for intid in [1020u16, 1021, 1022, 1023] {
            assert!(intid_is_special(intid));
        }
        assert!(!intid_is_special(ARCH_TIMER_PPI_INTID));

        complete_interrupt(claimed);
        assert_eq!(
            regs[GICC_EOIR_OFFSET / WORD],
            0,
            "GICv2 special INTIDs are not completed"
        );
    }

    /// With no controller configured nothing is claimed, so nothing can be completed either.
    #[test]
    fn an_unconfigured_controller_claims_nothing() {
        GIC_CONFIGURED.store(false, Ordering::Relaxed);
        assert_eq!(claim_interrupt(), None);
    }

    /// `CNTP_CTL_EL0 & 0x3 == 1`: enabled and unmasked. ISTATUS is transient and excluded.
    #[test]
    fn cntp_readback_accepts_only_enabled_and_unmasked() {
        assert!(cntp_ctl_is_armed(0b001), "ENABLE set, IMASK clear");
        assert!(cntp_ctl_is_armed(0b101), "ISTATUS is ignored");
        assert!(!cntp_ctl_is_armed(0b000), "not enabled");
        assert!(!cntp_ctl_is_armed(0b011), "enabled but masked");
        assert!(!cntp_ctl_is_armed(0b010), "masked and not enabled");
    }
}
