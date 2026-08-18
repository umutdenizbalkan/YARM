// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Source-grep scope tests for the RISC-V64 timer-IRQ feature gate
//! (Part 2 of the overnight dual task).
//!
//! Pins:
//! - `riscv64-timer-irq` cargo feature is declared.
//! - default path keeps STIE/SIE disabled.
//! - feature-on path is gated behind a further `STIE_AUDIT_COMPLETE`
//!   constant so the live-enable sequence is reachable only after the
//!   trap-bridge re-entrancy audit lands.
//! - live-enable sequence orders STIE before SIE.
//! - default path emits the explicit feature-disabled deferral reason.
//! - PLIC / external-IRQ delivery remains deferred.

#[test]
fn cargo_toml_declares_riscv64_timer_irq_feature() {
    let cargo = include_str!("../Cargo.toml");
    assert!(
        cargo.contains("riscv64-timer-irq = []"),
        "Cargo.toml must declare the riscv64-timer-irq feature"
    );
    // The feature must be documented inline so reviewers see the
    // default-OFF policy at the declaration site.
    assert!(
        cargo.contains("Default OFF"),
        "Cargo.toml feature comment must record the default-OFF policy"
    );
}

#[test]
fn timer_module_records_feature_and_audit_constants() {
    let timer = include_str!("../src/arch/riscv64/timer.rs");
    assert!(
        timer.contains(
            "pub const TIMER_IRQ_FEATURE_ENABLED: bool = cfg!(feature = \"riscv64-timer-irq\");"
        ),
        "TIMER_IRQ_FEATURE_ENABLED must mirror the cargo feature"
    );
    // 199E-R1: the trap-bridge re-entrancy audit HAS landed and its invariant is live-proven
    // (`sstatus.SIE` is set at exactly one program point, and the only S-mode code interruptible
    // with it set is the idle `wfi` loop), so this flag is now true. The default-OFF protection
    // does not weaken — it moves entirely onto the cargo feature, which
    // `timer_feature_is_declared_default_off` above and
    // `riscv64_s_mode_timer_bridge::the_timer_feature_remains_default_off` both pin.
    assert!(
        timer.contains("pub const STIE_AUDIT_COMPLETE: bool = true;"),
        "STIE_AUDIT_COMPLETE is true once the audit lands; default builds stay off via the feature"
    );
    assert!(
        timer.contains("pub fn s_mode_timer_boundary_armed()")
            && timer.contains("pub fn is_accepted_s_mode_timer_trap("),
        "the audit's mechanical acceptance predicate must exist alongside the flag"
    );
}

#[test]
fn timer_default_path_emits_feature_disabled_deferral() {
    let timer = include_str!("../src/arch/riscv64/timer.rs");
    assert!(
        timer.contains("DEFER_REASON_FEATURE_DISABLED: &str = \"timer_irq_feature_disabled\""),
        "feature-disabled deferral reason constant must be pinned"
    );
    assert!(
        timer.contains("RISCV_TIMER_DEFERRED reason="),
        "default path must emit RISCV_TIMER_DEFERRED"
    );
}

#[test]
fn timer_feature_on_path_emits_feature_enabled_marker() {
    let timer = include_str!("../src/arch/riscv64/timer.rs");
    assert!(
        timer.contains("RISCV_TIMER_IRQ_FEATURE_ENABLED"),
        "feature-on path must emit RISCV_TIMER_IRQ_FEATURE_ENABLED breadcrumb"
    );
}

#[test]
fn live_enable_sequence_orders_stie_before_sie() {
    let timer = include_str!("../src/arch/riscv64/timer.rs");
    let stie_pos = timer
        .find("RISCV_TIMER_STIE_ENABLED")
        .expect("STIE marker must exist");
    let sie_pos = timer
        .find("RISCV_TIMER_SIE_ENABLED")
        .expect("SIE marker must exist");
    assert!(
        stie_pos < sie_pos,
        "sie.STIE must be enabled before sstatus.SIE in the live-enable path"
    );
    // 199E-R1: scope the ordering to the live-enable function itself. `reestablish_idle_boundary`
    // also calls `set_sstatus_sie()` — it re-enables SIE on every later arrival at the SAME audited
    // idle boundary — and it is gated on `stie_enabled()`, so it can only run after this ordered
    // sequence already armed STIE. Searching the whole file would find that call first.
    let arm = timer
        .split("fn arm_one_shot_timer_and_enable()")
        .nth(1)
        .expect("the live-enable path");
    let set_stie_pos = arm
        .find("set_sie_stie();")
        .expect("set_sie_stie call must exist");
    let set_sie_pos = arm
        .find("set_sstatus_sie();")
        .expect("set_sstatus_sie call must exist");
    assert!(
        set_stie_pos < set_sie_pos,
        "set_sie_stie must be called before set_sstatus_sie"
    );
}

#[test]
fn live_enable_path_is_gated_behind_audit_complete() {
    let timer = include_str!("../src/arch/riscv64/timer.rs");
    // The arm_one_shot_timer_and_enable call sits behind an
    // `if !STIE_AUDIT_COMPLETE { return defer; }` guard so the live
    // sequence is unreachable until the audit flips the constant.
    let guard_pos = timer
        .find("if !STIE_AUDIT_COMPLETE")
        .expect("STIE_AUDIT_COMPLETE guard must exist");
    let arm_pos = timer
        .find("arm_one_shot_timer_and_enable()")
        .expect("arm_one_shot_timer_and_enable call must exist");
    assert!(
        guard_pos < arm_pos,
        "live arm call must be preceded by the STIE_AUDIT_COMPLETE guard"
    );
}

