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

pub fn acknowledge_interrupt(irq_line: u16) {
    let _ = irq_line;
    #[cfg(any(test, target_arch = "aarch64"))]
    {
        if !GIC_CONFIGURED.load(Ordering::Relaxed) {
            return;
        }
        let base = GIC_CPU_IF_BASE.load(Ordering::Relaxed);
        let iar = gic_read_iar(base);
        let acknowledged_irq = (iar & 0x3ff) as u16;
        if acknowledged_irq < 1020 {
            gic_write_eoir(base, acknowledged_irq);
        }
    }
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

    #[test]
    fn acknowledge_interrupt_reads_iar_and_writes_eoir() {
        let mut regs = [0u32; 64];
        let base = regs.as_mut_ptr() as usize;
        GIC_CPU_IF_BASE.store(base, Ordering::Relaxed);
        GIC_CONFIGURED.store(true, Ordering::Relaxed);
        regs[GICC_IAR_OFFSET / core::mem::size_of::<u32>()] = 31;

        acknowledge_interrupt(0);

        assert_eq!(regs[GICC_EOIR_OFFSET / core::mem::size_of::<u32>()], 31);
    }
}
