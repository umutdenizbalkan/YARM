// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Stage 189B — x86_64 AP user-dispatch readiness state machine + audited
//! wake-only clearing (scaffold).
//!
//! This module is the **single gate** by which an application processor (AP) may
//! ever transition out of the wake-only idle state toward running user tasks. It
//! is pure (no MMIO, no `asm`, no `KernelState`) so the decision logic is
//! unit-tested under `hosted-dev`; the bare-metal driver in [`super::smp`] applies
//! exactly this logic.
//!
//! # Stage 189B scope — scaffold / inert
//!
//! No AP user task is scheduled in this pass. The audited transition
//! [`ApReadiness::evaluate_clear`] REFUSES unless all four readiness conditions
//! hold, and `trap_return_ready` is deliberately **false** in 189B because the
//! live AP user trap-return path (`arch::x86_64::trap::ensure_user_return_cr3`
//! still resolves a global active-ASID and a BSP-tuned return-context stack) is
//! not yet proven per-CPU. The boot audit therefore emits the readiness markers
//! plus an honest `X86_AP_USER_DISPATCH_DEFERRED` and never clears a wake-only
//! bit. Stage 189C flips `trap_return_ready` once the live return path is proven,
//! and only then does the audited transition clear wake-only.
//!
//! # Invariants
//!
//! * Clearing an AP's wake-only bit for dispatch is permitted ONLY through the
//!   audited transition, and ONLY when [`ApReadiness::all_ready`] is true.
//! * The four conditions are independent AND-gated: a single missing bit refuses
//!   the whole transition with a specific [`ClearRefusal`] reason (surfaced, never
//!   hidden).
//! * `tlb_ready` is bound to the Stage 189A **genuine** remote-ACK primitive; a
//!   faked/absent ACK leaves it false and blocks dispatch.

/// Ordered readiness states for an AP transitioning from wake-only idle toward
/// user dispatch. Strictly forward; `UserDispatchEnabled` is the ONLY state in
/// which an AP may run user tasks and is reachable ONLY via the audited
/// transition when every readiness condition holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ApDispatchState {
    /// Idle, interrupt-serviceable, services the TLB mailbox; runs no user task.
    WakeOnly,
    /// The AP dispatcher scaffold is present and wired.
    DispatcherReady,
    /// The per-CPU run queue + admission guards are validated.
    RunQueueReady,
    /// A Stage 189A genuine remote TLB-shootdown ACK is available for this CPU.
    TlbReady,
    /// The live AP user trap-return path is proven (set in Stage 189C).
    TrapReturnReady,
    /// Terminal: the AP may run user tasks. Reached only via the audited clear.
    UserDispatchEnabled,
}

/// Reason a wake-only clear was refused. Every refusal is a visible, specific
/// cause — never a silent success.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClearRefusal {
    DispatcherNotReady,
    RunQueueNotReady,
    TlbNotReady,
    TrapReturnNotReady,
}

impl ClearRefusal {
    /// Stable snake_case reason string for markers.
    pub const fn reason(self) -> &'static str {
        match self {
            ClearRefusal::DispatcherNotReady => "dispatcher_not_ready",
            ClearRefusal::RunQueueNotReady => "run_queue_not_ready",
            ClearRefusal::TlbNotReady => "tlb_not_ready",
            ClearRefusal::TrapReturnNotReady => "trap_return_not_ready",
        }
    }
}

/// The four readiness conditions the audited transition AND-gates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApReadiness {
    /// AP dispatcher scaffold present + wired (this module + admission guards).
    pub dispatcher_ready: bool,
    /// Per-CPU run queue + admission guards validated (wake-only enqueue denial +
    /// least-loaded skip).
    pub run_queue_ready: bool,
    /// Stage 189A genuine remote TLB-shootdown ACK available for this CPU.
    pub tlb_ready: bool,
    /// LIVE AP user trap-return path proven (deferred to Stage 189C).
    pub trap_return_ready: bool,
}

impl ApReadiness {
    /// A readiness with nothing satisfied (the initial wake-only baseline).
    pub const fn none() -> Self {
        Self {
            dispatcher_ready: false,
            run_queue_ready: false,
            tlb_ready: false,
            trap_return_ready: false,
        }
    }

    /// Evaluate the audited wake-only-clear precondition. Returns the FIRST
    /// unmet condition (checked in dependency order) or `Ok(())` when all hold.
    /// This is the sole authority for whether wake-only may be cleared.
    pub const fn evaluate_clear(&self) -> Result<(), ClearRefusal> {
        if !self.dispatcher_ready {
            return Err(ClearRefusal::DispatcherNotReady);
        }
        if !self.run_queue_ready {
            return Err(ClearRefusal::RunQueueNotReady);
        }
        if !self.tlb_ready {
            return Err(ClearRefusal::TlbNotReady);
        }
        if !self.trap_return_ready {
            return Err(ClearRefusal::TrapReturnNotReady);
        }
        Ok(())
    }

    /// True only when EVERY readiness condition holds.
    pub const fn all_ready(&self) -> bool {
        self.evaluate_clear().is_ok()
    }

    /// The highest readiness state reached, for reporting/markers. Forward-only:
    /// a gap at any level caps the reported state at that level.
    pub const fn highest_state(&self) -> ApDispatchState {
        if !self.dispatcher_ready {
            return ApDispatchState::WakeOnly;
        }
        if !self.run_queue_ready {
            return ApDispatchState::DispatcherReady;
        }
        if !self.tlb_ready {
            return ApDispatchState::RunQueueReady;
        }
        if !self.trap_return_ready {
            return ApDispatchState::TlbReady;
        }
        // All readiness bits set: the audited transition MAY promote to
        // UserDispatchEnabled. Until it does, the AP is TrapReturnReady.
        ApDispatchState::TrapReturnReady
    }
}

