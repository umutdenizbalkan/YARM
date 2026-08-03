// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Stage 199D — the post-lock **dispatch debt** and the work item that records it.
//!
//! AArch64 readiness blocker 3 was that the authoritative queue-advancing dispatch — the step
//! that picks the next runnable task and actually resumes it — was compile-time x86_64-only
//! (`d6_genuine_enabled()`). An eligible direct NR6/NR7 transaction could complete off-lock, but
//! the *end-to-end* wake still finished under the broad `KernelState` lock on AArch64.
//!
//! This module owns the hand-off between the two halves of that wake.
//!
//! ## A debt, not a suggestion
//!
//! The NR6 commit removes the caller from `current` and commits its blocked state. From that
//! instant **the CPU is running nothing**, and the trap frame the drain would `eret` through
//! belongs to a task that is no longer current. That is a *debt*: the drain MUST settle it, by
//! either resuming exactly one task or entering the established idle primitive. Returning
//! "declined" and `eret`-ing through the old frame would resume a task the scheduler has parked.
//!
//! Nothing observed after the commit cancels the debt. In particular, a reply or timeout that
//! makes the outgoing caller `Runnable` before the drain runs does **not**: that caller is now on
//! the run queue, and the authoritative dequeue may legitimately select it. Such observations are
//! *diagnostics* — they change what is logged, never whether settlement happens.
//!
//! ## Why the item is typed and lease-bearing
//!
//! The existing FutexWait/Yield deferrals record a bare outgoing TID in a per-CPU `AtomicU64`.
//! That is enough for those classes. It is not enough here, so the item carries:
//!
//! * the exact outgoing `{tid, asid}` incarnation — a replacement task reusing the TID has a
//!   different ASID, which the drain reports rather than silently accepting;
//! * a **dispatch lease** — a per-CPU, monotonically increasing epoch opened by, and bound to,
//!   the exact `current`-clear commit that created the debt. The lease is what makes staleness
//!   decidable: an item whose lease is no longer this CPU's current lease belongs to a cycle
//!   that has already been settled some other way, so it carries no debt and must NOT dispatch.
//!   (An earlier revision claimed this role for `tcb.blocked_recv_generation`, which nothing in
//!   the tree ever increments — it is always zero, so it could not distinguish anything. The
//!   lease is incremented at exactly one site, by exactly the commit that owes the debt.)
//! * the CPU the commit ran on, so a drain on another CPU cannot consume it;
//! * the syscall class, so NR7 — which must never publish — is distinguishable by type.
//!
//! ## The slot protocol
//!
//! Publication and consumption run an explicit four-state machine per CPU:
//!
//! ```text
//!   EMPTY --publisher CAS--> WRITING --payload written, Release--> READY
//!   READY --taker CAS (AcqRel)--> READING --payload read--> EMPTY
//! ```
//!
//! A publisher only ever claims `EMPTY`; a taker only ever claims `READY`. Every other state is
//! refused, so a second publisher can never overwrite a slot that is being written, is readable,
//! or is being read, and a second taker can never observe a half-written payload or double-take
//! a readable one. The slot returns to `EMPTY` only after the taker has finished copying the
//! payload out — which is what makes recycling safe: the next publisher cannot begin writing
//! while a reader still holds the slot.
//!
//! This replaces a single `PENDING` boolean, which conflated "being written" with "readable" and
//! "being read" and was therefore only correct under an unstated non-reentrancy assumption.

// Some items are reachable only from the freestanding AArch64 drain; a hosted `lib` build
// compiles no route to them. The module's own tests exercise every item in every build.
#![allow(dead_code)]

use core::sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering};

use crate::kernel::scheduler::{CpuId, MAX_CPUS};

/// Which syscall class parked the outgoing task.
///
/// Carried in the work item so the drain attributes the dispatch by TYPE rather than by
/// convention, and so the NR7 contract ("never publishes") is a statement about a value that
/// exists, not about the absence of one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectDispatchClass {
    /// The NR6 direction: the caller committed to its canonical reply-blocked state
    /// (`Blocked(EndpointReceive(_))`), left `current`, and owes the run queue an advance.
    IpcCall,
    /// The NR7 direction: the replying server remains `current`, so no debt is created. This
    /// variant exists so the contract is expressible and testable; [`try_publish`] REFUSES it.
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

