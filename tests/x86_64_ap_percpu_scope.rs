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
//! - GS-base is written BY THE AP with a real `wrmsr`/`rdmsr` verify
//! - each AP loads its OWN GDT and TSS, graded by the admit poll
//! - APs still park (existing env scaffold preserved)
//! - no AP scheduler / userspace / timer / runqueue participation
//! - online_cpus=1 invariant preserved
//!
//! Stage 183 inc.2/inc.3 replaced three deferrals this file was originally written
//! against — the AP now installs a local GDT instead of inheriting the BSP's, loads a real
//! per-CPU TSS instead of deferring it, and performs `wrmsr IA32_GS_BASE` with an `rdmsr`
//! readback compare in its naked entry instead of deferring GS. The guards below assert
//! that landed behaviour; the superseded deferral strings are asserted ABSENT so the
//! contract cannot silently regress to the weaker scaffold.

/// The BSP-side AP bring-up: markers, grading, per-CPU record preparation.
const SMP: &str = include_str!("../src/arch/x86_64/smp.rs");
/// The naked AP entry: the AP's own GS/CR3/GDT/TSS work, in asm.
const TRAMPOLINE: &str = include_str!("../src/arch/x86_64/smp_trampoline.rs");

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
    for marker in [
        "X86_AP_PERCPU_BEGIN cpu=",
        "X86_AP_PERCPU_SLOT_READY cpu={} base=0x{:x} size=0x{:x}",
        "X86_AP_PERCPU_RECORD_READY cpu={} apic_id={} stack=0x{:x}",
        "X86_AP_GS_WRITE_BEGIN cpu={} base=0x{:x}",
        // The GS deferral was replaced by a real AP-side MSR write (Stage 183 inc.2).
        "X86_AP_GS_INIT_BY_AP cpu={} reason=wrmsr_in_ap_entry_graded_by_admit_poll",
        "X86_AP_PERCPU_READY cpu=",
    ] {
        assert!(
            SMP.contains(marker),
            "AP per-CPU scaffold missing marker: {marker}"
        );
    }
    // The superseded deferral must not come back: it claimed the AP entry was asm-only
    // with no MSR write, which is now false.
    assert!(
        !SMP.contains("X86_AP_GS_DEFERRED"),
        "the GS deferral is superseded by a real wrmsr; it must not be re-emitted"
    );
    // The scaffold only REPORTS: the record is prepared before SIPI, so this emitter
    // must not write into a record the AP is concurrently writing through gs:.
    let scaffold = SMP
        .split("fn emit_ap_percpu_scaffold(cpu: CpuId) {")
        .nth(1)
        .expect("the per-CPU scaffold emitter must exist")
        .split("\n}\n")
        .next()
        .expect("bounded body");
    for mutation in ["init_record_for_ap", "set_tss_ptr", "set_idle_task_meta"] {
        assert!(
            !scaffold.contains(mutation),
            "the scaffold emitter must be read-only after SIPI, not call `{mutation}`"
        );
    }
}

#[test]
fn gs_base_write_is_not_faked() {
    // The original fake-ready marker stays banned. It once claimed readiness while the
    // same AP emitted an accurate deferral; readiness is now expressed as X86_AP_GS_OK,
    // and only after the readback compare below actually succeeds.
    assert!(
        !SMP.contains("X86_AP_GS_READY"),
        "X86_AP_GS_READY was a fake-ready marker; readiness is X86_AP_GS_OK after readback"
    );
    // The AP performs a REAL write to IA32_GS_BASE (0xC000_0101) in its naked entry.
    for step in [
        "\"mov ecx, 0xC0000101\"",
        "\"wrmsr\"",
        "\"rdmsr\"",
        // Readback is reconstructed from edx:eax and compared with the value written.
        "\"shl rdx, 32\"",
        "\"or rax, rdx\"",
        "\"cmp r8, rsi\"",
    ] {
        assert!(
            TRAMPOLINE.contains(step),
            "the AP GS-base write must be a real MSR sequence, missing: {step}"
        );
    }
    // A mismatching readback must be a distinct, terminal failure — never silently
    // treated as success.
    assert!(
        TRAMPOLINE.contains("AP_STAGE_GS_MISMATCH: u32 = 254")
            && TRAMPOLINE.contains("rdmsr readback != value written"),
        "a GS readback mismatch must have its own terminal stage code"
    );
    // The BSP grades that result into OK/BAD, and only the OK path records the write.
    assert!(
        SMP.contains("crate::yarm_log!(\"X86_AP_GS_OK cpu={}\", cpu.0);")
            && SMP.contains("super::percpu::mark_gs_base_written(cpu);")
            && SMP.contains("crate::yarm_log!(\"X86_AP_GS_BAD cpu={}\", cpu.0);"),
        "the admit poll must grade the AP's GS verify into OK/BAD"
    );
    // `mark_gs_base_written` is what later gates MARK_PERCPU_GS_BASE_READY, so it must
    // be reached ONLY from the admit-OK arm — never unconditionally.
    let ok_arm = SMP
        .find("crate::yarm_log!(\"X86_AP_GS_OK cpu={}\", cpu.0);")
        .expect("GS OK marker");
    let marked = SMP
        .find("super::percpu::mark_gs_base_written(cpu);")
        .expect("GS write record");
    assert!(
        ok_arm < marked && marked - ok_arm < 200,
        "the GS-base write may only be recorded on the graded-OK path"
    );
    assert_eq!(
        SMP.matches("super::percpu::mark_gs_base_written(cpu);")
            .count(),
        1,
        "exactly one site may record the GS-base write"
    );
}

