// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Source-grep scope tests for RISC-V64 SMP topology + boot-hart selection.
//!
//! These tests pin the contract introduced for nonzero boot-hart handling
//! and per-N topology reporting on QEMU virt + OpenSBI.

#[test]
fn start_does_not_hardcode_hart_zero_as_boot_hart() {
    let boot = include_str!("../src/arch/riscv64/boot.rs");
    // The previous bug: `li t1, 0` + `bne s0, t1, .Lriscv64_secondary_cold_park`
    // unconditionally parked any hart whose id != 0, breaking nonzero boot
    // hart selection. Pin the absence of that literal.
    assert!(
        !boot.contains("li t1, 0                        // BOOTSTRAP_CPU_ID"),
        "_start must not hardcode hart 0 as the boot hart"
    );
    assert!(
        !boot.contains("bne s0, t1, .Lriscv64_secondary_cold_park"),
        "_start must not branch to secondary cold-park on hart != 0"
    );
}

#[test]
fn start_uses_atomic_cas_for_boot_hart_arrival() {
    let boot = include_str!("../src/arch/riscv64/boot.rs");
    assert!(
        boot.contains("amoswap.w.aq"),
        "_start must use an atomic swap to claim the boot-hart slot"
    );
    assert!(
        boot.contains("RISCV64_BOOT_HART_ARRIVAL"),
        "_start must reference the boot-hart arrival slot"
    );
    assert!(
        boot.contains("RISCV64_BOOT_HART_ID"),
        "_start must store the OpenSBI boot-hart id"
    );
}

#[test]
fn boot_hart_id_is_read_from_opensbi_a0() {
    let boot = include_str!("../src/arch/riscv64/boot.rs");
    assert!(
        boot.contains("pub fn boot_hart_id() -> usize"),
        "boot_hart_id accessor must exist"
    );
    assert!(
        boot.contains("let boot_hart = boot_hart_id();"),
        "park-secondaries must use the OpenSBI-reported boot-hart id"
    );
}

#[test]
fn park_secondaries_no_longer_uses_bootstrap_cpu_id_constant() {
    let boot = include_str!("../src/arch/riscv64/boot.rs");
    // The legacy assignment used the hardcoded constant; the new path reads
    // the real id via `boot_hart_id()`. Pin the absence of the old code
    // form so a regression cannot silently revert.
    assert!(
        !boot.contains(
            "let boot_hart = crate::arch::platform_constants::BOOTSTRAP_CPU_ID as usize;"
        ),
        "park-secondaries must not derive boot_hart from BOOTSTRAP_CPU_ID constant"
    );
}

#[test]
fn early_marker_takes_uart_line_lock_to_serialize_smp_output() {
    let boot = include_str!("../src/arch/riscv64/boot.rs");
    assert!(
        boot.contains("EARLY_MARKER_LOCK"),
        "early_sbi_marker must serialize multi-hart UART output"
    );
    assert!(
        boot.contains("early_marker_lock_acquire();"),
        "early_sbi_marker must acquire the UART lock per line"
    );
    assert!(
        boot.contains("early_marker_lock_release();"),
        "early_sbi_marker must release the UART lock after the line"
    );
}

#[test]
fn topology_marker_emits_present_cpus_bitmap_boot_hart() {
    let boot = include_str!("../src/arch/riscv64/boot.rs");
    assert!(
        boot.contains("RISCV_HART_PRESENT hart="),
        "per-hart present marker must be emitted"
    );
    assert!(
        boot.contains("RISCV_HART_TOPOLOGY present_cpus="),
        "RISCV_HART_TOPOLOGY marker missing"
    );
    assert!(
        boot.contains("present_bitmap=0x"),
        "RISCV_HART_TOPOLOGY must include present_bitmap=0x..."
    );
    assert!(
        boot.contains("boot_hart="),
        "RISCV_HART_TOPOLOGY must include boot_hart=..."
    );
    assert!(
        boot.contains(
            "RISCV_SCHEDULER_BSP_ONLY online_cpus=1 reason=riscv_smp_scheduler_not_enabled"
        ),
        "RISCV_SCHEDULER_BSP_ONLY breadcrumb must be emitted verbatim"
    );
    assert!(
        boot.contains("RISCV_SECONDARY_HARTS_PARKED count="),
        "RISCV_SECONDARY_HARTS_PARKED count marker missing"
    );
}

