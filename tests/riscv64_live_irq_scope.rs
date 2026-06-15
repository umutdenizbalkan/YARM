// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Source-grep scope tests for the RISC-V64 live-IRQ pass.
//!
//! Pins the new contracts:
//! - boot-hart-id captured-and-stored marker
//! - timer-mechanism breadcrumb
//! - DTB-driven PLIC base (`source=dtb` or `qemu_virt_fallback`)
//! - PLIC context marker carrying boot hart + mode=s
//! - per-source enumeration markers
//! - external-IRQ select-then-defer pair (no source enabled blindly)
//! - multi-hart topology-deferred breadcrumb

#[test]
fn primary_entry_emits_boot_hart_id_stored_marker() {
    let boot = include_str!("../src/arch/riscv64/boot.rs");
    assert!(
        boot.contains("RISCV_BOOT_HART_ID_STORED hart="),
        "yarm_riscv64_primary_entry must confirm the captured boot-hart id"
    );
}

#[test]
fn timer_module_emits_mechanism_breadcrumb() {
    let timer = include_str!("../src/arch/riscv64/timer.rs");
    assert!(
        timer.contains("RISCV_TIMER_MECHANISM value=sbi_time"),
        "timer init must emit RISCV_TIMER_MECHANISM value=sbi_time"
    );
}

#[test]
fn plic_base_marker_records_source_origin() {
    let plic = include_str!("../src/arch/riscv64/plic.rs");
    assert!(
        plic.contains("RISCV_PLIC_BASE value=0x{:x} source={}"),
        "RISCV_PLIC_BASE must record both the address and the discovery source"
    );
    assert!(
        plic.contains("\"dtb\""),
        "PLIC base discovery must report source=dtb when FDT lookup succeeds"
    );
    assert!(
        plic.contains("\"qemu_virt_fallback\""),
        "PLIC base discovery must fall back to qemu_virt_fallback with explicit marker"
    );
}

#[test]
fn plic_context_marker_carries_hart_and_mode() {
    let plic = include_str!("../src/arch/riscv64/plic.rs");
    assert!(
        plic.contains("RISCV_PLIC_CONTEXT value={} hart={} mode=s"),
        "RISCV_PLIC_CONTEXT must include hart and mode=s"
    );
}

#[test]
fn plic_enumerates_qemu_virt_known_sources() {
    let plic = include_str!("../src/arch/riscv64/plic.rs");
    assert!(
        plic.contains("RISCV_PLIC_SOURCE id="),
        "per-source enumeration marker must be emitted"
    );
    assert!(
        plic.contains("name=virtio_mmio"),
        "virtio_mmio sources must be enumerated"
    );
    assert!(
        plic.contains("name=uart0"),
        "uart0 source must be enumerated"
    );
    assert!(
        plic.contains("QEMU_VIRT_UART0_SOURCE_ID: u16 = 10"),
        "UART0 source id constant must be pinned"
    );
}

#[test]
fn plic_does_not_enable_any_source_in_this_pass() {
    let plic = include_str!("../src/arch/riscv64/plic.rs");
    // External-IRQ enable must be a select-then-defer pair: a candidate
    // source is named (for diagnostic transparency), and the deferral
    // reason is recorded immediately afterwards.
    assert!(
        plic.contains("RISCV_EXTIRQ_SELECT source="),
        "PLIC must emit RISCV_EXTIRQ_SELECT before deferring"
    );
    assert!(
        plic.contains("RISCV_EXTIRQ_DEFERRED reason="),
        "PLIC must defer external-IRQ with explicit reason"
    );
    // Pin the absence of a wholesale enable: if any source were enabled
    // we'd see a write to the PLIC source-enable register, which would
    // increment EXTIRQ_ENABLED_SOURCES from this module.
    assert!(
        !plic.contains("EXTIRQ_ENABLED_SOURCES.fetch_add"),
        "no PLIC source may be enabled in this pass"
    );
    assert!(
        !plic.contains("EXTIRQ_ENABLED_SOURCES.store(1"),
        "no PLIC source may be enabled in this pass"
    );
}

