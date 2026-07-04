// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Source-grep scope tests for the x86_64 AP per-CPU environment.
//!
//! Updated for the accepted Stage 183 (SMP-LIVE) model. APs are brought
//! **online but WAKE-ONLY**: they set up a real per-CPU environment and idle in
//! a scheduler-owned interruptible loop, but run no dispatcher and execute no
//! user tasks (`dispatching_cpu_count` stays 1). This file pins:
//! - AP env BEGIN/READY bracket markers
//! - the env scaffold no longer *defers* GDT/TSS/GS: they are really loaded and
//!   graded by the admit poll (`X86_AP_GDT_LOCAL_OK` / `X86_AP_TSS_OK` /
//!   `X86_AP_GS_OK`, with `..._BAD` on failure). IDT and FPU remain explicitly
//!   deferred with reason (interrupts masked / AP runs no FP code).
//! - X86_AP_RUST_PARK carries `reason=no_ap_scheduler_yet`
//! - APs run no scheduler dispatch, enter no userspace, arm no LAPIC timer, and
//!   join no runqueue (wake-only)
//! - the early `X86_SMP_STARTUP` summary keeps `online_cpus=1` (real AP
//!   scheduler-online admission is graded separately via `X86_SMP_ONLINE_READY`)

#[test]
fn ap_env_begin_marker_is_emitted_per_cpu() {
    let smp = include_str!("../src/arch/x86_64/smp.rs");
    assert!(
        smp.contains("X86_AP_ENV_BEGIN cpu={} apic_id={}"),
        "AP env scaffold must open with X86_AP_ENV_BEGIN cpu= apic_id="
    );
}

#[test]
fn ap_stack_marker_records_real_stack_top() {
    let smp = include_str!("../src/arch/x86_64/smp.rs");
    assert!(
        smp.contains("X86_AP_STACK_READY cpu={} stack=0x{:x}"),
        "X86_AP_STACK_READY must include the real stack_top address"
    );
    // Sanity: the stack address derives from the deterministic per-CPU
    // ap_stack_top helper so the marker matches what the AP loaded.
    assert!(
        smp.contains("let stack_top = ap_stack_top(cpu);"),
        "emit_ap_env_scaffold must source stack_top from ap_stack_top"
    );
}

#[test]
fn ap_gdt_is_marked_ready_with_explicit_reason() {
    let smp = include_str!("../src/arch/x86_64/smp.rs");
    // Stage 183 inc.3: GDT is now PER-AP (the AP does lgdt + CS/SS reload),
    // graded by the admit poll — no longer a shared-BSP-GDT deferral.
    assert!(
        smp.contains("X86_AP_GDT_READY cpu={} reason=ap_local_gdt_graded_by_admit_poll"),
        "X86_AP_GDT_READY must document that the per-AP GDT is graded by the admit poll"
    );
    assert!(
        smp.contains("X86_AP_GDT_LOCAL_OK cpu={} reason=lgdt_plus_kernel_cs_ss_reload"),
        "the admit poll must grade the real per-AP GDT load (lgdt + CS/SS reload)"
    );
}

#[test]
fn ap_tss_is_really_loaded_and_graded() {
    let smp = include_str!("../src/arch/x86_64/smp.rs");
    // Stage 183 inc.3: the AP now loads a real per-AP TSS (ltr; rsp0 = AP stack
    // top, ISTs zero until an AP IDT exists), graded by the admit poll. The old
    // "TSS deferred for a parked AP" marker must NOT come back.
    assert!(
        !smp.contains("X86_AP_TSS_DEFERRED"),
        "X86_AP_TSS_DEFERRED is obsolete — the AP loads a real per-AP TSS (Stage 183)"
    );
    assert!(
        smp.contains("X86_AP_TSS_OK cpu={} rsp0=0x{:x} busy=1 ist=zero_until_ap_idt"),
        "the admit poll must grade the real per-AP TSS load (ltr busy-bit + rsp0)"
    );
    assert!(
        smp.contains("X86_AP_TSS_BAD cpu="),
        "the TSS grade must have an explicit failure marker"
    );
}

#[test]
fn ap_idt_is_explicitly_deferred() {
    let smp = include_str!("../src/arch/x86_64/smp.rs");
    assert!(
        smp.contains("X86_AP_IDT_DEFERRED cpu={} reason=interrupts_masked_no_handlers"),
        "X86_AP_IDT_DEFERRED must explain why no AP-local IDT is required"
    );
}

