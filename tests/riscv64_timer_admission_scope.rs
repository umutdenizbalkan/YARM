// SPDX-License-Identifier: Apache-2.0

//! Canonical 199E — the RISC-V timer ADMISSION scope, after gate retirement.
//!
//! This file supersedes `riscv64_timer_feature_gate_scope.rs`, and the rename is the point: the
//! `riscv64-timer-irq` cargo feature, `TIMER_IRQ_FEATURE_ENABLED` and `STIE_AUDIT_COMPLETE` are
//! gone, so a file named for a gate would advertise a control that no longer exists.
//!
//! Every assertion that still describes a real invariant is carried over unchanged — the SBI TIME
//! mechanism, the `rdtime` source, the exact CSR bits, STIE-before-SIE ordering, the confinement
//! of the CSR-set helpers, PLIC still deferred, and no SMP scheduler coupling. The assertions that
//! pinned the gate are inverted rather than deleted: they now fail if any gate, dormant fallback
//! or disable knob comes back.

use std::vec::Vec;

const TIMER: &str = include_str!("../src/arch/riscv64/timer.rs");

/// `TIMER` with the module's own `#[cfg(test)]` block removed.
///
/// The retirement guards scan for banned identifiers, and the module's unit tests legitimately
/// name those identifiers as string literals in order to assert their absence. Scanning the raw
/// file would therefore report the guard's own evidence as a violation.
fn timer_code() -> &'static str {
    TIMER
        .split("#[cfg(test)]\nmod tests {")
        .next()
        .expect("the non-test portion of the timer module")
}
const BOOT: &str = include_str!("../src/arch/riscv64/boot.rs");
const CARGO: &str = include_str!("../Cargo.toml");

/// The admission gate is RETIRED — feature, mirror constant, audit flag and deferral reason.
#[test]
fn the_timer_admission_gate_is_fully_retired() {
    assert!(
        !CARGO.contains("riscv64-timer-irq"),
        "the cargo feature must be removed outright, not left as an empty no-op a reader could \
         mistake for a behaviour gate"
    );
    for banned in [
        "TIMER_IRQ_FEATURE_ENABLED",
        "STIE_AUDIT_COMPLETE",
        "timer_irq_feature_disabled",
        "stie_audit_pending",
        "trap_bridge_reentrancy_not_ready",
    ] {
        assert!(
            !timer_code().contains(banned),
            "`{banned}` still exists — the RISC-V timer can still be disabled"
        );
        assert!(
            !BOOT.contains(banned),
            "`{banned}` still referenced from boot — the RISC-V timer can still be disabled"
        );
    }
    // No cfg may gate the arm back off, under any spelling.
    assert!(
        !timer_code().contains("cfg!(feature = \"riscv64-timer-irq\")")
            && !timer_code().contains("#[cfg(feature = \"riscv64-timer-irq\")]"),
        "no cfg may re-introduce the admission gate"
    );
}