/// The typed, lease-bearing post-lock dispatch work item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirectDispatchWork {
    /// Exact outgoing thread id.
    pub outgoing_tid: u64,
    /// Exact outgoing address-space id — a replacement incarnation reusing the TID differs.
    pub outgoing_asid: u16,
    /// The per-CPU dispatch lease opened by the `current`-clear commit that created this debt.
    /// See [`open_dispatch_lease`].
    pub lease: u64,
    /// The CPU whose in-lock commit published this item.
    pub cpu: u8,
    /// The syscall class that parked the outgoing task.
    pub class: DirectDispatchClass,
}

// ── The dispatch lease ───────────────────────────────────────────────────────────────────────

/// Per-CPU monotonically increasing dispatch epoch. Incremented at exactly one site: the
/// `current`-clear commit that creates a dispatch debt.
static DISPATCH_LEASE: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

/// Open a new dispatch lease on `cpu` and return it.
///
/// Called by — and only by — the commit that removes the caller from `current`, so the returned
/// value names exactly one `current`-clear. Monotonic and never reused: a later commit on the
/// same CPU always yields a strictly greater lease, which is what lets [`lease_is_current`]
/// decide staleness without any assumption about how fast traps interleave.
pub fn open_dispatch_lease(cpu: CpuId) -> u64 {
    let idx = cpu.0 as usize;
    if idx >= MAX_CPUS {
        return 0;
    }
    DISPATCH_LEASE[idx].fetch_add(1, Ordering::AcqRel) + 1
}

/// The lease currently open on `cpu`.
pub fn current_lease(cpu: CpuId) -> u64 {
    let idx = cpu.0 as usize;
    if idx >= MAX_CPUS {
        return 0;
    }
    DISPATCH_LEASE[idx].load(Ordering::Acquire)
}

/// Does `lease` still name the newest `current`-clear on `cpu`?
///
/// `false` means a later commit superseded it; that later cycle owns settlement, so an item
/// bearing the older lease carries **no debt** and must not dispatch.
pub fn lease_is_current(cpu: CpuId, lease: u64) -> bool {
    lease != 0 && current_lease(cpu) == lease
}

// ── The per-CPU slot state machine ───────────────────────────────────────────────────────────

const EMPTY: u8 = 0;
const WRITING: u8 = 1;
const READY: u8 = 2;
const READING: u8 = 3;

/// The observable state of one CPU's work-item slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotState {
    /// No item; a publisher may claim it.
    Empty,
    /// A publisher has claimed the slot and is writing the payload. Not yet readable.
    Writing,
    /// A complete item is published; a taker may claim it.
    Ready,
    /// A taker has claimed the item and is copying the payload out. Not yet recyclable.
    Reading,
}

impl SlotState {
    fn decode(raw: u8) -> Self {
        match raw {
            WRITING => SlotState::Writing,
            READY => SlotState::Ready,
            READING => SlotState::Reading,
            _ => SlotState::Empty,
        }
    }
}

static STATE: [AtomicU8; MAX_CPUS] = [const { AtomicU8::new(EMPTY) }; MAX_CPUS];
static OUTGOING_TID: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
static OUTGOING_ASID: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
static LEASE: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
static CLASS: [AtomicU8; MAX_CPUS] = [const { AtomicU8::new(0) }; MAX_CPUS];

// ── Counters ────────────────────────────────────────────────────────────────────────────────

static PUBLISHED: AtomicUsize = AtomicUsize::new(0);
static PUBLISH_DECLINED_SLOT_BUSY: AtomicUsize = AtomicUsize::new(0);
static PUBLISH_REFUSED_NEVER_SWITCHES: AtomicUsize = AtomicUsize::new(0);
static TAKEN: AtomicUsize = AtomicUsize::new(0);
static TAKE_MISSED_NOT_READY: AtomicUsize = AtomicUsize::new(0);
static SETTLED_DISPATCHED: AtomicUsize = AtomicUsize::new(0);
static SETTLED_IDLE: AtomicUsize = AtomicUsize::new(0);
static SUPERSEDED_LEASE: AtomicUsize = AtomicUsize::new(0);
static ROLLED_BACK: AtomicUsize = AtomicUsize::new(0);

