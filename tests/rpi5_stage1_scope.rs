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
        "RPI5_STAGE2D_REAL_BEGIN",
        "RPI5_ENTER_USER_FAILED",
        "RPI5_STAGE2D_REAL_DEFERRED",
        "RPI5_STAGE2E_DEFERRED",
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
    let stage2b_end = diagnostics[stage2b_start..]
        .find("RPI5_STAGE2E_DEFERRED reason=el0_not_entered")
        .map(|offset| stage2b_start + offset + "RPI5_STAGE2E_DEFERRED reason=el0_not_entered".len())
        .unwrap();
    let stage2b = &diagnostics[stage2b_start..stage2b_end];
    assert!(stage2b.contains("rpi5_stage2b_find_init"));
    assert!(stage2b.contains("plan_rpi5_stage2b_init_elf"));
    assert!(stage2b.contains("plan_rpi5_stage2c_init_task"));
    assert!(stage2b.contains("rpi5_stage2c_build_init_task"));
    assert!(stage2b.contains("allocator, init_elf"));
    assert!(!stage2b.contains("PhysicalFrameAllocator::new_uninit()"));
    assert!(stage2b.contains("RPI5_INIT_TASK_BUILD_DONE"));
    assert!(stage2b.contains("RPI5_INIT_SPAWN_READY"));
    assert!(stage2b.contains("validate_rpi5_stage2d_enter_bridge"));
    assert!(policy.contains("TtbrSplitNotReady => \"ttbr_split_not_ready\""));
    assert!(!stage2b.contains("RPI5_ENTER_USER_ERET"));
    assert!(!stage2b.contains("RPI5_FIRST_USER_TRAP"));
    assert!(!stage2b.contains("RPI5_SERVICE_CHAIN_OK"));
    assert!(!stage2b.contains("RPI5_TTBR0_INSTALL root="));
    assert!(!stage2b.contains("\"msr TTBR0_EL1"));
    assert!(!stage2b.contains("\"msr daifclr"));
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

#[test]
fn rpi5_high_half_scaffold_is_explicit_and_non_default() {
    let policy = include_str!("../src/arch/aarch64_boot_policy.rs");
    let stage1_target = include_str!("../targets/aarch64-rpi5-stage1-none.json");
    let stage1_linker = include_str!("../targets/aarch64-rpi5-stage1-none.ld");
    let high_half_linker = include_str!("../targets/aarch64-rpi5-stage2-highhalf-none.ld");
    let documentation = include_str!("../doc/RPI5_BRINGUP.md");

    assert!(policy.contains("RPI5_KERNEL_VA_OFFSET: u64 = 0xffff_ff80_0000_0000"));
    assert!(policy.contains("RPI5_KERNEL_PHYS_LOAD_BASE: u64 = 0x0000_0000_0008_0000"));
    assert!(policy.contains("RPI5_KERNEL_VIRT_LOAD_BASE: u64 = 0xffff_ff80_0008_0000"));
    assert!(high_half_linker.contains("KERNEL_PHYS_LOAD_BASE = 0x0000000000080000"));
    assert!(high_half_linker.contains("KERNEL_VIRT_BASE = 0xffffff8000000000"));
    assert!(high_half_linker.contains("KERNEL_VIRT_LOAD_BASE = 0xffffff8000080000"));
    for symbol in [
        "__boot_low_start",
        "__boot_low_end",
        "__kernel_phys_start",
        "__kernel_phys_end",
        "__kernel_virt_start",
        "__kernel_virt_end",
        "__kernel_va_offset",
    ] {
        assert!(high_half_linker.contains(symbol));
    }
    assert!(stage1_linker.contains("KERNEL_LOAD_BASE = 0x80000"));
    assert!(stage1_target.contains("aarch64-rpi5-stage1-none.ld"));
    assert!(!stage1_target.contains("aarch64-rpi5-stage2-highhalf-none.ld"));
    assert!(documentation.contains("This scaffold does not install TTBR1"));
    assert!(documentation.contains("only then install a user root in TTBR0"));
}

