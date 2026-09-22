// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! U9-IRQ-FINAL §1/§2 — **the external-interrupt claim, and the completion that answers it.**
//!
//! # Why this module exists
//!
//! Before IRQ-FINAL, RISC-V had no device-interrupt identity. `decode_trap_context` supplied
//! `stval`, which is architecturally `0` for a supervisor external interrupt, and the PLIC
//! completion refused source `0` as the spec's reserved "no interrupt" encoding. No claim was
//! ever read and no completion was ever issued, so the split delivery route — which works on
//! x86_64 and AArch64 — refused the entire family to the terminal broad acquisition.
//!
//! This module supplies the missing identity. It is deliberately **arch-neutral**: the policy —
//! what a claim can be, when one may be read, and the exactly-once completion discipline — is
//! the same everywhere, and keeping it out of the `riscv64` module (which is compiled only on
//! RISC-V targets) is what lets it be exercised against a controller model on any host.
//!
//! # The one rule this module exists to keep
//!
//! A claim read is **destructive**. Reading the controller's claim register dequeues the
//! highest-priority pending source for a context and marks it *in flight* until the matching
//! completion is written. So:
//!
//! * it is read **once per trap**, by the trap entry owner, and carried — never by a decoder,
//!   which runs several times during one trap; and
//! * whatever is claimed is completed **exactly once**, against the context that produced it,
//!   whatever the routing policy then decides. A leaked claim wedges that context forever; a
//!   completion nobody claimed is a duplicate.
//!
//! [`PlicClaim::in_flight`] is how the second half is made a property of the type rather than of
//! a comment: `NoPending` and `Unavailable` have nothing to hand over, so a caller holding one
//! cannot produce completion arguments at all.

/// The controller context that produced a claim.
///
/// A completion is only valid against the same context, so the claim carries it rather than
/// letting the completion re-read a global that may have been reconfigured in between.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PlicContext {
    pub base: usize,
    pub context_index: usize,
}

/// Why no claim could be read. Both are **settled endings**, not fall-throughs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PlicUnavailableReason {
    /// No controller base/context has been configured for this hart.
    NotConfigured,
    /// The claim/complete register is not mapped under the address space that was active when
    /// the trap was taken, so reading it would raise a supervisor load fault.
    ///
    /// On QEMU virt this is the ordinary state, not an error: the PLIC window (`0x0C00_0000`)
    /// sits below RAM, and the only kernel mapping every user ASID carries is the shared
    /// gigapage at `0x8000_0000`. Naming it is what lets the claim refuse instead of faulting
    /// into `riscv_trap_halt("trap_from_s_mode")`.
    MmioUnreachable,
}

impl PlicUnavailableReason {
    pub fn marker(self) -> &'static str {
        match self {
            Self::NotConfigured => "not_configured",
            Self::MmioUnreachable => "mmio_unreachable_under_entering_satp",
        }
    }
}

/// The result of the one claim read a trap is allowed to perform.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PlicClaim {
    /// A real source was dequeued and is now in flight. It MUST be completed exactly once.
    Claimed { source: u32, context: PlicContext },
    /// The controller had nothing pending for this context — the claim register read `0`, which
    /// the spec reserves for "no interrupt". Nothing is in flight, so nothing may be completed.
    NoPending { context: PlicContext },
    /// No claim was attempted. Nothing is in flight.
    Unavailable { reason: PlicUnavailableReason },
}

