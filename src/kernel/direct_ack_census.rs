// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Stage 199D — the **independent endpoint receive-waiter census**.
//!
//! The acknowledgement store can report its own lifecycle: how many leases it reserved,
//! consumed, released and cancelled, and how many are live. What it cannot do is say whether
//! that population is *right*. A store that silently dropped a lease, held one for a waiter
//! that had gone, or refused a waiter a lease it should have had would balance its own books
//! perfectly — the previous capacity-8 store did exactly that, and its counters looked clean
//! while a ninth parked server was being turned away.
//!
//! This module measures the same population from the other side: from
//! [`IpcSubsystem::endpoint_waiters`], the authoritative table of blocked receivers, plus each
//! waiter's own blocked-recv state. It is deliberately **not bounded by the acknowledgement
//! store's capacity** — it counts waiters, whether or not the store has a lease for them, which
//! is precisely what makes it able to detect an under-sized store.
//!
//! Two measurements, both independent of the store's internal counters:
//!
//! * a **running census** — current and high-watermark endpoint receive-waiter counts,
//!   maintained by the waiter table's own mutators, so the high-watermark is a true peak and
//!   not a sample taken at whatever moment somebody happened to log;
//! * a **final bijection** — a two-pass scan proving that live leases and eligible waiters are
//!   in exact one-to-one correspondence, matched on the complete
//!   `{endpoint_index, endpoint_generation, waiter_tid, waiter_asid}` identity.

// The census is driven from the freestanding split-dispatch attestation; a hosted `lib` build
// compiles no route to some of it. Its own tests exercise every item in every build.
#![allow(dead_code)]

use core::sync::atomic::{AtomicUsize, Ordering};

/// Current number of endpoint receive-waiters linked in the waiter table.
static WAITERS_CURRENT: AtomicUsize = AtomicUsize::new(0);
/// Highest simultaneous endpoint receive-waiter count observed this boot.
static WAITERS_HIGH_WATERMARK: AtomicUsize = AtomicUsize::new(0);

/// A waiter was linked into the endpoint receive-waiter table.
///
/// Called from the table's own publisher, so the census cannot drift from the table by
/// forgetting a path — the same reason the acknowledgement lease is released from the
/// removal primitives rather than from each terminal branch.
pub(crate) fn note_waiter_linked() {
    let current = WAITERS_CURRENT.fetch_add(1, Ordering::Relaxed) + 1;
    WAITERS_HIGH_WATERMARK.fetch_max(current, Ordering::Relaxed);
}

/// A waiter was removed from the endpoint receive-waiter table.
pub(crate) fn note_waiter_unlinked() {
    // Saturating: a decrement without a matching link would otherwise wrap and make the
    // census read as enormous rather than as broken.
    let _ = WAITERS_CURRENT.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
        Some(n.saturating_sub(1))
    });
}

/// Current endpoint receive-waiter count.
pub(crate) fn waiters_current() -> usize {
    WAITERS_CURRENT.load(Ordering::Acquire)
}

/// Highest simultaneous endpoint receive-waiter count this boot.
///
/// **Not bounded by [`crate::kernel::direct_ack_store::DIRECT_ACK_STORE_CAPACITY`]**: it is a
/// property of the waiter table, so it can exceed the store and reveal an under-sized one.
pub(crate) fn waiters_high_watermark() -> usize {
    WAITERS_HIGH_WATERMARK.load(Ordering::Acquire)
}

/// Reset the running census (test setup / between boots).
pub(crate) fn reset() {
    WAITERS_CURRENT.store(0, Ordering::Relaxed);
    WAITERS_HIGH_WATERMARK.store(0, Ordering::Relaxed);
}

/// The result of comparing live acknowledgement leases against eligible blocked waiters.
///
/// Every field is a count of *violations* except the two populations, so a healthy system is
/// all zeros with the populations equal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct LeaseBijection {
    /// Waiters that are eligible for a lease: linked, blocked in a fully committed recv-v2,
    /// and on an endpoint admitted to the off-lock path.
    pub(crate) eligible_waiters: usize,
    /// Leases the store currently holds live.
    pub(crate) live_leases: usize,
    /// Eligible waiters with no live lease at all — the under-sized-store symptom.
    pub(crate) waiters_without_lease: usize,
    /// Live leases with no eligible waiter behind them — the orphan symptom.
    pub(crate) leases_without_waiter: usize,
    /// A lease and a waiter share an endpoint index but name different waiter incarnations.
    pub(crate) identity_mismatches: usize,
    /// A lease and a waiter share an endpoint index but different endpoint generations.
    pub(crate) generation_mismatches: usize,
    /// Two live leases claiming one endpoint incarnation.
    pub(crate) duplicate_endpoint_incarnations: usize,
}

