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
fn smoke_script_pins_canonical_timer_deferred_reasons() {
    let smoke = include_str!("../scripts/qemu-riscv64-core-smoke.sh");
    // The list of accepted deferred reasons must include every reason
    // the kernel emits; an unknown reason fails the gate.
    // Canonical 199E: the ADMISSION gate is retired, so only real platform/ownership facts may
    // defer the timer. The policy reasons are asserted ABSENT rather than deleted, so a gate
    // cannot be reintroduced by quietly re-adding its reason string to either side.
    let accepted = smoke
        .split("TIMER_DEFERRED_REASONS=(")
        .nth(1)
        .expect("the accepted-reason array")
        .split(')')
        .next()
        .expect("its contents");
    for retired in [
        "timer_irq_feature_disabled",
        "trap_bridge_reentrancy_not_ready",
        "stie_audit_pending",
    ] {
        assert!(
            !accepted.contains(retired),
            "smoke still ACCEPTS retired policy deferral reason: {retired}"
        );
    }
    for reason in ["sbi_time_ext_unavailable", "not_boot_hart", "already_armed"] {
        assert!(
            smoke.contains(&format!("\"{reason}\"")),
            "smoke must list canonical timer-deferred reason: {reason}"
        );
    }
    assert!(
        smoke.contains("RISCV_TIMER_DEFERRED reason=${timer_reason} is not canonical"),
        "smoke must reject unknown timer-deferred reasons"
    );
}

#[test]
fn smoke_script_requires_audit_markers() {
    let smoke = include_str!("../scripts/qemu-riscv64-core-smoke.sh");
    assert!(
        smoke.contains("\"RISCV_TIMER_AUDIT_BEGIN\""),
        "smoke must require RISCV_TIMER_AUDIT_BEGIN"
    );
    assert!(
        smoke.contains("\"RISCV_TIMER_AUDIT_DONE sbi_time=\""),
        "smoke must require RISCV_TIMER_AUDIT_DONE with audit fields"
    );
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
        "RISCV_TIMER_AUDIT_BEGIN",
        "RISCV_TIMER_AUDIT_DONE sbi_time=",
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
    // Canonical 199E: the audit-pending reason is retired along with the admission gate. What
    // replaces it is the positive evidence that the timer armed before any user task ran.
    assert!(
        !timer.contains("DEFER_REASON_AUDIT_PENDING"),
        "the audit-pending deferral is retired with the admission gate"
    );
    assert!(
        timer.contains("RISCV_TIMER_ARMED_PRE_IDLE owner=boot_hart"),
        "timer module must emit the pre-idle boot-arm attestation"
    );
    assert!(
        timer.contains("DEFER_REASON_NO_SBI_TIMER"),
        "timer module must expose no-SBI-Timer defer reason"
    );
    assert!(
        !timer.contains("DEFER_REASON_TRAP_BRIDGE_REENTRANCY"),
        "the trap-bridge-reentrancy deferral is retired: the S-mode fast path it waited for is \
         landed and unconditional"
    );
    assert!(
        timer.contains("DEFER_REASON_NOT_BOOT_HART"),
        "timer module must expose not-boot-hart defer reason for pass 2"
    );
}

#[test]
fn timer_audit_completes_before_any_csr_write() {
    let timer = include_str!("../src/arch/riscv64/timer.rs");
    // AUDIT_BEGIN must precede AUDIT_DONE, and both must precede the
    // first CSR-write call (`set_sie_stie` / `set_sstatus_sie`).
    let audit_begin = timer
        .find("RISCV_TIMER_AUDIT_BEGIN")
        .expect("AUDIT_BEGIN missing");
    let audit_done = timer
        .find("RISCV_TIMER_AUDIT_DONE")
        .expect("AUDIT_DONE missing");
    let arm_call = timer
        .find("arm_periodic_timer_for_user_delivery()")
        .expect("arm fn must exist");
    assert!(
        audit_begin < audit_done,
        "AUDIT_BEGIN must precede AUDIT_DONE"
    );
    assert!(
        audit_done < arm_call,
        "AUDIT_DONE must precede any live-arm call"
    );
}

#[test]
fn timer_boot_hart_only_guard_is_present() {
    let timer = include_str!("../src/arch/riscv64/timer.rs");
    assert!(
        timer.contains("current_hart_is_boot_hart()"),
        "timer module must guard the live path with a boot-hart check"
    );
    assert!(
        timer.contains("DEFER_REASON_NOT_BOOT_HART"),
        "timer module must defer with the not-boot-hart reason on secondaries"
    );
}

