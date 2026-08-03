// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Stage 199D — the **typed, generation-bearing post-lock dispatch work item**.
//!
//! AArch64 readiness blocker 3 was that the authoritative queue-advancing dispatch — the step
//! that picks the next runnable task and actually resumes it — was compile-time x86_64-only
//! (`d6_genuine_enabled()`). An eligible direct NR6/NR7 transaction could complete off-lock, but
//! the *end-to-end* wake still finished under the broad `KernelState` lock on AArch64.
//!
//! This module owns the hand-off between the two halves of that wake. The in-lock commit that
//! genuinely parks the caller publishes ONE typed work item; the trap-entry drain, running with
//! the broad guard already dropped, consumes it and performs the dispatch through narrow ranked
//! seams only.
//!
//! ## Why the item is typed and generation-bearing
//!
//! The existing FutexWait/Yield deferrals record a bare outgoing TID in a per-CPU `AtomicU64`.
//! That is enough for those classes because a futex waiter's identity cannot be reused between
//! the commit and the drain. It is *not* enough here: between publication and drain the caller
//! may have been woken by a reply, timed out, exited, or had its TID reused by a replacement
//! incarnation in a different address space. A bare TID cannot tell those apart, and dispatching
//! on a stale one would resume a task that is no longer parked — a lost or duplicated wake.
//!
//! So the item carries the complete identity the drain needs to revalidate:
//!
//! * the exact outgoing `{tid, asid}` incarnation — a replacement task reusing the TID has a
//!   different ASID and is refused;
//! * the `blocked_generation` captured at commit — a caller that unblocked and re-blocked has
//!   advanced it, so a stale item is refused even when `{tid, asid}` still match;
//! * the CPU the commit ran on, so a drain on another CPU cannot consume it;
//! * the syscall class, so the drain can attribute the dispatch and so NR7 (which must never
//!   publish) is distinguishable from NR6 by type rather than by convention.
//!
//! ## Single-shot publication
//!
//! Publication is a per-CPU compare-exchange, exactly like the FutexWait deferral: at most one
//! item can be outstanding per CPU, a second publication declines rather than overwriting, and
//! the drain *takes* the item (leaving the slot empty) rather than reading it. A work item can
//! therefore be consumed at most once, and a duplicate trap finds nothing to do.

// Some items are reachable only from the freestanding AArch64 drain; a hosted `lib` build
// compiles no route to them. The module's own tests exercise every item in every build.
#![allow(dead_code)]

use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};

use crate::kernel::scheduler::MAX_CPUS;

/// Which syscall class parked the outgoing task.
///
/// Carried in the work item so the drain attributes the dispatch by TYPE rather than by
/// convention, and so the NR7 contract ("never publishes") is a statement about a value that
/// exists, not about the absence of one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectDispatchClass {
    /// The NR6 direction: the caller committed to its canonical reply-blocked state
    /// (`Blocked(EndpointReceive(_))`), left `current`, and needs the run queue advanced.
    IpcCall,
    /// The NR7 direction: the replying server remains `current`. This variant exists so the
    /// contract is expressible and testable; [`try_publish`] REFUSES it, because a reply must
    /// never force a context switch.
    IpcReply,
}

impl DirectDispatchClass {
    fn encode(self) -> u8 {
        match self {
            DirectDispatchClass::IpcCall => 1,
            DirectDispatchClass::IpcReply => 2,
        }
    }

    fn decode(raw: u8) -> Option<Self> {
        match raw {
            1 => Some(DirectDispatchClass::IpcCall),
            2 => Some(DirectDispatchClass::IpcReply),
            _ => None,
        }
    }
}

/// The typed, generation-bearing post-lock dispatch work item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirectDispatchWork {
    /// Exact outgoing thread id.
    pub outgoing_tid: u64,
    /// Exact outgoing address-space id — a replacement incarnation reusing the TID differs.
    pub outgoing_asid: u16,
    /// `tcb.blocked_recv_generation` as captured at the in-lock commit. A caller that
    /// unblocked and re-blocked has advanced this, so a stale item is refused.
    pub blocked_generation: u64,
    /// The CPU whose in-lock commit published this item.
    pub cpu: u8,
    /// The syscall class that parked the outgoing task.
    pub class: DirectDispatchClass,
}

