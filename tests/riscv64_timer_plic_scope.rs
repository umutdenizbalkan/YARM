// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Source-grep scope tests for RISC-V64 timer + PLIC bring-up.
//!
//! These tests pin the conservative contract: the smoke gate accepts
//! either the live `RISCV_TIMER_SMOKE_OK ticks=...` / `RISCV_EXTIRQ_SMOKE_OK
//! source=...` markers OR the explicit deferral markers
//! `RISCV_TIMER_DEFERRED reason=...` / `RISCV_EXTIRQ_DEFERRED reason=...`.
//! The current build is on the deferred path; the strings below are
//! ABI between the Rust kernel and the smoke gate.

#[test]
fn smoke_script_references_official_artifact_paths() {
    let smoke = include_str!("../scripts/qemu-riscv64-core-smoke.sh");
    assert!(
        smoke.contains("build-riscv64/yarm-riscv64.bin"),
        "smoke script must default to the official kernel image path"
    );
    assert!(
        smoke.contains("build-riscv64/initramfs-core.cpio"),
        "smoke script must default to the official initramfs path"
    );
    assert!(
        smoke.contains("-bios"),
        "smoke script must specify -bios for OpenSBI"
    );
    assert!(
        smoke.contains("-machine"),
        "smoke script must pin the QEMU machine"
    );
}

#[test]
fn smoke_script_required_markers_present() {
    let smoke = include_str!("../scripts/qemu-riscv64-core-smoke.sh");
    for marker in [
        "YARM_BOOT_OK",
        "RISCV_KERNEL_BOOT_OK",
        "RISCV_LIVEEEEEEE",
        "RISCV_SYSCALL_ROUNDTRIP_OK",
        "RISCV_USER_RESUMED",
        "INITRAMFS_SRV_ENTRY",
        "DEVFS_SRV_ENTRY",
        "VFS_SRV_ENTRY",
        "VFS_MOUNT_TABLE_READY",
        "RISCV_KERNEL_IDLE_WAITING_FOR_IO reason=no_runnable_task all_services_blocked",
    ] {
        assert!(
            smoke.contains(marker),
            "smoke script missing required marker: {marker}"
        );
    }
}

#[test]
fn smoke_script_reject_patterns_present() {
    let smoke = include_str!("../scripts/qemu-riscv64-core-smoke.sh");
    for reject in [
        "RISCV_EARLY_TRAP",
        "PANIC",
        "FATAL",
        "ASSERT",
        "PAGE_FAULT_UNHANDLED",
        "TRAP_HANDLE failed",
    ] {
        assert!(
            smoke.contains(reject),
            "smoke script missing reject pattern: {reject}"
        );
    }
    assert!(
        smoke.contains("source=missing_dtb"),
        "smoke must enforce no repeated missing-DTB loop"
    );
}

#[test]
fn smoke_script_accepts_timer_live_or_deferred() {
    let smoke = include_str!("../scripts/qemu-riscv64-core-smoke.sh");
    assert!(smoke.contains("RISCV_TIMER_SMOKE_OK ticks="));
    assert!(smoke.contains("RISCV_TIMER_DEFERRED reason="));
}

#[test]
fn smoke_script_accepts_plic_init_or_deferred() {
    let smoke = include_str!("../scripts/qemu-riscv64-core-smoke.sh");
    assert!(smoke.contains("RISCV_PLIC_INIT_DONE"));
    assert!(smoke.contains("RISCV_PLIC_DEFERRED reason="));
}

#[test]
fn smoke_script_accepts_extirq_live_or_deferred() {
    let smoke = include_str!("../scripts/qemu-riscv64-core-smoke.sh");
    assert!(smoke.contains("RISCV_EXTIRQ_SMOKE_OK source="));
    assert!(smoke.contains("RISCV_EXTIRQ_DEFERRED reason="));
}

#[test]
fn smoke_script_supports_smp2_secondary_park_assertion() {
    let smoke = include_str!("../scripts/qemu-riscv64-core-smoke.sh");
    assert!(
        smoke.contains("--smp"),
        "smoke script must accept --smp CLI"
    );
    assert!(
        smoke.contains("RISCV_SECONDARY_HART_PARK hart="),
        "smoke must require RISCV_SECONDARY_HART_PARK when smp>=2"
    );
}

#[test]
fn timer_module_emits_required_markers() {
    let timer = include_str!("../src/arch/riscv64/timer.rs");
    for marker in [
        "RISCV_TIMER_INIT_BEGIN",
        "RISCV_TIMER_FREQ value=",
        "RISCV_TIMER_DEFERRED reason=",
        "RISCV_TIMER_TICK count=",
    ] {
        assert!(
            timer.contains(marker),
            "timer module missing marker: {marker}"
        );
    }
    assert!(
        timer.contains("DEFER_REASON_AUDIT_PENDING"),
        "timer module must expose audit-pending defer reason"
    );
    assert!(
        timer.contains("DEFER_REASON_NO_SBI_TIMER"),
        "timer module must expose no-SBI-Timer defer reason"
    );
}

#[test]
fn timer_init_is_invoked_only_at_idle_safe_point() {
    let boot = include_str!("../src/arch/riscv64/boot.rs");
    let idle_pos = boot
        .find("RISCV_KERNEL_IDLE_WAITING_FOR_IO")
        .expect("idle marker must be emitted in boot.rs");
    let init_pos = boot
        .find("init_timer_after_idle_safe_point")
        .expect("timer init must be wired");
    assert!(
        init_pos > idle_pos,
        "timer init must follow the idle marker, not precede it"
    );
    // The init call must be sequenced after the idle marker by a small
    // gap (same block). 4 KiB is a defensive ceiling on intervening lines.
    assert!(
        init_pos - idle_pos < 4_096,
        "timer init must be in the idle-safe block, not elsewhere in boot.rs"
    );
}

#[test]
fn plic_module_emits_discovery_markers() {
    let plic = include_str!("../src/arch/riscv64/plic.rs");
    for marker in [
        "RISCV_PLIC_DISCOVER_BEGIN",
        "RISCV_PLIC_BASE value=",
        "RISCV_PLIC_CONTEXT value=",
        "RISCV_PLIC_DISCOVER_DONE sources=",
        "RISCV_PLIC_INIT_BEGIN",
        "RISCV_PLIC_THRESHOLD_SET context=",
        "RISCV_PLIC_INIT_DONE",
        "RISCV_EXTIRQ_DEFERRED reason=",
    ] {
        assert!(
            plic.contains(marker),
            "plic module missing marker: {marker}"
        );
    }
}

#[test]
fn no_code_enables_all_plic_sources_blindly() {
    let plic = include_str!("../src/arch/riscv64/plic.rs");
    // The deferred path must not contain a wildcard "enable all sources"
    // sequence. Pinning the literal forms we'd guard against: a loop over
    // every IRQ line that writes the enable register.
    for forbidden in [
        "for source in 0..1024",
        "enable_all_plic_sources",
        "write_plic_enable_all",
    ] {
        assert!(
            !plic.contains(forbidden),
            "plic module must not enable all sources blindly ({forbidden})"
        );
    }
    // Must not write multiple enables — current pass enables zero sources.
    assert!(
        plic.contains("EXTIRQ_ENABLED_SOURCES"),
        "plic module must track external-IRQ enabled-source count"
    );
}

#[test]
fn secondary_harts_still_park() {
    let boot = include_str!("../src/arch/riscv64/boot.rs");
    assert!(
        boot.contains("RISCV_SECONDARY_HART_PARK hart="),
        "secondary-hart park marker must be preserved"
    );
}
