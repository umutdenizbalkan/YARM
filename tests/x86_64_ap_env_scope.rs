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

// ── Late AP breadcrumbs are isolated from structured COM1 ────────────────────────
//
// The late naked-assembly breadcrumbs ('a' persistent IRQ ack, 'd' ack follow-up,
// 'v' IRQ-smoke completion, 'q' scheduler-owned idle, 'z' remote-wake re-entry)
// execute AFTER AP admission, while the BSP is concurrently emitting structured
// `yarm_log!` lines on COM1. Sharing the port let raw bytes splice into the middle
// of marker lines and corrupt them. They now go to the independent x86 debugcon
// port (0xE9), which `arch/x86_64/console.rs` already uses as a secondary channel.
//
// The EARLY ladder is deliberately untouched: it emits as one contiguous run that
// completes before the BSP resumes formatted admission logging, and was never
// observed splitting a structured line.

const TRAMPOLINE: &str = include_str!("../src/arch/x86_64/smp_trampoline.rs");

/// Each late breadcrumb still emits its byte, but selects debugcon first and
/// restores the COM1 port immediately afterwards for the code that assumes `rdx`.
#[test]
fn late_ap_breadcrumbs_go_to_debugcon_and_restore_com1() {
    for (byte, what) in [
        ("0x61", "'a' persistent IRQ ack"),
        ("0x64", "'d' ack follow-up"),
        ("0x76", "'v' IRQ-smoke completion"),
        ("0x71", "'q' scheduler-owned idle"),
        ("0x7A", "'z' remote-wake re-entry"),
    ] {
        let needle = format!("\"mov al, {byte}\"");
        let at = TRAPOLINE_FIND(&needle, what);
        // The debugcon selection must immediately precede the byte load.
        let before = &TRAMPOLINE[..at];
        let prev = before
            .rfind("\"mov dx, ")
            .expect("a port selection precedes the breadcrumb");
        assert!(
            TRAMPOLINE[prev..at].contains("0xE9"),
            "{what}: must select debugcon (0xE9), not COM1"
        );
        // ... and COM1 must be restored right after the `out`.
        let after = &TRAMPOLINE[at..];
        let out = after.find("\"out dx, al\"").expect("the byte is emitted");
        let tail = &after[out..];
        let restore = tail
            .find("\"mov dx, 0x3F8\"")
            .expect("COM1 must be restored after the redirected out");
        assert!(
            restore < 200,
            "{what}: COM1 restore must follow the redirected out immediately"
        );
    }
}

#[allow(non_snake_case)]
fn TRAPOLINE_FIND(needle: &str, what: &str) -> usize {
    TRAMPOLINE
        .find(needle)
        .unwrap_or_else(|| panic!("{what}: breadcrumb byte must still be emitted"))
}

/// The authoritative non-serial evidence for each late breadcrumb is untouched:
/// the stage words, the persistent IRQ acknowledgement, the scheduler-stage
/// mirrors and the wake counters all remain.
#[test]
fn late_ap_breadcrumb_state_publications_are_unchanged() {
    for publication in [
        "\"mov dword ptr gs:[116], 1\"",    // irq_ack (persistent) — 'a'
        "\"mov dword ptr [rdi + 48], 36\"", // IRQ_ACK_WRITTEN — 'd'
        "\"mov dword ptr [rdi + 48], 28\"", // IRQ_SMOKE_DONE — 'v'
        "\"mov dword ptr [rdi + 48], 30\"", // SCHED_IDLE — 'q'
        "\"mov dword ptr gs:[120], 30\"",   // sched_stage mirror — 'q'
        "\"mov dword ptr [rdi + 48], 31\"", // SCHED_WAKE_REENTER — 'z'
        "\"mov dword ptr gs:[120], 31\"",   // sched_stage mirror — 'z'
        "\"add dword ptr [rdi + 132], 1\"", // wake_reenter_out++ — 'z'
        "\"add dword ptr gs:[124], 1\"",    // wake_reenter mirror++ — 'z'
    ] {
        assert!(
            TRAMPOLINE.contains(publication),
            "authoritative publication must remain: {publication}"
        );
    }
}

/// The early breadcrumb ladder still writes to COM1 — it is not the corruption
/// source and must not be redirected.
#[test]
fn early_ap_breadcrumb_ladder_still_uses_com1() {
    // The ladder's first byte is emitted right after the COM1 port is selected.
    let at = TRAMPOLINE
        .find("\"mov al, 0x40\"")
        .expect("'@' entry breadcrumb");
    let before = &TRAMPOLINE[..at];
    let prev = before.rfind("\"mov dx, ").expect("port selection");
    assert!(
        TRAMPOLINE[prev..at].contains("0x3F8"),
        "the early ladder must keep using COM1"
    );
    for byte in ["0x48", "0x56", "0x57", "0x4F", "0x4B", "0x79", "0x75"] {
        assert!(
            TRAMPOLINE.contains(&format!("\"mov al, {byte}\"")),
            "early ladder byte {byte} must remain"
        );
    }
}

/// The AP dispatch and TLB-shootdown regions are untouched by this repair.
#[test]
fn ap_dispatch_and_shootdown_regions_are_unchanged() {
    for anchor in [
        "\"call yarm_x86_ap_user_dispatch_entry\"",
        "\"mov eax, dword ptr gs:[160]\"",  // ap_dispatch_request
        "\"mov r10d, dword ptr gs:[128]\"", // tlb_req_gen
        "\"mov dword ptr gs:[132], r10d\"", // tlb_ack_gen published
        "\"invlpg [rax]\"",
        "\"mov r8d, dword ptr gs:[108]\"", // remote_wake_count
    ] {
        assert!(
            TRAMPOLINE.contains(anchor),
            "must remain unchanged: {anchor}"
        );
    }
}

/// The strict smoke gate carries no reconstruction/normalization helper.
#[test]
fn smoke_gate_uses_strict_literal_matching_only() {
    const COMMON: &str = include_str!("../scripts/qemu-smoke-common.sh");
    const CORE: &str = include_str!("../scripts/qemu-x86_64-core-smoke.sh");
    for forbidden in [
        "log_has_semantic_marker",
        "log_marker_semantic_count",
        "marker_line_matches",
        "icr_written_line_matches",
        "advq",
    ] {
        assert!(
            !COMMON.contains(forbidden) && !CORE.contains(forbidden),
            "no reconstruction helper may remain in the smoke gate: {forbidden}"
        );
    }
    assert!(
        CORE.contains("\"X86_IPI_FIXED_ICR_WRITTEN\" \\"),
        "the ICR marker must be asserted literally again"
    );
}