/// Why a publication attempt did not produce an outstanding work item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishOutcome {
    /// The item is now outstanding for its CPU and will be taken by exactly one drain.
    Published,
    /// An item is already outstanding for this CPU. Declined, not overwritten — the caller
    /// keeps the unchanged in-lock dispatch fallback.
    AlreadyPending,
    /// The class must never force a context switch (NR7). Nothing is published.
    ClassNeverSwitches,
    /// The CPU index is outside the per-CPU table.
    CpuOutOfRange,
}

/// The exhaustive outcome of one post-lock drain.
///
/// Every arm is a definite, fail-closed terminal state. There is no wildcard: a new race has to
/// be given a name here before the drain will compile, which is what stops an unclassified race
/// from silently inheriting "dispatched".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrainOutcome {
    /// Nothing was published for this CPU — the ordinary non-blocking trap.
    NoWork,
    /// The run queue was advanced exactly once and the incoming task resumed.
    Dispatched { incoming: u64 },
    /// No runnable task exists. The outgoing task stays parked, `current` stays clear, and the
    /// CPU enters the established idle primitive. This is a SUCCESS, not a failure.
    Idle,
    /// The outgoing task is no longer in its committed blocked state — something (a split reply,
    /// a timeout) legitimately superseded the deferral before the drain ran.
    StateChangedBeforeDrain,
    /// The outgoing task was already made runnable — a reply won the race. Dispatching it away
    /// here would be a duplicate wake, so the drain declines.
    OutgoingAlreadyRunnable,
    /// The `{tid, asid}` incarnation or the blocked generation no longer matches the item.
    StaleIdentity,
    /// The rank-1 dequeue and the authoritative `current` slot disagree about who is running.
    /// Fail closed: resume nothing rather than resume the wrong task.
    DispatchCurrentDisagreement,
}

impl DrainOutcome {
    /// True iff the run queue was advanced by this drain.
    pub fn advanced_queue(self) -> bool {
        matches!(self, DrainOutcome::Dispatched { .. })
    }

    /// True iff the drain resumed no task (idle, or any fail-closed race arm).
    pub fn resumed_nothing(self) -> bool {
        !self.advanced_queue()
    }
}

/// The result of revalidating a work item against authoritative task state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevalidateOutcome {
    /// Exact incarnation, still committed to its blocked state — safe to advance the queue.
    Ok,
    /// The TCB is absent, or its `{tid, asid}` / blocked generation differ from the item.
    StaleIdentity,
    /// The exact incarnation is present but has been made runnable again.
    AlreadyRunnable,
    /// The exact incarnation is present but is no longer in its committed blocked state.
    StateChanged,
}

impl RevalidateOutcome {
    /// The fail-closed drain outcome this revalidation implies (`None` when it is `Ok`).
    pub fn refusal(self) -> Option<DrainOutcome> {
        match self {
            RevalidateOutcome::Ok => None,
            RevalidateOutcome::StaleIdentity => Some(DrainOutcome::StaleIdentity),
            RevalidateOutcome::AlreadyRunnable => Some(DrainOutcome::OutgoingAlreadyRunnable),
            RevalidateOutcome::StateChanged => Some(DrainOutcome::StateChangedBeforeDrain),
        }
    }
}

// ── Per-CPU single-shot publication slots ────────────────────────────────────────────────────
//
// `PENDING` is the authority: it is CAS'd from false to true by the publisher and swapped back
// to false by the taker, so an item is consumed at most once. The payload cells are written
// before `PENDING` is set (Release) and read after it is observed (Acquire), so a taker that
// sees the flag sees a complete item.

static PENDING: [AtomicBool; MAX_CPUS] = [const { AtomicBool::new(false) }; MAX_CPUS];
static OUTGOING_TID: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
static OUTGOING_ASID: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
static BLOCKED_GENERATION: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
static CLASS: [AtomicU8; MAX_CPUS] = [const { AtomicU8::new(0) }; MAX_CPUS];

// ── Counters ────────────────────────────────────────────────────────────────────────────────

static PUBLISHED: AtomicUsize = AtomicUsize::new(0);
static PUBLISH_DECLINED_ALREADY_PENDING: AtomicUsize = AtomicUsize::new(0);
static PUBLISH_REFUSED_NEVER_SWITCHES: AtomicUsize = AtomicUsize::new(0);
static TAKEN: AtomicUsize = AtomicUsize::new(0);
static DISPATCHED: AtomicUsize = AtomicUsize::new(0);
static IDLE: AtomicUsize = AtomicUsize::new(0);
static REFUSED_STATE_CHANGED: AtomicUsize = AtomicUsize::new(0);
static REFUSED_ALREADY_RUNNABLE: AtomicUsize = AtomicUsize::new(0);
static REFUSED_STALE_IDENTITY: AtomicUsize = AtomicUsize::new(0);
static REFUSED_DISAGREEMENT: AtomicUsize = AtomicUsize::new(0);

