// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Source-grep scope tests for the x86_64 AP per-CPU record + GS-base
//! scaffold (Part 1 of the overnight dual task).
//!
//! Pins:
//! - PerCpuRecord layout exists and is stride-stable
//! - per-CPU slot table is bounded by MAX_CPUS
//! - emit_ap_percpu_scaffold helper emits the new markers in the right
//!   sequence
//! - GS-base is DEFERRED (no MSR write yet) with the exact reason
//! - APs still park (existing env scaffold preserved)
//! - no AP scheduler / userspace / timer / runqueue participation
//! - online_cpus=1 invariant preserved

#[test]
fn percpu_module_defines_record_type_and_slot_table() {
    let m = include_str!("../src/arch/x86_64/percpu.rs");
    assert!(
        m.contains("pub struct PerCpuRecord"),
        "PerCpuRecord type must be public"
    );
    assert!(
        m.contains("static mut PER_CPU_SLOTS: PerCpuSlots"),
        "Per-CPU slot table must live in .bss"
    );
    assert!(
        m.contains("pub const MAX_PERCPU_RECORDS: usize"),
        "Per-CPU record count must be a public constant"
    );
}

#[test]
fn percpu_record_layout_is_repr_c_with_pinned_offsets() {
    let m = include_str!("../src/arch/x86_64/percpu.rs");
    assert!(
        m.contains("#[repr(C, align(64))]"),
        "PerCpuRecord must be #[repr(C, align(64))]"
    );
    // The docstring lists the canonical offsets; the runtime test in
    // percpu.rs already verifies them, but pin the documentation here
    // too so reviewers see the layout in the diff.
    for offset_marker in [
        "`0`  : cpu_id",
        "`1`  : apic_id",
        "`8`  : stack_top",
        "`16` : flags",
        "`24` : tss_ptr",
        "`32` : idt_ptr",
        "`40` : scheduler_ptr",
    ] {
        assert!(
            m.contains(offset_marker),
            "PerCpuRecord layout doc must pin offset: {offset_marker}"
        );
    }
}

#[test]
fn record_base_accessor_is_bounded_by_cpu_id() {
    let m = include_str!("../src/arch/x86_64/percpu.rs");
    assert!(
        m.contains("pub fn record_base(cpu: CpuId) -> usize"),
        "record_base accessor must exist"
    );
    assert!(
        m.contains("(cpu.0 as usize).min(MAX_PERCPU_RECORDS - 1)"),
        "record_base must clamp to MAX_PERCPU_RECORDS"
    );
}

#[test]
fn smp_module_emits_percpu_scaffold_before_env_scaffold() {
    let smp = include_str!("../src/arch/x86_64/smp.rs");
    let percpu_pos = smp
        .find("emit_ap_percpu_scaffold(cpu);")
        .expect("emit_ap_percpu_scaffold must be invoked");
    let env_pos = smp
        .find("emit_ap_env_scaffold(cpu);")
        .expect("emit_ap_env_scaffold must be invoked");
    assert!(
        percpu_pos < env_pos,
        "per-CPU scaffold must run before the env scaffold so GS-base \
         marker reflects the real per-CPU area"
    );
}

#[test]
fn ap_percpu_scaffold_emits_required_marker_set() {
    let smp = include_str!("../src/arch/x86_64/smp.rs");
    for marker in [
        "X86_AP_PERCPU_BEGIN cpu=",
        "X86_AP_PERCPU_SLOT_READY cpu={} base=0x{:x} size=0x{:x}",
        "X86_AP_PERCPU_RECORD_READY cpu={} apic_id={} stack=0x{:x}",
        "X86_AP_GS_WRITE_BEGIN cpu={} base=0x{:x}",
        "X86_AP_GS_DEFERRED cpu={} reason=ap_entry_is_asm_only_no_msr_write_yet",
        "X86_AP_PERCPU_READY cpu=",
    ] {
        assert!(
            smp.contains(marker),
            "AP per-CPU scaffold missing marker: {marker}"
        );
    }
}

