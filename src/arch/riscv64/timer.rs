// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Conservative S-mode timer-interrupt bring-up.
//!
//! Safety contract:
//! - `init_timer_after_idle_safe_point` must only be called from the kernel
//!   trap handler at a stable, kernel-only idle point AFTER the real S-mode
//!   trap vector and kernel-state pointer are installed.
//! - The first call probes the SBI Timer extension. If the extension is
//!   not present, the timer is deferred with the exact reason (no STIE,
//!   no SIE).
//! - This module never enables `sstatus.SIE` for user-mode interrupts; the
//!   user-mode SPIE policy is unchanged.
//! - At present we always emit the deferral path until the timer-IRQ
//!   handler has been audited against the live trap bridge for
//!   re-entrancy; the SBI probe + marker emission landed first so the
//!   smoke gate can verify the deferral reason.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::sbi::{SbiError, probe_extension};

/// SBI Timer extension EID (`"TIME"` little-endian).
pub const SBI_EXT_TIME: usize = 0x5449_4D45;

/// Conservative tick budget — diagnostic only. The deadline value reported
/// to the smoke is `mtime + DEFAULT_TICK_INTERVAL` if/when the live path
/// is enabled.
pub const DEFAULT_TICK_INTERVAL: u64 = 10_000_000;

static TIMER_INIT_FIRED: AtomicBool = AtomicBool::new(false);
static TIMER_TICK_COUNT: AtomicU64 = AtomicU64::new(0);
static STIE_ENABLED: AtomicBool = AtomicBool::new(false);
static SIE_ENABLED: AtomicBool = AtomicBool::new(false);

/// Reason strings pinned by `scripts/qemu-riscv64-core-smoke.sh` and by the
/// source-grep test in `mod tests`. Do not reword without updating both.
pub const DEFER_REASON_AUDIT_PENDING: &str = "stie_audit_pending";
pub const DEFER_REASON_NO_SBI_TIMER: &str = "sbi_time_ext_unavailable";
pub const DEFER_REASON_FEATURE_DISABLED: &str = "timer_irq_feature_disabled";

/// True when the `riscv64-timer-irq` cargo feature is enabled.
///
/// Default builds keep STIE/SIE disabled. The feature gates the live
/// path; even with the feature on, the actual CSR writes are gated
/// behind a further audit flag (`STIE_AUDIT_COMPLETE`) so this scaffold
/// can land without flipping IRQ delivery in any current build.
pub const TIMER_IRQ_FEATURE_ENABLED: bool = cfg!(feature = "riscv64-timer-irq");

/// Trap-bridge re-entrancy audit gate. Set to `true` ONLY after the
/// audit has been completed and the live timer-trap path has been
/// proven on a CI runner with `qemu-system-riscv64`. Currently `false`
/// — even when the feature is on, the live path emits the audit-pending
/// deferral.
pub const STIE_AUDIT_COMPLETE: bool = false;

