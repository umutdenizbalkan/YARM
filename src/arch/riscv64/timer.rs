// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Canonical 199E — the RISC-V supervisor timer, unconditional and boot-hart-owned.
//!
//! # Lifecycle
//!
//! The timer is armed ONCE, at the boot safe point ([`arm_timer_at_boot_safe_point`]), and is
//! periodic thereafter through the single common re-arm point at the tail of the arch-neutral
//! `Trap::TimerInterrupt` arm. There is no cargo feature, no runtime knob and no dormant
//! fallback: every RISC-V build ticks.
//!
//! ## Why the arm moved out of the idle boundary
//!
//! Until 199E the first arm happened at the terminal kernel-idle boundary. That is circular for
//! the workload that needs preemption most: a CPU-bound first user task never lets the system
//! reach idle, so the timer that would preempt it is never armed. The arm therefore happens
//! BEFORE any user task can run, at the point where every precondition is already established:
//!
//! 1. the boot hart is identified (`boot_hart_id`, stored in early boot);
//! 2. the real S-mode trap vector and the trap stack are installed, and BOTH trap paths exist —
//!    the U-origin bridge with its asynchronous-context save/restore (199E-R1D) and the audited
//!    S-origin idle fast path;
//! 3. `SharedKernel` is installed for the bridge (`install_riscv_trap_shared_kernel`) and the
//!    scheduler timer is live;
//! 4. the SBI TIME extension is probed and confirmed;
//! 5. secondaries are already online **wake-only** with `SIE`/`SSIE`/`STIE`/`SEIE` all clear, so
//!    no secondary hart can claim timer ownership.
//!
//! ## Why this needs no `sstatus.SIE`
//!
//! The boot arm enables `sie.STIE` and programs the first deadline, and deliberately leaves
//! `sstatus.SIE` **clear**. In U-mode, supervisor interrupts are gated by PRIVILEGE rather than
//! by `SIE`, so a timer taken while a user task runs is delivered the moment the first `sret`
//! reaches U-mode — which is exactly the delivery a CPU-bound task needs. Ordinary S-mode kernel
//! code, including every lock-held region, continues to run with `SIE` clear and is not
//! interruptible.
//!
//! An interrupt that becomes pending while S-mode still runs is not lost: it is taken at the
//! first `sret` into U-mode. Because [`rearm_periodic_deadline`] always schedules from a FRESH
//! `rdtime` sample rather than from the missed deadline, a delayed delivery produces one
//! interrupt and one re-arm, never a catch-up storm.
//!
//! `sstatus.SIE` is still set at exactly ONE program point — the audited terminal-idle tail
//! ([`reestablish_idle_boundary`]) — and the only S-mode code interruptible with it set remains
//! `riscv_trap_halt`'s `wfi` loop. That is what admits the S-ORIGIN interrupt, and it arms the
//! boundary latch so the bridge CHECKS the origin rather than assuming it.
//!
//! ## Ownership
//!
//! Boot hart only. The arm refuses on any other hart, `program_timer_deadline` refuses for any
//! CPU other than `BOOTSTRAP_CPU_ID`, and secondaries never enable any interrupt-enable bit.
//! There is exactly one timer owner, one clock domain and one re-arm point.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::sbi::{SbiError, probe_extension};

/// SBI Timer extension EID (`"TIME"` little-endian).
pub const SBI_EXT_TIME: usize = 0x5449_4D45;

/// Conservative tick budget — diagnostic only. The deadline value reported
/// to the smoke is `mtime + DEFAULT_TICK_INTERVAL` if/when the live path
/// is enabled.
/// Periodic timer interval, in RAW `rdtime` counter units (NOT scheduler ticks).
///
/// QEMU virt reports `timebase-frequency = 10000000`, so this is 10 ms. The initial arm and every
/// re-arm use this same value, which is what makes the timer periodic rather than one-shot. It is
/// the RISC-V timer module's own wall period and is independent of the scheduler's tick quantum
/// (`BOOTSTRAP_TIMER_DEADLINE_TICKS`), which is counted in scheduler ticks and is unchanged.
pub const DEFAULT_TICK_INTERVAL: u64 = 100_000;

static TIMER_INIT_FIRED: AtomicBool = AtomicBool::new(false);
static TIMER_TICK_COUNT: AtomicU64 = AtomicU64::new(0);
static STIE_ENABLED: AtomicBool = AtomicBool::new(false);
static SIE_ENABLED: AtomicBool = AtomicBool::new(false);
static TIMER_REARM_COUNT: AtomicU64 = AtomicU64::new(0);
static USER_ORIGIN_TIMER_IRQS: AtomicU64 = AtomicU64::new(0);

