// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Stage 189A — x86_64 real cross-CPU TLB-shootdown ACK coordinator.
//!
//! This module is the **single source of truth** for the shootdown decision
//! logic and the acknowledgement state machine. It is pure (no MMIO, no `asm`,
//! no `KernelState`) so it is unit-tested under `hosted-dev`, and the same logic
//! drives the bare-metal IPI path in [`super::smp`]. The genuine remote
//! acknowledgement itself is produced by the target AP in the per-CPU mailbox
//! asm handler (`smp_trampoline.rs`, `gs:[132]` — `tlb_ack_gen`); this module
//! never writes an AP's ack and never fabricates one.
//!
//! # Invariants (Stage 189A)
//!
//! * A remote ACK is produced **only** by the target CPU after it handles the
//!   shootdown and performs the local invalidation. The initiator (BSP) may only
//!   *observe* a target-published ack generation; it can never advance it.
//! * The BSP cannot self-ack on behalf of an AP: there is no initiator-side
//!   method that writes `ack_gen`. [`GenTracker::observe_target_ack`] is the only
//!   writer, and it models the AP-owned mailbox write.
//! * Offline CPUs and the BSP are never in the target set; wake-only online APs
//!   are (they service the mailbox IPI in their managed sched-idle loop — see the
//!   policy note on [`ipi_capable_targets`]).
//! * Acknowledgement uses a **generation**, not a boolean: a stale ack from an
//!   earlier shootdown is observably `!=` the current request generation, so a
//!   late/lost ack cannot masquerade as a fresh success.
//! * A missing ack is a **visible timeout** ([`AckOutcome::TimedOut`]), never
//!   silently treated as success.

use crate::arch::platform_constants::MAX_CPUS;
use crate::arch::platform_layout::BOOTSTRAP_CPU_ID;

/// Emitted once, when the shootdown IPI vector/path is confirmed ready.
pub const MARK_IPI_READY: &str = "X86_TLB_SHOOTDOWN_IPI_READY";
/// Emitted by the initiator when it posts a request + sends the IPI to a target.
pub const MARK_SEND: &str = "X86_TLB_SHOOTDOWN_SEND";
/// Emitted for a target once its genuine (AP-published) ack is first observed —
/// attests the target handled the IPI and performed the invalidation.
pub const MARK_HANDLE: &str = "X86_TLB_SHOOTDOWN_HANDLE";
/// Emitted for a target when its acknowledgement generation is recorded.
pub const MARK_ACK: &str = "X86_TLB_SHOOTDOWN_ACK";
/// Terminal success marker: `result=ok` (remote round-trip) or `result=bsp_local`
/// (no valid remote target under the current wake-only topology).
pub const MARK_DONE: &str = "X86_TLB_SHOOTDOWN_DONE";
/// Emitted when a shootdown is intentionally not driven remotely (e.g. no target).
pub const MARK_DEFERRED: &str = "X86_TLB_SHOOTDOWN_DEFERRED";
/// Emitted on a real failure (e.g. ack timeout). Never emitted on success.
pub const MARK_FAIL: &str = "X86_TLB_SHOOTDOWN_FAIL";

/// Classification of a shootdown request's real target set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShootdownTargets {
    /// No valid remote target (current wake-only topology collapses to a local
    /// flush on the initiator). The honest terminal marker is
    /// `X86_TLB_SHOOTDOWN_DONE result=bsp_local`.
    BspLocal,
    /// One or more remote CPUs must be IPI'd and must each publish a genuine ACK.
    Remote(u64),
}

/// The set of remote CPUs able to receive **and** handle the shootdown IPI right
/// now.
///
/// Policy (Stage 189A): `online & wake_only & !bsp`.
///
/// * Offline CPUs are excluded because they are not in `online` — they cannot
///   receive the IPI and must never be waited on.
/// * The BSP (the initiator) is excluded — it flushes locally, it does not IPI
///   itself.
/// * Wake-only online APs ARE included: they run no user dispatcher yet, but they
///   DO service the per-CPU TLB mailbox in their managed sched-idle loop, so they
///   produce a genuine remote ACK. They idle on the kernel CR3 and hold no user
///   ASID, so invalidating any VA on them is correct-and-conservative (over-
///   invalidation is always safe). When a future AP dispatcher clears an AP's
///   wake-only bit, that CPU is a *dispatching* CPU and joins the precise
///   per-ASID target set computed by the VM layer instead — this function is the
///   IPI-capability filter, not the per-ASID liveness filter.
pub fn ipi_capable_targets(online: u64, wake_only: u64, bsp_id: u8) -> u64 {
    let bsp = 1u64 << bsp_id;
    online & wake_only & !bsp
}