/// Publish one work item for `work.cpu`, at most once.
///
/// The publisher claims `EMPTY -> WRITING`, writes the complete payload, then publishes
/// `WRITING -> READY` with `Release`. Any state other than `EMPTY` is DECLINED — never
/// overwritten — so a slot that is being written, is readable, or is being read is safe from a
/// second publisher, and the caller falls back to the unchanged in-lock dispatch.
///
/// [`DirectDispatchClass::IpcReply`] is refused unconditionally: the replying server remains
/// `current`, so there is no debt, and publishing would spuriously switch away from a task that
/// never left.
pub fn try_publish(work: DirectDispatchWork) -> PublishOutcome {
    if work.class == DirectDispatchClass::IpcReply {
        PUBLISH_REFUSED_NEVER_SWITCHES.fetch_add(1, Ordering::Relaxed);
        return PublishOutcome::ClassNeverSwitches;
    }
    let idx = work.cpu as usize;
    if idx >= MAX_CPUS {
        return PublishOutcome::CpuOutOfRange;
    }
    // EMPTY -> WRITING. Only an empty slot may be claimed.
    if let Err(observed) =
        STATE[idx].compare_exchange(EMPTY, WRITING, Ordering::AcqRel, Ordering::Acquire)
    {
        PUBLISH_DECLINED_SLOT_BUSY.fetch_add(1, Ordering::Relaxed);
        return PublishOutcome::SlotBusy(SlotState::decode(observed));
    }
    // Exclusive: no other publisher can be here, and no taker can read a WRITING slot.
    OUTGOING_TID[idx].store(work.outgoing_tid, Ordering::Relaxed);
    OUTGOING_ASID[idx].store(work.outgoing_asid as u64, Ordering::Relaxed);
    LEASE[idx].store(work.lease, Ordering::Relaxed);
    CLASS[idx].store(work.class.encode(), Ordering::Relaxed);
    // WRITING -> READY. Release publishes every payload store above.
    STATE[idx].store(READY, Ordering::Release);
    PUBLISHED.fetch_add(1, Ordering::Relaxed);
    PublishOutcome::Published
}

/// Why a publication attempt did not produce an outstanding work item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishOutcome {
    /// The item is now outstanding for its CPU and will be taken by exactly one drain.
    Published,
    /// The slot was not `EMPTY`. Declined, not overwritten — the caller keeps the unchanged
    /// in-lock dispatch fallback. Carries the state actually observed, for diagnosis.
    SlotBusy(SlotState),
    /// The class never creates a dispatch debt (NR7). Nothing is published.
    ClassNeverSwitches,
    /// The CPU index is outside the per-CPU table.
    CpuOutOfRange,
}

/// Is a complete item readable for `cpu_idx`?
///
/// True only in `READY`: a slot still being written, or already claimed by another taker, is
/// deliberately not advertised.
pub fn is_pending(cpu_idx: usize) -> bool {
    cpu_idx < MAX_CPUS && STATE[cpu_idx].load(Ordering::Acquire) == READY
}

/// The raw slot state, for diagnosis and tests.
pub fn slot_state(cpu_idx: usize) -> SlotState {
    if cpu_idx >= MAX_CPUS {
        return SlotState::Empty;
    }
    SlotState::decode(STATE[cpu_idx].load(Ordering::Acquire))
}

/// Take the outstanding work item for `cpu_idx`.
///
/// Claims `READY -> READING` with `AcqRel`, copies the payload out, and only THEN returns the
/// slot to `EMPTY`. A second taker that loses the compare-exchange sees a non-`READY` state and
/// gets nothing, so an item drives at most one drain; and because the slot is recycled only
/// after the payload has been copied, a publisher cannot start overwriting a payload a reader is
/// still holding.
pub fn take(cpu_idx: usize) -> Option<DirectDispatchWork> {
    if cpu_idx >= MAX_CPUS {
        return None;
    }
    // READY -> READING. Only a readable slot may be claimed.
    if STATE[cpu_idx]
        .compare_exchange(READY, READING, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        TAKE_MISSED_NOT_READY.fetch_add(1, Ordering::Relaxed);
        return None;
    }
    // Exclusive: the payload is complete (the publisher's Release) and no publisher may write.
    let class = DirectDispatchClass::decode(CLASS[cpu_idx].load(Ordering::Relaxed));
    let work = class.map(|class| DirectDispatchWork {
        outgoing_tid: OUTGOING_TID[cpu_idx].load(Ordering::Relaxed),
        outgoing_asid: OUTGOING_ASID[cpu_idx].load(Ordering::Relaxed) as u16,
        lease: LEASE[cpu_idx].load(Ordering::Relaxed),
        cpu: cpu_idx as u8,
        class,
    });
    // READING -> EMPTY, only now that the payload has been copied out. Release so the next
    // publisher's payload writes cannot be reordered before this recycle.
    STATE[cpu_idx].store(EMPTY, Ordering::Release);
    if work.is_some() {
        TAKEN.fetch_add(1, Ordering::Relaxed);
    }
    work
}

