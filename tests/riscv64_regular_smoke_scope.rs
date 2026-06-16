// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Source-grep contract for RISC-V64 as a *regular* (not orphan) smoke
//! target. These tests pin the build + smoke + matrix scripts'
//! deterministic shape so a future change cannot silently weaken the
//! gate that lets RISC-V join the global kernel-unlocking smoke policy.

const BUILD_SCRIPT: &str = include_str!("../scripts/build-qemu-riscv64-artifacts.sh");
const SMOKE_SCRIPT: &str = include_str!("../scripts/qemu-riscv64-core-smoke.sh");
const MATRIX_SCRIPT: &str = include_str!("../scripts/qemu-riscv64-smoke-matrix.sh");

#[test]
fn build_script_produces_canonical_artifacts() {
    assert!(
        BUILD_SCRIPT.contains("build-riscv64") && BUILD_SCRIPT.contains("yarm-riscv64.bin"),
        "build script must produce the canonical kernel image path"
    );
    assert!(
        BUILD_SCRIPT.contains("initramfs-core.cpio"),
        "build script must produce the canonical initramfs path"
    );
}

#[test]
fn build_script_fails_clearly_when_artifacts_missing() {
    assert!(
        BUILD_SCRIPT.contains("[fail] build-qemu-riscv64-artifacts:"),
        "build script must print an explicit failure summary line"
    );
    assert!(
        BUILD_SCRIPT.contains("size="),
        "build script must report artifact sizes for diagnostic transparency"
    );
}

#[test]
fn smoke_script_supports_smp_matrix_1_to_4() {
    // Per-N case arms cover the full --smp 1/2/3/4 matrix.
    for arm in [
        r#"1) expected_bitmap_hex="0x1""#,
        r#"2) expected_bitmap_hex="0x3""#,
        r#"3) expected_bitmap_hex="0x7""#,
        r#"4) expected_bitmap_hex="0xf""#,
    ] {
        assert!(SMOKE_SCRIPT.contains(arm), "smoke missing case arm: {arm}");
    }
    assert!(
        SMOKE_SCRIPT.contains("--smp)") && SMOKE_SCRIPT.contains("--smp=*)"),
        "smoke must accept both --smp N and --smp=N forms"
    );
    assert!(
        SMOKE_SCRIPT.contains("--timeout)") && SMOKE_SCRIPT.contains("--timeout=*)"),
        "smoke must accept --timeout SECS to override TIMEOUT_SECS"
    );
}

#[test]
fn smoke_script_requires_boot_entry_and_boot_hart_id_markers() {
    for marker in [
        r#""RISCV_BOOT_ENTRY hart=""#,
        r#""RISCV_BOOT_HART_SELECTED hart=""#,
        r#""RISCV_BOOT_HART_ID_STORED hart=""#,
        r#""RISCV_DTB_CPU_SCAN_DONE bitmap=""#,
        r#""RISCV_HART_TOPOLOGY present_cpus=""#,
        r#""RISCV_SCHEDULER_BSP_ONLY online_cpus=1 reason=riscv_smp_scheduler_not_enabled""#,
    ] {
        assert!(
            SMOKE_SCRIPT.contains(marker),
            "smoke required-marker missing: {marker}"
        );
    }
}

#[test]
fn smoke_script_requires_full_service_chain_markers() {
    for marker in [
        r#""RISCV_LIVEEEEEEE""#,
        r#""RISCV_SYSCALL_ROUNDTRIP_OK""#,
        r#""RISCV_USER_RESUMED""#,
        r#""INITRAMFS_SRV_ENTRY""#,
        r#""DEVFS_SRV_ENTRY""#,
        r#""VFS_SRV_ENTRY""#,
        r#""VFS_MOUNT_TABLE_READY""#,
        r#""RAMFS_MOUNT_READY""#,
        r#""VFS_MOUNT_REGISTER_RAMFS_OK""#,
        r#""EXT4_SRV_READY""#,
        r#""VFS_MOUNT_REGISTER_EXT4_OK""#,
        r#""RISCV_KERNEL_IDLE_WAITING_FOR_IO reason=no_runnable_task all_services_blocked""#,
    ] {
        assert!(
            SMOKE_SCRIPT.contains(marker),
            "smoke service-chain marker missing: {marker}"
        );
    }
}

