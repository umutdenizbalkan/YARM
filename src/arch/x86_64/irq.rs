// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

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
    lapic_write_u32(
        base,
        LAPIC_SVR_OFFSET,
        LAPIC_SVR_ENABLE | LAPIC_SPURIOUS_VECTOR,
    );
    // BT2: do NOT arm the LAPIC timer here. The timer is armed explicitly by
    // start_bsp_periodic_timer() after signal_bootstrap_scheduler_ready(), so
    // no timer ISR can race with borrow_kernel_for_boot()'s raw &mut alias
    // during bootstrap ELF loading (which takes >800ms in QEMU).
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

// ── Hosted-test LAPIC ownership ────────────────────────────────────────────────
//
// `LAPIC_MMIO_BASE` / `LAPIC_CONFIGURED` are PROCESS-global, and `acknowledge_interrupt`
// and `program_timer_deadline` write four bytes through that base on every timer tick and
// every acknowledged IRQ. On real hardware the base is a physical MMIO page that outlives
// everything. In a hosted test there is no LAPIC, so whatever a test publishes as the base
// is ordinary memory owned by that test — and once the test returns, EVERY later test in
// the process keeps writing through the stale pointer.
//
// That is a genuine use-after-free across tests: with the base left pointing at a returned
// test's stack frame, an unrelated test's `handle_trap` would write into memory the
// allocator had since handed to someone else, producing "double free or corruption",
// "free(): invalid pointer" and SIGSEGV under a parallel `cargo test`.
//
// Two rules close it, and `LapicTestConfig` enforces both:
//   1. The global base may only ever point at `TEST_LAPIC_PAGE`, a process-static stand-in
//      for the MMIO page. It is never freed, so a stray write can never land in memory
//      owned by someone else.
//   2. A test that configures the LAPIC restores the previous configuration when it ends,
//      including on panic. Outside that window `LAPIC_CONFIGURED` is false, so unrelated
//      tests take the early return and write nothing at all.
// Tests holding the guard are serialized against each other, since they share the page.

/// Process-static stand-in for the LAPIC MMIO page used by hosted tests.
/// `AtomicU32` cells so a concurrent stray write and a readback are not a data race.
#[cfg(test)]
static TEST_LAPIC_PAGE: [core::sync::atomic::AtomicU32; 1024] =
    [const { core::sync::atomic::AtomicU32::new(0) }; 1024];