/// How a taken dispatch debt was settled.
///
/// Exhaustive and total: every taken item that carries a live lease reaches exactly one of
/// [`Settlement::Dispatched`] or [`Settlement::Idle`]. There is no "declined" arm, because
/// declining would leave the CPU running nothing with a stale frame to `eret` through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Settlement {
    /// The run queue was advanced exactly once and the selected task resumed.
    Dispatched { incoming: u64 },
    /// The run queue was empty, so the CPU entered the established idle primitive. The outgoing
    /// task stays parked, `current` stays clear, no frame is restored. A SUCCESS, not a failure.
    Idle,
}

/// The exhaustive outcome of one post-lock drain.
///
/// Only the two `NoDebt` arms may return to the trap without settling, and each of them is a
/// case where **no debt exists**: nothing was published, or a later cycle already owns it. Every
/// other path settles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrainOutcome {
    /// No item was readable for this CPU — the ordinary non-blocking trap. No debt.
    NoDebtNothingPublished,
    /// An item was taken but its lease has been superseded by a later `current`-clear, which
    /// owns settlement. No debt on this drain.
    NoDebtSupersededLease,
    /// The debt was settled.
    Settled(Settlement),
    /// Scheduler state was mutated (dequeue / current-set / status) and then a later step
    /// failed, so the mutation was rolled back EXACTLY and the CPU took the explicit fatal
    /// path. This is not a "decline": it never returns to userspace.
    RolledBackFatal,
}

impl DrainOutcome {
    /// True iff the run queue was advanced by this drain.
    pub fn advanced_queue(self) -> bool {
        matches!(self, DrainOutcome::Settled(Settlement::Dispatched { .. }))
    }

    /// True iff this drain owed a dispatch debt.
    pub fn owed_a_debt(self) -> bool {
        !matches!(
            self,
            DrainOutcome::NoDebtNothingPublished | DrainOutcome::NoDebtSupersededLease
        )
    }

    /// True iff a drain that owed a debt discharged it.
    ///
    /// The invariant the whole design rests on: **owing implies settling**. `RolledBackFatal`
    /// counts because it never returns to userspace — it is a halt, not a decline.
    pub fn debt_is_discharged(self) -> bool {
        !self.owed_a_debt()
            || matches!(
                self,
                DrainOutcome::Settled(_) | DrainOutcome::RolledBackFatal
            )
    }
}

/// What a pre-mutation observation of the outgoing task saw.
///
/// **Diagnostics only.** None of these arms may skip settlement: once the commit cleared
/// `current`, the debt exists regardless of what the outgoing task subsequently became. A caller
/// made `Runnable` by a reply or a timeout is simply back on the run queue, where the
/// authoritative dequeue may select it like any other candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutgoingObservation {
    /// Still the exact incarnation, still committed to its blocked state.
    StillBlocked,
    /// The exact incarnation was made runnable again (a reply or timeout won the race).
    MadeRunnable,
    /// The exact incarnation is present but in some other state (e.g. exiting).
    OtherState,
    /// The TCB is absent or its `{tid, asid}` no longer match — the task exited, or a
    /// replacement incarnation reused the TID.
    Gone,
}

