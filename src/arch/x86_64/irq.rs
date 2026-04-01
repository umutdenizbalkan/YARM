#[cfg(all(not(test), not(feature = "hosted-dev")))]
use crate::kernel::vm::{Asid, PageFlags, PhysAddr, VirtAddr};
#[cfg(any(test, not(feature = "hosted-dev")))]
use core::ptr::write_volatile;
#[cfg(any(test, not(feature = "hosted-dev")))]
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[cfg(any(test, not(feature = "hosted-dev")))]
const LAPIC_EOI_OFFSET: usize = 0xB0;
#[cfg(any(test, not(feature = "hosted-dev")))]
const LAPIC_SVR_OFFSET: usize = 0xF0;
#[cfg(any(test, not(feature = "hosted-dev")))]
const LAPIC_LVT_TIMER_OFFSET: usize = 0x320;
#[cfg(any(test, not(feature = "hosted-dev")))]
const LAPIC_TIMER_INITIAL_COUNT_OFFSET: usize = 0x380;
#[cfg(any(test, not(feature = "hosted-dev")))]
const LAPIC_TIMER_DIVIDE_CONFIG_OFFSET: usize = 0x3E0;
#[cfg(any(test, not(feature = "hosted-dev")))]
const LAPIC_TIMER_VECTOR: u32 = 0x20;
#[cfg(any(test, not(feature = "hosted-dev")))]
const LAPIC_TIMER_DIVIDE_BY_16: u32 = 0x3;
#[cfg(any(test, not(feature = "hosted-dev")))]
const LAPIC_SPURIOUS_VECTOR: u32 = 0xFF;
#[cfg(any(test, not(feature = "hosted-dev")))]
const LAPIC_SVR_ENABLE: u32 = 1 << 8;

#[cfg(any(test, not(feature = "hosted-dev")))]
static LAPIC_MMIO_BASE: AtomicUsize = AtomicUsize::new(0);
#[cfg(any(test, not(feature = "hosted-dev")))]
static LAPIC_CONFIGURED: AtomicBool = AtomicBool::new(false);

#[cfg(any(test, not(feature = "hosted-dev")))]
pub fn init_lapic_mmio_base(base: usize) {
    if base == 0 {
        return;
    }
    #[cfg(all(not(test), not(feature = "hosted-dev")))]
    {
        if crate::arch::selected_isa::page_table::map_page(
            Asid(0),
            VirtAddr(base as u64),
            PhysAddr(base as u64),
            PageFlags::DEVICE_RW,
        )
        .is_err()
        {
            return;
        }
    }
    lapic_write_u32(base, LAPIC_SVR_OFFSET, LAPIC_SVR_ENABLE | LAPIC_SPURIOUS_VECTOR);
    lapic_program_timer_deadline(base, super::platform_layout::BOOTSTRAP_TIMER_DEADLINE_TICKS);
    LAPIC_MMIO_BASE.store(base, Ordering::Relaxed);
    LAPIC_CONFIGURED.store(true, Ordering::Relaxed);
}

#[cfg(all(feature = "hosted-dev", not(test)))]
pub fn init_lapic_mmio_base(_base: usize) {}

pub fn configure_lapic_from_platform_layout() {
    init_lapic_mmio_base(super::platform_layout::LAPIC_MMIO_BASE);
}

pub fn try_configure_lapic_from_description(description: &[u8]) -> bool {
    let Some(base) =
        crate::arch::irq_description::parse_usize_token(description, "lapic_mmio_base")
    else {
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

#[cfg(any(test, not(feature = "hosted-dev")))]
fn lapic_write_u32(base: usize, offset: usize, value: u32) {
    unsafe {
        write_volatile((base + offset) as *mut u32, value);
    }
}

#[cfg(any(test, not(feature = "hosted-dev")))]
fn lapic_program_timer_deadline(base: usize, ticks_from_now: u64) {
    let count = ticks_from_now.clamp(1, u32::MAX as u64) as u32;
    lapic_write_u32(
        base,
        LAPIC_TIMER_DIVIDE_CONFIG_OFFSET,
        LAPIC_TIMER_DIVIDE_BY_16,
    );
    lapic_write_u32(base, LAPIC_LVT_TIMER_OFFSET, LAPIC_TIMER_VECTOR);
    lapic_write_u32(base, LAPIC_TIMER_INITIAL_COUNT_OFFSET, count);
}

pub fn acknowledge_interrupt(_irq_line: u16) {
    #[cfg(any(test, not(feature = "hosted-dev")))]
    {
        if !LAPIC_CONFIGURED.load(Ordering::Relaxed) {
            return;
        }
        lapic_write_eoi(LAPIC_MMIO_BASE.load(Ordering::Relaxed));
    }
}

pub fn program_timer_deadline(_cpu: crate::kernel::scheduler::CpuId, _ticks_from_now: u64) {
    #[cfg(any(test, not(feature = "hosted-dev")))]
    {
        if !LAPIC_CONFIGURED.load(Ordering::Relaxed) {
            return;
        }
        lapic_program_timer_deadline(LAPIC_MMIO_BASE.load(Ordering::Relaxed), _ticks_from_now);
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
pub fn external_irq_eoi(irq_line: u16) {
    acknowledge_interrupt(irq_line);
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
        let mut regs = [0u32; 512];
        LAPIC_CONFIGURED.store(false, Ordering::Relaxed);
        init_lapic_mmio_base(regs.as_mut_ptr() as usize);
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

    #[test]
    fn program_timer_deadline_writes_lapic_timer_registers() {
        let mut regs = [0u32; 512];
        LAPIC_MMIO_BASE.store(regs.as_mut_ptr() as usize, Ordering::Relaxed);
        LAPIC_CONFIGURED.store(true, Ordering::Relaxed);

        program_timer_deadline(crate::kernel::scheduler::CpuId(0), 42);

        assert_eq!(
            regs[LAPIC_TIMER_DIVIDE_CONFIG_OFFSET / core::mem::size_of::<u32>()],
            0x3
        );
        assert_eq!(
            regs[LAPIC_LVT_TIMER_OFFSET / core::mem::size_of::<u32>()],
            LAPIC_TIMER_VECTOR
        );
        assert_eq!(
            regs[LAPIC_TIMER_INITIAL_COUNT_OFFSET / core::mem::size_of::<u32>()],
            42
        );
    }

    #[test]
    fn init_lapic_programs_spurious_vector_enable_bit() {
        let mut regs = [0u32; 512];
        init_lapic_mmio_base(regs.as_mut_ptr() as usize);
        assert_eq!(
            regs[LAPIC_SVR_OFFSET / core::mem::size_of::<u32>()],
            LAPIC_SVR_ENABLE | LAPIC_SPURIOUS_VECTOR
        );
    }

    #[test]
    fn init_lapic_programs_bootstrap_timer_deadline() {
        let mut regs = [0u32; 512];
        init_lapic_mmio_base(regs.as_mut_ptr() as usize);
        let expected = crate::arch::x86_64::platform_layout::BOOTSTRAP_TIMER_DEADLINE_TICKS
            .clamp(1, u32::MAX as u64) as u32;
        assert_eq!(
            regs[LAPIC_TIMER_INITIAL_COUNT_OFFSET / core::mem::size_of::<u32>()],
            expected
        );
    }
}
