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
pub fn external_irq_eoi(_irq_line: u16) {
    // TODO(arch/riscv64): complete PLIC claim/complete handshake for this IRQ line.
}

#[cfg(all(not(feature = "hosted-dev"), not(target_arch = "riscv64")))]
pub fn external_irq_eoi(_irq_line: u16) {}