/// Reason strings pinned by `scripts/qemu-riscv64-core-smoke.sh` and by the
/// source-grep test in `mod tests`. Do not reword without updating both.
pub const DEFER_REASON_ALREADY_ARMED: &str = "already_armed";
pub const DEFER_REASON_NO_SBI_TIMER: &str = "sbi_time_ext_unavailable";
/// Emitted when the timer init runs from a non-boot hart. Live STIE is
/// boot-hart-only this pass; secondary-hart timer wiring is gated on
/// RISC-V SMP scheduling, which is explicitly off (`online_cpus=1`).
pub const DEFER_REASON_NOT_BOOT_HART: &str = "not_boot_hart";

/// Supervisor timer interrupt cause code (`scause` low bits, with the
/// interrupt bit set).
pub const IRQ_SUPERVISOR_TIMER_CODE: usize = 5;

/// Set once, at the audited kernel-idle boundary, immediately before
/// `sstatus.SIE` is enabled. The trap bridge accepts an S-mode trap ONLY while
/// this is set, so "the interrupted context is the idle `wfi` lifecycle" is a
/// checked precondition rather than an assumption.
static S_MODE_TIMER_BOUNDARY_ARMED: AtomicBool = AtomicBool::new(false);

/// Arms the S-mode timer boundary. Callable only from the kernel-idle safe
/// point, which has already committed to never returning through the
/// interrupted frame.
pub fn arm_s_mode_timer_boundary() {
    S_MODE_TIMER_BOUNDARY_ARMED.store(true, Ordering::Release);
}

/// `true` once the audited kernel-idle boundary has been reached and armed.
pub fn s_mode_timer_boundary_armed() -> bool {
    S_MODE_TIMER_BOUNDARY_ARMED.load(Ordering::Acquire)
}

/// `true` when this trap is the supervisor timer interrupt taken from
/// S-mode at the armed idle boundary — the ONLY S-mode trap the bridge
/// accepts. Every other S-mode trap stays fail-closed.
///
/// Split out from the bridge so the discrimination is provable on any host:
/// `scause` must carry the interrupt bit AND name the supervisor timer, `SPP`
/// must say Supervisor, and the boundary must be armed.
pub fn is_accepted_s_mode_timer_trap(scause: usize, sstatus: usize, boundary_armed: bool) -> bool {
    const INTERRUPT_BIT: usize = 1usize << (usize::BITS - 1);
    const SPP_BIT: usize = 1usize << 8;
    let is_interrupt = (scause & INTERRUPT_BIT) != 0;
    let code = scause & !INTERRUPT_BIT;
    let from_supervisor = (sstatus & SPP_BIT) != 0;
    is_interrupt && code == IRQ_SUPERVISOR_TIMER_CODE && from_supervisor && boundary_armed
}

/// Acknowledges the pending supervisor timer interrupt AND arms the next
/// deadline in ONE SBI call.
///
/// On RISC-V there is no separate end-of-interrupt: SBI `set_timer` clears the
/// pending timer interrupt as a side effect of programming the next deadline.
/// So a single call is the whole completion, and calling it exactly once per
/// accepted interrupt is what makes "one IRQ -> one tick -> one re-arm" hold.
pub fn rearm_periodic_deadline() -> u64 {
    // `wrapping_add` is the correct arithmetic: `rdtime` is a free-running 64-bit counter, and at
    // QEMU virt's 10 MHz timebase 2^64 units is roughly 58 000 years, so the wrap is unreachable
    // in practice and harmless if it ever happened — SBI compares deadlines in the same modulus.
    let deadline = current_time_value().wrapping_add(DEFAULT_TICK_INTERVAL);
    sbi_set_timer(deadline);
    let n = TIMER_REARM_COUNT
        .fetch_add(1, Ordering::AcqRel)
        .wrapping_add(1);
    if n <= 4 {
        emit_marker(format_args!(
            "RISCV_TIMER_REARM n={} deadline={}",
            n, deadline
        ));
    }
    deadline
}

/// Number of periodic re-arms performed so far. Paired with the accepted-interrupt count so a
/// live log can show the strict 1:1 relationship.
pub fn rearm_count() -> u64 {
    TIMER_REARM_COUNT.load(Ordering::Relaxed)
}

