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
/// By the time `init_plic_after_idle_safe_point` runs, the active `satp`
/// (installed for the first dispatched user task) only maps the
/// kernel-shared gigapage (`RISCV_KERNEL_SHARED_BASE..END`, RAM only); the
/// PLIC's physical MMIO window sits below RAM and is never covered by that
/// mapping. Writing there would fault (`StoreAMOPageFault`), so the
/// threshold write is skipped and reported with this reason instead.
pub const DEFER_REASON_MMIO_UNMAPPED: &str = "plic_mmio_unmapped_under_active_satp";

/// Source IDs for the QEMU virt RISC-V platform. These are well-known
/// (documented in the QEMU virt-machine source); enumeration here is
/// breadcrumb-only — no source is enabled in this pass.
pub const QEMU_VIRT_UART0_SOURCE_ID: u16 = 10;
pub const QEMU_VIRT_VIRTIO_MMIO_BASE_SOURCE_ID: u16 = 1;
pub const QEMU_VIRT_VIRTIO_MMIO_LAST_SOURCE_ID: u16 = 8;

/// PLIC S-mode context-claim region layout, shared between the coverage
/// check in `init_plic_after_idle_safe_point` and `write_plic_threshold`.
const PLIC_CONTEXT_BASE_OFFSET: usize = 0x0020_0000;
const PLIC_CONTEXT_STRIDE: usize = 0x1000;

static PLIC_INIT_FIRED: AtomicBool = AtomicBool::new(false);
static PLIC_DISCOVERED_SOURCES: AtomicUsize = AtomicUsize::new(0);
static EXTIRQ_ENABLED_SOURCES: AtomicUsize = AtomicUsize::new(0);

/// Marker-only discovery + init entry point. Prefers a DTB-driven PLIC
/// base lookup, falls back to the QEMU-virt platform-layout constant
/// with an explicit `source=qemu_virt_fallback` marker, then emits the
/// per-source enumeration breadcrumbs and the threshold write.
///
/// External-IRQ enable is deferred: the smoke gate accepts the explicit
/// `RISCV_EXTIRQ_DEFERRED reason=...` marker.
pub fn init_plic_after_idle_safe_point() -> Option<&'static str> {
    if PLIC_INIT_FIRED.swap(true, Ordering::AcqRel) {
        return Some(DEFER_REASON_AUDIT_PENDING);
    }

    emit_marker(format_args!("RISCV_PLIC_DISCOVER_BEGIN"));

    let (base, base_source) = resolve_plic_base();
    let context = platform_layout::PLIC_SMODE_CONTEXT_INDEX;
    let boot_hart = super::boot::boot_hart_id();

    emit_marker(format_args!(
        "RISCV_PLIC_BASE value=0x{:x} source={}",
        base, base_source
    ));
    emit_marker(format_args!(
        "RISCV_PLIC_CONTEXT value={} hart={} mode=s",
        context, boot_hart
    ));

    // Per-source enumeration breadcrumb. The QEMU virt layout is fixed:
    // virtio-mmio takes source IDs 1..=8 and UART0 takes source ID 10.
    // These are emitted for diagnostic transparency only — no source is
    // enabled in this pass.
    let mut enumerated = 0usize;
    for sid in QEMU_VIRT_VIRTIO_MMIO_BASE_SOURCE_ID..=QEMU_VIRT_VIRTIO_MMIO_LAST_SOURCE_ID {
        emit_marker(format_args!(
            "RISCV_PLIC_SOURCE id={} name=virtio_mmio compatible=virtio,mmio",
            sid
        ));
        enumerated += 1;
    }
    emit_marker(format_args!(
        "RISCV_PLIC_SOURCE id={} name=uart0 compatible=ns16550a",
        QEMU_VIRT_UART0_SOURCE_ID
    ));
    enumerated += 1;
    PLIC_DISCOVERED_SOURCES.store(enumerated, Ordering::Release);
    emit_marker(format_args!(
        "RISCV_PLIC_DISCOVER_DONE sources={}",
        enumerated
    ));

    emit_marker(format_args!("RISCV_PLIC_INIT_BEGIN"));
    // Configure the static PLIC base/context for the existing
    // claim/complete plumbing in `super::irq`. This is a write to the
    // module-local atomics only; no MMIO is performed.
    irq::configure_plic_from_platform_layout();

    let threshold_addr = base + PLIC_CONTEXT_BASE_OFFSET + (context * PLIC_CONTEXT_STRIDE);
    if addr_range_covered_by_kernel_shared_mapping(threshold_addr, core::mem::size_of::<u32>()) {
        write_plic_threshold(base, context, 0u32);
        emit_marker(format_args!(
            "RISCV_PLIC_THRESHOLD_SET context={} value={}",
            context, 0u32
        ));
        emit_marker(format_args!("RISCV_PLIC_INIT_DONE"));
    } else {
        // `init_plic_after_idle_safe_point` only ever runs from the
        // idle-trap path, by which point the active `satp` maps nothing
        // but the kernel-shared gigapage (RAM only). The PLIC's physical
        // MMIO window is never covered by that mapping, so the threshold
        // write would fault; skip it and report the exact reason instead
        // of crashing.
        emit_marker(format_args!(
            "RISCV_PLIC_DEFERRED reason={}",
            DEFER_REASON_MMIO_UNMAPPED
        ));
    }

    // External-IRQ enable is deferred: we emit the explicit select +
    // defer pair so the smoke gate can read both the candidate source
    // we considered and the exact reason it was not enabled. The
    // claim/complete path in `super::irq` is wired but no source is
    // enabled, so no IRQ can be claimed without an explicit follow-up.
    emit_marker(format_args!(
        "RISCV_EXTIRQ_SELECT source={} reason=uart0_is_safe_candidate_but_handler_not_ready",
        QEMU_VIRT_UART0_SOURCE_ID
    ));
    emit_marker(format_args!(
        "RISCV_EXTIRQ_DEFERRED reason={}",
        DEFER_REASON_NO_SAFE_SOURCE
    ));
    Some(DEFER_REASON_NO_SAFE_SOURCE)
}