#[test]
fn rpi5_hh2_transition_is_explicit_bounded_and_never_enters_el0() {
    let cargo = include_str!("../Cargo.toml");
    let boot = include_str!("../src/arch/aarch64/boot.rs");
    let policy = include_str!("../src/arch/aarch64_boot_policy.rs");
    let stage1_target = include_str!("../targets/aarch64-rpi5-stage1-none.json");
    let high_target = include_str!("../targets/aarch64-rpi5-stage2-highhalf-none.json");
    let high_linker = include_str!("../targets/aarch64-rpi5-stage2-highhalf-none.ld");
    let hh_start = boot.find("RPI5_HH_LOW_ENTRY").unwrap();
    let hh_end = boot[hh_start..].find("RPI5_HH3_DONE").unwrap() + hh_start;
    let hh = &boot[hh_start..hh_end + "RPI5_HH3_DONE".len()];

    assert!(cargo.contains("rpi5-highhalf = [\"rpi5-stage1\"]"));
    assert!(high_target.contains("aarch64-rpi5-stage2-highhalf-none.ld"));
    assert!(high_target.contains("\"code-model\": \"large\""));
    assert!(!stage1_target.contains("aarch64-rpi5-stage2-highhalf-none.ld"));
    for symbol in [
        "__hh_pt_pool_start",
        "__hh_pt_pool_end",
        "__hh_ttbr0_root",
        "__hh_ttbr1_root",
        "__hh_empty_ttbr0_root",
        "__hh_uart0_l2",
        "__hh_uart0_l3",
        "__hh_uart1_l2",
        "__hh_uart1_l3",
        "__hh_heap_start",
        "__hh_heap_end",
    ] {
        assert!(high_linker.contains(symbol));
    }
    for marker in [
        "RPI5_HH_LOW_ENTRY",
        "RPI5_HH_PLAN_BEGIN",
        "RPI5_HH_MAP_KERNEL",
        "RPI5_HH_MAP_STACK",
        "RPI5_HH_MAP_DTB",
        "RPI5_HH_MAP_HEAP",
        "RPI5_HH_MAP_UART",
        "RPI5_HH_TTBR0_ROOT",
        "RPI5_HH_TTBR1_ROOT",
        "RPI5_HH_TCR",
        "RPI5_HH_PLAN_DONE",
        "RPI5_HH_ENABLE_BEGIN",
        "RPI5_HH_ENABLE_DONE",
        "RPI5_HH_JUMP_HIGH",
        "RPI5_HH_HIGH_ENTRY_OK",
        "RPI5_HH_VBAR_HIGH_OK",
        "RPI5_HH_RUST_ENTRY",
        "RPI5_HH_RUST_AFTER_ENTRY",
        "RPI5_HH_READ_PC_BEGIN",
        "RPI5_HH_READ_PC_CAPTURED",
        "RPI5_HH_READ_PC_PRINT_BEGIN",
        "RPI5_HH_HEX_BEGIN",
        "RPI5_HH_HEX_DIGIT_BEGIN",
        "RPI5_HH_HEX_DIGIT_DONE",
        "RPI5_HH_HEX_DONE",
        "RPI5_HH_HEX_FAILED reason=",
        "RPI5_HH3_FAULT_BOUNDARY reason=hex_output",
        "RPI5_HH_READ_PC_DONE value=0x",
        "RPI5_HH_READ_PC_FAILED reason=",
        "RPI5_HH3_FAULT_BOUNDARY reason=pc_read_or_print",
        "RPI5_HH_READ_SP_BEGIN",
        "RPI5_HH_READ_SP_CAPTURED",
        "RPI5_HH_SP_HEX_BEGIN",
        "RPI5_HH_SP_HEX_DIGIT_BEGIN",
        "RPI5_HH_SP_HEX_DIGIT_DONE",
        "RPI5_HH_SP_HEX_DONE",
        "RPI5_HH_SP_HEX_FAILED reason=",
        "RPI5_HH3_FAULT_BOUNDARY reason=sp_hex_output",
        "RPI5_HH_READ_SP_DONE value=0x",
        "RPI5_HH_READ_VBAR_BEGIN",
        "RPI5_HH_READ_VBAR_CAPTURED",
        "RPI5_HH_VBAR_HEX_BEGIN",
        "RPI5_HH_VBAR_HEX_DIGIT_BEGIN",
        "RPI5_HH_VBAR_HEX_DIGIT_DONE",
        "RPI5_HH_VBAR_HEX_DONE",
        "RPI5_HH_VBAR_HEX_FAILED reason=",
        "RPI5_HH3_FAULT_BOUNDARY reason=vbar_hex_output",
        "RPI5_HH_READ_VBAR_DONE value=0x",
        "RPI5_HH_READ_TTBR_BEGIN",
        "RPI5_HH_READ_TTBR_DONE",
        "RPI5_HH_PRINT_REGS_BEGIN",
        "RPI5_HH_PRINT_REGS_FIRST_BEGIN",
        "RPI5_HH_PRINT_REGS_FIRST_HEX_BEGIN",
        "RPI5_HH_PRINT_REGS_FIRST_HEX_DIGIT_BEGIN",
        "RPI5_HH_PRINT_REGS_FIRST_HEX_DIGIT_DONE",
        "RPI5_HH_PRINT_REGS_FIRST_HEX_DONE",
        "RPI5_HH_PRINT_REGS_FIRST_HEX_FAILED reason=",
        "RPI5_HH3_FAULT_BOUNDARY reason=print_regs_first_hex_output",
        "RPI5_HH_PRINT_REGS_FIRST_DONE value=0x",
        "RPI5_HH_PRINT_REGS_SP_BEGIN",
        "RPI5_HH_PRINT_REGS_SP_HEX_BEGIN",
        "RPI5_HH_PRINT_REGS_SP_HEX_DIGIT_BEGIN",
        "RPI5_HH_PRINT_REGS_SP_HEX_DIGIT_DONE",
        "RPI5_HH_PRINT_REGS_SP_HEX_DONE",
        "RPI5_HH_PRINT_REGS_SP_HEX_FAILED reason=",
        "RPI5_HH3_FAULT_BOUNDARY reason=print_regs_sp_hex_output",
        "RPI5_HH_PRINT_REGS_SP_DONE value=0x",
        "RPI5_HH_PRINT_REGS_DONE",
        "RPI5_HH3_PRECHECK_DONE",
        "RPI5_HH_PRINT_REGS_SP_DONE value=0x",
        "RPI5_HH_VBAR value=0x",
        "RPI5_HH_TTBR0 value=0x",
        "RPI5_HH_TTBR1 value=0x",
        "RPI5_HH_TCR value=0x",
        "RPI5_HH_REGISTERS_OK",
        "RPI5_HH_RUST_UART_OK",
        "RPI5_HH3_DONE",
        "RPI5_HH4_BEGIN",
        "RPI5_HH4_DONE",
        "RPI5_HH5_BEGIN",
        "RPI5_HH5_ENTER_USER_ATTEMPT",
        "RPI5_HH3_FAILED reason=",
        "RPI5_HH3_FAULT_BOUNDARY reason=",
        "RPI5_HH_REGISTER_MISMATCH reason=",
    ] {
        assert!(boot.contains(marker), "missing {marker}");
    }
    assert!(boot.contains("msr TTBR0_EL1, x21"));
    assert!(boot.contains("msr TTBR1_EL1, x22"));
    assert!(boot.contains("ldr x19, =HH_UART_VIRT"));
    assert!(boot.contains("br x0"));
    assert!(boot.contains("extern \"C\" fn yarm_rpi5_hh_rust_continue() -> !"));
    assert!(boot.contains("\"adr {pc}, 2f\""));
    assert!(boot.contains("\"2:\""));
    assert!(boot.contains("while nibble_index < 16"));
    assert!(boot.contains("core::ptr::write_volatile(hh_pc_hex_data, $byte as u32)"));
    assert!(!boot.contains("rpi5_hh_write_hex_line(&RPI5_HH_READ_PC_DONE_MARKER, pc)"));
    assert!(boot.contains("RPI5_HH_READ_SP_CAPTURED_MARKER"));
    assert!(boot.contains("RPI5_HH_SP_HEX_BEGIN_MARKER"));
    assert!(boot.contains("RPI5_HH_SP_HEX_DIGIT_BEGIN_MARKER"));
    assert!(boot.contains("RPI5_HH_SP_HEX_DIGIT_DONE_MARKER"));
    assert!(boot.contains("RPI5_HH_SP_HEX_DONE_MARKER"));
    assert!(boot.contains("RPI5_HH_READ_VBAR_CAPTURED_MARKER"));
    assert!(boot.contains("RPI5_HH_VBAR_HEX_BEGIN_MARKER"));
    assert!(boot.contains("RPI5_HH_VBAR_HEX_DIGIT_BEGIN_MARKER"));
    assert!(boot.contains("RPI5_HH_VBAR_HEX_DIGIT_DONE_MARKER"));
    assert!(boot.contains("RPI5_HH_VBAR_HEX_DONE_MARKER"));
    assert!(boot.contains("RPI5_HH_SP_HEX_FAILED_MARKER"));
    assert!(boot.contains("RPI5_HH3_SP_HEX_FAULT_BOUNDARY_MARKER"));
    assert!(boot.contains("core::ptr::write_volatile(hh_sp_hex_data, $byte as u32)"));
    assert!(!boot.contains("rpi5_hh_write_hex_line(&RPI5_HH_READ_SP_DONE_MARKER, sp)"));
    assert!(boot.contains("RPI5_HH_READ_VBAR_CAPTURED_MARKER"));
    assert!(boot.contains("RPI5_HH_VBAR_HEX_BEGIN_MARKER"));
    assert!(boot.contains("RPI5_HH_VBAR_HEX_DIGIT_BEGIN_MARKER"));
    assert!(boot.contains("RPI5_HH_VBAR_HEX_DIGIT_DONE_MARKER"));
    assert!(boot.contains("RPI5_HH_VBAR_HEX_DONE_MARKER"));
    assert!(boot.contains("RPI5_HH_VBAR_HEX_FAILED_MARKER"));
    assert!(boot.contains("RPI5_HH3_VBAR_HEX_FAULT_BOUNDARY_MARKER"));
    assert!(boot.contains("core::ptr::write_volatile(hh_vbar_hex_data, $byte as u32)"));
    assert!(!boot.contains("rpi5_hh_write_hex_line(&RPI5_HH_READ_VBAR_DONE_MARKER, vbar)"));
    assert!(boot.contains("RPI5_HH_PRINT_REGS_FIRST_BEGIN_MARKER"));
    assert!(boot.contains("RPI5_HH_PRINT_REGS_FIRST_HEX_BEGIN_MARKER"));
    assert!(boot.contains("RPI5_HH_PRINT_REGS_FIRST_HEX_DIGIT_BEGIN_MARKER"));
    assert!(boot.contains("RPI5_HH_PRINT_REGS_FIRST_HEX_DIGIT_DONE_MARKER"));
    assert!(boot.contains("RPI5_HH_PRINT_REGS_FIRST_HEX_DONE_MARKER"));
    assert!(boot.contains("RPI5_HH_PRINT_REGS_SP_BEGIN_MARKER"));
    assert!(boot.contains("RPI5_HH_PRINT_REGS_SP_HEX_BEGIN_MARKER"));
    assert!(boot.contains("RPI5_HH_PRINT_REGS_SP_HEX_DONE_MARKER"));
    assert!(boot.contains("RPI5_HH_PRINT_REGS_FIRST_HEX_FAILED_MARKER"));
    assert!(boot.contains("RPI5_HH3_PRINT_REGS_FIRST_HEX_FAULT_BOUNDARY_MARKER"));
    assert!(boot.contains("core::ptr::write_volatile(hh_print_regs_first_hex_data, $byte as u32)"));
    assert!(!boot.contains("rpi5_hh_write_hex_line(b\"RPI5_HH_PC value=0x\", pc)"));
    assert!(boot.contains("RPI5_HH_PRINT_REGS_SP_BEGIN_MARKER"));
    assert!(boot.contains("RPI5_HH_PRINT_REGS_SP_HEX_BEGIN_MARKER"));
    assert!(boot.contains("RPI5_HH_PRINT_REGS_SP_HEX_DIGIT_BEGIN_MARKER"));
    assert!(boot.contains("RPI5_HH_PRINT_REGS_SP_HEX_DIGIT_DONE_MARKER"));
    assert!(boot.contains("RPI5_HH_PRINT_REGS_SP_HEX_DONE_MARKER"));
    assert!(boot.contains("RPI5_HH_PRINT_REGS_SP_HEX_FAILED_MARKER"));
    assert!(boot.contains("RPI5_HH3_PRINT_REGS_SP_HEX_FAULT_BOUNDARY_MARKER"));
    assert!(boot.contains("core::ptr::write_volatile(hh_print_regs_sp_hex_data, $byte as u32)"));
    assert!(!boot.contains("rpi5_hh_write_hex_line(b\"RPI5_HH_SP value=0x\", sp)"));
    assert!(boot.contains("RPI5_HH_VA_OFFSET: u64 = 0xffff_ff80_0000_0000"));
    assert!(boot.contains("tcr & (1 << 23) != 0"));
    assert!(policy.contains("plan_rpi5_high_half_transition"));
    assert!(policy.contains("AttributeOverlap"));
    assert!(!hh.contains("Stage2C"));
    assert!(!hh.contains("RPI5_ENTER_USER_ERET"));
    assert!(!hh.contains("yarm_log!"));
    assert!(!hh.contains("printk"));
    assert!(!hh.contains("SpawnV5"));
    assert!(!hh.contains("scheduler"));
    assert!(!hh.contains("init_gic"));
    assert!(!hh.contains("init_rp1"));
    assert!(!hh.contains("init_pcie"));
}