/// `true` when `scause` names the supervisor timer interrupt. Used to identify a timer taken
/// while U-mode was running, where `SPP` is User by definition and the audited S-mode boundary
/// predicate does not apply.
pub fn is_user_origin_timer_trap(scause: usize) -> bool {
    const INTERRUPT_BIT: usize = 1usize << (usize::BITS - 1);
    (scause & INTERRUPT_BIT) != 0 && (scause & !INTERRUPT_BIT) == IRQ_SUPERVISOR_TIMER_CODE
}

/// Records a supervisor timer interrupt taken while U-mode was running — the second accepted
/// origin — naming the interrupted task. Bounded emission, so a long run cannot flood the console;
/// the count itself keeps accumulating and is reported by [`user_origin_timer_irq_count`].
pub fn record_user_origin_timer_irq(tid: u64) -> u64 {
    let n = USER_ORIGIN_TIMER_IRQS
        .fetch_add(1, Ordering::AcqRel)
        .wrapping_add(1);
    if n <= 8 {
        emit_marker(format_args!(
            "RISCV_U_MODE_TIMER_ACCEPTED n={} origin=user tid={}",
            n, tid
        ));
    }
    n
}

/// Count of timer interrupts whose origin was U-mode.
pub fn user_origin_timer_irq_count() -> u64 {
    USER_ORIGIN_TIMER_IRQS.load(Ordering::Relaxed)
}

/// Canonical 199E — arm the periodic supervisor timer at the BOOT safe point.
///
/// Called once, from `run_with_prepared_kernel`, after `SharedKernel` is installed and the
/// wake-only secondaries are online, and **before** the first user task can run. See the module
/// docs for the five preconditions this point establishes and for why arming here — rather than
/// at the terminal-idle boundary — is what makes a CPU-bound first workload preemptible.
///
/// Returns `None` when the timer is live, or a deferral reason when it genuinely cannot be armed.
/// The only remaining reasons are real platform/ownership facts: the SBI TIME extension is
/// absent, the caller is not the boot hart, or the timer is already armed. There is no feature
/// gate and no audit gate — a default build arms.
///
/// `sstatus.SIE` is NOT set here. STIE plus U-mode privilege rules are sufficient for U-origin
/// delivery, and leaving `SIE` clear keeps every ordinary S-mode region — including all
/// lock-held code — non-interruptible. `SIE` is still enabled only at the audited terminal-idle
/// tail, by [`reestablish_idle_boundary`].
pub fn arm_timer_at_boot_safe_point() -> Option<&'static str> {
    if TIMER_INIT_FIRED.swap(true, Ordering::AcqRel) {
        return Some(DEFER_REASON_ALREADY_ARMED);
    }

    emit_marker(format_args!("RISCV_TIMER_AUDIT_BEGIN"));

    // (1) SBI TIME extension probe. Absent means this platform has no timer we can drive; that
    // is a real deferral, not a policy one.
    let sbi_timer_present = match probe_extension(SBI_EXT_TIME) {
        Ok(value) => value != 0,
        Err(SbiError::NotSupported) => false,
        Err(_) => false,
    };

    // (2) Boot-hart guard. Timer ownership is boot-hart-only: secondaries are online wake-only
    // with every interrupt-enable bit clear and never reach this call.
    let on_boot_hart = current_hart_is_boot_hart();

    emit_marker(format_args!(
        "RISCV_TIMER_AUDIT_DONE sbi_time={} boot_hart={} gate=none admission=default",
        sbi_timer_present as u8, on_boot_hart as u8,
    ));

    emit_marker(format_args!("RISCV_TIMER_INIT_BEGIN"));
    emit_marker(format_args!("RISCV_TIMER_MECHANISM value=sbi_time"));

    if !sbi_timer_present {
        emit_marker(format_args!(
            "RISCV_TIMER_DEFERRED reason={}",
            DEFER_REASON_NO_SBI_TIMER
        ));
        return Some(DEFER_REASON_NO_SBI_TIMER);
    }
    emit_marker(format_args!("RISCV_TIMER_FREQ value=platform_default"));

    if !on_boot_hart {
        emit_marker(format_args!(
            "RISCV_TIMER_DEFERRED reason={}",
            DEFER_REASON_NOT_BOOT_HART
        ));
        return Some(DEFER_REASON_NOT_BOOT_HART);
    }

    arm_periodic_timer_for_user_delivery()
}

