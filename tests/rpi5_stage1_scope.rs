// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#[test]
fn rpi5_stage1_does_not_start_rp1_pcie_or_userspace_policy() {
    let policy = include_str!("../src/arch/aarch64_boot_policy.rs");
    let boot = include_str!("../src/arch/aarch64/boot.rs");
    for forbidden in ["rp1_gpio", "SpawnV5", "driver_manager", "pcie_init"] {
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
    let console = include_str!("../src/arch/aarch64/console.rs");
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
        "RPI5_TRY_WRITE_ENTER",
        "RPI5_TRY_WRITE_BYTE_BEGIN",
        "RPI5_TRY_WRITE_TX_READY",
        "RPI5_TRY_WRITE_BYTE_DONE",
        "RPI5_TRY_WRITE_TIMEOUT",
        "RPI5_TRY_WRITE_RETURN_OK",
        "RPI5_TRY_WRITE_RETURN_ERR",
        "RPI5_PL011_FR value=0x",
        "RPI5_AFTER_CONSOLE_WRITE",
        "RPI5_BEFORE_BOOT01",
        "RPI5_AFTER_BOOT01",
        "RPI5_BEFORE_BOOT02",
        "RPI5_AFTER_BOOT02",
        "RPI5_BEFORE_BOOT03",
        "RPI5_AFTER_BOOT03",
        "RPI5_DTB_DIAG_BEGIN",
        "RPI5_DTB_MEMORY_RANGE",
        "RPI5_DTB_RESERVED_RANGE",
        "RPI5_DTB_INITRD",
        "RPI5_DTB_BOOTARGS",
        "RPI5_DTB_IRQC",
        "RPI5_DTB_IRQC_L2",
        "RPI5_DTB_GIC_DIST",
        "RPI5_DTB_GIC_REDIST",
        "RPI5_DTB_GIC_MISSING",
        "RPI5_DTB_PSCI",
        "RPI5_DTB_CPU_BITMAP",
        "RPI5_DTB_RP1_PCIE",
        "RPI5_DTB_PCIE_CONTROLLER",
        "RPI5_DTB_RP1_NODE",
        "RPI5_DTB_DIAG_DONE",
    ] {
        assert!(
            boot.contains(marker) || console.contains(marker),
            "missing breadcrumb {marker}"
        );
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
    assert!(console.contains("feature = \"rpi5-stage1\""));
    assert!(console.contains("const TX_READY_POLL_LIMIT: usize = 1_048_576"));
    assert!(console.contains("return false"));
    assert!(console.contains("pub fn try_write_line(msg: &str) -> bool"));
    assert!(console.contains("rpi5_write_byte_bounded(b'\\r', diagnostic_probe)"));
    assert!(console.contains("rpi5_write_byte_bounded(b'\\n', diagnostic_probe)"));
    assert!(console.contains("super::boot::rpi5_emergency_hex"));
    assert!(console.contains("const PL011_DR: usize = 0x000"));
    assert!(console.contains("const PL011_FR: usize = 0x018"));
    assert!(console.contains("const PL011_FR_TXFF: u32 = 1 << 5"));
    assert!(boot.contains("ldr w13, [x10, #0x18]"));
    assert!(boot.contains("tbz w13, #5"));
    assert!(boot.contains("str w11, [x10]"));
    assert!(console.contains(
        "#[cfg(all(not(feature = \"hosted-dev\"), not(feature = \"rpi5-stage1\")))]\n\
         static UART_LOG_LOCK"
    ));
    assert!(!console.contains("0x10_7d00_1000"));
    assert!(!console.contains("0x107d001000"));
    assert!(boot.contains("RPI5_UART_TRANSLATION_FAILED"));
    assert!(policy.contains("assert_eq!(info.serial.unwrap().base, 0x10_7d00_1000)"));
    assert!(boot.contains("rpi5_emergency_marker(b\"RPI5_BOOT_01_DTB_PTR\\r\\n\\0\")"));
    assert!(boot.contains("b\"RPI5_BOOT_01_DTB_PTR value=0x\\0\""));
    assert!(boot.contains("rpi5_emergency_marker(b\"RPI5_BOOT_02_UART_SELECTED\\r\\n\\0\")"));
    assert!(boot.contains("b\"RPI5_BOOT_02_UART_SELECTED base=0x\\0\""));
    assert!(boot.contains("rpi5_emergency_marker(b\"RPI5_BOOT_03_UART_OK\\r\\n\\0\")"));
    assert!(boot.contains("#[cfg(not(feature = \"rpi5-stage1\"))]\n            crate::yarm_log!"));
}

#[test]
fn rpi5_stage1b_diagnostics_are_bounded_lock_free_and_halt() {
    let boot = include_str!("../src/arch/aarch64/boot.rs");
    let policy = include_str!("../src/arch/aarch64_boot_policy.rs");
    let start = boot
        .find("fn rpi5_stage1_dtb_diagnostics")
        .expect("Stage1B diagnostics function");
    let end = boot[start..]
        .find("fn yarm_aarch64_boot_marker_start")
        .map(|offset| start + offset)
        .expect("end of Stage1B diagnostics function");
    let diagnostics = &boot[start..end];
    assert!(diagnostics.contains("console::try_write_line"));
    assert!(!diagnostics.contains("yarm_log!"));
    assert!(!diagnostics.contains("printk"));
    assert!(diagnostics.contains("bytes: [u8; 384]"));
    assert!(diagnostics.contains("RPI5_DTB_DIAG_DONE"));
    assert!(diagnostics.contains("RPI5_DTB_IRQC_L2"));
    assert!(diagnostics.contains("RPI5_DTB_GIC_MISSING"));
    assert!(diagnostics.contains("RPI5_DTB_PCIE_CONTROLLER"));
    assert!(diagnostics.contains("halt_stage1();"));
    assert!(policy.contains("const MAX_DIAGNOSTIC_RANGES: usize = 8"));
    assert!(policy.contains("const MAX_DIAGNOSTIC_BOOTARGS: usize = 256"));
    assert!(policy.contains("pub fn parse_platform_dtb_diagnostics"));
    assert!(policy.contains("is_bcm7271_l2_compatible"));
    assert!(policy.contains("is_arm_gic_compatible"));
    assert!(policy.contains("is_pcie_node_name"));
    assert!(policy.contains("is_excluded_pcie_node_name"));
    assert!(policy.contains("is_known_pcie_compatible"));
    assert!(policy.contains("first_string(value) == b\"pci\""));
    assert!(policy.contains("const MAX_DIAGNOSTIC_PCIE_CONTROLLERS: usize = 8"));
    assert!(policy.contains("find_pcie_controller(&out, parent)"));
    assert!(boot.contains("RPI5_DTB_RP1_PCIE present=1 controller_index={}"));
    for forbidden_init in ["init_gic", "init_rp1", "init_pcie", "pcie_init"] {
        assert!(
            !diagnostics.contains(forbidden_init),
            "Stage1C added production initializer {forbidden_init}"
        );
    }

    let after_boot03 = boot.find("RPI5_AFTER_BOOT03").unwrap();
    let uart_halt = boot[after_boot03..]
        .find("options.boot_phase == BootPhase::Uart")
        .map(|offset| after_boot03 + offset)
        .unwrap();
    let dtb_diagnostics = boot.find("rpi5_stage1_dtb_diagnostics(dtb").unwrap();
    assert!(uart_halt < dtb_diagnostics);
}

#[test]
fn rpi5_stage1e_identity_mmu_is_bounded_and_precedes_userspace() {
    let boot = include_str!("../src/arch/aarch64/boot.rs");
    let policy = include_str!("../src/arch/aarch64_boot_policy.rs");
    let start = boot
        .find("fn rpi5_stage1_kernel_core_diagnostics")
        .expect("Stage1D diagnostics function");
    let end = boot[start..]
        .find("fn yarm_aarch64_boot_marker_start")
        .map(|offset| start + offset)
        .expect("end of Stage1D diagnostics function");
    let diagnostics = &boot[start..end];

    for marker in [
        "RPI5_KERNEL_PLAN_BEGIN",
        "RPI5_KERNEL_IMAGE_RANGE",
        "RPI5_KERNEL_DTB_RANGE",
        "RPI5_KERNEL_RESERVED_RANGE",
        "RPI5_KERNEL_RESERVED_ZERO_SKIPPED",
        "RPI5_KERNEL_USABLE_RANGE",
        "RPI5_KERNEL_PT_POOL",
        "RPI5_KERNEL_EARLY_HEAP",
        "RPI5_KERNEL_PLAN_DONE",
        "RPI5_MMU_PLAN_BEGIN",
        "RPI5_MMU_MAP_NORMAL",
        "RPI5_MMU_MAP_DEVICE",
        "RPI5_MMU_PT_ROOT",
        "RPI5_MMU_PLAN_DONE",
        "RPI5_MMU_ENABLE_BEGIN",
        "RPI5_MMU_ENABLE_DONE",
        "RPI5_UART_AFTER_MMU_OK",
        "RPI5_KERNEL_CORE_DONE",
        "RPI5_ALLOC_PLAN_BEGIN",
        "RPI5_ALLOC_RESERVED",
        "RPI5_ALLOC_USABLE",
        "RPI5_EARLY_HEAP_READY",
        "RPI5_FRAME_ALLOC_READY",
        "RPI5_FRAME_ALLOC_TEST_BEGIN",
        "RPI5_FRAME_ALLOC_TEST_PAGE",
        "RPI5_FRAME_ALLOC_TEST_DONE",
        "RPI5_ALLOC_PLAN_DONE",
        "RPI5_KERNEL_ALLOCATOR_READY",
        "RPI5_KERNEL_CORE_ALLOC_DONE",
        "RPI5_IRQTIMER_DIAG_BEGIN",
        "RPI5_TIMER_CNTFRQ",
        "RPI5_TIMER_CNTPCT_BEGIN",
        "RPI5_TIMER_CNTPCT_END",
        "RPI5_TIMER_CNTPCT_DELTA",
        "RPI5_TIMER_COUNTER_OK",
        "RPI5_PSCI_CONDUIT",
        "RPI5_GICD_PROBE_BEGIN",
        "RPI5_GICD_TYPER",
        "RPI5_GICD_IIDR",
        "RPI5_GICD_PROBE_DONE",
        "RPI5_GICR_PROBE_BEGIN",
        "RPI5_GICR_TYPER",
        "RPI5_GICR_PROBE_DONE",
        "RPI5_L2_INTC_PROBE_DEFERRED",
        "RPI5_IRQTIMER_DIAG_DONE",
        "RPI5_KERNEL_IRQTIMER_READY",
        "RPI5_IRQ_INIT_BEGIN",
        "RPI5_GICR_VALIDATE_BEGIN",
        "RPI5_GICR_FRAME",
        "RPI5_GICR_VALIDATE_DONE",
        "RPI5_GICR_VALIDATE_FAILED",
        "RPI5_IRQ_INIT_DEFERRED",
        "RPI5_TIMER_INIT_BEGIN",
        "RPI5_TIMER_CTL_BEFORE",
        "RPI5_TIMER_TVAL_SET",
        "RPI5_TIMER_CTL_AFTER",
        "RPI5_TIMER_INIT_DONE",
        "RPI5_KERNEL_BOOT_PREP_DONE",
        "RPI5_KERNEL_BOOT_BEGIN",
        "RPI5_KERNEL_PLATFORM_READY",
        "RPI5_KERNEL_MEMORY_READY",
        "RPI5_KERNEL_CPU0_READY",
        "RPI5_KERNEL_TRAP_READY",
        "RPI5_KERNEL_STATE_BEGIN",
        "RPI5_KERNEL_STATE_READY",
        "RPI5_KERNEL_BOOTSTRAP_NO_USERSPACE",
        "RPI5_KERNEL_BOOT_OK",
        "RPI5_INITRD_DETECT_BEGIN",
        "RPI5_INITRD_DTB_PROPS",
        "RPI5_INITRD_RANGE",
        "RPI5_INITRD_RESERVED",
        "RPI5_INITRD_CPIO_CHECK_BEGIN",
        "RPI5_INITRD_CPIO_MAGIC_OK",
        "RPI5_INITRD_CPIO_FIRST_ENTRY",
        "RPI5_INITRD_READY",
        "RPI5_STAGE2A_DONE",
        "RPI5_STAGE2B_BEGIN",
        "RPI5_INIT_LOOKUP_BEGIN",
        "RPI5_INIT_LOOKUP_OK",
        "RPI5_INIT_ELF_CHECK_BEGIN",
        "RPI5_INIT_ELF_HEADER_OK",
        "RPI5_INIT_ELF_LOAD_PLAN_BEGIN",
        "RPI5_INIT_ELF_SEGMENT",
        "RPI5_INIT_ELF_LOAD_PLAN_DONE",
        "RPI5_STAGE2B_DEFERRED",
        "RPI5_STAGE2B_DONE",
        "RPI5_STAGE2C_BEGIN",
        "RPI5_INIT_TASK_BUILD_BEGIN",
        "RPI5_INIT_ADDRESS_SPACE_BEGIN",
        "RPI5_INIT_SEGMENT_MAP_BEGIN",
        "RPI5_INIT_SEGMENT_MAPPED",
        "RPI5_INIT_BSS_ZEROED",
        "RPI5_INIT_ADDRESS_SPACE_READY",
        "RPI5_INIT_STACK_READY",
        "RPI5_INIT_TRAP_FRAME_READY",
        "RPI5_INIT_TASK_BUILD_DONE",
        "RPI5_INIT_SPAWN_READY",
        "RPI5_STAGE2C_DONE",
        "RPI5_STAGE2D_BEGIN",
        "RPI5_ENTER_USER_ATTEMPT",
        "RPI5_STAGE2D_DEFERRED",
        "RPI5_STAGE2D_DONE",
    ] {
        assert!(
            diagnostics.contains(marker),
            "missing Stage1D marker {marker}"
        );
    }
    assert!(diagnostics.contains("console::try_write_line"));
    assert!(diagnostics.contains("bytes: [u8; 192]"));
    assert!(!diagnostics.contains("yarm_log!"));
    assert!(!diagnostics.contains("printk"));
    assert!(!diagnostics.contains("boot_initrd_bytes"));
    assert!(!diagnostics.contains("install_boot_initrd_bytes"));
    for forbidden in [
        "SpawnV5",
        "init_gic",
        "init_rp1",
        "init_pcie",
        "pcie_init",
        "start_secondary_cpus",
    ] {
        assert!(
            !diagnostics.contains(forbidden),
            "Stage1D added forbidden path {forbidden}"
        );
    }
    assert!(boot.contains("if matches!(options.boot_phase, BootPhase::Mmu | BootPhase::Kernel)"));
    assert!(boot.contains("rpi5_stage1_kernel_core_diagnostics(dtb)"));
    assert!(policy.contains("const STAGE1_PT_POOL_SIZE: u64 = 256 * 1024"));
    assert!(policy.contains("const STAGE1_EARLY_HEAP_SIZE: u64 = 2 * 1024 * 1024"));
    assert!(policy.contains("plan_rpi5_stage1_kernel_memory"));
    assert!(policy.contains("plan_rpi5_stage1_identity_map"));
    assert!(policy.contains("plan_rpi5_stage1_allocator_handoff"));
    assert!(policy.contains("Stage1KernelRange::new(0, RPI5_FIRMWARE_LOW_RESERVED_END)"));
    assert!(diagnostics.contains("rpi5_stage1_build_identity_tables"));
    assert!(diagnostics.contains("rpi5_stage1_enable_identity_mmu"));
    assert!(diagnostics.contains("next: plan.pt_pool.start"));
    assert!(diagnostics.contains("end: plan.pt_pool.end"));
    assert!(diagnostics.contains("return Err(\"pt_pool_exhausted\")"));
    assert!(diagnostics.contains("RPI5_STAGE1_DEVICE_FLAGS"));
    assert!(diagnostics.contains("RPI5_STAGE1_MAIR_EL1: u64 = 0x04ff"));
    assert!(diagnostics.contains("RPI5_STAGE1_TCR_EL1"));
    assert!(diagnostics.contains("\"tlbi vmalle1\""));
    assert!(diagnostics.contains("\"msr SCTLR_EL1, {0}\""));
    assert!(!diagnostics.contains("RPI5_MMU_DEFERRED"));
    assert!(diagnostics.contains("PhysicalFrameAllocator::new_uninit()"));
    assert!(diagnostics.contains("allocator.alloc_frame()"));
    assert!(diagnostics.contains("allocator.free_frame(test_frame)"));
    assert!(diagnostics.contains("plan.early_heap.start as *mut PhysicalFrameAllocator"));
    assert!(diagnostics.contains("\"mrs {0}, CNTFRQ_EL0\""));
    assert!(diagnostics.contains("\"mrs {0}, CNTPCT_EL0\""));
    assert!(diagnostics.contains("(gicd_base + 0x004) as *const u32"));
    assert!(diagnostics.contains("(gicd_base + 0x008) as *const u32"));
    assert!(diagnostics.contains("(gicr_base + 0x008) as *const u64"));
    assert!(diagnostics.contains("core::ptr::read_volatile"));
    assert!(diagnostics.contains("no_reviewed_read_only_offset"));
    let stage1g_start = diagnostics.find("RPI5_IRQTIMER_DIAG_BEGIN").unwrap();
    let stage1g_end = diagnostics[stage1g_start..]
        .find("RPI5_KERNEL_IRQTIMER_READY")
        .map(|offset| stage1g_start + offset)
        .unwrap();
    let stage1g = &diagnostics[stage1g_start..stage1g_end];
    assert!(!stage1g.contains("write_volatile"));
    assert!(!stage1g.contains("CNTP_CTL_EL0"));
    assert!(!stage1g.contains("CNTP_TVAL_EL0"));
    assert!(!stage1g.contains("GICD_CTLR"));
    assert!(!stage1g.contains("ISENABLER"));
    assert!(!stage1g.contains("ICENABLER"));
    let stage1h_start = diagnostics.find("RPI5_IRQ_INIT_BEGIN").unwrap();
    let stage1h_end = diagnostics[stage1h_start..]
        .find("RPI5_KERNEL_BOOT_PREP_DONE")
        .map(|offset| stage1h_start + offset)
        .unwrap();
    let stage1h = &diagnostics[stage1h_start..stage1h_end];
    let validation = stage1h.find("RPI5_GICR_VALIDATE_BEGIN").unwrap();
    let deferral = stage1h.find("RPI5_IRQ_INIT_DEFERRED").unwrap();
    assert!(validation < deferral);
    assert!(stage1h.contains("core::ptr::read_volatile"));
    assert!(!stage1h.contains("write_volatile"));
    assert!(!stage1h.contains("GICD_CTLR"));
    assert!(!stage1h.contains("ISENABLER"));
    assert!(!stage1h.contains("ICENABLER"));
    assert!(stage1h.contains("\"msr CNTP_TVAL_EL0, {0}\""));
    assert!(stage1h.contains("\"msr CNTP_CTL_EL0, {0}\""));
    assert_eq!(stage1h.matches("\"msr CNTP_").count(), 2);
    assert!(!stage1h.contains("start_secondary_cpus"));
    assert!(!stage1h.contains("scheduler"));
    let stage1i_start = diagnostics.find("RPI5_KERNEL_BOOT_BEGIN").unwrap();
    let stage1i_end = diagnostics[stage1i_start..]
        .find("RPI5_KERNEL_BOOT_OK")
        .map(|offset| stage1i_start + offset)
        .unwrap();
    let stage1i = &diagnostics[stage1i_start..stage1i_end];
    assert!(stage1i.contains("build_rpi5_stage1_kernel_bootstrap_record"));
    assert!(stage1i.contains("allocator.total_frames()"));
    assert!(stage1i.contains("allocator.free_frames()"));
    assert!(!stage1i.contains("PhysicalFrameAllocator::new_uninit()"));
    assert!(!stage1i.contains("init_from_memory_map"));
    assert!(!stage1i.contains("bootstrap_first_user_task"));
    assert!(!stage1i.contains("Bootstrap::init"));
    assert!(!stage1i.contains("scheduler"));
    assert!(!stage1i.contains("start_secondary_cpus"));
    assert!(!stage1i.contains("init_gic"));
    assert!(!stage1i.contains("init_rp1"));
    assert!(!stage1i.contains("init_pcie"));
    assert!(!stage1i.contains("yarm_log!"));
    assert!(!stage1i.contains("printk"));
    assert!(stage1i.contains("\"msr daifset, #0xf\""));
    let stage2a_start = diagnostics.find("RPI5_INITRD_DETECT_BEGIN").unwrap();
    let stage2a = &diagnostics[stage2a_start..];
    assert!(stage2a.contains("plan_rpi5_stage2a_initrd"));
    assert!(stage2a.contains("rpi5_stage2a_cpio_first_name"));
    assert!(stage2a.contains("RPI5_INITRD_MISSING"));
    assert!(stage2a.contains("RPI5_STAGE2A_DEFERRED reason=no_initrd"));
    assert!(!stage2a.contains("bootstrap_first_user_task"));
    assert!(!stage2a.contains("Bootstrap::init"));
    assert!(!stage2a.contains("scheduler"));
    assert!(!stage2a.contains("start_secondary_cpus"));
    assert!(!stage2a.contains("init_gic"));
    assert!(!stage2a.contains("init_rp1"));
    assert!(!stage2a.contains("init_pcie"));
    assert!(!stage2a.contains("yarm_log!"));
    assert!(!stage2a.contains("printk"));
    let stage2b_start = diagnostics.find("RPI5_STAGE2B_BEGIN").unwrap();
    let stage2b = &diagnostics[stage2b_start..];
    assert!(stage2b.contains("rpi5_stage2b_find_init"));
    assert!(stage2b.contains("plan_rpi5_stage2b_init_elf"));
    assert!(stage2b.contains("plan_rpi5_stage2c_init_task"));
    assert!(stage2b.contains("rpi5_stage2c_build_init_task"));
    assert!(stage2b.contains("allocator, init_elf"));
    assert!(!stage2b.contains("PhysicalFrameAllocator::new_uninit()"));
    assert!(stage2b.contains("RPI5_INIT_TASK_BUILD_DONE"));
    assert!(stage2b.contains("RPI5_INIT_SPAWN_READY"));
    assert!(stage2b.contains("enter_user_bridge_not_ready"));
    assert!(!stage2b.contains("RPI5_ENTER_USER_ERET"));
    assert!(!stage2b.contains("RPI5_FIRST_USER_TRAP"));
    assert!(!stage2b.contains("bootstrap_first_user_task"));
    assert!(!stage2b.contains("Bootstrap::init"));
    assert!(!stage2b.contains("SpawnV5"));
    assert!(!stage2b.contains("scheduler"));
    assert!(!stage2b.contains("start_secondary_cpus"));
    assert!(!stage2b.contains("init_gic"));
    assert!(!stage2b.contains("init_rp1"));
    assert!(!stage2b.contains("init_pcie"));
    assert!(!stage2b.contains("yarm_log!"));
    assert!(!stage2b.contains("printk"));
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
    assert!(
        include_str!("../targets/aarch64-rpi5-stage1-none.ld")
            .contains("KERNEL_LOAD_BASE = 0x80000")
    );
    assert!(
        include_str!("../targets/aarch64-yarm-none.ld").contains("KERNEL_LOAD_BASE = 0x40080000")
    );
}