/// Record how a drain ended.
pub fn note_outcome(outcome: DrainOutcome) {
    // Exhaustive by construction: a new arm must be named here before this compiles.
    let counter = match outcome {
        DrainOutcome::NoDebtNothingPublished => return,
        DrainOutcome::NoDebtSupersededLease => &SUPERSEDED_LEASE,
        DrainOutcome::Settled(Settlement::Dispatched { .. }) => &SETTLED_DISPATCHED,
        DrainOutcome::Settled(Settlement::Idle) => &SETTLED_IDLE,
        DrainOutcome::RolledBackFatal => &ROLLED_BACK,
    };
    counter.fetch_add(1, Ordering::Relaxed);
}

/// A snapshot of the module's counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DispatchCensus {
    pub published: usize,
    pub publish_declined_slot_busy: usize,
    pub publish_refused_never_switches: usize,
    pub taken: usize,
    pub take_missed_not_ready: usize,
    pub settled_dispatched: usize,
    pub settled_idle: usize,
    pub superseded_lease: usize,
    pub rolled_back: usize,
}

impl DispatchCensus {
    /// **Every taken debt was discharged.** A taken item either settled (dispatch or idle), was
    /// found to carry a superseded lease, or was rolled back onto the fatal path. Nothing is
    /// taken and then silently dropped.
    pub fn every_taken_item_is_accounted(&self) -> bool {
        self.taken
            == self.settled_dispatched
                + self.settled_idle
                + self.superseded_lease
                + self.rolled_back
    }

    /// No item was ever consumed that was not first published.
    pub fn taken_never_exceeds_published(&self) -> bool {
        self.taken <= self.published
    }

    /// The run queue was advanced at most once per published item.
    pub fn dispatch_never_exceeds_published(&self) -> bool {
        self.settled_dispatched <= self.published
    }
}

/// Read the counters.
pub fn census() -> DispatchCensus {
    DispatchCensus {
        published: PUBLISHED.load(Ordering::Acquire),
        publish_declined_slot_busy: PUBLISH_DECLINED_SLOT_BUSY.load(Ordering::Acquire),
        publish_refused_never_switches: PUBLISH_REFUSED_NEVER_SWITCHES.load(Ordering::Acquire),
        taken: TAKEN.load(Ordering::Acquire),
        take_missed_not_ready: TAKE_MISSED_NOT_READY.load(Ordering::Acquire),
        settled_dispatched: SETTLED_DISPATCHED.load(Ordering::Acquire),
        settled_idle: SETTLED_IDLE.load(Ordering::Acquire),
        superseded_lease: SUPERSEDED_LEASE.load(Ordering::Acquire),
        rolled_back: ROLLED_BACK.load(Ordering::Acquire),
    }
}