#[cfg(test)]
static TEST_LAPIC_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Address of the simulated MMIO page. Callers that cannot hold a `LapicTestConfig` (a
/// bare `fn` provider, for instance) use this; the owning test still holds the guard.
#[cfg(test)]
pub(crate) fn test_lapic_page_base() -> usize {
    TEST_LAPIC_PAGE.as_ptr() as usize
}

/// Exclusive, panic-safe ownership of the process-global LAPIC configuration for one test.
#[cfg(test)]
pub(crate) struct LapicTestConfig {
    prev_base: usize,
    prev_configured: bool,
    // Declared last so it is released last: the configuration is restored first.
    _lock: std::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
impl LapicTestConfig {
    /// Take the LAPIC for this test, with the simulated page zeroed and unconfigured.
    pub(crate) fn acquire() -> Self {
        // Poison-tolerant: a panicking test still hands the LAPIC on rather than
        // cascading into unrelated failures.
        let lock = TEST_LAPIC_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let this = Self {
            prev_base: LAPIC_MMIO_BASE.load(Ordering::Relaxed),
            prev_configured: LAPIC_CONFIGURED.load(Ordering::Relaxed),
            _lock: lock,
        };
        for cell in TEST_LAPIC_PAGE.iter() {
            cell.store(0, Ordering::Relaxed);
        }
        LAPIC_CONFIGURED.store(false, Ordering::Relaxed);
        LAPIC_MMIO_BASE.store(0, Ordering::Relaxed);
        this
    }

    /// The address of the simulated MMIO page — the ONLY value a test may publish as the
    /// global LAPIC base.
    pub(crate) fn base(&self) -> usize {
        test_lapic_page_base()
    }

    /// Read one simulated register back by byte offset.
    pub(crate) fn reg(&self, byte_offset: usize) -> u32 {
        TEST_LAPIC_PAGE[byte_offset / core::mem::size_of::<u32>()].load(Ordering::Relaxed)
    }
}

#[cfg(test)]
impl Drop for LapicTestConfig {
    fn drop(&mut self) {
        LAPIC_MMIO_BASE.store(self.prev_base, Ordering::Relaxed);
        LAPIC_CONFIGURED.store(self.prev_configured, Ordering::Relaxed);
    }
}

#[cfg(any(test, not(feature = "hosted-dev")))]
fn lapic_write_eoi(base: usize) {
    lapic_write_u32(base, LAPIC_EOI_OFFSET, 0);
}

/// Real MMIO store — a volatile write to the physical LAPIC page.
#[cfg(all(not(test), not(feature = "hosted-dev")))]
fn lapic_write_u32(base: usize, offset: usize, value: u32) {
    unsafe {
        write_volatile((base + offset) as *mut u32, value);
    }
}

/// Simulated MMIO store. In hosted tests every `base` addresses `AtomicU32` cells — the
/// `LapicTestConfig` page for the global path, and an `AtomicU32` buffer for the tests that
/// call the helpers directly — so the store is atomic and a concurrent readback from
/// another test is not a data race.
#[cfg(test)]
fn lapic_write_u32(base: usize, offset: usize, value: u32) {
    let cell = (base + offset) as *const core::sync::atomic::AtomicU32;
    // SAFETY: `base + offset` is inside a live `[AtomicU32; N]` owned by the caller; see
    // the ownership rules on `LapicTestConfig`.
    unsafe { (*cell).store(value, Ordering::Relaxed) };
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

#[cfg(feature = "hosted-dev")]
pub fn enable_interrupts_for_boot() {}

#[cfg(not(feature = "hosted-dev"))]
pub fn enable_interrupts_for_boot() {
    unsafe {
        core::arch::asm!("sti", options(nomem, preserves_flags));
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

    use core::sync::atomic::AtomicU32;

    #[test]
    fn lapic_eoi_write_targets_expected_register() {
        // A direct helper call: the base is never published to the global, so a local
        // buffer is safe. It is `AtomicU32` because that is what the simulated store
        // writes through.
        let regs = [const { AtomicU32::new(0xdead_beef) }; 64];
        lapic_write_eoi(regs.as_ptr() as usize);
        assert_eq!(
            regs[LAPIC_EOI_OFFSET / core::mem::size_of::<u32>()].load(Ordering::Relaxed),
            0
        );
    }

    #[test]
    fn init_lapic_marks_controller_configured() {
        let lapic = LapicTestConfig::acquire();
        init_lapic_mmio_base(lapic.base());
        assert!(LAPIC_CONFIGURED.load(Ordering::Relaxed));
        assert_eq!(LAPIC_MMIO_BASE.load(Ordering::Relaxed), lapic.base());
    }

    #[test]
    fn lapic_configuration_parses_description() {
        let lapic = LapicTestConfig::acquire();
        let description = crate::std::format!("lapic_mmio_base=0x{:x}", lapic.base());
        assert!(try_configure_lapic_from_description(description.as_bytes()));
        assert!(LAPIC_CONFIGURED.load(Ordering::Relaxed));
    }

    #[test]
    fn program_timer_deadline_writes_lapic_timer_registers() {
        let lapic = LapicTestConfig::acquire();
        LAPIC_MMIO_BASE.store(lapic.base(), Ordering::Relaxed);
        LAPIC_CONFIGURED.store(true, Ordering::Relaxed);

        program_timer_deadline(crate::kernel::scheduler::CpuId(0), 42);

        assert_eq!(lapic.reg(LAPIC_TIMER_DIVIDE_CONFIG_OFFSET), 0x3);
        assert_eq!(lapic.reg(LAPIC_LVT_TIMER_OFFSET), LAPIC_TIMER_VECTOR);
        assert_eq!(lapic.reg(LAPIC_TIMER_INITIAL_COUNT_OFFSET), 42);
    }

    #[test]
    fn init_lapic_programs_spurious_vector_enable_bit() {
        let lapic = LapicTestConfig::acquire();
        init_lapic_mmio_base(lapic.base());
        assert_eq!(
            lapic.reg(LAPIC_SVR_OFFSET),
            LAPIC_SVR_ENABLE | LAPIC_SPURIOUS_VECTOR
        );
    }

    #[test]
    fn init_lapic_does_not_arm_timer_before_signal() {
        // BT2: init_lapic_mmio_base must NOT arm the LAPIC timer. The timer is
        // armed explicitly after signal_bootstrap_scheduler_ready() to prevent
        // ISR aliasing with borrow_kernel_for_boot() during bootstrap ELF load.
        let lapic = LapicTestConfig::acquire();
        init_lapic_mmio_base(lapic.base());
        assert_eq!(
            lapic.reg(LAPIC_TIMER_INITIAL_COUNT_OFFSET),
            0,
            "LAPIC timer must not be armed during init_lapic_mmio_base (BT2 invariant)"
        );
    }

    /// The regression guard for the cross-test use-after-free: after a test that
    /// configured the LAPIC has finished, the process-global configuration must be back
    /// to "unconfigured", so an unrelated test's `acknowledge_interrupt` writes nowhere.
    #[test]
    fn lapic_configuration_does_not_leak_out_of_a_test() {
        let before = (
            LAPIC_MMIO_BASE.load(Ordering::Relaxed),
            LAPIC_CONFIGURED.load(Ordering::Relaxed),
        );
        {
            let lapic = LapicTestConfig::acquire();
            init_lapic_mmio_base(lapic.base());
            assert!(LAPIC_CONFIGURED.load(Ordering::Relaxed));
        }
        assert_eq!(
            (
                LAPIC_MMIO_BASE.load(Ordering::Relaxed),
                LAPIC_CONFIGURED.load(Ordering::Relaxed),
            ),
            before,
            "a configuring test must restore the process-global LAPIC state"
        );
    }
}