#[test]
fn dtb_topology_is_staged_for_bootstrap() {
    let boot = include_str!("../src/arch/riscv64/boot.rs");
    assert!(
        boot.contains("stage_riscv64_present_cpu_bitmap"),
        "DTB topology must be staged before bootstrap"
    );
    assert!(
        boot.contains("stage_present_cpu_bitmap_for_bootstrap"),
        "Topology staging must go through the arch boot_entry helper"
    );
}

#[test]
fn topology_module_prefers_binary_fdt_over_text_scan() {
    let topology = include_str!("../src/arch/riscv64/topology.rs");
    assert!(
        topology.contains("crate::arch::fdt::cpus_hart_id_bitmap"),
        "topology must call the binary FDT walker first"
    );
}

#[test]
fn fdt_module_exposes_cpus_hart_id_bitmap() {
    let fdt = include_str!("../src/arch/fdt.rs");
    assert!(
        fdt.contains("pub fn cpus_hart_id_bitmap"),
        "fdt module must expose cpus_hart_id_bitmap"
    );
}

#[test]
fn smoke_script_enforces_per_n_topology() {
    let smoke = include_str!("../scripts/qemu-riscv64-core-smoke.sh");
    // Each per-N bitmap literal must appear as the case-arm value the shell
    // expands when --smp N is requested.
    for arm in [
        r#"1) expected_bitmap_hex="0x1""#,
        r#"2) expected_bitmap_hex="0x3""#,
        r#"3) expected_bitmap_hex="0x7""#,
        r#"4) expected_bitmap_hex="0xf""#,
    ] {
        assert!(
            smoke.contains(arm),
            "smoke script missing per-N case arm: {arm}"
        );
    }
    assert!(
        smoke.contains("YARM_BOOT_OK present_cpus=${QEMU_SMP} present_bitmap=${expected_bitmap_hex} online_cpus=1"),
        "smoke must assert YARM_BOOT_OK uses the staged bitmap"
    );
    assert!(
        smoke.contains("online_cpus=1"),
        "smoke must enforce online_cpus=1"
    );
    assert!(
        smoke.contains("appears in RISCV_SECONDARY_HART_PARK list"),
        "smoke must reject boot-hart appearing in secondary-park list"
    );
    assert!(
        smoke.contains(
            "RISCV_SCHEDULER_BSP_ONLY online_cpus=1 reason=riscv_smp_scheduler_not_enabled"
        ),
        "smoke must require RISCV_SCHEDULER_BSP_ONLY breadcrumb"
    );
}

#[test]
fn smoke_script_keeps_default_smp_one() {
    let smoke = include_str!("../scripts/qemu-riscv64-core-smoke.sh");
    assert!(
        smoke.contains("QEMU_SMP=${QEMU_SMP:-1}"),
        "smoke must default to -smp 1"
    );
}

#[test]
fn online_cpus_remains_one_until_riscv_smp_scheduler_lands() {
    let boot = include_str!("../src/arch/riscv64/boot.rs");
    // The scheduler-online breadcrumb explicitly records the deferral
    // reason. The string is pinned by the smoke gate.
    assert!(boot.contains("riscv_smp_scheduler_not_enabled"));
}

#[test]
fn secondary_park_marker_format_is_stable_for_smoke_match() {
    let boot = include_str!("../src/arch/riscv64/boot.rs");
    assert!(
        boot.contains("RISCV_SECONDARY_HART_PARK hart="),
        "secondary park marker format unchanged"
    );
}