impl PlicClaim {
    pub fn marker(self) -> &'static str {
        match self {
            Self::Claimed { .. } => "claimed",
            Self::NoPending { .. } => "no_pending",
            Self::Unavailable { .. } => "unavailable",
        }
    }

    /// The source in flight, if any.
    ///
    /// This is the only way to obtain completion arguments, which is what makes "no-claim
    /// outcomes must not complete anything" structural: a `NoPending` or `Unavailable` trap has
    /// nothing to pass and therefore cannot write a completion.
    pub fn in_flight(self) -> Option<(u32, PlicContext)> {
        match self {
            Self::Claimed { source, context } => Some((source, context)),
            _ => None,
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════════════════════════
// The controller model — for hosted and unit evidence, and labelled as such.
//
// There is no PLIC on a hosted build, and no source is enabled on any build, so the claim and
// completion paths would otherwise be untestable and the exactly-once properties would rest on
// reading the code. The model stands in for the register file: it answers the claim read
// destructively, exactly as the register does, and records completions with their context. That
// makes "one claim per trap" and "one completion per claim, against the claim's own context"
// measurements of executed code.
//
// **Model evidence is not hardware-controller qualification.** It cannot show that a controller
// ever raises a line, nor that the MMIO addresses are right.
// ═════════════════════════════════════════════════════════════════════════════════════════════
#[cfg(any(test, feature = "hosted-dev"))]
pub mod model {
    use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    /// `0` means "no interrupt", exactly as the register does.
    pub(super) static PENDING: AtomicUsize = AtomicUsize::new(0);
    pub(super) static CONFIGURED: AtomicBool = AtomicBool::new(false);
    pub(super) static REACHABLE: AtomicBool = AtomicBool::new(true);
    pub(super) static BASE: AtomicUsize = AtomicUsize::new(0x0C00_0000);
    pub(super) static CONTEXT: AtomicUsize = AtomicUsize::new(1);
    pub(super) static CLAIM_READS: AtomicUsize = AtomicUsize::new(0);
    pub(super) static COMPLETIONS: AtomicUsize = AtomicUsize::new(0);
    pub(super) static LAST_COMPLETED_SOURCE: AtomicUsize = AtomicUsize::new(0);
    pub(super) static LAST_COMPLETED_BASE: AtomicUsize = AtomicUsize::new(0);
    pub(super) static LAST_COMPLETED_CONTEXT: AtomicUsize = AtomicUsize::new(0);

    /// Arm the model with one pending source at `base`/`context`. `source == 0` models a
    /// configured, reachable controller with nothing pending.
    pub fn arm(source: u32, base: usize, context: usize) {
        PENDING.store(source as usize, Ordering::SeqCst);
        BASE.store(base, Ordering::SeqCst);
        CONTEXT.store(context, Ordering::SeqCst);
        CONFIGURED.store(true, Ordering::SeqCst);
        REACHABLE.store(true, Ordering::SeqCst);
    }

    pub fn set_configured(configured: bool) {
        CONFIGURED.store(configured, Ordering::SeqCst);
    }

    pub fn set_reachable(reachable: bool) {
        REACHABLE.store(reachable, Ordering::SeqCst);
    }

    pub fn reset() {
        PENDING.store(0, Ordering::SeqCst);
        CONFIGURED.store(false, Ordering::SeqCst);
        REACHABLE.store(true, Ordering::SeqCst);
        BASE.store(0x0C00_0000, Ordering::SeqCst);
        CONTEXT.store(1, Ordering::SeqCst);
        CLAIM_READS.store(0, Ordering::SeqCst);
        COMPLETIONS.store(0, Ordering::SeqCst);
        LAST_COMPLETED_SOURCE.store(0, Ordering::SeqCst);
        LAST_COMPLETED_BASE.store(0, Ordering::SeqCst);
        LAST_COMPLETED_CONTEXT.store(0, Ordering::SeqCst);
    }

    /// How many times the claim register has been read since the last [`reset`].
    pub fn claim_reads() -> usize {
        CLAIM_READS.load(Ordering::SeqCst)
    }

    /// How many completions have been written since the last [`reset`].
    pub fn completions() -> usize {
        COMPLETIONS.load(Ordering::SeqCst)
    }

    /// `(source, base, context_index)` of the most recent completion.
    pub fn last_completion() -> (u32, usize, usize) {
        (
            LAST_COMPLETED_SOURCE.load(Ordering::SeqCst) as u32,
            LAST_COMPLETED_BASE.load(Ordering::SeqCst),
            LAST_COMPLETED_CONTEXT.load(Ordering::SeqCst),
        )
    }
}

/// **The one claim read.**
///
/// Called exactly once per trap, by the trap entry owner, and never by a decoder.
///
/// The read is skipped — and reported, never silently swallowed — when the controller is not
/// configured, or when its claim/complete register is not mapped under the address space that
/// was active when the trap was taken. Attempting it in either case would fault in S-mode.
pub fn claim_external_interrupt_once() -> PlicClaim {
    #[cfg(any(test, feature = "hosted-dev"))]
    {
        use core::sync::atomic::Ordering;
        if !model::CONFIGURED.load(Ordering::SeqCst) {
            return PlicClaim::Unavailable {
                reason: PlicUnavailableReason::NotConfigured,
            };
        }
        if !model::REACHABLE.load(Ordering::SeqCst) {
            return PlicClaim::Unavailable {
                reason: PlicUnavailableReason::MmioUnreachable,
            };
        }
        let context = PlicContext {
            base: model::BASE.load(Ordering::SeqCst),
            context_index: model::CONTEXT.load(Ordering::SeqCst),
        };
        // Destructive, exactly as the register is: the model hands the source over once, so a
        // second read in the same trap observes an empty controller and a test asserting "one
        // claim" measures the real property rather than a counter.
        let source = model::PENDING.swap(0, Ordering::SeqCst) as u32;
        model::CLAIM_READS.fetch_add(1, Ordering::SeqCst);
        if source == 0 {
            return PlicClaim::NoPending { context };
        }
        PlicClaim::Claimed { source, context }
    }
    #[cfg(not(any(test, feature = "hosted-dev")))]
    {
        hardware_claim_once()
    }
}

#[cfg(all(not(test), not(feature = "hosted-dev"), target_arch = "riscv64"))]
fn hardware_claim_once() -> PlicClaim {
    use crate::arch::riscv64::irq;
    let Some(context) = irq::configured_context() else {
        return PlicClaim::Unavailable {
            reason: PlicUnavailableReason::NotConfigured,
        };
    };
    let addr = irq::claim_complete_register(context);
    if !crate::arch::riscv64::plic::mmio_range_reachable_under_active_satp(
        addr,
        core::mem::size_of::<u32>(),
    ) {
        return PlicClaim::Unavailable {
            reason: PlicUnavailableReason::MmioUnreachable,
        };
    }
    let source = irq::read_claim_register(context);
    if source == 0 {
        return PlicClaim::NoPending { context };
    }
    PlicClaim::Claimed { source, context }
}

/// No external-interrupt controller of this kind on this target, so nothing can be claimed.
#[cfg(all(not(test), not(feature = "hosted-dev"), not(target_arch = "riscv64")))]
fn hardware_claim_once() -> PlicClaim {
    PlicClaim::Unavailable {
        reason: PlicUnavailableReason::NotConfigured,
    }
}

/// **The one completion owner**, written against the context the claim came from.
///
/// It is reached only for a source that is actually in flight, because [`PlicClaim::in_flight`]
/// is the only source of its arguments.
///
/// There is deliberately **no second reachability test** here. The claim proved the register
/// reachable a moment ago, under the same address space this trap still runs in; a check that
/// could refuse the completion half would make the pair separable, which is exactly the way a
/// claim gets leaked.
pub fn complete_external_interrupt_claim(source: u32, context: PlicContext) {
    #[cfg(any(test, feature = "hosted-dev"))]
    {
        use core::sync::atomic::Ordering;
        model::COMPLETIONS.fetch_add(1, Ordering::SeqCst);
        model::LAST_COMPLETED_SOURCE.store(source as usize, Ordering::SeqCst);
        model::LAST_COMPLETED_BASE.store(context.base, Ordering::SeqCst);
        model::LAST_COMPLETED_CONTEXT.store(context.context_index, Ordering::SeqCst);
    }
    #[cfg(all(not(test), not(feature = "hosted-dev"), target_arch = "riscv64"))]
    {
        crate::arch::riscv64::irq::write_claim_completion(context, source);
    }
    #[cfg(all(not(test), not(feature = "hosted-dev"), not(target_arch = "riscv64")))]
    {
        let _ = (source, context);
    }
}
