// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Stage 199D — production counters and attestations for the direct NR6/NR7 paths.
//!
//! Every direct attempt now lands in exactly one terminal bucket, and the acknowledgement
//! store's lifecycle is reported alongside it. This is what makes the eventual
//! production-default flip *auditable on a real boot* rather than inferred from hosted
//! tests: a boot log can say how many ordinary calls took the direct path, how many fell
//! back and why, whether the ack lifecycle balanced, and whether any fail-closed fuse tripped.
//!
//! # Observational only
//!
//! Counters are plain relaxed atomics incremented **after** a decision has already been
//! made. Nothing here reads back into a disposition, blocks, allocates, takes a lock, wakes
//! anything, or touches the scheduler. Removing every call site would leave behaviour
//! byte-identical — which is exactly the property the guards in this module's tests pin.
//!
//! # The terminal-bucket invariant
//!
//! For each direction, every counted attempt resolves to exactly one of:
//!
//! ```text
//!   attempts = declined_preflight           (ineligible: never reached the transaction)
//!            + declined_pre_transaction     (eligible, but no ack / copy fault / no snapshot)
//!            + completed                    (the transaction committed)
//!            + failed (Σ failed_by_error_code)
//!            + legacy_fallback_after_decline (the transaction declined post-drain)
//! ```
//!
//! `eligible` and `declined_ineligible_mode` are *breakdowns*, not additional buckets:
//! `eligible = attempts - declined_preflight`, and `declined_ineligible_mode` is the subset
//! of `declined_preflight` caused specifically by a `Synchronous` endpoint.

// Consumed by the split-dispatch helpers, which are freestanding-only: a hosted `lib`
// build compiles no route to them, so this whole module reads as dead there. The
// counters's own tests exercise every item in every build.
#![allow(dead_code)]

use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use crate::kernel::syscall::SyscallError;

/// Buckets for `failed_by_error_code`: codes 1..=10 plus a final bucket for `Internal`
/// (255) and any code outside the dense range.
const FAILED_CODE_SLOTS: usize = 11;
const FAILED_CODE_OTHER: usize = FAILED_CODE_SLOTS - 1;

/// Map a `SyscallError` code onto its dense counter slot.
const fn failed_slot(code: usize) -> usize {
    if code >= 1 && code <= 10 {
        code - 1
    } else {
        FAILED_CODE_OTHER
    }
}

/// One direction's counters.
#[derive(Debug)]
pub(crate) struct DirectPathCounters {
    attempts: AtomicU64,
    eligible: AtomicU64,
    declined_ineligible_mode: AtomicU64,
    declined_preflight: AtomicU64,
    declined_pre_transaction: AtomicU64,
    completed: AtomicU64,
    legacy_fallback_after_decline: AtomicU64,
    failed_by_code: [AtomicU64; FAILED_CODE_SLOTS],
    /// Per-direction attestation latches (see `maybe_emit_attestation`).
    attested_first: AtomicUsize,
    attested_settled: AtomicUsize,
}

impl DirectPathCounters {
    pub(crate) const fn new() -> Self {
        Self {
            attempts: AtomicU64::new(0),
            eligible: AtomicU64::new(0),
            declined_ineligible_mode: AtomicU64::new(0),
            declined_preflight: AtomicU64::new(0),
            declined_pre_transaction: AtomicU64::new(0),
            completed: AtomicU64::new(0),
            legacy_fallback_after_decline: AtomicU64::new(0),
            failed_by_code: [const { AtomicU64::new(0) }; FAILED_CODE_SLOTS],
            attested_first: AtomicUsize::new(0),
            attested_settled: AtomicUsize::new(0),
        }
    }

    pub(crate) fn reset(&self) {
        self.attempts.store(0, Ordering::Relaxed);
        self.eligible.store(0, Ordering::Relaxed);
        self.declined_ineligible_mode.store(0, Ordering::Relaxed);
        self.declined_preflight.store(0, Ordering::Relaxed);
        self.declined_pre_transaction.store(0, Ordering::Relaxed);
        self.completed.store(0, Ordering::Relaxed);
        self.legacy_fallback_after_decline
            .store(0, Ordering::Relaxed);
        for slot in &self.failed_by_code {
            slot.store(0, Ordering::Relaxed);
        }
        self.attested_first.store(0, Ordering::Relaxed);
        self.attested_settled.store(0, Ordering::Relaxed);
    }

