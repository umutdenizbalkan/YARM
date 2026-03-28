#[cfg(any(test, not(feature = "hosted-dev")))]
use core::ptr::write_volatile;
#[cfg(any(test, not(feature = "hosted-dev")))]
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[cfg(any(test, not(feature = "hosted-dev")))]
const LAPIC_EOI_OFFSET: usize = 0xB0;

#[cfg(any(test, not(feature = "hosted-dev")))]
static LAPIC_MMIO_BASE: AtomicUsize = AtomicUsize::new(0);
#[cfg(any(test, not(feature = "hosted-dev")))]
static LAPIC_CONFIGURED: AtomicBool = AtomicBool::new(false);

#[cfg(any(test, not(feature = "hosted-dev")))]
pub fn init_lapic_mmio_base(base: usize) {
    if base == 0 {
        return;
    }
    LAPIC_MMIO_BASE.store(base, Ordering::Relaxed);
    LAPIC_CONFIGURED.store(true, Ordering::Relaxed);
}

pub fn configure_lapic_from_platform_layout() {
    init_lapic_mmio_base(super::platform_layout::LAPIC_MMIO_BASE);
}

fn parse_usize_token(description: &[u8], key: &str) -> Option<usize> {
    let text = core::str::from_utf8(description).ok()?;
    for token in text.split(|ch: char| ch.is_ascii_whitespace() || matches!(ch, ',' | ';')) {
        if token.is_empty() {
            continue;
        }
        let (lhs, rhs) = token.split_once('=')?;
        if lhs != key {
            continue;
        }
        if let Some(hex) = rhs.strip_prefix("0x").or_else(|| rhs.strip_prefix("0X")) {
            return usize::from_str_radix(hex, 16).ok();
        }
        if let Ok(value) = rhs.parse::<usize>() {
            return Some(value);
        }
    }
    None
}

pub fn try_configure_lapic_from_description(description: &[u8]) -> bool {
    let Some(base) = parse_usize_token(description, "lapic_mmio_base") else {
        return false;
    };
    if base == 0 {
        return false;
    }
    init_lapic_mmio_base(base);
    true
}

#[cfg(test)]
pub fn reset_lapic_config_for_test() {
    LAPIC_CONFIGURED.store(false, Ordering::Relaxed);
    LAPIC_MMIO_BASE.store(0, Ordering::Relaxed);
}

#[cfg(test)]
pub fn lapic_mmio_base_for_test() -> usize {
    LAPIC_MMIO_BASE.load(Ordering::Relaxed)
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
    if !LAPIC_CONFIGURED.load(Ordering::Relaxed) {
        return;
    }
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

    #[test]
    fn init_lapic_marks_controller_configured() {
        LAPIC_CONFIGURED.store(false, Ordering::Relaxed);
        init_lapic_mmio_base(0x1000);
        assert!(LAPIC_CONFIGURED.load(Ordering::Relaxed));
    }

    #[test]
    fn lapic_configuration_parses_description() {
        LAPIC_CONFIGURED.store(false, Ordering::Relaxed);
        assert!(try_configure_lapic_from_description(
            b"lapic_mmio_base=0xfee00000"
        ));
        assert!(LAPIC_CONFIGURED.load(Ordering::Relaxed));
    }
}