/// Classify a computed target mask into the honest terminal shape.
pub fn classify(targets: u64) -> ShootdownTargets {
    if targets == 0 {
        ShootdownTargets::BspLocal
    } else {
        ShootdownTargets::Remote(targets)
    }
}

/// Outcome of waiting for a single target's acknowledgement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AckOutcome {
    /// The target published `ack_gen == want`. Genuine remote acknowledgement.
    Acked,
    /// The target did not ack within the bound. Surfaced, never hidden.
    TimedOut,
}

/// Host-side model of the per-CPU shootdown generation mailbox.
///
/// Mirrors the bare-metal per-CPU record fields `tlb_req_gen` / `tlb_ack_gen`.
/// The split of writers is deliberate and is the core anti-fake-ACK invariant:
///
/// * [`GenTracker::post_request`] — the **initiator** bumps `req_gen` only.
/// * [`GenTracker::observe_target_ack`] — models the **target AP** advancing its
///   own `ack_gen` to the request generation after invalidation.
///
/// There is intentionally **no** method by which the initiator writes `ack_gen`.
#[derive(Debug, Clone)]
pub struct GenTracker {
    req_gen: [u32; MAX_CPUS],
    ack_gen: [u32; MAX_CPUS],
}

impl Default for GenTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl GenTracker {
    pub const fn new() -> Self {
        Self {
            req_gen: [0; MAX_CPUS],
            ack_gen: [0; MAX_CPUS],
        }
    }

    /// Initiator: post a new request generation for `cpu`; returns the generation
    /// the target must ack. Writes only `req_gen` — never `ack_gen`.
    pub fn post_request(&mut self, cpu: usize) -> u32 {
        let g = self.req_gen[cpu].wrapping_add(1);
        self.req_gen[cpu] = g;
        g
    }

    /// Current request generation for `cpu`.
    pub fn req_gen(&self, cpu: usize) -> u32 {
        self.req_gen[cpu]
    }

    /// Current acknowledged generation for `cpu`.
    pub fn ack_gen(&self, cpu: usize) -> u32 {
        self.ack_gen[cpu]
    }

    /// Model the TARGET CPU acknowledging: advance `cpu`'s `ack_gen` to its
    /// current `req_gen`. This is the ONLY writer of `ack_gen`. On real hardware
    /// this is the AP's `gs:[132] = req_gen` write, executed by the AP after it
    /// performs the local invalidation — never by the BSP.
    pub fn observe_target_ack(&mut self, cpu: usize) {
        self.ack_gen[cpu] = self.req_gen[cpu];
    }

    /// Initiator-side check: has `cpu` published an ack for generation `want`?
    /// Uses generation equality (not a boolean), so a stale ack from an earlier
    /// shootdown does not satisfy a newer request.
    pub fn is_acked(&self, cpu: usize, want: u32) -> bool {
        self.ack_gen[cpu] == want
    }

    /// Initiator-side bounded wait for `cpu` to ack `want`, polling a
    /// caller-supplied observer up to `max_polls` times. The observer returns the
    /// *currently observed* ack generation (on hardware: a volatile read of the
    /// AP-owned mailbox field). Returns a visible [`AckOutcome`].
    pub fn wait_for_ack(
        &self,
        cpu: usize,
        want: u32,
        max_polls: usize,
        mut observe_ack: impl FnMut() -> u32,
    ) -> AckOutcome {
        for _ in 0..max_polls {
            if observe_ack() == want {
                return AckOutcome::Acked;
            }
        }
        // Final read of the local model as a fallback (models the last mailbox read).
        if self.is_acked(cpu, want) {
            AckOutcome::Acked
        } else {
            AckOutcome::TimedOut
        }
    }
}

