#[cfg(any(test, target_arch = "aarch64"))]
use core::ptr::write_volatile;
#[cfg(any(test, target_arch = "aarch64"))]
use core::sync::atomic::{AtomicUsize, Ordering};

#[cfg(any(test, target_arch = "aarch64"))]
const GIC_CPU_IF_DEFAULT_BASE: usize = 0x0801_0000;
#[cfg(any(test, target_arch = "aarch64"))]
const GICC_EOIR_OFFSET: usize = 0x10;

#[cfg(any(test, target_arch = "aarch64"))]
static GIC_CPU_IF_BASE: AtomicUsize = AtomicUsize::new(GIC_CPU_IF_DEFAULT_BASE);

#[cfg(any(test, target_arch = "aarch64"))]
pub fn init_gic_cpu_if_base(base: usize) {
    if base == 0 {
        return;
    }
    GIC_CPU_IF_BASE.store(base, Ordering::Relaxed);
}

#[cfg(any(test, target_arch = "aarch64"))]
fn gic_write_eoir(base: usize, irq_line: u16) {
    unsafe {
        write_volatile((base + GICC_EOIR_OFFSET) as *mut u32, irq_line as u32);
    }
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
    gic_write_eoir(GIC_CPU_IF_BASE.load(Ordering::Relaxed), irq_line);
}

#[cfg(all(not(feature = "hosted-dev"), not(target_arch = "aarch64")))]
pub fn external_irq_eoi(_irq_line: u16) {}

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
}