#[test]
fn rpi5_hh3_build_and_generator_paths_are_explicit() {
    let build = include_str!("../scripts/build-rpi5-highhalf-artifact.sh");
    let generator = include_str!("../scripts/create-rpi5-stage1-boot-dir.sh");
    let generator_test = include_str!("../scripts/test-create-rpi5-stage1-boot-dir.sh");

    assert!(build.contains("build-rpi5/kernel_2712_hh.img"));
    assert!(build.contains("--features rpi5-highhalf"));
    assert!(build.contains("aarch64-rpi5-stage2-highhalf-none.json"));
    assert!(build.contains("refusing to overwrite the default RPi5 artifact"));
    assert!(build.contains("required_markers=("));
    assert!(build.contains("validate_raw_image_markers"));
    assert!(build.contains("grep -aFq -- \"$marker\" \"$image\""));
    assert!(build.contains("--validate-image"));
    assert!(build.contains("__hh_empty_ttbr0_root"));
    assert!(build.contains("empty TTBR0 root is not distinct"));
    for marker in [
        "RPI5_HH_LOW_ENTRY",
        "RPI5_HH_PLAN_DONE",
        "RPI5_HH_ENABLE_DONE",
        "RPI5_HH_JUMP_HIGH",
        "RPI5_HH_HIGH_ENTRY_OK",
        "RPI5_HH_RUST_ENTRY",
        "RPI5_HH_RUST_AFTER_ENTRY",
        "RPI5_HH_READ_PC_BEGIN",
        "RPI5_HH_READ_PC_CAPTURED",
        "RPI5_HH_READ_PC_PRINT_BEGIN",
        "RPI5_HH_HEX_BEGIN",
        "RPI5_HH_HEX_DIGIT_BEGIN",
        "RPI5_HH_HEX_DIGIT_DONE",
        "RPI5_HH_HEX_DONE",
        "RPI5_HH_HEX_FAILED",
        "RPI5_HH3_FAULT_BOUNDARY reason=hex_output",
        "RPI5_HH_READ_PC_DONE",
        "RPI5_HH_READ_PC_FAILED",
        "RPI5_HH3_FAULT_BOUNDARY reason=pc_read_or_print",
        "RPI5_HH_READ_SP_BEGIN",
        "RPI5_HH_READ_SP_CAPTURED",
        "RPI5_HH_SP_HEX_BEGIN",
        "RPI5_HH_SP_HEX_DIGIT_BEGIN",
        "RPI5_HH_SP_HEX_DIGIT_DONE",
        "RPI5_HH_SP_HEX_DONE",
        "RPI5_HH_SP_HEX_FAILED",
        "RPI5_HH3_FAULT_BOUNDARY reason=sp_hex_output",
        "RPI5_HH_READ_SP_DONE",
        "RPI5_HH_READ_VBAR_BEGIN",
        "RPI5_HH_READ_VBAR_CAPTURED",
        "RPI5_HH_VBAR_HEX_BEGIN",
        "RPI5_HH_VBAR_HEX_DIGIT_BEGIN",
        "RPI5_HH_VBAR_HEX_DIGIT_DONE",
        "RPI5_HH_VBAR_HEX_DONE",
        "RPI5_HH_VBAR_HEX_FAILED",
        "RPI5_HH3_FAULT_BOUNDARY reason=vbar_hex_output",
        "RPI5_HH_READ_VBAR_DONE",
        "RPI5_HH3_PRECHECK_DONE",
        "RPI5_HH_REGISTERS_OK",
        "RPI5_HH_RUST_UART_OK",
        "RPI5_HH3_DONE",
        "RPI5_HH4_BEGIN",
        "RPI5_HH4_DONE",
        "RPI5_HH5_BEGIN",
        "RPI5_HH5_ENTER_USER_ATTEMPT",
    ] {
        assert!(build.contains(marker), "build validation omits {marker}");
    }
    assert!(generator.contains("--highhalf"));
    assert!(generator.contains("Explicit high-half diagnostic mode"));
    assert!(generator.contains("RPI5_HH3_DONE"));
    assert!(generator_test.contains("fake-kernel_2712_hh.img"));
    assert!(generator_test.contains("HH mode without explicit kernel input"));
    assert!(generator_test.contains("HH marker validator accepted an incomplete raw image"));
}