#[test]
fn ap_gs_is_really_initialized_and_graded() {
    let smp = include_str!("../src/arch/x86_64/smp.rs");
    // Stage 183: the AP now performs a real GS-base write (WRMSR IA32_GS_BASE by
    // the AP itself) and the admit poll grades it. The old "GS deferred, no
    // per-CPU area" marker must NOT come back, and the grade must be real
    // (X86_AP_GS_OK / X86_AP_GS_BAD), not a faked X86_AP_GS_READY (see
    // ap_gs_ready_is_never_faked below).
    assert!(
        !smp.contains("X86_AP_GS_DEFERRED"),
        "X86_AP_GS_DEFERRED is obsolete — the AP writes a real GS base (Stage 183)"
    );
    assert!(
        smp.contains("X86_AP_GS_OK cpu={}"),
        "the admit poll must grade the real per-AP GS-base write"
    );
    assert!(
        smp.contains("X86_AP_GS_BAD cpu={}"),
        "the GS grade must have an explicit failure marker"
    );
}

// Stage 183 (SMP-LIVE) accepted model: the AP env is really set up and the AP is
// admitted scheduler-online (wake-only). Pin the positive grade/admission markers
// so a regression back to a "deferred / parked" AP env cannot silently pass.
#[test]
fn ap_env_reaches_accepted_live_online_grades() {
    let smp = include_str!("../src/arch/x86_64/smp.rs");
    for marker in [
        "X86_AP_GDT_LOCAL_OK cpu=",
        "X86_AP_TSS_OK cpu=",
        "X86_AP_GS_OK cpu=",
        "X86_AP_LAPIC_OK cpu=",
        "X86_AP_SCHED_PREREQ_OK cpu=",
        "X86_AP_SCHED_ONLINE_OK cpu=",
        "X86_SMP_ONLINE_READY present=",
    ] {
        assert!(
            smp.contains(marker),
            "accepted Stage 183 live-env/admission marker must be present: {marker}"
        );
    }
}

#[test]
fn ap_fpu_is_explicitly_deferred() {
    let smp = include_str!("../src/arch/x86_64/smp.rs");
    assert!(
        smp.contains("X86_AP_FPU_DEFERRED cpu={} reason=ap_runs_no_fp_code"),
        "X86_AP_FPU_DEFERRED must record why FPU init can be deferred for the parked AP"
    );
}

#[test]
fn ap_env_ready_marker_closes_scaffold() {
    let smp = include_str!("../src/arch/x86_64/smp.rs");
    assert!(
        smp.contains("X86_AP_ENV_READY cpu={}"),
        "AP env scaffold must close with X86_AP_ENV_READY"
    );
}

#[test]
fn ap_park_marker_carries_no_scheduler_reason() {
    let smp = include_str!("../src/arch/x86_64/smp.rs");
    assert!(
        smp.contains("X86_AP_RUST_PARK cpu={} reason=no_ap_scheduler_yet"),
        "X86_AP_RUST_PARK must record reason=no_ap_scheduler_yet"
    );
}

#[test]
fn ap_env_scaffold_helper_lives_in_smp_module() {
    let smp = include_str!("../src/arch/x86_64/smp.rs");
    assert!(
        smp.contains("fn emit_ap_env_scaffold(cpu: CpuId)"),
        "AP env scaffold helper must be the single entry point"
    );
}

#[test]
fn smp_startup_summary_keeps_online_cpus_at_one() {
    let smp = include_str!("../src/arch/x86_64/smp.rs");
    assert!(
        smp.contains("X86_SMP_STARTUP started_secondary={} online_cpus=1 present_cpus={}"),
        "X86_SMP_STARTUP must keep online_cpus=1 verbatim"
    );
}

#[test]
fn ap_path_does_not_dispatch_scheduler() {
    let smp = include_str!("../src/arch/x86_64/smp.rs");
    // The AP loop must not call into any production scheduler dispatch.
    // Pin the absence of the dispatch entry points to catch regressions.
    for forbidden in [
        "kernel.dispatch_next_task",
        "scheduler.dispatch_next",
        "yield_current()",
        "enter_dispatched_user_task_if_available",
    ] {
        assert!(
            !smp.contains(forbidden),
            "AP path must not call scheduler dispatch: {forbidden}"
        );
    }
}