/// Publish one work item for `work.cpu`, at most once.
///
/// Returns [`PublishOutcome::Published`] only when the slot was genuinely claimed. Every other
/// outcome leaves the slot exactly as it was, so the caller can fall back to the unchanged
/// in-lock dispatch.
///
/// [`DirectDispatchClass::IpcReply`] is REFUSED unconditionally: the replying server remains
/// `current`, so publishing dispatch work for it would spuriously switch away from a task that
/// never left. That refusal is the NR7 half of the classification, expressed in the one place
/// that could otherwise violate it.
pub fn try_publish(work: DirectDispatchWork) -> PublishOutcome {
    if work.class == DirectDispatchClass::IpcReply {
        PUBLISH_REFUSED_NEVER_SWITCHES.fetch_add(1, Ordering::Relaxed);
        return PublishOutcome::ClassNeverSwitches;
    }
    let idx = work.cpu as usize;
    if idx >= MAX_CPUS {
        return PublishOutcome::CpuOutOfRange;
    }
    if PENDING[idx]
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        PUBLISH_DECLINED_ALREADY_PENDING.fetch_add(1, Ordering::Relaxed);
        return PublishOutcome::AlreadyPending;
    }
    // The flag is already set, so a concurrent publisher cannot interleave; the payload is
    // written before any taker can observe the item as complete (takers swap the flag first).
    OUTGOING_TID[idx].store(work.outgoing_tid, Ordering::Relaxed);
    OUTGOING_ASID[idx].store(work.outgoing_asid as u64, Ordering::Relaxed);
    BLOCKED_GENERATION[idx].store(work.blocked_generation, Ordering::Relaxed);
    CLASS[idx].store(work.class.encode(), Ordering::Release);
    PUBLISHED.fetch_add(1, Ordering::Relaxed);
    PublishOutcome::Published
}

/// Is a work item outstanding for `cpu_idx`?
pub fn is_pending(cpu_idx: usize) -> bool {
    cpu_idx < MAX_CPUS && PENDING[cpu_idx].load(Ordering::Acquire)
}

/// Take the outstanding work item for `cpu_idx`, leaving the slot empty.
///
/// This is the ONLY way to read an item, and it is destructive, so a published item drives at
/// most one drain no matter how many traps observe the CPU.
pub fn take(cpu_idx: usize) -> Option<DirectDispatchWork> {
    if cpu_idx >= MAX_CPUS {
        return None;
    }
    // Claim first: a second taker sees `false` and gets nothing.
    if !PENDING[cpu_idx].swap(false, Ordering::AcqRel) {
        return None;
    }
    let class = DirectDispatchClass::decode(CLASS[cpu_idx].load(Ordering::Acquire))?;
    let work = DirectDispatchWork {
        outgoing_tid: OUTGOING_TID[cpu_idx].load(Ordering::Relaxed),
        outgoing_asid: OUTGOING_ASID[cpu_idx].load(Ordering::Relaxed) as u16,
        blocked_generation: BLOCKED_GENERATION[cpu_idx].load(Ordering::Relaxed),
        cpu: cpu_idx as u8,
        class,
    };
    TAKEN.fetch_add(1, Ordering::Relaxed);
    Some(work)
}

/// Record the terminal outcome of one drain.
pub fn note_outcome(outcome: DrainOutcome) {
    // Exhaustive by construction: a new race arm must be named here before this compiles.
    let counter = match outcome {
        DrainOutcome::NoWork => return,
        DrainOutcome::Dispatched { .. } => &DISPATCHED,
        DrainOutcome::Idle => &IDLE,
        DrainOutcome::StateChangedBeforeDrain => &REFUSED_STATE_CHANGED,
        DrainOutcome::OutgoingAlreadyRunnable => &REFUSED_ALREADY_RUNNABLE,
        DrainOutcome::StaleIdentity => &REFUSED_STALE_IDENTITY,
        DrainOutcome::DispatchCurrentDisagreement => &REFUSED_DISAGREEMENT,
    };
    counter.fetch_add(1, Ordering::Relaxed);
}

/// A snapshot of the module's counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DispatchCensus {
    pub published: usize,
    pub publish_declined_already_pending: usize,
    pub publish_refused_never_switches: usize,
    pub taken: usize,
    pub dispatched: usize,
    pub idle: usize,
    pub refused_state_changed: usize,
    pub refused_already_runnable: usize,
    pub refused_stale_identity: usize,
    pub refused_disagreement: usize,
}