impl LeaseBijection {
    /// Exact one-to-one correspondence: every live lease has exactly one matching waiter, and
    /// every eligible waiter has exactly one live lease.
    pub(crate) fn ok(&self) -> bool {
        self.eligible_waiters == self.live_leases
            && self.waiters_without_lease == 0
            && self.leases_without_waiter == 0
            && self.identity_mismatches == 0
            && self.generation_mismatches == 0
            && self.duplicate_endpoint_incarnations == 0
    }
}

/// One waiter as the census sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CensusWaiter {
    pub(crate) endpoint_index: usize,
    pub(crate) endpoint_generation: u64,
    pub(crate) tid: u64,
    pub(crate) asid: u16,
    /// Linked, fully committed recv-v2, on an admitted endpoint.
    pub(crate) eligible: bool,
}

/// Compare one endpoint index's waiter against the lease held for it.
///
/// Pure, so every disagreement shape is directly constructible in a test rather than needing a
/// live race to produce it. `lease` is `(endpoint_generation, tid, asid)`.
pub(crate) fn classify_slot(
    waiter: Option<CensusWaiter>,
    lease: Option<(u64, u64, u16)>,
    into: &mut LeaseBijection,
) {
    let eligible = waiter.filter(|w| w.eligible);
    if eligible.is_some() {
        into.eligible_waiters += 1;
    }
    if lease.is_some() {
        into.live_leases += 1;
    }
    match (eligible, lease) {
        (None, None) => {}
        (Some(_), None) => into.waiters_without_lease += 1,
        (None, Some(_)) => into.leases_without_waiter += 1,
        (Some(w), Some((generation, tid, asid))) => {
            // Both halves of the identity are load-bearing: a replacement task that reused a
            // numeric TID carries a different ASID, and a recycled endpoint slot carries a
            // different generation.
            if w.endpoint_generation != generation {
                into.generation_mismatches += 1;
            }
            if w.tid != tid || w.asid != asid {
                into.identity_mismatches += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn waiter(eligible: bool) -> CensusWaiter {
        CensusWaiter {
            endpoint_index: 3,
            endpoint_generation: 7,
            tid: 42,
            asid: 2,
            eligible,
        }
    }

    #[test]
    fn a_matched_pair_is_a_clean_bijection() {
        let mut b = LeaseBijection::default();
        classify_slot(Some(waiter(true)), Some((7, 42, 2)), &mut b);
        assert_eq!(b.eligible_waiters, 1);
        assert_eq!(b.live_leases, 1);
        assert!(b.ok(), "{b:?}");
    }

    #[test]
    fn an_empty_slot_contributes_nothing() {
        let mut b = LeaseBijection::default();
        classify_slot(None, None, &mut b);
        assert_eq!(b, LeaseBijection::default());
        assert!(b.ok());
    }

    /// The under-sized-store symptom: a parked server the store had no room for.
    #[test]
    fn an_eligible_waiter_without_a_lease_is_a_violation() {
        let mut b = LeaseBijection::default();
        classify_slot(Some(waiter(true)), None, &mut b);
        assert_eq!(b.waiters_without_lease, 1);
        assert_eq!(b.eligible_waiters, 1);
        assert_eq!(b.live_leases, 0);
        assert!(!b.ok());
    }

    /// The orphan symptom: a lease whose waiter is gone.
    #[test]
    fn a_lease_without_a_waiter_is_a_violation() {
        let mut b = LeaseBijection::default();
        classify_slot(None, Some((7, 42, 2)), &mut b);
        assert_eq!(b.leases_without_waiter, 1);
        assert!(!b.ok());
        // An INELIGIBLE waiter is not a waiter for this purpose: a lease held against one is
        // still an orphan.
        let mut b2 = LeaseBijection::default();
        classify_slot(Some(waiter(false)), Some((7, 42, 2)), &mut b2);
        assert_eq!(b2.leases_without_waiter, 1);
        assert_eq!(b2.eligible_waiters, 0);
        assert!(!b2.ok());
    }

    /// An ineligible waiter with no lease is the ordinary case, not a violation: most endpoint
    /// waiters are not fully committed recv-v2 receivers on admitted endpoints.
    #[test]
    fn an_ineligible_waiter_without_a_lease_is_fine() {
        let mut b = LeaseBijection::default();
        classify_slot(Some(waiter(false)), None, &mut b);
        assert_eq!(b, LeaseBijection::default());
        assert!(b.ok());
    }

    #[test]
    fn both_identity_halves_are_load_bearing() {
        // A replacement task that reused the numeric TID with a fresh ASID.
        let mut b = LeaseBijection::default();
        classify_slot(Some(waiter(true)), Some((7, 42, 9)), &mut b);
        assert_eq!(b.identity_mismatches, 1);
        assert!(!b.ok());
        // A different TID entirely.
        let mut b = LeaseBijection::default();
        classify_slot(Some(waiter(true)), Some((7, 43, 2)), &mut b);
        assert_eq!(b.identity_mismatches, 1);
        // A recycled endpoint incarnation.
        let mut b = LeaseBijection::default();
        classify_slot(Some(waiter(true)), Some((8, 42, 2)), &mut b);
        assert_eq!(b.generation_mismatches, 1);
        assert_eq!(b.identity_mismatches, 0);
        assert!(!b.ok());
    }

    /// Populations must match as well as pair up — a count-only check would miss a store that
    /// held one extra lease and dropped one waiter's.
    #[test]
    fn unequal_populations_fail_even_when_no_slot_disagrees() {
        let mut b = LeaseBijection {
            eligible_waiters: 3,
            live_leases: 2,
            ..LeaseBijection::default()
        };
        assert!(!b.ok());
        b.live_leases = 3;
        assert!(b.ok());
    }

    #[test]
    fn duplicate_endpoint_incarnations_fail() {
        let b = LeaseBijection {
            duplicate_endpoint_incarnations: 1,
            ..LeaseBijection::default()
        };
        assert!(!b.ok());
    }

    #[test]
    fn the_running_census_tracks_link_and_unlink_and_peaks() {
        reset();
        assert_eq!(waiters_current(), 0);
        assert_eq!(waiters_high_watermark(), 0);
        for _ in 0..5 {
            note_waiter_linked();
        }
        assert_eq!(waiters_current(), 5);
        assert_eq!(waiters_high_watermark(), 5);
        for _ in 0..3 {
            note_waiter_unlinked();
        }
        assert_eq!(waiters_current(), 2);
        assert_eq!(
            waiters_high_watermark(),
            5,
            "the watermark is a peak, not a sample"
        );
        note_waiter_linked();
        note_waiter_linked();
        note_waiter_linked();
        note_waiter_linked();
        assert_eq!(waiters_current(), 6);
        assert_eq!(waiters_high_watermark(), 6, "a later peak is recorded");
        reset();
    }

    /// The census is a property of the waiter table, so it is NOT clamped to the
    /// acknowledgement store's capacity — that is exactly what lets it reveal an under-sized
    /// store rather than agreeing with it.
    #[test]
    fn the_census_is_not_bounded_by_the_ack_store_capacity() {
        use crate::kernel::direct_ack_store::DIRECT_ACK_STORE_CAPACITY;
        reset();
        for _ in 0..(DIRECT_ACK_STORE_CAPACITY + 17) {
            note_waiter_linked();
        }
        assert_eq!(waiters_current(), DIRECT_ACK_STORE_CAPACITY + 17);
        assert_eq!(
            waiters_high_watermark(),
            DIRECT_ACK_STORE_CAPACITY + 17,
            "the census counts waiters, not lease slots"
        );
        reset();
    }

    /// An unmatched decrement saturates rather than wrapping: a broken census must read as
    /// broken, not as astronomically large.
    #[test]
    fn an_unmatched_unlink_saturates_at_zero() {
        reset();
        note_waiter_unlinked();
        assert_eq!(waiters_current(), 0);
        reset();
    }
}