#[test]
fn smoke_script_accepts_timer_plic_extirq_deferred_or_live() {
    // The accept-regex pairs allow either the live OR deferred form, but
    // both halves of every pair must remain present so a partial
    // bring-up can never silently sneak through as a half-match.
    for pair in [
        "RISCV_TIMER_SMOKE_OK ticks=|RISCV_TIMER_DEFERRED reason=",
        "RISCV_PLIC_INIT_DONE|RISCV_PLIC_DEFERRED reason=",
        "RISCV_EXTIRQ_SMOKE_OK source=|RISCV_EXTIRQ_DEFERRED reason=",
    ] {
        assert!(
            SMOKE_SCRIPT.contains(pair),
            "smoke must accept live OR deferred for: {pair}"
        );
    }
}

#[test]
fn smoke_script_rejects_destabilizing_patterns() {
    for reject in [
        "RISCV_EARLY_TRAP",
        r"\bPANIC\b",
        r"\bFATAL\b",
        r"\bASSERT\b",
        "PAGE_FAULT_UNHANDLED",
        "TRAP_HANDLE failed",
        r"Vm\(Full\)",
        r"\boom\b",
        r"\bcapacity\b",
        "RISCV_DTB_CPU_SCAN_FAILED",
    ] {
        assert!(
            SMOKE_SCRIPT.contains(reject),
            "smoke reject pattern missing: {reject}"
        );
    }
}

#[test]
fn smoke_script_rejects_present_cpus_one_under_smp_gt_one() {
    // The per-N bitmap assertion encodes both the expected present_cpus
    // count and the expected bitmap, so a fallback to present_cpus=1
    // under --smp >1 is rejected via mismatch with the expected_bitmap_hex.
    assert!(
        SMOKE_SCRIPT.contains("YARM_BOOT_OK present_cpus=${QEMU_SMP} present_bitmap=${expected_bitmap_hex} online_cpus=1"),
        "smoke must enforce per-N present_cpus/bitmap from --smp value"
    );
}

#[test]
fn smoke_script_rejects_boot_hart_in_secondary_park_list() {
    assert!(
        SMOKE_SCRIPT.contains("appears in RISCV_SECONDARY_HART_PARK list"),
        "smoke must reject the boot hart appearing in the parked list"
    );
}

#[test]
fn smoke_script_no_live_timer_or_plic_irq_required_in_this_pass() {
    // Belt-and-suspenders: confirm the smoke gate still allows the
    // deferred form for timer / PLIC / external IRQ.  Tightening these
    // to live-only requires landing the real bring-up first.
    for deferred in [
        "RISCV_TIMER_DEFERRED reason=",
        "RISCV_PLIC_DEFERRED reason=",
        "RISCV_EXTIRQ_DEFERRED reason=",
    ] {
        assert!(
            SMOKE_SCRIPT.contains(deferred),
            "regular smoke must continue to accept {deferred} until live bring-up lands"
        );
    }
}

#[test]
fn matrix_script_runs_full_smp_matrix() {
    assert!(
        MATRIX_SCRIPT.contains("SMP_MATRIX") && MATRIX_SCRIPT.contains("1 2 3 4"),
        "matrix must default to --smp 1/2/3/4"
    );
    assert!(
        MATRIX_SCRIPT.contains("qemu-riscv64-core-smoke.sh"),
        "matrix must delegate per-N to the canonical smoke script"
    );
    assert!(
        MATRIX_SCRIPT.contains("build-qemu-riscv64-artifacts.sh"),
        "matrix must invoke the build script first (skippable via SKIP_BUILD=1)"
    );
    assert!(
        MATRIX_SCRIPT.contains("SKIP_BUILD"),
        "matrix must allow skipping the build step"
    );
}

#[test]
fn matrix_script_summary_columns_cover_required_axes() {
    for col in [
        "STATUS",
        "SMP",
        "BOOT_HART",
        "PRESENT",
        "BITMAP",
        "ONLINE",
        "IDLE",
        "TIMER",
        "PLIC",
    ] {
        assert!(
            MATRIX_SCRIPT.contains(col),
            "matrix summary missing column: {col}"
        );
    }
}