#[test]
fn timer_set_uses_sbi_time_extension() {
    let timer = include_str!("../src/arch/riscv64/timer.rs");
    assert!(
        timer.contains("fn sbi_set_timer(deadline: u64)"),
        "SBI set_timer wrapper must be defined"
    );
    assert!(
        timer.contains("in(\"a7\") SBI_EXT_TIME"),
        "SBI set_timer ecall must pass SBI_EXT_TIME in a7"
    );
}

#[test]
fn current_time_value_uses_rdtime_csr_on_riscv64() {
    let timer = include_str!("../src/arch/riscv64/timer.rs");
    assert!(
        timer.contains("rdtime {0}"),
        "current_time_value must use rdtime on riscv64"
    );
}

#[test]
fn live_enable_block_writes_correct_csr_bits() {
    let timer = include_str!("../src/arch/riscv64/timer.rs");
    // STIE is bit 5 of sie.
    assert!(
        timer.contains("in(reg) 1usize << 5"),
        "STIE bit (1 << 5) must be set in sie"
    );
    // SIE is bit 1 of sstatus.
    assert!(
        timer.contains("in(reg) 1usize << 1"),
        "SIE bit (1 << 1) must be set in sstatus"
    );
}

#[test]
fn default_build_does_not_enable_csrs() {
    let timer = include_str!("../src/arch/riscv64/timer.rs");
    // The csr-set helpers only run from arm_one_shot_timer_and_enable,
    // which is gated behind STIE_AUDIT_COMPLETE. Pin the literal so the
    // gate cannot be silently bypassed by an unguarded call.
    let csr_calls: Vec<_> = timer
        .match_indices("set_sie_stie();")
        .chain(timer.match_indices("set_sstatus_sie();"))
        .collect();
    assert!(
        !csr_calls.is_empty(),
        "csr-set helpers must be referenced by the live-enable path"
    );
    // 199E-R1: the CSR-set helpers may be reached from EXACTLY TWO places, and both are gated.
    //
    //  * `arm_one_shot_timer_and_enable` — the live-enable path, itself behind the cargo feature
    //    and `STIE_AUDIT_COMPLETE`; and
    //  * `reestablish_idle_boundary` — which re-enables `sstatus.SIE` on every LATER arrival at the
    //    same audited idle boundary (hardware cleared it on the intervening trap entry, so without
    //    this the idle `wfi` would loop masked and the timer would stop after the first dispatch).
    //    It is gated on `stie_enabled()`, so it can only ever run after the live-enable path
    //    already armed STIE — it cannot be a bypass.
    //
    // Anything else calling them would be an unguarded enable, which is what this guard forbids.
    let span = |name: &str| {
        let start = timer
            .find(name)
            .unwrap_or_else(|| panic!("{name} must exist"));
        let end = timer[start..]
            .find("\nfn ")
            .or_else(|| timer[start..].find("\npub fn "))
            .map(|rel| start + rel)
            .unwrap_or(timer.len());
        (start, end)
    };
    let arm = span("fn arm_one_shot_timer_and_enable()");
    let idle = span("pub fn reestablish_idle_boundary()");
    for (pos, _) in &csr_calls {
        let in_arm = *pos > arm.0 && *pos < arm.1;
        let in_idle = *pos > idle.0 && *pos < idle.1;
        assert!(
            in_arm || in_idle,
            "csr-set helper call must live inside arm_one_shot_timer_and_enable or the gated \
             reestablish_idle_boundary"
        );
    }
    // And the idle re-establishment must actually carry its gate.
    let idle_body = &timer[idle.0..idle.1];
    assert!(
        idle_body.contains("if !stie_enabled() {"),
        "reestablish_idle_boundary must be inert unless the live-enable path already armed STIE"
    );
}

#[test]
fn plic_external_irq_remains_deferred_alongside_timer_feature() {
    let plic = include_str!("../src/arch/riscv64/plic.rs");
    assert!(
        plic.contains("RISCV_EXTIRQ_DEFERRED reason="),
        "PLIC external-IRQ must remain deferred when timer feature lands"
    );
    // No source may be enabled — same invariant as the prior pass.
    assert!(
        !plic.contains("EXTIRQ_ENABLED_SOURCES.fetch_add"),
        "no PLIC source may be enabled in this pass"
    );
}

#[test]
fn timer_module_does_not_introduce_smp_scheduler_changes() {
    let timer = include_str!("../src/arch/riscv64/timer.rs");
    // The timer module must not directly hook the RISC-V SMP scheduler;
    // multi-hart timer-IRQ delivery is explicitly out of scope.
    for forbidden in [
        "scheduler.bring_up_cpu",
        "online_cpu_count",
        "set_present_cpu_bitmap",
    ] {
        assert!(
            !timer.contains(forbidden),
            "timer module must not touch SMP scheduler: {forbidden}"
        );
    }
}