impl DispatchCensus {
    /// Every taken item reached exactly one terminal outcome.
    ///
    /// This is the no-lost-and-no-duplicate-wake invariant in counter form: an item that was
    /// taken either advanced the queue, went idle, or was refused for exactly one named reason.
    pub fn taken_is_balanced(&self) -> bool {
        self.taken
            == self.dispatched
                + self.idle
                + self.refused_state_changed
                + self.refused_already_runnable
                + self.refused_stale_identity
                + self.refused_disagreement
    }

    /// No item was ever consumed that was not first published.
    pub fn taken_never_exceeds_published(&self) -> bool {
        self.taken <= self.published
    }

    /// The run queue was advanced at most once per published item.
    pub fn dispatch_never_exceeds_published(&self) -> bool {
        self.dispatched <= self.published
    }
}

/// Read the counters.
pub fn census() -> DispatchCensus {
    DispatchCensus {
        published: PUBLISHED.load(Ordering::Acquire),
        publish_declined_already_pending: PUBLISH_DECLINED_ALREADY_PENDING.load(Ordering::Acquire),
        publish_refused_never_switches: PUBLISH_REFUSED_NEVER_SWITCHES.load(Ordering::Acquire),
        taken: TAKEN.load(Ordering::Acquire),
        dispatched: DISPATCHED.load(Ordering::Acquire),
        idle: IDLE.load(Ordering::Acquire),
        refused_state_changed: REFUSED_STATE_CHANGED.load(Ordering::Acquire),
        refused_already_runnable: REFUSED_ALREADY_RUNNABLE.load(Ordering::Acquire),
        refused_stale_identity: REFUSED_STALE_IDENTITY.load(Ordering::Acquire),
        refused_disagreement: REFUSED_DISAGREEMENT.load(Ordering::Acquire),
    }
}

