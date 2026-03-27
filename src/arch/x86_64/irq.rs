#[cfg(any(test, not(feature = "hosted-dev")))]
use core::ptr::write_volatile;
#[cfg(any(test, not(feature = "hosted-dev")))]
use core::sync::atomic::{AtomicUsize, Ordering};

#[cfg(any(test, not(feature = "hosted-dev")))]
const LAPIC_MMIO_DEFAULT_BASE: usize = 0xFEE0_0000;
#[cfg(any(test, not(feature = "hosted-dev")))]
const LAPIC_EOI_OFFSET: usize = 0xB0;

#[cfg(any(test, not(feature = "hosted-dev")))]
static LAPIC_MMIO_BASE: AtomicUsize = AtomicUsize::new(LAPIC_MMIO_DEFAULT_BASE);

#[cfg(any(test, not(feature = "hosted-dev")))]
pub fn init_lapic_mmio_base(base: usize) {
    if base == 0 {
        return;
    }
    LAPIC_MMIO_BASE.store(base, Ordering::Relaxed);
}

#[cfg(any(test, not(feature = "hosted-dev")))]
fn lapic_write_eoi(base: usize) {
    unsafe {
        write_volatile((base + LAPIC_EOI_OFFSET) as *mut u32, 0);
    }
}

#[derive(Clone, Copy)]
pub struct X86IrqState {
    pub interrupts_were_enabled: bool,
}

#[cfg(feature = "hosted-dev")]
pub fn irq_save() -> X86IrqState {
    X86IrqState {
        interrupts_were_enabled: true,
    }
}

#[cfg(feature = "hosted-dev")]
pub fn irq_restore(_state: X86IrqState) {}

#[cfg(not(feature = "hosted-dev"))]
pub fn irq_save() -> X86IrqState {
    unsafe {
        let flags: usize;
        core::arch::asm!("pushfq", "pop {}", out(reg) flags, options(nomem, preserves_flags));
        core::arch::asm!("cli", options(nomem, preserves_flags));
        X86IrqState {
            interrupts_were_enabled: (flags & (1 << 9)) != 0,
        }
    }
}

#[cfg(not(feature = "hosted-dev"))]
pub fn irq_restore(state: X86IrqState) {
    if !state.interrupts_were_enabled {
        return;
    }
    unsafe {
        core::arch::asm!("sti", options(nomem, preserves_flags));
    }
}

#[cfg(feature = "hosted-dev")]
pub fn external_irq_eoi(_irq_line: u16) {}

#[cfg(not(feature = "hosted-dev"))]
pub fn external_irq_eoi(_irq_line: u16) {
    lapic_write_eoi(LAPIC_MMIO_BASE.load(Ordering::Relaxed));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lapic_eoi_write_targets_expected_register() {
        let mut regs = [0u32; 64];
        let base = regs.as_mut_ptr() as usize;
        lapic_write_eoi(base);
        assert_eq!(regs[LAPIC_EOI_OFFSET / core::mem::size_of::<u32>()], 0);
    }
}