#[test]
fn percpu_scaffold_initializes_record_with_real_values() {
    // The record is initialized with the AP's REAL stack top. Stage 183 inc.3 moved this
    // into `prepare_trampoline_for_cpu`, i.e. BEFORE SIPI, because the AP concurrently
    // writes env_canary/saved_rsp into the same record through gs: — a BSP write after
    // SIPI would race it.
    assert!(
        SMP.contains("super::percpu::init_record_for_ap(cpu, cpu.0, ap_stack_top(cpu));"),
        "per-CPU record must be initialized with the AP's real stack top"
    );
    let prepare = SMP
        .split("fn prepare_trampoline_for_cpu(kernel: &KernelState, cpu: CpuId) -> Option<usize> {")
        .nth(1)
        .expect("the pre-SIPI preparation function must exist")
        .split("\nfn ")
        .next()
        .expect("bounded body");
    for step in [
        "super::percpu::init_record_for_ap(cpu, cpu.0, ap_stack_top(cpu));",
        "super::percpu::set_tss_ptr(cpu, ap_tss_base);",
    ] {
        assert!(
            prepare.contains(step),
            "the AP record must be prepared before SIPI, missing: {step}"
        );
    }
    // The initialization must precede the SIPI that starts the AP.
    let init_at = prepare
        .find("super::percpu::init_record_for_ap(")
        .expect("record init");
    if let Some(sipi_at) = prepare.find("send_sipi") {
        assert!(
            init_at < sipi_at,
            "the record must be initialized BEFORE the SIPI, or the AP races the BSP"
        );
    }
    // And the record is still read back to confirm what the AP observes.
    assert!(
        SMP.contains("let record = super::percpu::read_record(cpu);"),
        "per-CPU scaffold must read the record back to confirm initialization"
    );
}

#[test]
fn ap_env_scaffold_markers_still_present() {
    // The env smoke contract, as it stands after Stage 183 inc.3. IDT and FPU are still
    // genuinely deferred (the AP parks interrupt-masked and runs no FP code); GDT and TSS
    // are not, and their markers say so.
    for marker in [
        "X86_AP_ENV_BEGIN cpu={} apic_id={}",
        "X86_AP_STACK_READY cpu={} stack=0x{:x}",
        "X86_AP_GDT_READY cpu={} reason=ap_local_gdt_graded_by_admit_poll",
        "X86_AP_IDT_DEFERRED cpu={} reason=interrupts_masked_no_handlers",
        "X86_AP_FPU_DEFERRED cpu={} reason=ap_runs_no_fp_code",
        "X86_AP_ENV_READY cpu=",
        "X86_AP_RUST_PARK cpu={} reason=no_ap_scheduler_yet",
    ] {
        assert!(
            SMP.contains(marker),
            "AP env scaffold marker must remain: {marker}"
        );
    }
    // The superseded claims must not return: the AP no longer shares the BSP GDT, and no
    // longer defers its TSS.
    for superseded in [
        "bsp_gdt_shared_safe_while_ap_masked",
        "X86_AP_TSS_DEFERRED",
        "no_ap_local_tss_required_for_parked_ap",
    ] {
        assert!(
            !SMP.contains(superseded),
            "superseded AP env claim must not be re-emitted: {superseded}"
        );
    }
}

/// Each AP loads its OWN GDT and TSS, and the BSP grades both from evidence the AP wrote.
#[test]
fn ap_installs_local_gdt_and_real_tss() {
    // Prepared per-AP, before SIPI, with the AP's own stack top as rsp0.
    assert!(
        SMP.contains(
            "super::descriptor_tables::prepare_ap_descriptor_tables(cpu.0 as usize, ap_stack_top(cpu))"
        ),
        "each AP must get its own GDT + TSS prepared with its real stack top"
    );
    // The AP itself performs lgdt / CS+SS reload / ltr in the naked entry.
    for step in ["\"lgdt", "\"ltr"] {
        assert!(
            TRAMPOLINE.contains(step),
            "the AP must load its own descriptor tables, missing: {step}"
        );
    }
    // Grading is from AP-written evidence, not assumed.
    assert!(
        SMP.contains("let gdt_ok = (env & ap_env::GDT_LOADED) != 0;")
            && SMP.contains("X86_AP_GDT_LOCAL_OK cpu={} reason=lgdt_plus_kernel_cs_ss_reload"),
        "the per-AP GDT must be graded from the AP's own GDT_LOADED flag"
    );
    // The TSS grade requires the GDT to be loaded AND the BUSY bit that `ltr` itself wrote
    // into this AP's descriptor — a flag alone is not accepted.
    assert!(
        SMP.contains("let tss_ok = gdt_ok && (env & ap_env::TSS_LOADED) != 0 && tss_busy;")
            && SMP.contains(
                "let tss_busy = super::descriptor_tables::ap_tss_descriptor_busy(cpu.0 as usize);"
            ),
        "the per-AP TSS grade must require the ltr-written BUSY bit, not just a flag"
    );
    // Every failure mode is named distinctly rather than collapsing into one verdict.
    for bad in [
        "X86_AP_TSS_BAD cpu={} reason=gdt_not_loaded",
        "X86_AP_TSS_BAD cpu={} reason=ltr_flag_missing",
        "X86_AP_TSS_BAD cpu={} reason=busy_bit_not_set",
    ] {
        assert!(
            SMP.contains(bad),
            "TSS grading must name its failure: {bad}"
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