#[test]
fn plic_resolve_base_prefers_dtb_over_fallback() {
    let plic = include_str!("../src/arch/riscv64/plic.rs");
    let dtb_pos = plic
        .find("crate::arch::fdt::find_node_reg_by_name_prefix")
        .expect("PLIC must consult DTB first");
    let fallback_pos = plic
        .find("platform_layout::PLIC_MMIO_BASE")
        .expect("PLIC must keep the platform-layout fallback");
    assert!(
        dtb_pos < fallback_pos,
        "PLIC must consult DTB before the platform-layout fallback"
    );
}

#[test]
fn plic_threshold_write_targets_correct_context_register() {
    let plic = include_str!("../src/arch/riscv64/plic.rs");
    // The threshold register lives at base + 0x20_0000 + context * 0x1000.
    assert!(
        plic.contains("PLIC_CONTEXT_BASE_OFFSET: usize = 0x0020_0000"),
        "threshold offset must match the QEMU virt + sifive,plic-1.0.0 layout"
    );
    assert!(
        plic.contains("PLIC_CONTEXT_STRIDE: usize = 0x1000"),
        "threshold stride must be 4 KiB"
    );
    assert!(
        plic.contains("write_volatile(threshold_addr as *mut u32, value)"),
        "threshold write must be volatile"
    );
}

#[test]
fn captured_dtb_state_is_present_in_boot() {
    let boot = include_str!("../src/arch/riscv64/boot.rs");
    assert!(
        boot.contains("CAPTURED_DTB_PTR"),
        "boot.rs must expose a captured DTB pointer slot"
    );
    assert!(
        boot.contains("pub fn captured_dtb() -> Option<&'static [u8]>"),
        "boot.rs must expose a captured_dtb accessor"
    );
    assert!(
        boot.contains("save_dtb_for_late_consumers(dtb);"),
        "prepare_arch_boot must save the DTB for late consumers"
    );
}

#[test]
fn fdt_module_exposes_find_node_reg_by_name_prefix() {
    let fdt = include_str!("../src/arch/fdt.rs");
    assert!(
        fdt.contains("pub fn find_node_reg_by_name_prefix"),
        "fdt module must expose find_node_reg_by_name_prefix"
    );
}

#[test]
fn multi_hart_topology_emits_irq_deferred_marker() {
    let boot = include_str!("../src/arch/riscv64/boot.rs");
    assert!(
        boot.contains("RISCV_IRQ_SMP_TOPOLOGY_DEFERRED reason=present_topology_not_live_validated"),
        "multi-hart boots must record the live-IRQ topology deferral"
    );
    assert!(
        boot.contains("if bitmap.count_ones() > 1"),
        "topology-deferred marker must be gated on present_cpus > 1"
    );
}

#[test]
fn smoke_script_requires_new_live_irq_markers() {
    let smoke = include_str!("../scripts/qemu-riscv64-core-smoke.sh");
    for marker in [
        "RISCV_BOOT_HART_ID_STORED hart=",
        "RISCV_TIMER_MECHANISM value=",
        "RISCV_PLIC_BASE value=",
        "RISCV_PLIC_CONTEXT value=",
    ] {
        assert!(
            smoke.contains(marker),
            "smoke must require marker: {marker}"
        );
    }
    assert!(
        smoke
            .contains("RISCV_IRQ_SMP_TOPOLOGY_DEFERRED reason=present_topology_not_live_validated"),
        "smoke must check the multi-hart topology-deferred marker"
    );
}

#[test]
fn timer_keeps_stie_and_sie_off_by_default() {
    let timer = include_str!("../src/arch/riscv64/timer.rs");
    // Pin the absence of any unconditional CSR set of sie/sstatus from
    // the timer module. The live STIE/SIE enable path is gated until the
    // trap-bridge re-entrancy audit lands; the test fails if the gate is
    // bypassed by a regression.
    assert!(
        !timer.contains("csrs sie"),
        "timer module must not unconditionally set sie.STIE"
    );
    assert!(
        !timer.contains("csrs sstatus"),
        "timer module must not unconditionally set sstatus.SIE"
    );
}