#[test]
fn rpi5_hh3_success_markers_are_retained_in_the_high_image() {
    let boot = include_str!("../src/arch/aarch64/boot.rs");
    let linker = include_str!("../targets/aarch64-rpi5-stage2-highhalf-none.ld");

    assert!(boot.contains("link_section = \".rodata.rpi5_hh_markers\""));
    assert!(boot.contains("#[used]"));
    assert!(linker.contains("KEEP(*(.rodata.rpi5_hh_markers))"));
    assert!(boot.contains("rpi5_hh_write_line(&RPI5_HH_RUST_ENTRY_MARKER)"));
    assert!(boot.contains("rpi5_hh_write_line(&RPI5_HH_RUST_AFTER_ENTRY_MARKER)"));
    assert!(boot.contains("rpi5_hh_write_line(&RPI5_HH_READ_PC_CAPTURED_MARKER)"));
    assert!(boot.contains("rpi5_hh_write_line(&RPI5_HH_READ_PC_PRINT_BEGIN_MARKER)"));
    assert!(boot.contains("RPI5_HH_HEX_BEGIN_MARKER"));
    assert!(boot.contains("RPI5_HH_HEX_DIGIT_BEGIN_MARKER"));
    assert!(boot.contains("RPI5_HH_HEX_DIGIT_DONE_MARKER"));
    assert!(boot.contains("RPI5_HH_HEX_DONE_MARKER"));
    assert!(boot.contains("RPI5_HH_READ_SP_CAPTURED_MARKER"));
    assert!(boot.contains("RPI5_HH_SP_HEX_BEGIN_MARKER"));
    assert!(boot.contains("RPI5_HH_SP_HEX_DIGIT_BEGIN_MARKER"));
    assert!(boot.contains("RPI5_HH_SP_HEX_DIGIT_DONE_MARKER"));
    assert!(boot.contains("RPI5_HH_SP_HEX_DONE_MARKER"));
    assert!(boot.contains("RPI5_HH_READ_VBAR_CAPTURED_MARKER"));
    assert!(boot.contains("RPI5_HH_VBAR_HEX_BEGIN_MARKER"));
    assert!(boot.contains("RPI5_HH_VBAR_HEX_DIGIT_BEGIN_MARKER"));
    assert!(boot.contains("RPI5_HH_VBAR_HEX_DIGIT_DONE_MARKER"));
    assert!(boot.contains("RPI5_HH_VBAR_HEX_DONE_MARKER"));
    assert!(boot.contains("rpi5_hh_write_line(&RPI5_HH3_PRECHECK_DONE_MARKER)"));
    assert!(boot.contains("rpi5_hh_write_line(&RPI5_HH_REGISTERS_OK_MARKER)"));
    assert!(boot.contains("rpi5_hh_write_line(&RPI5_HH_RUST_UART_OK_MARKER)"));
    assert!(boot.contains("RPI5_HH_PRINT_REGS_FIRST_BEGIN_MARKER"));
    assert!(boot.contains("RPI5_HH_PRINT_REGS_FIRST_HEX_BEGIN_MARKER"));
    assert!(boot.contains("RPI5_HH_PRINT_REGS_FIRST_HEX_DONE_MARKER"));
    assert!(boot.contains("RPI5_HH_PRINT_REGS_SP_BEGIN_MARKER"));
    assert!(boot.contains("RPI5_HH_PRINT_REGS_SP_HEX_BEGIN_MARKER"));
    assert!(boot.contains("RPI5_HH_PRINT_REGS_SP_HEX_DONE_MARKER"));
    assert!(boot.contains("rpi5_hh_write_line(&RPI5_HH3_DONE_MARKER)"));
}