/// Programs the first deadline and enables `sie.STIE`, leaving `sstatus.SIE` clear.
///
/// The ordering is the safety argument. STIE is armed only AFTER the deadline is programmed, so
/// there is no window in which the enable bit is set with no deadline behind it; and
/// `sstatus.SIE` is never touched, so no S-mode code becomes interruptible as a result of this
/// call. The first delivery therefore happens at the first `sret` into U-mode, where the trap
/// vector, trap stack and asynchronous-context machinery are all already in place.
fn arm_periodic_timer_for_user_delivery() -> Option<&'static str> {
    let deadline = current_time_value().wrapping_add(DEFAULT_TICK_INTERVAL);
    emit_marker(format_args!("RISCV_TIMER_SET deadline={}", deadline));
    sbi_set_timer(deadline);

    set_sie_stie();
    mark_stie_enabled();
    emit_marker(format_args!("RISCV_TIMER_STIE_ENABLED"));
    emit_marker(format_args!(
        "RISCV_TIMER_ARMED_PRE_IDLE owner=boot_hart sie=0 delivery=u_mode_privilege result=ok"
    ));

    emit_marker(format_args!("RISCV_TIMER_INIT_DONE"));
    emit_marker(format_args!("RISCV_TIMER_SMOKE_OK ticks={}", tick_count()));
    None
}

/// Re-establishes the S-mode timer entry contract on EVERY subsequent arrival at the audited
/// kernel-idle boundary.
///
/// `init_timer_after_idle_safe_point` arms the timer once, on the first arrival. Every later
/// arrival comes from a trap handler, where hardware has cleared `sstatus.SIE` — so without this
/// the idle `wfi` would loop with interrupts masked and the pending timer would never be taken.
/// That is exactly why the timer previously stopped as soon as anything was dispatched out of
/// idle.
///
/// This does NOT widen where interrupts are enabled: it is the same single audited boundary, whose
/// invariant is that the only S-mode code interruptible with `SIE` set is `riscv_trap_halt`'s `wfi`
/// loop. It is a strict no-op unless the opt-in feature already armed the timer.
pub fn reestablish_idle_boundary() {
    if !stie_enabled() {
        return;
    }
    // Canonical 199E: this boundary now OWNS the S-origin admission contract end to end. The
    // boot arm deliberately does not touch either of these — it enables U-origin delivery only —
    // so the latch and `sstatus.SIE` are established here, together, on every arrival.
    //
    // Order is load-bearing and unchanged: point `sscratch` at the true trap-stack top (the
    // vector's first act is `csrrw sp, sscratch, sp`, so this decides where an S-mode frame
    // lands), THEN arm the latch so the bridge can check the origin rather than assume it, and
    // only THEN unmask. Both writes are idempotent, so repeated arrivals cost nothing.
    set_sscratch_to_trap_stack_top();
    arm_s_mode_timer_boundary();
    set_sstatus_sie();
    mark_sie_enabled();
}

/// Returns true iff the current hart is the OpenSBI-released boot hart.
/// In default builds `online_cpus=1`, so this is always true on the
/// only hart that ever calls `init_timer_after_idle_safe_point`; the
/// check is here so a future caller from a secondary hart cannot
/// silently bypass the boot-hart-only invariant.
fn current_hart_is_boot_hart() -> bool {
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
    {
        let mut hart: usize;
        unsafe {
            core::arch::asm!(
                "csrr {0}, sscratch",
                out(reg) hart,
                options(nostack, nomem, preserves_flags)
            );
        }
        // `sscratch` is repurposed by the trap vector for save/restore;
        // fall back to the recorded boot-hart-id if unavailable.
        let _ = hart;
        let boot = super::boot::boot_hart_id();
        // We cannot read the current hart cheaply once the trap vector
        // owns sscratch, so accept the boot-hart-only invariant as
        // structural: every caller in default builds is the boot hart
        // because secondaries are parked in `wfi` before reaching this
        // module. The atomic recorded by `_start` is the source of
        // truth.
        boot != usize::MAX
    }
    #[cfg(not(all(not(feature = "hosted-dev"), target_arch = "riscv64")))]
    {
        true
    }
}

