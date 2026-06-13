// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#[test]
fn rpi5_stage1_does_not_start_rp1_pcie_or_userspace_policy() {
    let policy = include_str!("../src/arch/aarch64_boot_policy.rs");
    let boot = include_str!("../src/arch/aarch64/boot.rs");
    for forbidden in [
        "rp1_gpio",
        "SpawnV5",
        "driver_manager",
        "pcie_init",
        "rp1_pcie",
    ] {
        assert!(!policy.contains(forbidden), "policy contains {forbidden}");
    }
    assert!(boot.contains("RPI5_BOOT_KERNEL_REFUSED reason=stage1_uart_only"));
}

#[test]
fn raw_entry_marker_is_confined_to_the_rpi5_stage1_feature() {
    let boot = include_str!("../src/arch/aarch64/boot.rs");
    let marker = boot.find("RPI5_RAW_ENTRY").expect("raw entry marker");
    let feature_gate = boot[..marker]
        .rfind("feature = \"rpi5-stage1\"")
        .expect("RPi5 feature gate before raw marker");
    assert!(marker - feature_gate < 2_000);
    assert!(boot.contains("_start:\n    mov x20, x0\n    bl yarm_aarch64_raw_entry_marker"));
    assert!(boot.contains("mov x0, x20\n    bl yarm_aarch64_select_early_console"));
}

#[test]
fn existing_architecture_defaults_remain_explicit() {
    let aarch64 = include_str!("../src/arch/aarch64/platform_layout.rs");
    let x86 = include_str!("../src/arch/x86_64/platform_layout.rs");
    let options = include_str!("../src/kernel/boot_command_line.rs");
    assert!(aarch64.contains("KERNEL_BOOTSTRAP_PHYS_BASE: u64 = 0x4008_0000"));
    assert!(aarch64.contains("NEXT_ANON_PHYS_BASE: u64 = 0x5000_0000"));
    assert!(x86.contains("KERNEL_BOOTSTRAP_PHYS_BASE"));
    assert!(options.contains("#[default]\n    Kernel"));
    assert!(options.contains("#[default]\n    Auto"));
}
