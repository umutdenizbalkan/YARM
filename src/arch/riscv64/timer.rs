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
        DEFER_REASON_AUDIT_PENDING
    ));
    Some(DEFER_REASON_AUDIT_PENDING)
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
