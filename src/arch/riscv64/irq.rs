#[cfg(any(test, target_arch = "riscv64"))]
use core::ptr::write_volatile;
#[cfg(any(test, target_arch = "riscv64"))]
use core::sync::atomic::{AtomicUsize, Ordering};

#[cfg(any(test, target_arch = "riscv64"))]
const PLIC_CONTEXT_BASE_OFFSET: usize = 0x0020_0000;
#[cfg(any(test, target_arch = "riscv64"))]
const PLIC_CONTEXT_STRIDE: usize = 0x1000;
#[cfg(any(test, target_arch = "riscv64"))]
const PLIC_CLAIM_COMPLETE_OFFSET: usize = 0x4;

#[cfg(any(test, target_arch = "riscv64"))]
static PLIC_MMIO_BASE: AtomicUsize = AtomicUsize::new(super::platform_layout::PLIC_MMIO_BASE);
#[cfg(any(test, target_arch = "riscv64"))]
static PLIC_CONTEXT_INDEX: AtomicUsize =
    AtomicUsize::new(super::platform_layout::PLIC_SMODE_CONTEXT_INDEX);

#[cfg(any(test, target_arch = "riscv64"))]
pub fn init_plic_mmio_base(base: usize) {
    if base == 0 {
        return;
    }
    PLIC_MMIO_BASE.store(base, Ordering::Relaxed);
}

#[cfg(any(test, target_arch = "riscv64"))]
pub fn init_plic_context_index(context_index: usize) {
    PLIC_CONTEXT_INDEX.store(context_index, Ordering::Relaxed);
}

pub fn configure_plic_from_platform_layout() {
    init_plic_mmio_base(super::platform_layout::PLIC_MMIO_BASE);
    init_plic_context_index(super::platform_layout::PLIC_SMODE_CONTEXT_INDEX);
}

#[cfg(any(test, target_arch = "riscv64"))]
fn plic_claim_complete_addr(base: usize, context_index: usize) -> usize {
    base + PLIC_CONTEXT_BASE_OFFSET
        + (context_index * PLIC_CONTEXT_STRIDE)
        + PLIC_CLAIM_COMPLETE_OFFSET
}

#[cfg(any(test, target_arch = "riscv64"))]
fn plic_write_complete(addr: usize, irq_line: u16) {
    unsafe {
        write_volatile(addr as *mut u32, irq_line as u32);
    }
}

#[derive(Clone, Copy)]
pub struct Riscv64IrqState {
    pub interrupts_were_enabled: bool,
}

#[cfg(feature = "hosted-dev")]
pub fn irq_save() -> Riscv64IrqState {
    Riscv64IrqState {
        interrupts_were_enabled: true,
    }
}

#[cfg(feature = "hosted-dev")]
pub fn irq_restore(_state: Riscv64IrqState) {}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
pub fn irq_save() -> Riscv64IrqState {
    unsafe {
        let sstatus: usize;
        core::arch::asm!("csrrc {0}, sstatus, {1}", out(reg) sstatus, in(reg) 1usize << 1, options(nomem));
        Riscv64IrqState {
            interrupts_were_enabled: (sstatus & (1 << 1)) != 0,
        }
    }
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
pub fn irq_restore(state: Riscv64IrqState) {
    if !state.interrupts_were_enabled {
        return;
    }
    unsafe {
        core::arch::asm!("csrs sstatus, {0}", in(reg) 1usize << 1, options(nomem));
    }
}

#[cfg(all(not(feature = "hosted-dev"), not(target_arch = "riscv64")))]
pub fn irq_save() -> Riscv64IrqState {
    Riscv64IrqState {
        interrupts_were_enabled: true,
    }
}

#[cfg(all(not(feature = "hosted-dev"), not(target_arch = "riscv64")))]
pub fn irq_restore(_state: Riscv64IrqState) {}

#[cfg(feature = "hosted-dev")]
pub fn external_irq_eoi(_irq_line: u16) {}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
pub fn external_irq_eoi(irq_line: u16) {
    let base = PLIC_MMIO_BASE.load(Ordering::Relaxed);
    let context_index = PLIC_CONTEXT_INDEX.load(Ordering::Relaxed);
    let complete_addr = plic_claim_complete_addr(base, context_index);
    plic_write_complete(complete_addr, irq_line);
}

#[cfg(all(not(feature = "hosted-dev"), not(target_arch = "riscv64")))]
pub fn external_irq_eoi(_irq_line: u16) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plic_complete_write_targets_expected_register() {
        let mut regs = [0u32; 64];
        let total_offset =
            PLIC_CONTEXT_BASE_OFFSET + (3 * PLIC_CONTEXT_STRIDE) + PLIC_CLAIM_COMPLETE_OFFSET;
        let base = (regs.as_mut_ptr() as usize).saturating_sub(total_offset);
        let context = 3usize;
        let addr = plic_claim_complete_addr(base, context);
        plic_write_complete(addr, 37);
        let word = (addr - (regs.as_mut_ptr() as usize)) / core::mem::size_of::<u32>();
        assert_eq!(regs[word], 37);
    }
}