#[test]
fn gs_base_write_is_not_faked() {
    let smp = include_str!("../src/arch/x86_64/smp.rs");
    // The new READY marker form (carrying base=0x...) signals that a
    // real WRMSR IA32_GS_BASE + RDMSR readback succeeded. It must
    // remain absent until that path lands. The legacy
    // `X86_AP_GS_READY ... reason=no_per_cpu_yet` form is preserved
    // for backward-compat and does NOT imply a real write.
    assert!(
        !smp.contains("X86_AP_GS_READY cpu={} base=0x{:x}"),
        "X86_AP_GS_READY base=0x... must not be emitted until WRMSR + readback land"
    );
    // The deferral reason must specifically name the asm-only gap so a
    // regression cannot silently regress to the weaker prior reason.
    assert!(
        smp.contains("reason=ap_entry_is_asm_only_no_msr_write_yet"),
        "GS deferral must record the asm-only blocker reason"
    );
}

#[test]
fn percpu_scaffold_initializes_record_with_real_values() {
    let smp = include_str!("../src/arch/x86_64/smp.rs");
    assert!(
        smp.contains("super::percpu::init_record_for_ap(cpu, cpu.0, stack_top);"),
        "per-CPU scaffold must initialize the AP record with the real stack_top"
    );
    assert!(
        smp.contains("let record = super::percpu::read_record(cpu);"),
        "per-CPU scaffold must read the record back to confirm initialization"
    );
}

#[test]
fn ap_env_scaffold_markers_still_present() {
    let smp = include_str!("../src/arch/x86_64/smp.rs");
    // The previous-pass scaffold markers must remain so the existing
    // env smoke contract is unchanged.
    for marker in [
        "X86_AP_ENV_BEGIN cpu={} apic_id={}",
        "X86_AP_STACK_READY cpu={} stack=0x{:x}",
        "X86_AP_GDT_READY cpu={} reason=bsp_gdt_shared_safe_while_ap_masked",
        "X86_AP_TSS_DEFERRED cpu={} reason=no_ap_local_tss_required_for_parked_ap",
        "X86_AP_IDT_DEFERRED cpu={} reason=interrupts_masked_no_handlers",
        "X86_AP_FPU_DEFERRED cpu={} reason=ap_runs_no_fp_code",
        "X86_AP_ENV_READY cpu=",
        "X86_AP_RUST_PARK cpu={} reason=no_ap_scheduler_yet",
    ] {
        assert!(
            smp.contains(marker),
            "AP env scaffold marker must remain: {marker}"
        );
    }
}

#[test]
fn ap_path_does_not_dispatch_scheduler() {
    let smp = include_str!("../src/arch/x86_64/smp.rs");
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
    for forbidden in ["add_to_runqueue", "enqueue_runnable", "push_runnable_task"] {
        assert!(
            !smp.contains(forbidden),
            "AP path must not join any runqueue: {forbidden}"
        );
    }
}

#[test]
fn online_cpus_summary_is_unchanged() {
    let smp = include_str!("../src/arch/x86_64/smp.rs");
    assert!(
        smp.contains("X86_SMP_STARTUP started_secondary={} online_cpus=1 present_cpus={}"),
        "production scheduler online_cpus must remain pinned at 1"
    );
    assert!(
        smp.contains("X86_SMP_OBSERVATION_OK rust_aps={} scheduler_aps=0"),
        "scheduler_aps=0 invariant must remain"
    );
}

#[test]
fn smp1_path_is_unchanged_no_ap_path_runs_when_present_bitmap_is_one() {
    let smp = include_str!("../src/arch/x86_64/smp.rs");
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
fn percpu_module_is_listed_in_arch_mod() {
    let m = include_str!("../src/arch/x86_64/mod.rs");
    assert!(
        m.contains("pub mod percpu;"),
        "src/arch/x86_64/mod.rs must export the percpu module"
    );
}