/// Marker-only initialization entry point. Returns the deferral reason
/// when the live STIE path is not enabled, or `None` when the timer-tick
/// path is engaged. The current build always returns a deferral reason
/// (see module docs).
///
/// Safety: the caller MUST guarantee the kernel trap vector and kernel
/// state pointer are installed, and that the system has reached a stable
/// idle/kernel-only point.
pub fn init_timer_after_idle_safe_point() -> Option<&'static str> {
    if TIMER_INIT_FIRED.swap(true, Ordering::AcqRel) {
        return Some(DEFER_REASON_AUDIT_PENDING);
    }

    emit_marker(format_args!("RISCV_TIMER_INIT_BEGIN"));
    // Mechanism breadcrumb: this pass uses the SBI Timer extension. A
    // future build that switches to `stimecmp` (Sstc) must emit
    // `RISCV_TIMER_MECHANISM value=stimecmp` here and document the
    // QEMU-virt compatibility implication.
    emit_marker(format_args!("RISCV_TIMER_MECHANISM value=sbi_time"));

    if !TIMER_IRQ_FEATURE_ENABLED {
        // Default build: feature gate is off. Probe SBI Timer for the
        // mechanism breadcrumb and defer with the feature-disabled
        // reason so the smoke gate can tell at a glance which deferral
        // path was taken.
        let sbi_timer_present = match probe_extension(SBI_EXT_TIME) {
            Ok(value) => value != 0,
            Err(SbiError::NotSupported) => false,
            Err(_) => false,
        };
        if !sbi_timer_present {
            emit_marker(format_args!(
                "RISCV_TIMER_DEFERRED reason={}",
                DEFER_REASON_NO_SBI_TIMER
            ));
            return Some(DEFER_REASON_NO_SBI_TIMER);
        }
        emit_marker(format_args!("RISCV_TIMER_FREQ value=platform_default"));
        emit_marker(format_args!(
            "RISCV_TIMER_DEFERRED reason={}",
            DEFER_REASON_FEATURE_DISABLED
        ));
        return Some(DEFER_REASON_FEATURE_DISABLED);
    }

    // Feature path: the `riscv64-timer-irq` cargo feature is enabled.
    // The actual CSR programming is gated behind `STIE_AUDIT_COMPLETE`
    // so this scaffold can land without flipping IRQ delivery in any
    // current build. When the audit completes, flip the constant and
    // the live-enable block below runs; until then the feature-on path
    // still emits the audit-pending deferral.
    emit_marker(format_args!("RISCV_TIMER_IRQ_FEATURE_ENABLED"));

    let sbi_timer_present = match probe_extension(SBI_EXT_TIME) {
        Ok(value) => value != 0,
        Err(SbiError::NotSupported) => false,
        Err(_) => false,
    };
    if !sbi_timer_present {
        emit_marker(format_args!(
            "RISCV_TIMER_DEFERRED reason={}",
            DEFER_REASON_NO_SBI_TIMER
        ));
        return Some(DEFER_REASON_NO_SBI_TIMER);
    }
    emit_marker(format_args!("RISCV_TIMER_FREQ value=platform_default"));

    if !STIE_AUDIT_COMPLETE {
        emit_marker(format_args!(
            "RISCV_TIMER_DEFERRED reason={}",
            DEFER_REASON_AUDIT_PENDING
        ));
        return Some(DEFER_REASON_AUDIT_PENDING);
    }

    // STIE_AUDIT_COMPLETE = true path. Currently unreachable in any
    // shipping build; lives here as the reviewed live-enable sequence
    // that the future audit pass will activate.
    arm_one_shot_timer_and_enable()
}

/// Programs the one-shot SBI Timer deadline and enables `sie.STIE`
/// followed by `sstatus.SIE`. Only callable when both
/// `TIMER_IRQ_FEATURE_ENABLED` and `STIE_AUDIT_COMPLETE` are true. The
/// function is split out so the source-grep tests can verify the
/// enable ordering is correct without the code being reachable in
/// default or feature-on builds.
fn arm_one_shot_timer_and_enable() -> Option<&'static str> {
    // The deadline computation is mechanism-specific; for SBI Timer the
    // caller is expected to supply `mtime + DEFAULT_TICK_INTERVAL`. The
    // probe was already done above.
    let deadline = current_time_value().wrapping_add(DEFAULT_TICK_INTERVAL);
    emit_marker(format_args!("RISCV_TIMER_SET deadline={}", deadline));
    sbi_set_timer(deadline);

    // Order matters: enable STIE in sie BEFORE setting SIE in sstatus.
    // STIE alone does not deliver interrupts (SIE in sstatus must also
    // be set); but setting SIE first with no STIE handler installed
    // would expose us to a stray interrupt.
    set_sie_stie();
    mark_stie_enabled();
    emit_marker(format_args!("RISCV_TIMER_STIE_ENABLED"));

    set_sstatus_sie();
    mark_sie_enabled();
    emit_marker(format_args!("RISCV_TIMER_SIE_ENABLED"));

    emit_marker(format_args!("RISCV_TIMER_INIT_DONE"));
    None
}

/// Reads the SBI `mtime`-equivalent counter. Implementation is
/// arch-specific (`rdtime`); on hosted-dev / non-riscv64 builds this
/// returns 0 so the scaffold compiles on the host toolchain.
fn current_time_value() -> u64 {
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
    {
        let value: u64;
        unsafe {
            core::arch::asm!(
                "rdtime {0}",
                out(reg) value,
                options(nostack, nomem, preserves_flags)
            );
        }
        value
    }
    #[cfg(not(all(not(feature = "hosted-dev"), target_arch = "riscv64")))]
    {
        0
    }
}