#[test]
fn rpi5_hh4_retires_low_ttbr0_and_hh5_defers_without_eret() {
    let boot = include_str!("../src/arch/aarch64/boot.rs");
    let linker = include_str!("../targets/aarch64-rpi5-stage2-highhalf-none.ld");
    let hh4_start = boot.find("fn rpi5_hh4_retire_low_ttbr0").unwrap();
    let hh5_end = boot[hh4_start..]
        .find("fn yarm_rpi5_hh_rust_continue")
        .unwrap()
        + hh4_start;
    let hh45 = &boot[hh4_start..hh5_end];

    assert!(linker.contains("__hh_empty_ttbr0_root"));
    assert!(hh45.contains("if pc < RPI5_HH_VA_OFFSET"));
    assert!(hh45.contains("if sp < RPI5_HH_VA_OFFSET"));
    assert!(hh45.contains("vbar < RPI5_HH_VA_OFFSET"));
    assert!(hh45.contains("empty_ttbr0_root & 0xfff != 0"));
    assert!(hh45.contains("empty_ttbr0_root == expected_ttbr1"));
    assert!(hh45.contains("\"dsb ishst\""));
    assert!(hh45.contains("\"msr TTBR0_EL1, {root}\""));
    assert!(hh45.contains("\"tlbi vmalle1\""));
    assert!(hh45.contains("\"dsb ish\""));
    assert!(hh45.contains("RPI5_HH4_UART_AFTER_TTBR0_OK_MARKER"));
    assert!(hh45.contains("Rpi5Hh4Ready"));
    assert!(hh45.contains("fn rpi5_hh5_defer(hh4: Rpi5Hh4Ready) -> !"));
    assert!(hh45.contains("high_half_initrd_allocator_bridge_not_ready"));
    assert!(!hh45.contains("core::arch::asm!(\"eret\""));
    assert!(!boot.contains("RPI5_HH5_ENTER_USER_ERET"));
    assert!(!boot.contains("RPI5_HH5_FIRST_USER_TRAP"));
    assert!(!hh45.contains("yarm_log!"));
    assert!(!hh45.contains("printk"));
    assert!(!hh45.contains("scheduler"));
    assert!(!hh45.contains("init_gic"));
    assert!(!hh45.contains("init_rp1"));
    assert!(!hh45.contains("init_pcie"));
}