    /// True once THIS direction produced a terminal beyond a preflight decline.
    pub(crate) fn has_transaction_terminal(&self) -> bool {
        self.completed() + self.failed_total() + self.legacy_fallback_after_decline() > 0
    }

    /// One direct attempt entered the split helper.
    pub(crate) fn note_attempt(&self) {
        self.attempts.fetch_add(1, Ordering::Relaxed);
    }

    /// The attempt passed the eligibility contract.
    pub(crate) fn note_eligible(&self) {
        self.eligible.fetch_add(1, Ordering::Relaxed);
    }

    /// The attempt was declined by the eligibility contract, before any mutation.
    /// `ineligible_mode` marks the `Synchronous`-endpoint subset.
    pub(crate) fn note_declined_preflight(&self, ineligible_mode: bool) {
        self.declined_preflight.fetch_add(1, Ordering::Relaxed);
        if ineligible_mode {
            self.declined_ineligible_mode
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    /// The attempt passed eligibility but stopped before the transaction ran: the source
    /// copy faulted, the owned snapshot could not be built, or no acknowledgement was
    /// claimable for this endpoint incarnation. Nothing was mutated and the legacy path runs.
    ///
    /// This bucket exists because a live boot showed `eligible` exceeding the terminals: the
    /// oracle server's bounded pre-acknowledgement NR7 retries pass eligibility and then find
    /// nothing to claim. Without it the balance invariant is unprovable, not merely untidy.
    pub(crate) fn note_declined_pre_transaction(&self) {
        self.declined_pre_transaction
            .fetch_add(1, Ordering::Relaxed);
    }

    /// The transaction committed.
    pub(crate) fn note_completed(&self) {
        self.completed.fetch_add(1, Ordering::Relaxed);
    }

    /// The transaction failed with a canonical error.
    pub(crate) fn note_failed(&self, err: SyscallError) {
        self.failed_by_code[failed_slot(err.code())].fetch_add(1, Ordering::Relaxed);
    }

    /// The transaction declined post-drain and the legacy path will run.
    pub(crate) fn note_legacy_fallback_after_decline(&self) {
        self.legacy_fallback_after_decline
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn attempts(&self) -> u64 {
        self.attempts.load(Ordering::Acquire)
    }
    pub(crate) fn eligible(&self) -> u64 {
        self.eligible.load(Ordering::Acquire)
    }
    pub(crate) fn declined_ineligible_mode(&self) -> u64 {
        self.declined_ineligible_mode.load(Ordering::Acquire)
    }
    pub(crate) fn declined_preflight(&self) -> u64 {
        self.declined_preflight.load(Ordering::Acquire)
    }
    pub(crate) fn declined_pre_transaction(&self) -> u64 {
        self.declined_pre_transaction.load(Ordering::Acquire)
    }
    pub(crate) fn completed(&self) -> u64 {
        self.completed.load(Ordering::Acquire)
    }
    pub(crate) fn legacy_fallback_after_decline(&self) -> u64 {
        self.legacy_fallback_after_decline.load(Ordering::Acquire)
    }

    /// Failures recorded for one canonical error code.
    pub(crate) fn failed_with(&self, err: SyscallError) -> u64 {
        self.failed_by_code[failed_slot(err.code())].load(Ordering::Acquire)
    }

    /// Total failures across every code.
    pub(crate) fn failed_total(&self) -> u64 {
        self.failed_by_code
            .iter()
            .map(|slot| slot.load(Ordering::Acquire))
            .sum()
    }

    /// The terminal-bucket invariant: every counted attempt landed in exactly one bucket.
    pub(crate) fn terminals_balance(&self) -> bool {
        self.attempts()
            == self.declined_preflight()
                + self.declined_pre_transaction()
                + self.completed()
                + self.failed_total()
                + self.legacy_fallback_after_decline()
    }

    /// `eligible` is the complement of `declined_preflight`.
    pub(crate) fn eligibility_balances(&self) -> bool {
        self.attempts() == self.eligible() + self.declined_preflight()
            && self.declined_ineligible_mode() <= self.declined_preflight()
    }
}

/// NR6 request-path counters.
pub(crate) static REQUEST: DirectPathCounters = DirectPathCounters::new();
/// NR7 reply-path counters.
pub(crate) static REPLY: DirectPathCounters = DirectPathCounters::new();

/// Record the terminal bucket a classified disposition falls into.
///
/// Observational: it reads the disposition that has ALREADY been decided and increments one
/// counter. It never returns a value the caller acts on.
pub(crate) fn note_disposition(
    counters: &DirectPathCounters,
    disposition: crate::kernel::direct_disposition::DirectDisposition,
) {
    use crate::kernel::direct_disposition::DirectDisposition;
    match disposition {
        DirectDisposition::Completed => counters.note_completed(),
        DirectDisposition::DeclinedBeforeMutation => counters.note_legacy_fallback_after_decline(),
        DirectDisposition::Failed(err) => counters.note_failed(err),
    }
}

/// Reset both directions (test setup / between boots). Does NOT reset the acknowledgement
/// stores — those are owned by their own modules.
pub(crate) fn reset() {
    REQUEST.reset();
    REPLY.reset();
}

/// Emit the direct-IPC attestation, at most **twice per direction** per boot. Purely a
/// report: it reads counters and logs, and returns `false` without logging when it has
/// nothing new to say.
///
/// Two bounded samples per direction, because one is not enough for either audience:
///
/// * the **first** sample fires as soon as that direction has been attempted at all. On an
///   ordinary boot — where every service-chain call declines at the endpoint confinement and
///   no transaction ever runs — this is the only sample there will be, and it is exactly the
///   decline census a production audit wants;
/// * the **settled** sample fires once THAT DIRECTION has produced a terminal beyond a
///   preflight decline. The latches are per-direction on purpose: a reply necessarily
///   follows its request, so a shared latch would settle NR7 at the instant NR6 completed
///   and report the reply direction as having done nothing.
///
/// Deliberately not wired into any hot path — the caller is the same off-lock `DebugLog`
/// observation point the ServerDies link-balance attestation uses, so this adds no new
/// emission site and no scheduling effect.
pub(crate) fn maybe_emit_attestation() -> bool {
    let mut emitted = false;
    emitted |= maybe_emit_one(
        "nr6",
        &REQUEST,
        crate::kernel::boot::ipccall_direct_ack::store(),
    );
    emitted |= maybe_emit_one(
        "nr7",
        &REPLY,
        crate::kernel::boot::ipcreply_direct_ack::store(),
    );
    emitted
}

fn maybe_emit_one(
    which: &str,
    counters: &DirectPathCounters,
    store: &crate::kernel::direct_ack_store::DirectAckStore,
) -> bool {
    if counters.attempts() == 0 {
        return false;
    }
    let phase = if counters.attested_first.swap(1, Ordering::AcqRel) == 0 {
        "first"
    } else if counters.has_transaction_terminal()
        && counters.attested_settled.swap(1, Ordering::AcqRel) == 0
    {
        "settled"
    } else {
        return false;
    };
    emit_direction(phase, which, counters, store);
    true
}

fn emit_direction(
    phase: &str,
    which: &str,
    counters: &DirectPathCounters,
    store: &crate::kernel::direct_ack_store::DirectAckStore,
) {
    crate::yarm_log!(
        "IPC_DIRECT_PATH_COUNTERS phase={} dir={} attempts={} eligible={} declined_ineligible_mode={} declined_preflight={} declined_pre_txn={} completed={} failed={} legacy_fallback={} balanced={}",
        phase,
        which,
        counters.attempts(),
        counters.eligible(),
        counters.declined_ineligible_mode(),
        counters.declined_preflight(),
        counters.declined_pre_transaction(),
        counters.completed(),
        counters.failed_total(),
        counters.legacy_fallback_after_decline(),
        (counters.terminals_balance() && counters.eligibility_balances()) as u8,
    );
    crate::yarm_log!(
        "IPC_DIRECT_ACK_COUNTERS phase={} dir={} reserve={} commit={} consume={} cancel={} live={} high_watermark={} capacity={}",
        phase,
        which,
        store.reserve_count(),
        store.commit_count(),
        store.consume_count(),
        store.cancel_count(),
        store.live_pair_count(),
        store.occupancy_high_watermark(),
        crate::kernel::direct_ack_store::DIRECT_ACK_STORE_CAPACITY,
    );
    crate::yarm_log!(
        "IPC_DIRECT_ACK_FUSES phase={} dir={} capacity_refused={} overwrite_fuse={} stale={} foreign={} duplicate={} not_committed={}",
        phase,
        which,
        store.capacity_refusal_count(),
        store.endpoint_live_refusal_count(),
        store.stale_generation_rejection_count(),
        store.foreign_waiter_rejection_count(),
        store.duplicate_consume_rejection_count(),
        store.not_committed_rejection_count(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> DirectPathCounters {
        DirectPathCounters::new()
    }

    #[test]
    fn a_fresh_counter_set_balances_trivially() {
        let c = fresh();
        assert!(c.terminals_balance());
        assert!(c.eligibility_balances());
        assert_eq!(c.attempts(), 0);
        assert_eq!(c.failed_total(), 0);
    }

    /// Every attempt lands in exactly one terminal bucket.
    #[test]
    fn every_attempt_lands_in_exactly_one_terminal_bucket() {
        let c = fresh();
        // A preflight decline (Synchronous).
        c.note_attempt();
        c.note_declined_preflight(true);
        // A preflight decline for another reason.
        c.note_attempt();
        c.note_declined_preflight(false);
        // A completed transaction.
        c.note_attempt();
        c.note_eligible();
        c.note_completed();
        // Two failures with distinct codes.
        c.note_attempt();
        c.note_eligible();
        c.note_failed(SyscallError::InvalidArgs);
        c.note_attempt();
        c.note_eligible();
        c.note_failed(SyscallError::ServerDied);
        // A post-drain decline that hands over to legacy.
        c.note_attempt();
        c.note_eligible();
        c.note_legacy_fallback_after_decline();
        // An eligible attempt that stopped before the transaction (no claimable ack).
        c.note_attempt();
        c.note_eligible();
        c.note_declined_pre_transaction();

        assert_eq!(c.attempts(), 7);
        assert_eq!(c.declined_preflight(), 2);
        assert_eq!(c.declined_ineligible_mode(), 1);
        assert_eq!(c.eligible(), 5);
        assert_eq!(c.declined_pre_transaction(), 1);
        assert_eq!(c.completed(), 1);
        assert_eq!(c.failed_total(), 2);
        assert_eq!(c.legacy_fallback_after_decline(), 1);
        assert!(c.terminals_balance(), "terminal buckets sum to attempts");
        assert!(c.eligibility_balances(), "eligible + declined = attempts");
    }

    #[test]
    fn failures_are_bucketed_by_canonical_error_code() {
        let c = fresh();
        for err in [
            SyscallError::InvalidArgs,
            SyscallError::InvalidArgs,
            SyscallError::WrongObject,
            SyscallError::ServerDied,
            SyscallError::Internal,
        ] {
            c.note_attempt();
            c.note_eligible();
            c.note_failed(err);
        }
        assert_eq!(c.failed_with(SyscallError::InvalidArgs), 2);
        assert_eq!(c.failed_with(SyscallError::WrongObject), 1);
        assert_eq!(c.failed_with(SyscallError::ServerDied), 1);
        assert_eq!(c.failed_with(SyscallError::Internal), 1);
        assert_eq!(c.failed_with(SyscallError::WouldBlock), 0);
        assert_eq!(c.failed_total(), 5);
        assert!(c.terminals_balance());
    }

    /// `Internal` (255) shares the overflow bucket, and every dense code has its own.
    #[test]
    fn error_code_slots_are_distinct_for_the_dense_range() {
        let dense = [
            SyscallError::InvalidNumber,
            SyscallError::InvalidArgs,
            SyscallError::InvalidCapability,
            SyscallError::MissingRight,
            SyscallError::WrongObject,
            SyscallError::QueueFull,
            SyscallError::WouldBlock,
            SyscallError::PageFault,
            SyscallError::TimedOut,
            SyscallError::ServerDied,
        ];
        for (i, err) in dense.iter().enumerate() {
            assert_eq!(failed_slot(err.code()), i, "{err:?}");
        }
        assert_eq!(
            failed_slot(SyscallError::Internal.code()),
            FAILED_CODE_OTHER
        );
        // The overflow bucket is genuinely shared, and never collides with a dense slot.
        assert!(FAILED_CODE_OTHER >= dense.len());
    }

    #[test]
    fn reset_clears_every_bucket() {
        let c = fresh();
        c.note_attempt();
        c.note_eligible();
        c.note_failed(SyscallError::WrongObject);
        c.note_declined_preflight(true);
        c.note_legacy_fallback_after_decline();
        c.reset();
        assert_eq!(c.attempts(), 0);
        assert_eq!(c.eligible(), 0);
        assert_eq!(c.declined_preflight(), 0);
        assert_eq!(c.declined_ineligible_mode(), 0);
        assert_eq!(c.completed(), 0);
        assert_eq!(c.failed_total(), 0);
        assert_eq!(c.legacy_fallback_after_decline(), 0);
        assert!(c.terminals_balance());
    }

    /// An imbalance is DETECTED, not masked — the invariant is a real check.
    #[test]
    fn the_balance_invariant_actually_detects_an_imbalance() {
        let c = fresh();
        c.note_attempt();
        // No terminal recorded for it.
        assert!(!c.terminals_balance(), "a missing terminal must be visible");
        assert!(!c.eligibility_balances());
        c.note_completed();
        c.note_eligible();
        assert!(c.terminals_balance());
        assert!(c.eligibility_balances());
    }

    /// The counters are observational: nothing in this module reads a counter to decide
    /// anything, and nothing here can block, allocate, lock, wake or schedule.
    #[test]
    fn counters_are_observational_only() {
        let src = include_str!("direct_ipc_counters.rs");
        // CODE only — the prose above legitimately names the things it promises not to do.
        let code: alloc::string::String = src
            .split("#[cfg(test)]")
            .next()
            .expect("production half")
            .lines()
            .filter(|line| {
                let t = line.trim_start();
                !t.starts_with("//") && !t.starts_with("///") && !t.starts_with("//!")
            })
            .collect::<alloc::vec::Vec<_>>()
            .join("\n");
        // `note_disposition` names the disposition type to MATCH on an already-decided
        // value; it never constructs or returns one, which this pins.
        let code_no_note = code
            .split("pub(crate) fn note_disposition(")
            .next()
            .expect("pre-note half");
        assert!(
            !code_no_note.contains("DirectDisposition"),
            "only note_disposition may name the disposition type"
        );
        // Matching on an already-decided disposition is the whole point; CONSTRUCTING or
        // RETURNING one would mean the counters could steer a decision.
        assert!(
            !code.contains("-> DirectDisposition")
                && !code.contains("= DirectDisposition::")
                && !code.contains("=> DirectDisposition::"),
            "counters must never construct or return a disposition"
        );
        for forbidden in [
            "with_ipc", "with_cpu", "dispatch", "enqueue", "wake", "yield", "alloc::", "lock()",
            "SpinLock", "set_ok", "set_err",
        ] {
            assert!(
                !code.contains(forbidden),
                "counters must stay observational ({forbidden})"
            );
        }
        // Only relaxed increments and acquire reads — no compare-exchange, no fences.
        assert!(
            !code.contains("compare_exchange"),
            "no decision-bearing atomics; the one-shot latch uses a plain swap"
        );
    }
}