/// Only REAL platform/ownership facts may defer the timer. A policy deferral would be the gate
/// wearing a different name.
#[test]
fn only_platform_and_ownership_deferrals_remain() {
    assert!(TIMER.contains(r#"DEFER_REASON_NO_SBI_TIMER: &str = "sbi_time_ext_unavailable""#));
    assert!(TIMER.contains(r#"DEFER_REASON_NOT_BOOT_HART: &str = "not_boot_hart""#));
    assert!(TIMER.contains(r#"DEFER_REASON_ALREADY_ARMED: &str = "already_armed""#));
    let count = TIMER.matches("DEFER_REASON_").count();
    let declarations = TIMER.matches("pub const DEFER_REASON_").count();
    assert_eq!(
        declarations, 3,
        "exactly three deferral reasons may exist (found {declarations}, {count} references)"
    );
}

/// The arm happens at the BOOT safe point, before any user task can run — not at terminal idle.
/// The old arrangement was circular: a CPU-bound first workload never reaches idle.
#[test]
fn the_timer_is_armed_before_the_first_user_task() {
    assert!(
        BOOT.contains("crate::arch::riscv64::timer::arm_timer_at_boot_safe_point()"),
        "boot must arm the timer through the boot safe-point entry point"
    );
    assert!(
        !BOOT.contains("init_timer_after_idle_safe_point"),
        "the idle-first arm must be gone: it could never preempt a CPU-bound first task"
    );
    let arm = BOOT
        .find("timer::arm_timer_at_boot_safe_point()")
        .expect("the arm site");
    let run = BOOT.find("\n    run(kernel);").expect("the boot run call");
    assert!(
        arm < run,
        "the timer must be armed BEFORE control passes to the workload"
    );
    // …and after SharedKernel is installed, so the bridge can reach the kernel when it fires.
    let shared = BOOT
        .find("install_riscv_trap_shared_kernel(shared);")
        .expect("the SharedKernel install");
    assert!(
        shared < arm,
        "SharedKernel must be installed before the arm"
    );
}

/// The boot arm must NOT unmask supervisor interrupts. U-origin delivery rides on privilege
/// rules; enabling `sstatus.SIE` there would make ordinary lock-held S-mode code interruptible.
#[test]
fn the_boot_arm_never_unmasks_supervisor_interrupts() {
    let arm = TIMER
        .split("fn arm_periodic_timer_for_user_delivery() -> Option<&'static str> {")
        .nth(1)
        .expect("the boot arm")
        .split("\n}")
        .next()
        .expect("its body");
    assert!(!arm.contains("set_sstatus_sie()"));
    assert!(!arm.contains("arm_s_mode_timer_boundary()"));
    assert!(arm.contains("set_sie_stie();"));
}

/// STIE before SIE, and the deadline before STIE. Carried over from the retired file, re-anchored
/// on the two functions that now own the two halves.
#[test]
fn live_enable_sequence_orders_deadline_stie_then_sie() {
    let arm = TIMER
        .split("fn arm_periodic_timer_for_user_delivery() -> Option<&'static str> {")
        .nth(1)
        .expect("the boot arm")
        .split("\n}")
        .next()
        .expect("its body");
    let deadline = arm.find("sbi_set_timer(deadline);").expect("deadline");
    let stie = arm.find("set_sie_stie();").expect("stie");
    assert!(
        deadline < stie,
        "the deadline must be programmed before STIE, so the enable bit never stands alone"
    );
    let idle = TIMER
        .split("pub fn reestablish_idle_boundary() {")
        .nth(1)
        .expect("the idle boundary")
        .split("\n}")
        .next()
        .expect("its body");
    let scratch = idle
        .find("set_sscratch_to_trap_stack_top();")
        .expect("sscratch");
    let latch = idle.find("arm_s_mode_timer_boundary();").expect("latch");
    let sie = idle.find("set_sstatus_sie();").expect("sie");
    assert!(
        scratch < latch && latch < sie,
        "the S-origin boundary order must be sscratch -> latch -> unmask"
    );
}

/// The CSR-set helpers may still be reached from EXACTLY TWO places, and both are bounded.
#[test]
fn csr_set_helpers_stay_confined_to_two_call_sites() {
    let csr_calls: Vec<_> = TIMER
        .match_indices("set_sie_stie();")
        .chain(TIMER.match_indices("set_sstatus_sie();"))
        .filter(|(pos, _)| {
            // Ignore mentions inside the module's own test block.
            TIMER[..*pos].rfind("#[cfg(test)]").is_none()
                || TIMER[..*pos].rfind("#[cfg(test)]") < TIMER.find("pub fn tick_count")
        })
        .collect();
    assert!(!csr_calls.is_empty(), "the helpers must be referenced");
    let span = |name: &str| {
        let start = TIMER
            .find(name)
            .unwrap_or_else(|| panic!("{name} must exist"));
        let end = TIMER[start..]
            .find("\nfn ")
            .or_else(|| TIMER[start..].find("\npub fn "))
            .map(|rel| start + rel)
            .unwrap_or(TIMER.len());
        (start, end)
    };
    let arm = span("fn arm_periodic_timer_for_user_delivery()");
    let idle = span("pub fn reestablish_idle_boundary()");
    for (pos, _) in &csr_calls {
        let in_arm = *pos > arm.0 && *pos < arm.1;
        let in_idle = *pos > idle.0 && *pos < idle.1;
        assert!(
            in_arm || in_idle,
            "a CSR-set helper is called outside the boot arm and the audited idle boundary"
        );
    }
    let idle_body = &TIMER[idle.0..idle.1];
    assert!(
        idle_body.contains("if !stie_enabled() {"),
        "the idle boundary must be inert if the boot arm deferred"
    );
}

/// The exact CSR bits, carried over unchanged.
#[test]
fn enable_block_writes_correct_csr_bits() {
    assert!(TIMER.contains("in(reg) 1usize << 5"), "STIE is sie bit 5");
    assert!(
        TIMER.contains("in(reg) 1usize << 1"),
        "SIE is sstatus bit 1"
    );
}

/// Mechanism and clock source, carried over unchanged.
#[test]
fn timer_uses_sbi_time_and_rdtime() {
    assert!(TIMER.contains("RISCV_TIMER_MECHANISM value=sbi_time"));
    assert!(TIMER.contains("pub const SBI_EXT_TIME: usize = 0x5449_4D45;"));
    assert!(TIMER.contains("rdtime {0}"), "rdtime is the clock source");
}

/// Re-arm schedules from a FRESH sample, so a delayed delivery cannot become a catch-up storm.
#[test]
fn rearm_schedules_from_a_fresh_sample() {
    let body = TIMER
        .split("pub fn rearm_periodic_deadline() -> u64 {")
        .nth(1)
        .expect("the re-arm")
        .split("\n}")
        .next()
        .expect("its body");
    assert!(
        body.contains("current_time_value().wrapping_add(DEFAULT_TICK_INTERVAL)"),
        "the next deadline must be sampled fresh, never derived from the missed deadline"
    );
}

/// PLIC external IRQ remains deferred — retiring the TIMER gate does not admit external IRQs.
#[test]
fn plic_external_irq_remains_deferred() {
    let plic = include_str!("../src/arch/riscv64/plic.rs");
    assert!(plic.contains("RISCV_EXTIRQ_DEFERRED reason="));
    assert!(!plic.contains("EXTIRQ_ENABLED_SOURCES.fetch_add"));
}

/// Timer ownership stays boot-hart-only and introduces no SMP scheduler coupling.
#[test]
fn timer_ownership_is_boot_hart_only() {
    for forbidden in [
        "scheduler.bring_up_cpu",
        "online_cpu_count",
        "set_present_cpu_bitmap",
    ] {
        assert!(
            !TIMER.contains(forbidden),
            "timer module must not touch the SMP scheduler: {forbidden}"
        );
    }
    assert!(
        TIMER.contains("current_hart_is_boot_hart()"),
        "the arm must refuse on a non-boot hart"
    );
    let irq = include_str!("../src/arch/riscv64/irq.rs");
    assert!(
        irq.contains("if cpu.0 != crate::arch::platform_constants::BOOTSTRAP_CPU_ID {"),
        "the single re-arm point must refuse any CPU other than the boot hart"
    );
}