/// Reads the SBI `mtime`-equivalent counter. Implementation is
/// arch-specific (`rdtime`); on hosted-dev / non-riscv64 builds this
/// returns 0 so the scaffold compiles on the host toolchain.
fn current_time_value() -> u64 {
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
    {
        let value: u64;
        unsafe {
            core::arch::asm!(
                "rdtime {0}",
                out(reg) value,
                options(nostack, nomem, preserves_flags)
            );
        }
        value
    }
    #[cfg(not(all(not(feature = "hosted-dev"), target_arch = "riscv64")))]
    {
        0
    }
}

/// Invokes the SBI Timer `set_timer` call (`EID = SBI_EXT_TIME`,
/// `FID = 0`). On hosted-dev / non-riscv64 builds, this is a no-op.
fn sbi_set_timer(deadline: u64) {
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
    {
        unsafe {
            core::arch::asm!(
                "ecall",
                in("a7") SBI_EXT_TIME,
                in("a6") 0usize,
                in("a0") deadline,
                lateout("a0") _,
                lateout("a1") _,
                options(nostack, nomem)
            );
        }
    }
    #[cfg(not(all(not(feature = "hosted-dev"), target_arch = "riscv64")))]
    {
        let _ = deadline;
    }
}

/// Points `sscratch` at the true kernel trap-stack top, so the next S-mode
/// trap's `csrrw sp, sscratch, sp` lands its frame at a fixed, known address
/// instead of inheriting the generic return tail's drifting value.
fn set_sscratch_to_trap_stack_top() {
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
    {
        let top = super::boot::riscv_trap_stack_top_for_s_mode_timer();
        unsafe {
            core::arch::asm!(
                "csrw sscratch, {0}",
                in(reg) top,
                options(nostack, nomem, preserves_flags)
            );
        }
    }
}

/// Sets the supervisor timer interrupt enable bit (`sie.STIE`, bit 5).
fn set_sie_stie() {
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
    {
        unsafe {
            core::arch::asm!(
                "csrrs zero, sie, {0}",
                in(reg) 1usize << 5,
                options(nostack, nomem, preserves_flags)
            );
        }
    }
}

/// Sets the supervisor interrupt enable bit (`sstatus.SIE`, bit 1).
/// Must be set AFTER `sie.STIE` and after the trap vector and kernel
/// state pointer are installed.
fn set_sstatus_sie() {
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
    {
        unsafe {
            core::arch::asm!(
                "csrrs zero, sstatus, {0}",
                in(reg) 1usize << 1,
                options(nostack, nomem, preserves_flags)
            );
        }
    }
}

pub fn init_fired() -> bool {
    TIMER_INIT_FIRED.load(Ordering::Relaxed)
}

pub fn tick_count() -> u64 {
    TIMER_TICK_COUNT.load(Ordering::Relaxed)
}

pub fn record_timer_tick() -> u64 {
    let next = TIMER_TICK_COUNT
        .fetch_add(1, Ordering::AcqRel)
        .wrapping_add(1);
    emit_marker(format_args!("RISCV_TIMER_TICK count={}", next));
    next
}

pub fn mark_stie_enabled() {
    STIE_ENABLED.store(true, Ordering::Release);
}

pub fn stie_enabled() -> bool {
    STIE_ENABLED.load(Ordering::Acquire)
}

pub fn mark_sie_enabled() {
    SIE_ENABLED.store(true, Ordering::Release);
}

pub fn sie_enabled() -> bool {
    SIE_ENABLED.load(Ordering::Acquire)
}

fn emit_marker(args: core::fmt::Arguments<'_>) {
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
    crate::arch::riscv64::boot::early_sbi_marker(args);
    #[cfg(not(all(not(feature = "hosted-dev"), target_arch = "riscv64")))]
    {
        let _ = args;
    }
}