/// Build the shootdown target set from live topology bitmaps and classify it.
/// Convenience wrapper over [`ipi_capable_targets`] + [`classify`] using the
/// x86_64 BSP id.
pub fn plan(online: u64, wake_only: u64) -> ShootdownTargets {
    classify(ipi_capable_targets(online, wake_only, BOOTSTRAP_CPU_ID))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BSP: u8 = BOOTSTRAP_CPU_ID;

    #[test]
    fn ipi_targets_exclude_bsp_and_offline_include_wake_only_aps() {
        // CPUs 0(BSP),1,2 online; 1,2 wake-only APs; 3 offline.
        let online = 0b0111;
        let wake_only = 0b0110;
        let targets = ipi_capable_targets(online, wake_only, BSP);
        assert_eq!(targets, 0b0110, "only online wake-only APs, never the BSP");
        // BSP is never a target even if (impossibly) marked wake-only.
        assert_eq!(ipi_capable_targets(0b0111, 0b0111, BSP) & 1, 0);
        // Offline CPU (bit 3) never a target even if wake-only bit set.
        assert_eq!(ipi_capable_targets(0b0111, 0b1110, BSP) & (1 << 3), 0);
    }

    #[test]
    fn classify_empty_is_bsp_local() {
        assert_eq!(classify(0), ShootdownTargets::BspLocal);
        assert_eq!(classify(0b110), ShootdownTargets::Remote(0b110));
    }

    #[test]
    fn bsp_only_topology_collapses_to_bsp_local() {
        // Only the BSP online, no APs -> no remote target -> honest bsp_local.
        assert_eq!(plan(0b0001, 0b0000), ShootdownTargets::BspLocal);
    }

    #[test]
    fn ack_uses_generation_not_boolean_stale_state() {
        let mut t = GenTracker::new();
        let cpu = 1;
        // First shootdown: post gen, target acks it.
        let g1 = t.post_request(cpu);
        assert!(!t.is_acked(cpu, g1), "not acked before the target acks");
        t.observe_target_ack(cpu);
        assert!(t.is_acked(cpu, g1), "acked once the target publishes g1");

        // Second shootdown on the SAME cpu: the OLD ack (g1) must NOT satisfy g2.
        let g2 = t.post_request(cpu);
        assert_ne!(g1, g2, "generations advance");
        assert!(
            !t.is_acked(cpu, g2),
            "a boolean would falsely report acked; generations catch the stale ack"
        );
        t.observe_target_ack(cpu);
        assert!(t.is_acked(cpu, g2), "acked once the target publishes g2");
    }

    #[test]
    fn initiator_cannot_fabricate_a_remote_ack() {
        // There is no initiator-side ack writer; polling without a target ack must
        // TIME OUT (visible), never spontaneously succeed.
        let mut t = GenTracker::new();
        let cpu = 2;
        let want = t.post_request(cpu);
        let outcome = t.wait_for_ack(cpu, want, 8, || t.ack_gen(cpu));
        assert_eq!(
            outcome,
            AckOutcome::TimedOut,
            "initiator cannot self-ack for a remote AP; missing ack is a visible timeout"
        );
        assert!(!t.is_acked(cpu, want));
    }

    #[test]
    fn genuine_target_ack_is_observed_as_success() {
        let mut t = GenTracker::new();
        let cpu = 3;
        let want = t.post_request(cpu);
        // Model the target acking after a couple of polls.
        t.observe_target_ack(cpu);
        let outcome = t.wait_for_ack(cpu, want, 8, || t.ack_gen(cpu));
        assert_eq!(outcome, AckOutcome::Acked);
    }

    #[test]
    fn one_target_ack_does_not_ack_a_different_cpu() {
        // BSP must not treat CPU 1's ack as CPU 2's ack.
        let mut t = GenTracker::new();
        let g1 = t.post_request(1);
        let g2 = t.post_request(2);
        t.observe_target_ack(1);
        assert!(t.is_acked(1, g1));
        assert!(
            !t.is_acked(2, g2),
            "CPU 2 has not acked; no cross-CPU credit"
        );
    }
}
