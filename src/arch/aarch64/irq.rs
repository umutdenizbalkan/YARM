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