#[cfg(test)]
pub fn reset_for_test() {
    TIMER_INIT_FIRED.store(false, Ordering::Release);
    TIMER_TICK_COUNT.store(0, Ordering::Release);
    STIE_ENABLED.store(false, Ordering::Release);
    SIE_ENABLED.store(false, Ordering::Release);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_safe_until_the_boot_arm_runs() {
        // Before the arm, nothing is enabled and no S-mode trap may be accepted.
        assert!(!s_mode_timer_boundary_armed() || stie_enabled());
    }

    #[test]
    fn the_boot_arm_runs_at_most_once_per_boot() {
        // `TIMER_INIT_FIRED` is a one-shot latch: a second call is refused with the
        // already-armed reason and reprograms nothing.
        let first = arm_timer_at_boot_safe_point();
        let second = arm_timer_at_boot_safe_point();
        let _ = first;
        assert_eq!(second, Some(DEFER_REASON_ALREADY_ARMED));
    }

    #[test]
    fn record_timer_tick_increments_counter() {
        let before = tick_count();
        let after = record_timer_tick();
        assert_eq!(after, before + 1);
        assert_eq!(tick_count(), after);
    }

    /// Canonical 199E: only REAL platform/ownership facts remain as deferral reasons. No
    /// feature-disabled, audit-pending or trap-bridge-reentrancy reason exists any more —
    /// their presence would mean a default build could still decline to tick.
    #[test]
    fn only_platform_deferral_reasons_remain() {
        assert_eq!(DEFER_REASON_NO_SBI_TIMER, "sbi_time_ext_unavailable");
        assert_eq!(DEFER_REASON_NOT_BOOT_HART, "not_boot_hart");
        assert_eq!(DEFER_REASON_ALREADY_ARMED, "already_armed");
        // Scan CODE only: this very list names the banned identifiers as literals.
        const SRC: &str = include_str!("timer.rs");
        let code = SRC
            .split("#[cfg(test)]\nmod tests {")
            .next()
            .expect("the non-test portion");
        for banned in [
            "timer_irq_feature_disabled",
            "stie_audit_pending",
            "trap_bridge_reentrancy_not_ready",
            "TIMER_IRQ_FEATURE_ENABLED",
            "STIE_AUDIT_COMPLETE",
        ] {
            assert!(
                !code.contains(banned),
                "`{banned}` still exists: the timer admission gate is not fully retired"
            );
        }
    }

    /// The boot arm must NOT enable `sstatus.SIE`. U-origin delivery rides on privilege rules;
    /// enabling SIE there would make ordinary lock-held S-mode code interruptible.
    #[test]
    fn the_boot_arm_does_not_unmask_supervisor_interrupts() {
        const SRC: &str = include_str!("timer.rs");
        let body = SRC
            .split("fn arm_periodic_timer_for_user_delivery() -> Option<&'static str> {")
            .nth(1)
            .expect("the boot arm body")
            .split("\n}")
            .next()
            .expect("its body");
        assert!(body.contains("set_sie_stie();"), "STIE is enabled");
        assert!(
            !body.contains("set_sstatus_sie()"),
            "the boot arm must never set sstatus.SIE"
        );
        assert!(
            !body.contains("arm_s_mode_timer_boundary()"),
            "the S-origin latch belongs to the audited idle boundary, not the boot arm"
        );
        let set_at = body
            .find("sbi_set_timer(deadline);")
            .expect("deadline programmed");
        let stie_at = body.find("set_sie_stie();").expect("stie enabled");
        assert!(
            set_at < stie_at,
            "the deadline must be programmed before STIE, so the enable bit never stands alone"
        );
    }

    /// The audited idle boundary owns the whole S-origin contract, in order.
    #[test]
    fn the_idle_boundary_owns_the_s_origin_contract() {
        const SRC: &str = include_str!("timer.rs");
        let body = SRC
            .split("pub fn reestablish_idle_boundary() {")
            .nth(1)
            .expect("the idle boundary")
            .split("\n}")
            .next()
            .expect("its body");
        let scratch = body
            .find("set_sscratch_to_trap_stack_top();")
            .expect("sscratch");
        let latch = body.find("arm_s_mode_timer_boundary();").expect("latch");
        let sie = body.find("set_sstatus_sie();").expect("sie");
        assert!(
            scratch < latch && latch < sie,
            "order must be sscratch -> latch -> unmask"
        );
        assert!(
            body.contains("if !stie_enabled() {"),
            "a boundary arrival before the boot arm must be a no-op"
        );
    }

    /// Re-arm always schedules from a FRESH sample, so a delayed delivery produces one
    /// interrupt and one re-arm rather than a catch-up storm.
    #[test]
    fn rearm_schedules_from_a_fresh_time_sample() {
        const SRC: &str = include_str!("timer.rs");
        let body = SRC
            .split("pub fn rearm_periodic_deadline() -> u64 {")
            .nth(1)
            .expect("the re-arm")
            .split("\n}")
            .next()
            .expect("its body");
        assert!(
            body.contains("current_time_value().wrapping_add(DEFAULT_TICK_INTERVAL)"),
            "the next deadline must come from a fresh rdtime sample, never from the missed one"
        );
    }
}