// ── Marker vocabulary (Stage 189B) ──────────────────────────────────────────
/// The AP dispatcher scaffold (this state machine + audited transition) exists.
pub const MARK_DISPATCHER_SCAFFOLD_READY: &str = "X86_AP_DISPATCHER_SCAFFOLD_READY";
/// The run-queue admission guards (wake-only enqueue denial + least-loaded skip).
pub const MARK_ADMISSION_GUARD_READY: &str = "X86_AP_ADMISSION_GUARD_READY";
/// The AP trap-return structural audit passed (shared BSP path + per-CPU prereqs).
pub const MARK_TRAP_RETURN_AUDIT_OK: &str = "X86_AP_TRAP_RETURN_AUDIT_OK";
/// The Stage 189A genuine remote ACK is available for this CPU's shootdown.
pub const MARK_TLB_READY: &str = "X86_AP_TLB_READY_FOR_DISPATCH";
/// A wake-only clear was NOT performed; carries the refusal reason.
pub const MARK_WAKE_ONLY_CLEAR_DEFERRED: &str = "X86_AP_WAKE_ONLY_CLEAR_DEFERRED";
/// User dispatch was NOT enabled this pass; carries the reason.
pub const MARK_USER_DISPATCH_DEFERRED: &str = "X86_AP_USER_DISPATCH_DEFERRED";

// Live-only markers — emitted ONLY by the audited transition when it actually
// promotes a CPU to UserDispatchEnabled (never in the Stage 189B scaffold pass).
/// The audited transition cleared this CPU's wake-only bit.
pub const MARK_WAKE_ONLY_CLEAR: &str = "X86_AP_WAKE_ONLY_CLEAR";
/// A user task began dispatching on this AP.
pub const MARK_USER_DISPATCH_BEGIN: &str = "X86_AP_USER_DISPATCH_BEGIN";
/// The AP returned to user mode through the validated return path.
pub const MARK_USER_TRAP_RETURN_OK: &str = "X86_AP_USER_TRAP_RETURN_OK";
/// The AP user-dispatch slice completed successfully.
pub const MARK_USER_DISPATCH_DONE: &str = "X86_AP_USER_DISPATCH_DONE";

#[cfg(test)]
mod tests {
    use super::*;

    fn all_true() -> ApReadiness {
        ApReadiness {
            dispatcher_ready: true,
            run_queue_ready: true,
            tlb_ready: true,
            trap_return_ready: true,
        }
    }

    #[test]
    fn baseline_wake_only_refuses_clear() {
        let r = ApReadiness::none();
        assert_eq!(r.evaluate_clear(), Err(ClearRefusal::DispatcherNotReady));
        assert!(!r.all_ready());
        assert_eq!(r.highest_state(), ApDispatchState::WakeOnly);
    }

    #[test]
    fn each_missing_bit_refuses_with_its_own_reason() {
        let mut r = all_true();
        r.dispatcher_ready = false;
        assert_eq!(r.evaluate_clear(), Err(ClearRefusal::DispatcherNotReady));

        let mut r = all_true();
        r.run_queue_ready = false;
        assert_eq!(r.evaluate_clear(), Err(ClearRefusal::RunQueueNotReady));
        assert_eq!(r.highest_state(), ApDispatchState::DispatcherReady);

        let mut r = all_true();
        r.tlb_ready = false;
        assert_eq!(r.evaluate_clear(), Err(ClearRefusal::TlbNotReady));
        assert_eq!(r.highest_state(), ApDispatchState::RunQueueReady);

        let mut r = all_true();
        r.trap_return_ready = false;
        assert_eq!(r.evaluate_clear(), Err(ClearRefusal::TrapReturnNotReady));
        assert_eq!(r.highest_state(), ApDispatchState::TlbReady);
    }

    #[test]
    fn stage_189b_baseline_is_trap_return_not_ready() {
        // The exact 189B scaffold readiness: everything but the live trap-return.
        let r = ApReadiness {
            dispatcher_ready: true,
            run_queue_ready: true,
            tlb_ready: true,
            trap_return_ready: false,
        };
        assert_eq!(r.evaluate_clear(), Err(ClearRefusal::TrapReturnNotReady));
        assert!(!r.all_ready());
        assert_eq!(r.highest_state(), ApDispatchState::TlbReady);
        assert_eq!(
            r.evaluate_clear().unwrap_err().reason(),
            "trap_return_not_ready"
        );
    }

    #[test]
    fn only_all_ready_permits_clear() {
        let r = all_true();
        assert_eq!(r.evaluate_clear(), Ok(()));
        assert!(r.all_ready());
        assert_eq!(r.highest_state(), ApDispatchState::TrapReturnReady);
    }

    #[test]
    fn tlb_not_ready_blocks_even_with_trap_return_ready() {
        // A genuine ACK is mandatory: no AP user task without real TLB coverage.
        let r = ApReadiness {
            dispatcher_ready: true,
            run_queue_ready: true,
            tlb_ready: false,
            trap_return_ready: true,
        };
        assert_eq!(r.evaluate_clear(), Err(ClearRefusal::TlbNotReady));
    }

    #[test]
    fn refusal_reasons_are_stable_strings() {
        assert_eq!(
            ClearRefusal::DispatcherNotReady.reason(),
            "dispatcher_not_ready"
        );
        assert_eq!(
            ClearRefusal::RunQueueNotReady.reason(),
            "run_queue_not_ready"
        );
        assert_eq!(ClearRefusal::TlbNotReady.reason(), "tlb_not_ready");
        assert_eq!(
            ClearRefusal::TrapReturnNotReady.reason(),
            "trap_return_not_ready"
        );
    }
}