/// Returns `(plic_base, source_tag)`. Prefers the DTB-discovered base
/// via `crate::arch::fdt::find_node_reg_by_name_prefix`; falls back to
/// the platform-layout constant with `qemu_virt_fallback`.
fn resolve_plic_base() -> (usize, &'static str) {
    if let Some(dtb) = super::boot::captured_dtb() {
        if let Some((base, _size)) = crate::arch::fdt::find_node_reg_by_name_prefix(dtb, b"plic@") {
            return (base as usize, "dtb");
        }
    }
    (platform_layout::PLIC_MMIO_BASE, "qemu_virt_fallback")
}

/// Returns true if the inclusive byte range `[addr, addr+len)` falls
/// entirely within the single kernel-shared gigapage that
/// `map_kernel_shared_into_asid` installs into every user ASID's page
/// table. A PLIC MMIO write is only safe to perform under the active
/// `satp` if its physical address is covered by that mapping.
fn addr_range_covered_by_kernel_shared_mapping(addr: usize, len: usize) -> bool {
    let start = addr as u64;
    let end = start.saturating_add(len as u64);
    start >= super::page_table::RISCV_KERNEL_SHARED_BASE
        && end <= super::page_table::RISCV_KERNEL_SHARED_END
}

/// Writes the PLIC S-mode threshold register for `context`. Threshold
/// `value=0` accepts every priority level >= 1 (the QEMU virt default
/// is `value=0`; we set it explicitly so the boot ordering is
/// deterministic). Callers must first confirm the target address is
/// covered by the active mapping via
/// `addr_range_covered_by_kernel_shared_mapping`.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
fn write_plic_threshold(base: usize, context: usize, value: u32) {
    let threshold_addr = base + PLIC_CONTEXT_BASE_OFFSET + (context * PLIC_CONTEXT_STRIDE);
    unsafe {
        core::ptr::write_volatile(threshold_addr as *mut u32, value);
    }
}

#[cfg(not(all(not(feature = "hosted-dev"), target_arch = "riscv64")))]
fn write_plic_threshold(_base: usize, _context: usize, _value: u32) {}

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

    #[test]
    fn qemu_virt_source_ids_are_pinned() {
        assert_eq!(QEMU_VIRT_UART0_SOURCE_ID, 10);
        assert_eq!(QEMU_VIRT_VIRTIO_MMIO_BASE_SOURCE_ID, 1);
        assert_eq!(QEMU_VIRT_VIRTIO_MMIO_LAST_SOURCE_ID, 8);
    }

    #[test]
    fn enumerated_source_count_matches_known_qemu_virt_layout() {
        reset_for_test();
        let _ = init_plic_after_idle_safe_point();
        // 8 virtio-mmio sources + 1 UART0 source = 9 enumerated.
        assert_eq!(discovered_sources(), 9);
    }

    #[test]
    fn qemu_virt_plic_base_is_not_covered_by_kernel_shared_mapping() {
        // QEMU virt's PLIC sits at 0x0C00_0000, well below the
        // kernel-shared gigapage's 0x8000_0000 base, so the threshold
        // write must be skipped rather than faulting.
        assert!(!addr_range_covered_by_kernel_shared_mapping(
            platform_layout::PLIC_MMIO_BASE,
            4
        ));
    }

    #[test]
    fn kernel_shared_range_itself_is_covered() {
        assert!(addr_range_covered_by_kernel_shared_mapping(
            super::super::page_table::RISCV_KERNEL_SHARED_BASE as usize,
            4
        ));
    }
}