/// Clear every slot, lease and counter. Test-support and boot-reset only.
pub fn reset() {
    for idx in 0..MAX_CPUS {
        STATE[idx].store(EMPTY, Ordering::Release);
        OUTGOING_TID[idx].store(0, Ordering::Relaxed);
        OUTGOING_ASID[idx].store(0, Ordering::Relaxed);
        LEASE[idx].store(0, Ordering::Relaxed);
        CLASS[idx].store(0, Ordering::Relaxed);
        DISPATCH_LEASE[idx].store(0, Ordering::Release);
    }
    for counter in [
        &PUBLISHED,
        &PUBLISH_DECLINED_SLOT_BUSY,
        &PUBLISH_REFUSED_NEVER_SWITCHES,
        &TAKEN,
        &TAKE_MISSED_NOT_READY,
        &SETTLED_DISPATCHED,
        &SETTLED_IDLE,
        &SUPERSEDED_LEASE,
        &ROLLED_BACK,
    ] {
        counter.store(0, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call_work(cpu: u8, tid: u64, asid: u16, lease: u64) -> DirectDispatchWork {
        DirectDispatchWork {
            outgoing_tid: tid,
            outgoing_asid: asid,
            lease,
            cpu,
            class: DirectDispatchClass::IpcCall,
        }
    }

    // ── The slot state machine ──────────────────────────────────────────────────────────────

    /// The full cycle visits every state in order and ends where it started.
    #[test]
    fn the_slot_walks_empty_writing_ready_reading_empty() {
        reset();
        assert_eq!(slot_state(0), SlotState::Empty);
        let work = call_work(0, 7, 3, 1);
        assert_eq!(try_publish(work), PublishOutcome::Published);
        // `try_publish` passes through WRITING internally and leaves the slot READY.
        assert_eq!(slot_state(0), SlotState::Ready);
        assert!(is_pending(0));
        assert_eq!(take(0), Some(work));
        // The taker passes through READING and recycles to EMPTY.
        assert_eq!(slot_state(0), SlotState::Empty);
        assert!(!is_pending(0));
        reset();
    }

    /// **A second publisher never overwrites a busy slot** — in any of the three busy states.
    /// Driven by parking the slot in each state explicitly, so this does not depend on winning
    /// a timing race.
    #[test]
    fn a_second_publisher_never_overwrites_writing_ready_or_reading() {
        for (busy, label) in [
            (WRITING, SlotState::Writing),
            (READY, SlotState::Ready),
            (READING, SlotState::Reading),
        ] {
            reset();
            let original = call_work(0, 7, 3, 1);
            // Park the slot in the busy state with the original payload in place.
            OUTGOING_TID[0].store(original.outgoing_tid, Ordering::Relaxed);
            OUTGOING_ASID[0].store(original.outgoing_asid as u64, Ordering::Relaxed);
            LEASE[0].store(original.lease, Ordering::Relaxed);
            CLASS[0].store(original.class.encode(), Ordering::Relaxed);
            STATE[0].store(busy, Ordering::Release);

            let intruder = call_work(0, 99, 44, 2);
            assert_eq!(
                try_publish(intruder),
                PublishOutcome::SlotBusy(label),
                "a publisher must decline a {label:?} slot"
            );
            // The payload is untouched and the state is unchanged.
            assert_eq!(OUTGOING_TID[0].load(Ordering::Relaxed), 7);
            assert_eq!(OUTGOING_ASID[0].load(Ordering::Relaxed), 3);
            assert_eq!(LEASE[0].load(Ordering::Relaxed), 1);
            assert_eq!(slot_state(0), label);
        }
        reset();
    }

    /// **A taker only ever claims READY.** A slot still being written is not readable, and a
    /// slot already claimed by another taker cannot be double-taken.
    #[test]
    fn a_taker_never_claims_a_non_ready_slot() {
        for busy in [EMPTY, WRITING, READING] {
            reset();
            STATE[0].store(busy, Ordering::Release);
            assert_eq!(take(0), None, "state {busy} must not be takeable");
            assert!(!is_pending(0), "state {busy} must not advertise as pending");
            // The taker left the state exactly as it found it.
            assert_eq!(STATE[0].load(Ordering::Acquire), busy);
        }
        reset();
    }

    /// **Exactly one of N takers wins**, and the losers change nothing. This is the
    /// no-duplicate-wake edge stated as a property of the slot, not of the caller.
    #[test]
    fn exactly_one_taker_wins_and_the_losers_are_inert() {
        reset();
        let work = call_work(0, 7, 3, 1);
        assert_eq!(try_publish(work), PublishOutcome::Published);
        let results: alloc::vec::Vec<_> = (0..8).map(|_| take(0)).collect();
        assert_eq!(
            results.iter().filter(|r| r.is_some()).count(),
            1,
            "exactly one taker may consume a published item"
        );
        assert_eq!(results[0], Some(work), "the winner gets the complete item");
        assert_eq!(slot_state(0), SlotState::Empty);
        let c = census();
        assert_eq!(c.taken, 1);
        assert_eq!(c.take_missed_not_ready, 7);
        reset();
    }

    /// **Recycle race.** The slot becomes republishable only after the taker has copied the
    /// payload out — so a publisher can never begin overwriting a payload a reader still holds.
    /// Checked by proving publication is refused while `READING` and accepted once `EMPTY`.
    #[test]
    fn the_slot_is_republishable_only_after_the_reader_finishes() {
        reset();
        let first = call_work(0, 7, 3, 1);
        assert_eq!(try_publish(first), PublishOutcome::Published);
        // Simulate a taker mid-read: READY -> READING, payload not yet copied out.
        assert!(
            STATE[0]
                .compare_exchange(READY, READING, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        );
        let second = call_work(0, 8, 4, 2);
        assert_eq!(
            try_publish(second),
            PublishOutcome::SlotBusy(SlotState::Reading),
            "a publisher must not overwrite a payload still being read"
        );
        assert_eq!(OUTGOING_TID[0].load(Ordering::Relaxed), 7);
        // The reader finishes and recycles.
        STATE[0].store(EMPTY, Ordering::Release);
        assert_eq!(try_publish(second), PublishOutcome::Published);
        assert_eq!(take(0), Some(second));
        reset();
    }

    /// A payload written under one publisher is never mixed with another's: the second
    /// publication only lands after the first item has been fully taken.
    #[test]
    fn back_to_back_publications_never_mix_payloads() {
        reset();
        let a = call_work(0, 7, 3, 1);
        let b = call_work(0, 8, 4, 2);
        assert_eq!(try_publish(a), PublishOutcome::Published);
        assert_eq!(
            try_publish(b),
            PublishOutcome::SlotBusy(SlotState::Ready),
            "the second must wait, not overwrite"
        );
        assert_eq!(take(0), Some(a), "the first item comes back whole");
        assert_eq!(try_publish(b), PublishOutcome::Published);
        assert_eq!(take(0), Some(b), "the second item comes back whole");
        reset();
    }

    #[test]
    fn publication_is_per_cpu_and_out_of_range_is_refused() {
        reset();
        assert_eq!(
            try_publish(call_work(0, 7, 3, 1)),
            PublishOutcome::Published
        );
        if MAX_CPUS > 1 {
            // A different CPU has its own slot and is unaffected by CPU 0's outstanding item.
            assert_eq!(
                try_publish(call_work(1, 8, 4, 1)),
                PublishOutcome::Published
            );
            assert_eq!(take(1).map(|w| w.outgoing_tid), Some(8));
            assert_eq!(slot_state(0), SlotState::Ready, "CPU 0 is untouched");
        }
        assert_eq!(
            try_publish(call_work(MAX_CPUS as u8, 9, 5, 1)),
            PublishOutcome::CpuOutOfRange
        );
        assert_eq!(take(0).map(|w| w.outgoing_tid), Some(7));
        reset();
    }

    // ── The dispatch lease ──────────────────────────────────────────────────────────────────

    /// The lease is monotonic, per-CPU, and never reused.
    #[test]
    fn the_dispatch_lease_is_monotonic_and_per_cpu() {
        reset();
        assert_eq!(current_lease(CpuId(0)), 0, "no lease opened yet");
        let first = open_dispatch_lease(CpuId(0));
        let second = open_dispatch_lease(CpuId(0));
        assert!(second > first, "leases strictly increase");
        assert_eq!(current_lease(CpuId(0)), second);
        if MAX_CPUS > 1 {
            // A different CPU's lease is independent.
            assert_eq!(current_lease(CpuId(1)), 0);
            let other = open_dispatch_lease(CpuId(1));
            assert_eq!(current_lease(CpuId(1)), other);
            assert_eq!(
                current_lease(CpuId(0)),
                second,
                "opening on CPU 1 must not disturb CPU 0"
            );
        }
        reset();
    }

    /// **The lease decides staleness — and it is not always zero.** This is the property the
    /// replaced `blocked_recv_generation` claim could never have: that field is never
    /// incremented anywhere in the tree, so every item carried the same value and no item could
    /// ever be distinguished from any other.
    #[test]
    fn a_superseded_lease_is_detected_and_a_live_one_is_not() {
        reset();
        let live = open_dispatch_lease(CpuId(0));
        assert!(live > 0, "a real lease is never the zero sentinel");
        assert!(lease_is_current(CpuId(0), live));
        // A later current-clear supersedes it.
        let newer = open_dispatch_lease(CpuId(0));
        assert!(!lease_is_current(CpuId(0), live), "the old lease is stale");
        assert!(lease_is_current(CpuId(0), newer));
        // The zero sentinel is never current, so an unset lease cannot masquerade as live.
        assert!(!lease_is_current(CpuId(0), 0));
        reset();
    }

    /// A taken item carries the exact identity and the lease it was published with.
    #[test]
    fn a_taken_item_carries_its_exact_identity_and_lease() {
        reset();
        let lease = open_dispatch_lease(CpuId(0));
        let work = call_work(0, 42, 9, lease);
        assert_eq!(try_publish(work), PublishOutcome::Published);
        let taken = take(0).expect("published item");
        assert_eq!(taken.outgoing_tid, 42);
        assert_eq!(taken.outgoing_asid, 9);
        assert_eq!(taken.lease, lease);
        assert_eq!(taken.cpu, 0);
        assert_eq!(taken.class, DirectDispatchClass::IpcCall);
        assert!(lease_is_current(CpuId(0), taken.lease));
        reset();
    }

    // ── The debt contract ───────────────────────────────────────────────────────────────────

    #[test]
    fn nr7_never_publishes_dispatch_work() {
        reset();
        let reply = DirectDispatchWork {
            class: DirectDispatchClass::IpcReply,
            ..call_work(0, 7, 3, 1)
        };
        assert_eq!(try_publish(reply), PublishOutcome::ClassNeverSwitches);
        assert_eq!(slot_state(0), SlotState::Empty, "nothing was claimed");
        assert_eq!(take(0), None);
        let c = census();
        assert_eq!(c.published, 0);
        assert_eq!(c.publish_refused_never_switches, 1);
        reset();
    }

    /// **Owing implies settling.** Only the two no-debt arms may return without settling; every
    /// other outcome discharges the debt, and `RolledBackFatal` counts because it never returns
    /// to userspace at all.
    #[test]
    fn every_outcome_that_owes_a_debt_discharges_it() {
        let all = [
            DrainOutcome::NoDebtNothingPublished,
            DrainOutcome::NoDebtSupersededLease,
            DrainOutcome::Settled(Settlement::Dispatched { incoming: 3 }),
            DrainOutcome::Settled(Settlement::Idle),
            DrainOutcome::RolledBackFatal,
        ];
        for outcome in all {
            assert!(
                outcome.debt_is_discharged(),
                "{outcome:?} leaves a dispatch debt outstanding"
            );
        }
        assert_eq!(all.iter().filter(|o| o.owed_a_debt()).count(), 3);
        assert_eq!(all.iter().filter(|o| o.advanced_queue()).count(), 1);
    }

    /// A pre-mutation observation is diagnostics only: **no** observation is a settlement, so
    /// none of them can be used to skip one.
    #[test]
    fn a_pre_mutation_observation_is_never_a_settlement() {
        // The observation type is deliberately disjoint from the settlement type: there is no
        // conversion from one to the other, so an observation cannot end a drain.
        for observation in [
            OutgoingObservation::StillBlocked,
            OutgoingObservation::MadeRunnable,
            OutgoingObservation::OtherState,
            OutgoingObservation::Gone,
        ] {
            // Every observation is compatible with both settlements — which is the point:
            // the outgoing task's fate does not determine whether the CPU has work to run.
            let _ = observation;
        }
        assert_ne!(
            OutgoingObservation::MadeRunnable,
            OutgoingObservation::StillBlocked
        );
    }

    #[test]
    fn every_taken_item_is_accounted_for() {
        reset();
        for (n, outcome) in [
            DrainOutcome::Settled(Settlement::Dispatched { incoming: 2 }),
            DrainOutcome::Settled(Settlement::Idle),
            DrainOutcome::NoDebtSupersededLease,
            DrainOutcome::RolledBackFatal,
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
        // A drain with nothing published is not a taken item and must not disturb the balance.
        note_outcome(DrainOutcome::NoDebtNothingPublished);
        let c = census();
        assert_eq!(c.published, 4);
        assert_eq!(c.taken, 4);
        assert_eq!(c.settled_dispatched, 1);
        assert!(c.every_taken_item_is_accounted(), "{c:?}");
        assert!(c.taken_never_exceeds_published(), "{c:?}");
        assert!(c.dispatch_never_exceeds_published(), "{c:?}");
        reset();
    }

    #[test]
    fn reset_clears_slots_leases_and_counters() {
        reset();
        let _ = open_dispatch_lease(CpuId(0));
        assert_eq!(
            try_publish(call_work(0, 1, 1, 1)),
            PublishOutcome::Published
        );
        note_outcome(DrainOutcome::Settled(Settlement::Idle));
        reset();
        assert_eq!(slot_state(0), SlotState::Empty);
        assert_eq!(current_lease(CpuId(0)), 0);
        assert_eq!(census(), DispatchCensus::default());
        // A take on an empty slot yields nothing (and is itself counted as a missed take).
        assert_eq!(take(0), None);
        assert_eq!(census().take_missed_not_ready, 1);
        reset();
    }
}