#[test]
fn ap_path_does_not_enter_userspace() {
    let smp = include_str!("../src/arch/x86_64/smp.rs");
    for forbidden in ["yarm_x86_64_enter_user", "sysret", "iretq_to_user"] {
        assert!(
            !smp.contains(forbidden),
            "AP path must not enter userspace: {forbidden}"
        );
    }
}

#[test]
fn ap_path_does_not_enable_lapic_timer() {
    let smp = include_str!("../src/arch/x86_64/smp.rs");
    // The AP path must not arm the LAPIC timer. The BSP-side LAPIC arm
    // lives in src/arch/x86_64/irq.rs; the AP code in smp.rs must not
    // reference any of the arming entry points.
    for forbidden in [
        "program_timer_deadline",
        "lapic_timer_arm",
        "init_lapic_timer",
        "LVT_TIMER",
    ] {
        assert!(
            !smp.contains(forbidden),
            "AP path must not arm the LAPIC timer: {forbidden}"
        );
    }
}

#[test]
fn ap_path_does_not_join_runqueue() {
    let smp = include_str!("../src/arch/x86_64/smp.rs");
    for forbidden in [
        "add_to_runqueue",
        "enqueue_runnable",
        "push_runnable_task",
        "scheduler.online_count() += 1",
    ] {
        assert!(
            !smp.contains(forbidden),
            "AP path must not join any runqueue: {forbidden}"
        );
    }
}

#[test]
fn smp1_path_is_unchanged_no_ap_path_runs_when_present_bitmap_is_one() {
    let smp = include_str!("../src/arch/x86_64/smp.rs");
    // The AP loop iterates `present & (1 << cpu)`; under -smp 1 the
    // bitmap is 0x1 and the loop body is skipped for every cpu != BSP.
    // Pin the loop form so the gate cannot regress.
    assert!(
        smp.contains("if (present & (1u64 << cpu.0)) == 0"),
        "AP loop must skip absent CPUs based on the present bitmap"
    );
    assert!(
        smp.contains("if cpu.0 == crate::arch::platform_constants::BOOTSTRAP_CPU_ID"),
        "AP loop must skip the BSP"
    );
}

#[test]
fn ap_env_failure_path_parks_safely() {
    let smp = include_str!("../src/arch/x86_64/smp.rs");
    // The AP_TIMEOUT / RUST_TIMEOUT paths emit a marker and `continue`
    // to the next AP without dispatching scheduler. Pin both forms.
    assert!(
        smp.contains("X86_AP_RUST_TIMEOUT cpu="),
        "AP Rust-online timeout must be reported"
    );
    assert!(
        smp.contains("YARM_SMP_AP_TIMEOUT"),
        "AP trampoline timeout must be reported"
    );
}

#[test]
fn ap_legacy_markers_preserved_for_existing_smoke_grep() {
    let smp = include_str!("../src/arch/x86_64/smp.rs");
    // Existing smoke scripts and doc references match the legacy
    // marker names. Keep them so this scaffold pass is purely additive.
    for legacy in [
        "X86_AP_GDT_TSS_READY",
        "X86_AP_IDT_READY",
        "X86_AP_CPU_LOCAL_READY",
        "X86_AP_ONLINE",
    ] {
        assert!(
            smp.contains(legacy),
            "legacy AP marker must remain: {legacy}"
        );
    }
}

#[test]
fn ap_gs_ready_is_never_faked() {
    let smp = include_str!("../src/arch/x86_64/smp.rs");
    // Unlike the other legacy READY markers, X86_AP_GS_READY must NOT be
    // emitted at all until a real WRMSR IA32_GS_BASE + readback exists.
    // The prior `X86_AP_GS_READY cpu={} reason=no_per_cpu_yet` line was a
    // fake-ready marker that contradicted the accurate
    // `X86_AP_GS_DEFERRED reason=ap_entry_is_asm_only_no_msr_write_yet`
    // emitted moments earlier for the same AP.
    assert!(
        !smp.contains("X86_AP_GS_READY"),
        "X86_AP_GS_READY must not be emitted until a real GS-base write + readback lands"
    );
}