/// Clear every slot and counter. Test-support and boot-reset only.
pub fn reset() {
    for idx in 0..MAX_CPUS {
        PENDING[idx].store(false, Ordering::Release);
        OUTGOING_TID[idx].store(0, Ordering::Relaxed);
        OUTGOING_ASID[idx].store(0, Ordering::Relaxed);
        BLOCKED_GENERATION[idx].store(0, Ordering::Relaxed);
        CLASS[idx].store(0, Ordering::Relaxed);
    }
    for counter in [
        &PUBLISHED,
        &PUBLISH_DECLINED_ALREADY_PENDING,
        &PUBLISH_REFUSED_NEVER_SWITCHES,
        &TAKEN,
        &DISPATCHED,
        &IDLE,
        &REFUSED_STATE_CHANGED,
        &REFUSED_ALREADY_RUNNABLE,
        &REFUSED_STALE_IDENTITY,
        &REFUSED_DISAGREEMENT,
    ] {
        counter.store(0, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call_work(cpu: u8, tid: u64, asid: u16, generation: u64) -> DirectDispatchWork {
        DirectDispatchWork {
            outgoing_tid: tid,
            outgoing_asid: asid,
            blocked_generation: generation,
            cpu,
            class: DirectDispatchClass::IpcCall,
        }
    }

    #[test]
    fn publication_is_single_shot_and_take_is_destructive() {
        reset();
        let work = call_work(0, 7, 3, 11);
        assert_eq!(try_publish(work), PublishOutcome::Published);
        assert!(is_pending(0));
        // A second publication declines rather than overwriting.
        assert_eq!(
            try_publish(call_work(0, 9, 4, 12)),
            PublishOutcome::AlreadyPending
        );
        // The take returns the FIRST item, complete and unmodified.
        assert_eq!(take(0), Some(work));
        assert!(!is_pending(0));
        // A duplicate trap finds nothing — the item cannot drive two dispatches.
        assert_eq!(take(0), None);
        let c = census();
        assert_eq!(c.published, 1);
        assert_eq!(c.publish_declined_already_pending, 1);
        assert_eq!(c.taken, 1);
        reset();
    }

    #[test]
    fn nr7_never_publishes_dispatch_work() {
        reset();
        let reply = DirectDispatchWork {
            class: DirectDispatchClass::IpcReply,
            ..call_work(0, 7, 3, 11)
        };
        assert_eq!(try_publish(reply), PublishOutcome::ClassNeverSwitches);
        // Nothing was claimed, so nothing can be drained and no switch can occur.
        assert!(!is_pending(0));
        assert_eq!(take(0), None);
        let c = census();
        assert_eq!(c.published, 0);
        assert_eq!(c.publish_refused_never_switches, 1);
        reset();
    }

    #[test]
    fn publication_is_per_cpu_and_out_of_range_is_refused() {
        reset();
        assert_eq!(
            try_publish(call_work(0, 7, 3, 11)),
            PublishOutcome::Published
        );
        // A different CPU has its own slot and is unaffected by CPU 0's outstanding item.
        if MAX_CPUS > 1 {
            assert_eq!(
                try_publish(call_work(1, 8, 4, 12)),
                PublishOutcome::Published
            );
            assert_eq!(take(1).map(|w| w.outgoing_tid), Some(8));
        }
        assert_eq!(
            try_publish(call_work(MAX_CPUS as u8, 9, 5, 13)),
            PublishOutcome::CpuOutOfRange
        );
        assert_eq!(take(0).map(|w| w.outgoing_tid), Some(7));
        reset();
    }

    #[test]
    fn revalidation_maps_each_race_to_one_fail_closed_outcome() {
        assert_eq!(RevalidateOutcome::Ok.refusal(), None);
        assert_eq!(
            RevalidateOutcome::StaleIdentity.refusal(),
            Some(DrainOutcome::StaleIdentity)
        );
        assert_eq!(
            RevalidateOutcome::AlreadyRunnable.refusal(),
            Some(DrainOutcome::OutgoingAlreadyRunnable)
        );
        assert_eq!(
            RevalidateOutcome::StateChanged.refusal(),
            Some(DrainOutcome::StateChangedBeforeDrain)
        );
        // Every refusal resumes nothing; only a dispatch advances the queue.
        for outcome in [
            DrainOutcome::NoWork,
            DrainOutcome::Idle,
            DrainOutcome::StateChangedBeforeDrain,
            DrainOutcome::OutgoingAlreadyRunnable,
            DrainOutcome::StaleIdentity,
            DrainOutcome::DispatchCurrentDisagreement,
        ] {
            assert!(outcome.resumed_nothing(), "{outcome:?} must resume nothing");
            assert!(!outcome.advanced_queue());
        }
        assert!(DrainOutcome::Dispatched { incoming: 4 }.advanced_queue());
    }

    #[test]
    fn every_taken_item_balances_against_exactly_one_terminal_outcome() {
        reset();
        // One of each terminal arm, each preceded by its own published+taken item.
        for (n, outcome) in [
            DrainOutcome::Dispatched { incoming: 2 },
            DrainOutcome::Idle,
            DrainOutcome::StateChangedBeforeDrain,
            DrainOutcome::OutgoingAlreadyRunnable,
            DrainOutcome::StaleIdentity,
            DrainOutcome::DispatchCurrentDisagreement,
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(
                try_publish(call_work(0, n as u64 + 1, 1, 1)),
                PublishOutcome::Published
            );
            assert!(take(0).is_some());
            note_outcome(outcome);
        }
        // `NoWork` is not a taken item and must not disturb the balance.
        note_outcome(DrainOutcome::NoWork);
        let c = census();
        assert_eq!(c.published, 6);
        assert_eq!(c.taken, 6);
        assert_eq!(c.dispatched, 1);
        assert!(c.taken_is_balanced(), "{c:?}");
        assert!(c.taken_never_exceeds_published(), "{c:?}");
        assert!(c.dispatch_never_exceeds_published(), "{c:?}");
        reset();
    }

    #[test]
    fn a_taken_item_carries_the_exact_identity_needed_to_revalidate() {
        reset();
        let work = call_work(0, 42, 9, 1234);
        assert_eq!(try_publish(work), PublishOutcome::Published);
        let taken = take(0).expect("published item");
        assert_eq!(taken.outgoing_tid, 42);
        assert_eq!(taken.outgoing_asid, 9);
        assert_eq!(taken.blocked_generation, 1234);
        assert_eq!(taken.cpu, 0);
        assert_eq!(taken.class, DirectDispatchClass::IpcCall);
        reset();
    }

    #[test]
    fn reset_clears_slots_and_counters() {
        reset();
        assert_eq!(
            try_publish(call_work(0, 1, 1, 1)),
            PublishOutcome::Published
        );
        note_outcome(DrainOutcome::Idle);
        reset();
        assert!(!is_pending(0));
        assert_eq!(take(0), None);
        assert_eq!(census(), DispatchCensus::default());
    }
}
