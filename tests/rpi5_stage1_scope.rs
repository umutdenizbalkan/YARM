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
    assert!(marker - feature_gate < 8_000);
    assert!(boot.contains("_start:\n    mov x20, x0\n    mov x21, x1"));
    assert!(boot.contains("mov x0, x20\n    mov x1, x21\n    mov x2, x22\n    mov x3, x23"));
    assert!(boot.contains("bl yarm_aarch64_select_early_console"));
    assert!(boot.contains("stp x9, x10, [sp, #-16]!"));
    assert!(boot.contains("ldp x9, x10, [sp], #16"));
}

#[test]
fn raw_entry_breadcrumb_ladder_has_all_expected_markers() {
    let boot = include_str!("../src/arch/aarch64/boot.rs");
    for marker in [
        "RPI5_RAW_ENTRY",
        "RPI5_RAW_AFTER_MARKER",
        "RPI5_DTB_X0 value=0x",
        "RPI5_BSS_CLEAR_BEGIN",
        "RPI5_BSS_CLEAR_DONE",
        "RPI5_STACK_READY",
        "RPI5_BEFORE_EL1",
        "RPI5_AFTER_EL1",
        "RPI5_BEFORE_RUST",
        "RPI5_RUST_ENTRY",
        "RPI5_BOOT_OPTIONS_BEGIN",
        "RPI5_BOOT_OPTIONS_DONE",
        "RPI5_DTB_PARSE_BEGIN",
        "RPI5_DTB_PARSE_DONE",
        "RPI5_AFTER_BOOT_OPTIONS",
        "RPI5_CONSOLE_SELECT_BEGIN",
        "RPI5_SELECTED_UART_BASE value=0x",
        "RPI5_CONSOLE_SELECT_DONE",
        "RPI5_CONSOLE_WRITE_BEGIN",
        "RPI5_CONSOLE_WRITE_DONE",
    ] {
        assert!(boot.contains(marker), "missing breadcrumb {marker}");
    }
}

#[test]
fn rpi5_console_transition_is_bounded_and_uses_the_proven_uart() {
    let boot = include_str!("../src/arch/aarch64/boot.rs");
    let console = include_str!("../src/arch/aarch64/console.rs");
    let policy = include_str!("../src/arch/aarch64_boot_policy.rs");

    assert!(boot.contains("const RPI5_EMERGENCY_UART_BASE: u64 = 0x10_7d00_1000"));
    assert!(boot.contains("serial.base != RPI5_EMERGENCY_UART_BASE"));
    assert!(boot.contains("rpi5_emergency_marker(b\"RPI5_BOOT_00_ENTRY\\r\\n\\0\")"));
    assert!(!boot.contains("console::write_line(\"RPI5_BOOT_00_ENTRY\")"));
    assert!(console.contains("#[cfg(feature = \"rpi5-stage1\")]"));
    assert!(console.contains("const TX_READY_POLL_LIMIT: usize = 1_048_576"));
    assert!(console.contains("return false"));
    assert!(!console.contains("0x10_7d00_1000"));
    assert!(!console.contains("0x107d001000"));
    assert!(boot.contains("RPI5_UART_TRANSLATION_FAILED"));
    assert!(policy.contains("assert_eq!(info.serial.unwrap().base, 0x10_7d00_1000)"));
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