#[test]
fn timer_stie_audit_flag_requires_the_s_mode_fast_path() {
    let timer = include_str!("../src/arch/riscv64/timer.rs");
    let boot = include_str!("../src/arch/riscv64/boot.rs");
    // The DEPENDENCY this guard pins is unchanged, but it is no longer conditional on an audit
    // flag: canonical 199E made the timer unconditional, so the S-mode fast path and its
    // mechanical acceptance predicate must exist UNCONDITIONALLY. Without them, every `wfi`
    // would re-enter the bridge as `trap_from_s_mode` on the first idle arrival.
    {
        assert!(
            boot.contains("fn riscv_s_mode_timer_trap("),
            "the unconditional timer requires the S-mode supervisor-timer fast path"
        );
        assert!(
            timer.contains("pub fn is_accepted_s_mode_timer_trap("),
            "nor without the mechanical acceptance predicate the bridge checks"
        );
        assert!(
            boot.contains("yarm_riscv64_s_mode_timer_return"),
            "nor without the dedicated S-mode return that re-points sscratch"
        );
        assert!(
            boot.contains("riscv_trap_halt(\"trap_from_s_mode\")"),
            "and every other S-mode trap must still be fail-closed"
        );
    }
}

/// Canonical 199E INVERTED this case, and the inversion is the delivery.
///
/// The timer used to be armed only at the terminal kernel-idle safe point, which is circular for
/// the workload that most needs preemption: a CPU-bound first user task never reaches idle, so
/// the timer that would preempt it was never armed. The arm now happens at the BOOT safe point,
/// before any user task runs. What the idle boundary still owns is the S-ORIGIN admission
/// contract, not the arm.
#[test]
fn timer_is_armed_at_the_boot_safe_point_not_at_idle() {
    let boot = include_str!("../src/arch/riscv64/boot.rs");
    assert!(
        !boot.contains("init_timer_after_idle_safe_point"),
        "the idle-first arm must be retired: it could never preempt a CPU-bound first task"
    );
    // Asserted STRUCTURALLY rather than by file offset: `run_with_prepared_kernel` sits near the
    // end of boot.rs while the idle block sits above it, so a raw byte comparison would measure
    // source layout instead of execution order.
    let boot_fn = boot
        .split("pub fn run_with_prepared_kernel(run: fn(&mut crate::kernel::boot::KernelState)) {")
        .nth(1)
        .expect("the boot entry point")
        .split("\n}")
        .next()
        .expect("its body");
    let arm_pos = boot_fn
        .find("timer::arm_timer_at_boot_safe_point()")
        .expect("the boot arm must be wired into the boot entry point");
    let run_pos = boot_fn.find("run(kernel);").expect("the workload handoff");
    assert!(
        arm_pos < run_pos,
        "the timer must be armed before control passes to the workload"
    );
    // The idle block still re-establishes the S-origin contract, and arms nothing.
    let idle_block = boot
        .split("RISCV_KERNEL_IDLE_WAITING_FOR_IO")
        .nth(1)
        .expect("the idle block")
        .split("riscv_trap_halt(")
        .next()
        .expect("up to the halt");
    assert!(
        idle_block.contains("timer::reestablish_idle_boundary()"),
        "the idle block must still re-establish the S-origin boundary on every arrival"
    );
    assert!(
        !idle_block.contains("arm_timer_at_boot_safe_point"),
        "the idle block must not arm the timer: that is the retired circular arrangement"
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
fn plic_threshold_write_is_gated_by_mapping_coverage_check() {
    let plic = include_str!("../src/arch/riscv64/plic.rs");
    // The threshold register's physical address is below RAM and is
    // never covered by the single kernel-shared gigapage mapped into the
    // active satp once a user task has been dispatched; the raw MMIO
    // write used to run unconditionally and fault (StoreAMOPageFault).
    // Pin the guard so this cannot silently regress.
    assert!(
        plic.contains("addr_range_covered_by_kernel_shared_mapping"),
        "plic module must check MMIO coverage before writing the threshold register"
    );
    assert!(
        plic.contains("DEFER_REASON_MMIO_UNMAPPED"),
        "plic module must expose the MMIO-unmapped defer reason"
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