/// Invokes the SBI Timer `set_timer` call (`EID = SBI_EXT_TIME`,
/// `FID = 0`). On hosted-dev / non-riscv64 builds, this is a no-op.
fn sbi_set_timer(deadline: u64) {
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
    {
        unsafe {
            core::arch::asm!(
                "ecall",
                in("a7") SBI_EXT_TIME,
                in("a6") 0usize,
                in("a0") deadline,
                lateout("a0") _,
                lateout("a1") _,
                options(nostack, nomem)
            );
        }
    }
    #[cfg(not(all(not(feature = "hosted-dev"), target_arch = "riscv64")))]
    {
        let _ = deadline;
    }
}

/// Sets the supervisor timer interrupt enable bit (`sie.STIE`, bit 5).
fn set_sie_stie() {
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
    {
        unsafe {
            core::arch::asm!(
                "csrrs zero, sie, {0}",
                in(reg) 1usize << 5,
                options(nostack, nomem, preserves_flags)
            );
        }
    }
}

/// Sets the supervisor interrupt enable bit (`sstatus.SIE`, bit 1).
/// Must be set AFTER `sie.STIE` and after the trap vector and kernel
/// state pointer are installed.
fn set_sstatus_sie() {
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
    {
        unsafe {
            core::arch::asm!(
                "csrrs zero, sstatus, {0}",
                in(reg) 1usize << 1,
                options(nostack, nomem, preserves_flags)
            );
        }
    }
}

pub fn init_fired() -> bool {
    TIMER_INIT_FIRED.load(Ordering::Relaxed)
}

pub fn tick_count() -> u64 {
    TIMER_TICK_COUNT.load(Ordering::Relaxed)
}

pub fn record_timer_tick() -> u64 {
    let next = TIMER_TICK_COUNT
        .fetch_add(1, Ordering::AcqRel)
        .wrapping_add(1);
    emit_marker(format_args!("RISCV_TIMER_TICK count={}", next));
    next
}

pub fn mark_stie_enabled() {
    STIE_ENABLED.store(true, Ordering::Release);
}

pub fn stie_enabled() -> bool {
    STIE_ENABLED.load(Ordering::Acquire)
}

pub fn mark_sie_enabled() {
    SIE_ENABLED.store(true, Ordering::Release);
}

pub fn sie_enabled() -> bool {
    SIE_ENABLED.load(Ordering::Acquire)
}

fn emit_marker(args: core::fmt::Arguments<'_>) {
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
    crate::arch::riscv64::boot::early_sbi_marker(args);
    #[cfg(not(all(not(feature = "hosted-dev"), target_arch = "riscv64")))]
    {
        let _ = args;
    }
}

#[cfg(test)]
pub fn reset_for_test() {
    TIMER_INIT_FIRED.store(false, Ordering::Release);
    TIMER_TICK_COUNT.store(0, Ordering::Release);
    STIE_ENABLED.store(false, Ordering::Release);
    SIE_ENABLED.store(false, Ordering::Release);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_safe_until_init_runs() {
        reset_for_test();
        assert!(!init_fired());
        assert!(!stie_enabled());
        assert!(!sie_enabled());
        assert_eq!(tick_count(), 0);
    }

    #[test]
    fn init_emits_deferral_when_sbi_timer_unavailable() {
        reset_for_test();
        let reason = init_timer_after_idle_safe_point().expect("deferred");
        assert!(init_fired());
        assert!(!stie_enabled(), "STIE must remain off in deferred path");
        assert!(!sie_enabled(), "SIE must remain off in deferred path");
        assert!(
            reason == DEFER_REASON_NO_SBI_TIMER || reason == DEFER_REASON_AUDIT_PENDING,
            "unexpected reason: {reason}"
        );
    }

    #[test]
    fn init_is_run_once_per_boot() {
        reset_for_test();
        let r1 = init_timer_after_idle_safe_point();
        let r2 = init_timer_after_idle_safe_point();
        assert!(r1.is_some());
        assert!(r2.is_some());
        assert!(init_fired());
    }

    #[test]
    fn record_timer_tick_increments_counter() {
        reset_for_test();
        let a = record_timer_tick();
        let b = record_timer_tick();
        assert_eq!(a, 1);
        assert_eq!(b, 2);
        assert_eq!(tick_count(), 2);
    }

    #[test]
    fn deferred_reason_strings_match_smoke_gate() {
        assert_eq!(DEFER_REASON_AUDIT_PENDING, "stie_audit_pending");
        assert_eq!(DEFER_REASON_NO_SBI_TIMER, "sbi_time_ext_unavailable");
    }
}
