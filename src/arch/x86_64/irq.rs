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
    // TODO(arch/x86_64): write LAPIC EOI register (0xB0) for external IRQ completion.
}
