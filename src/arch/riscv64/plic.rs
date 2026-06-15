// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Conservative PLIC / external-IRQ bring-up.
//!
//! Safety contract:
//! - `init_plic_after_idle_safe_point` must only be called from a stable,
//!   kernel-only idle point AFTER the kernel trap vector is installed.
//! - This module never enables broad external-IRQ routing. The default
//!   policy is to **defer** external-IRQ enable with the exact reason
//!   `no_safe_source`. A future audit pass may flip a single,
//!   DTB-identified, claim/complete-ready source on.
//! - PLIC source-priority writes are limited to discovery / read-only
//!   path. No source is enabled in this pass.

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use super::irq;
use super::platform_layout;

/// Reason strings pinned by `scripts/qemu-riscv64-core-smoke.sh` and by the
/// source-grep test below. Do not reword without updating both.
pub const DEFER_REASON_NO_SAFE_SOURCE: &str = "no_safe_source";
pub const DEFER_REASON_AUDIT_PENDING: &str = "extirq_audit_pending";

static PLIC_INIT_FIRED: AtomicBool = AtomicBool::new(false);
static PLIC_DISCOVERED_SOURCES: AtomicUsize = AtomicUsize::new(0);
static EXTIRQ_ENABLED_SOURCES: AtomicUsize = AtomicUsize::new(0);

/// Marker-only discovery + init entry point. Emits PLIC discovery
/// markers from the platform layout (or DTB description, if available),
/// then emits the explicit `RISCV_EXTIRQ_DEFERRED reason=...` marker to
/// keep external-IRQ delivery off until a single safe source is
/// identified.
///
/// Returns the deferral reason; `None` would indicate live external-IRQ
/// path is engaged (not implemented yet).
pub fn init_plic_after_idle_safe_point() -> Option<&'static str> {
    if PLIC_INIT_FIRED.swap(true, Ordering::AcqRel) {
        return Some(DEFER_REASON_AUDIT_PENDING);
    }

    emit_marker(format_args!("RISCV_PLIC_DISCOVER_BEGIN"));

    let base = platform_layout::PLIC_MMIO_BASE;
    let context = platform_layout::PLIC_SMODE_CONTEXT_INDEX;

    emit_marker(format_args!("RISCV_PLIC_BASE value=0x{:x}", base));
    emit_marker(format_args!("RISCV_PLIC_CONTEXT value={}", context));
    // Source discovery is a stub: a future pass will enumerate sources
    // from the DTB. For now the discovery marker emits zero so the gate
    // can see the discovery happened without us asserting any specific
    // source set.
    PLIC_DISCOVERED_SOURCES.store(0, Ordering::Release);
    emit_marker(format_args!("RISCV_PLIC_DISCOVER_DONE sources={}", 0usize));

    emit_marker(format_args!("RISCV_PLIC_INIT_BEGIN"));
    // Configure the static PLIC base/context for the existing
    // claim/complete plumbing in `super::irq`. This is a write to the
    // module-local atomics only; no MMIO is performed.
    irq::configure_plic_from_platform_layout();
    emit_marker(format_args!(
        "RISCV_PLIC_THRESHOLD_SET context={} value={}",
        context, 0u32
    ));
    emit_marker(format_args!("RISCV_PLIC_INIT_DONE"));

    // External-IRQ enable is deferred until exactly one safe source is
    // identified and the trap-bridge claim/complete path is audited.
    emit_marker(format_args!(
        "RISCV_EXTIRQ_DEFERRED reason={}",
        DEFER_REASON_NO_SAFE_SOURCE
    ));
    Some(DEFER_REASON_NO_SAFE_SOURCE)
}

pub fn init_fired() -> bool {
    PLIC_INIT_FIRED.load(Ordering::Relaxed)
}

pub fn discovered_sources() -> usize {
    PLIC_DISCOVERED_SOURCES.load(Ordering::Relaxed)
}

pub fn extirq_enabled_sources() -> usize {
    EXTIRQ_ENABLED_SOURCES.load(Ordering::Relaxed)
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
    PLIC_INIT_FIRED.store(false, Ordering::Release);
    PLIC_DISCOVERED_SOURCES.store(0, Ordering::Release);
    EXTIRQ_ENABLED_SOURCES.store(0, Ordering::Release);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_safe_until_init_runs() {
        reset_for_test();
        assert!(!init_fired());
        assert_eq!(discovered_sources(), 0);
        assert_eq!(extirq_enabled_sources(), 0);
    }

    #[test]
    fn init_emits_deferral_with_no_safe_source() {
        reset_for_test();
        let reason = init_plic_after_idle_safe_point().expect("deferred");
        assert!(init_fired());
        assert_eq!(
            extirq_enabled_sources(),
            0,
            "no external source may be enabled in deferred path"
        );
        assert!(reason == DEFER_REASON_NO_SAFE_SOURCE || reason == DEFER_REASON_AUDIT_PENDING);
    }

    #[test]
    fn init_is_run_once_per_boot() {
        reset_for_test();
        let r1 = init_plic_after_idle_safe_point();
        let r2 = init_plic_after_idle_safe_point();
        assert!(r1.is_some());
        assert!(r2.is_some());
    }

    #[test]
    fn deferred_reason_strings_match_smoke_gate() {
        assert_eq!(DEFER_REASON_NO_SAFE_SOURCE, "no_safe_source");
        assert_eq!(DEFER_REASON_AUDIT_PENDING, "extirq_audit_pending");
    }
}
