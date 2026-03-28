#[cfg(any(test, target_arch = "aarch64"))]
use core::ptr::write_volatile;
#[cfg(any(test, target_arch = "aarch64"))]
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[cfg(any(test, target_arch = "aarch64"))]
const GICC_EOIR_OFFSET: usize = 0x10;

#[cfg(any(test, target_arch = "aarch64"))]
static GIC_CPU_IF_BASE: AtomicUsize = AtomicUsize::new(0);
#[cfg(any(test, target_arch = "aarch64"))]
static GIC_CONFIGURED: AtomicBool = AtomicBool::new(false);

#[cfg(any(test, target_arch = "aarch64"))]
pub fn init_gic_cpu_if_base(base: usize) {
    if base == 0 {
        return;
    }
    GIC_CPU_IF_BASE.store(base, Ordering::Relaxed);
    GIC_CONFIGURED.store(true, Ordering::Relaxed);
}

pub fn configure_gic_from_platform_layout() {
    init_gic_cpu_if_base(super::platform_layout::GIC_CPU_IF_BASE);
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

pub fn try_configure_gic_from_description(description: &[u8]) -> bool {
    let Some(base) = parse_usize_token(description, "gic_cpu_if_base") else {
        return false;
    };
    if base == 0 {
        return false;
    }
    init_gic_cpu_if_base(base);
    true
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
    if !GIC_CONFIGURED.load(Ordering::Relaxed) {
        return;
    }
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

    #[test]
    fn init_gic_marks_controller_configured() {
        GIC_CONFIGURED.store(false, Ordering::Relaxed);
        init_gic_cpu_if_base(0x3000);
        assert!(GIC_CONFIGURED.load(Ordering::Relaxed));
    }

    #[test]
    fn gic_configuration_parses_description() {
        GIC_CONFIGURED.store(false, Ordering::Relaxed);
        assert!(try_configure_gic_from_description(
            b"gic_cpu_if_base=0x08010000"
        ));
        assert!(GIC_CONFIGURED.load(Ordering::Relaxed));
    }
}
